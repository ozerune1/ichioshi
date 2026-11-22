use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::mock_http::{Req, mock, reply};

fn endpoints(server: &str) -> Endpoints {
    Endpoints {
        anilist_auth: format!("{server}/anilist/authorize"),
        anilist_api: format!("{server}/graphql"),
        mal_auth: format!("{server}/mal/authorize"),
        mal_token: format!("{server}/mal/token"),
        mal_api: format!("{server}/mal/api"),
        port: 0,
    }
}

fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[test]
fn changes_entries() {
    let watching = Entry {
        status: Status::Watching,
        score: 0.0,
        progress: 3,
    };
    let done = Change::Status(Status::Completed).apply(Some(&watching), Some(12));
    assert_eq!(
        done,
        Entry {
            status: Status::Completed,
            score: 0.0,
            progress: 12
        }
    );
    assert_eq!(
        Change::Progress(40)
            .apply(Some(&watching), Some(12))
            .progress,
        12
    );
    assert_eq!(
        Change::Progress(40).apply(Some(&watching), None).progress,
        40
    );
    assert_eq!(Change::Score(7.5).apply(Some(&watching), None).score, 7.5);
    let new = Change::Status(Status::Planning).apply(None, Some(12));
    assert_eq!(
        new,
        Entry {
            status: Status::Planning,
            score: 0.0,
            progress: 0
        }
    );
    assert_eq!(
        (score_text(0.0), score_text(8.0), score_text(7.5)),
        ("–".into(), "8".into(), "7.5".into())
    );
}

/// A browser doing what the sign-in page asks: open it, run its script.
async fn anilist_browser(url: String, token: &str) {
    let q = query_of(&url);
    assert_eq!(q["response_type"], "token");
    assert_eq!(q["client_id"], "1234");
    let redirect = q["redirect_uri"].clone();
    let page = reqwest::get(&redirect).await.unwrap().text().await.unwrap();
    assert!(
        page.contains("fetch('/anilist/token'"),
        "the page hands the token over: {page}"
    );
    let resp = reqwest::Client::new()
        .post(format!("{redirect}/token"))
        .body(format!(
            "access_token={token}&token_type=Bearer&expires_in=31536000"
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

#[tokio::test]
async fn anilist_signs_in_reads_and_saves() {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let server = mock(move |req: &Req| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let query = body["query"].as_str().unwrap_or("").to_owned();
        let vars = body["variables"].clone();
        log.lock().unwrap().push(body.clone());
        let authed = req.headers.get("authorization").map(String::as_str) == Some("Bearer tok");
        if query.contains("Viewer") {
            return if authed {
                reply(200, json!({"data": {"Viewer": {"id": 77, "name": "jake"}}}))
            } else {
                reply(401, json!({"errors": [{"message": "Invalid token"}], "data": null}))
            };
        }
        if query.contains("MediaListCollection") {
            assert!(authed);
            assert_eq!(vars["user"], 77);
            return match vars["chunk"].as_u64() {
                Some(1) => reply(200, json!({"data": {"MediaListCollection": {"hasNextChunk": true, "lists": [
                    {"entries": [{"mediaId": 1, "status": "CURRENT", "progress": 3, "score": 7.5}]}]}}})),
                _ => reply(200, json!({"data": {"MediaListCollection": {"hasNextChunk": false, "lists": [
                    {"entries": [{"mediaId": 2, "status": "COMPLETED", "progress": 12, "score": 9}]},
                    {"entries": [{"mediaId": 3, "status": "REPEATING", "progress": 1, "score": 0}]}]}}})),
            };
        }
        if query.contains("SaveMediaListEntry") {
            assert!(authed);
            let score = vars["score"].as_f64().unwrap() / 10.0;
            return reply(200, json!({"data": {"SaveMediaListEntry": {"mediaId": vars["media"], "status": vars["status"],
                "progress": vars["progress"], "score": score}}}));
        }
        if query.contains("idMal") {
            return reply(200, json!({"data": {"Page": {"media": [{"id": 1, "idMal": 52991}, {"id": 2, "idMal": null}]}}}));
        }
        reply(404, json!({}))
    })
    .await;
    let ep = endpoints(&server);
    let http = reqwest::Client::new();

    // Sign in, the browser handing the token back.
    let cancel = AtomicBool::new(false);
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let browser = tokio::spawn(async move { anilist_browser(url_rx.await.unwrap(), "tok").await });
    let signed = anilist::sign_in(&http, &ep, "1234", &cancel, move |url| {
        url_tx.send(url).unwrap();
    })
    .await
    .unwrap();
    browser.await.unwrap();
    assert_eq!(
        (
            signed.token.as_str(),
            signed.user.name.as_str(),
            signed.user.id
        ),
        ("tok", "jake", 77)
    );
    assert!(signed.expires_at > chrono::Utc::now().timestamp() + 300 * 24 * 3600);

    // The list, in two chunks.
    let session = anilist::Session::new(http.clone(), ep.anilist_api.clone(), "tok".into(), 77);
    let list = session.list().await.unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(
        list[&1],
        Entry {
            status: Status::Watching,
            score: 7.5,
            progress: 3
        }
    );
    assert_eq!(list[&2].status, Status::Completed);
    assert_eq!(list[&3].status, Status::Rewatching);

    // Saving: the score goes as 0–100.
    let saved = session
        .save(
            1,
            &Entry {
                status: Status::Paused,
                score: 8.5,
                progress: 4,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        saved,
        Entry {
            status: Status::Paused,
            score: 8.5,
            progress: 4
        }
    );
    let last = seen.lock().unwrap().last().unwrap()["variables"].clone();
    assert_eq!(
        last,
        json!({"media": 1, "status": "PAUSED", "score": 85, "progress": 4})
    );

    // MyAnimeList IDs.
    let ids = anilist::mal_ids(&http, &ep.anilist_api, &[1, 2])
        .await
        .unwrap();
    assert_eq!(ids, HashMap::from([(1, 52991)]));

    // A dead token: signed out.
    let stale = anilist::Session::new(http.clone(), ep.anilist_api.clone(), "old".into(), 77);
    let err = stale.viewer().await.unwrap_err();
    assert!(err.is::<SignedOut>(), "{err:#}");
}

#[tokio::test]
async fn anilist_sign_in_can_be_refused() {
    let ep = endpoints("http://127.0.0.1:9");
    let cancel = AtomicBool::new(false);
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
    let browser = tokio::spawn(async move {
        let q = query_of(&url_rx.await.unwrap());
        reqwest::get(format!("{}?error=access_denied", q["redirect_uri"]))
            .await
            .unwrap();
    });
    let result = anilist::sign_in(&reqwest::Client::new(), &ep, "1234", &cancel, move |url| {
        url_tx.send(url).unwrap();
    })
    .await;
    browser.await.unwrap();
    let err = result.err().expect("refused");
    assert!(format!("{err:#}").contains("access_denied"));
}

#[tokio::test]
async fn mal_signs_in_with_pkce_refreshes_and_saves() {
    let challenge: Arc<Mutex<String>> = Arc::default();
    let seen_challenge = challenge.clone();
    let patches: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let log = patches.clone();
    let server = mock(move |req: &Req| {
        let path = req.target.split('?').next().unwrap().to_owned();
        let auth = req.headers.get("authorization").cloned().unwrap_or_default();
        match (req.method.as_str(), path.as_str()) {
            ("POST", "/mal/token") => {
                let f = req.form();
                assert_eq!(f["client_id"], "abc");
                assert!(!f.contains_key("client_secret"), "no secret for this client");
                match f["grant_type"].as_str() {
                    "authorization_code" => {
                        assert_eq!(f["code"], "c1");
                        assert_eq!(f["code_verifier"], *seen_challenge.lock().unwrap(), "plain PKCE");
                        assert!(f["redirect_uri"].ends_with("/mal"));
                        // Short-lived: refreshed before the next request.
                        reply(200, json!({"token_type": "Bearer", "access_token": "a1", "refresh_token": "r1", "expires_in": 60}))
                    }
                    "refresh_token" if f["refresh_token"] == "r1" => {
                        reply(200, json!({"token_type": "Bearer", "access_token": "a2", "refresh_token": "r2", "expires_in": 2678400}))
                    }
                    _ => reply(400, json!({"error": "invalid_grant"})),
                }
            }
            ("GET", "/mal/api/users/@me") if auth.starts_with("Bearer a") => reply(200, json!({"id": 5, "name": "jake"})),
            ("GET", "/mal/api/users/@me/animelist") => {
                assert_eq!(auth, "Bearer a2");
                let q = req.query();
                if !q.contains_key("offset") {
                    assert_eq!(q["fields"], "list_status");
                    let next = format!("http://{}/mal/api/users/@me/animelist?offset=1000", req.headers["host"]);
                    reply(200, json!({"data": [{"node": {"id": 52991}, "list_status": {"status": "watching", "score": 8, "num_episodes_watched": 3, "is_rewatching": false}}],
                        "paging": {"next": next}}))
                } else {
                    reply(200, json!({"data": [{"node": {"id": 1}, "list_status": {"status": "completed", "score": 0, "num_episodes_watched": 26, "is_rewatching": true}}],
                        "paging": {}}))
                }
            }
            ("PATCH", "/mal/api/anime/52991/my_list_status") => {
                let f = req.form();
                log.lock().unwrap().push(f.clone());
                reply(200, json!({"status": f["status"], "score": f["score"].parse::<u32>().unwrap(),
                    "num_episodes_watched": f["num_watched_episodes"].parse::<u32>().unwrap(), "is_rewatching": f["is_rewatching"] == "true"}))
            }
            _ => reply(404, json!({})),
        }
    })
    .await;
    let ep = endpoints(&server);
    let http = reqwest::Client::new();
    let cancel = AtomicBool::new(false);
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
    let browser = tokio::spawn(async move {
        let q = query_of(&url_rx.await.unwrap());
        assert_eq!(
            (
                q["response_type"].as_str(),
                q["code_challenge_method"].as_str()
            ),
            ("code", "plain")
        );
        assert!(q["code_challenge"].len() >= 43);
        *challenge.lock().unwrap() = q["code_challenge"].clone();
        let redirect = &q["redirect_uri"];
        // Someone else's sign-in: refused, and still waiting.
        let wrong = reqwest::get(format!("{redirect}?code=evil&state=wrong"))
            .await
            .unwrap();
        assert_eq!(wrong.status(), 400);
        let ok = reqwest::get(format!("{redirect}?code=c1&state={}", q["state"]))
            .await
            .unwrap();
        assert!(ok.text().await.unwrap().contains("Signed in"));
    });
    let (tokens, name) = mal::sign_in(&http, &ep, "abc", "", &cancel, move |url| {
        url_tx.send(url).unwrap();
    })
    .await
    .unwrap();
    browser.await.unwrap();
    assert_eq!(
        (
            tokens.access.as_str(),
            tokens.refresh.as_str(),
            name.as_str()
        ),
        ("a1", "r1", "jake")
    );

    // About to expire: refreshed first. The list comes in two pages.
    let session = mal::Session::new(
        http.clone(),
        ep.clone(),
        "abc".into(),
        String::new(),
        tokens,
    );
    let list = session.list().await.unwrap();
    assert_eq!(
        list[&52991],
        Entry {
            status: Status::Watching,
            score: 8.0,
            progress: 3
        }
    );
    assert_eq!(
        list[&1].status,
        Status::Rewatching,
        "completed and rewatching"
    );
    let refreshed = session.take_refreshed().expect("new tokens to save");
    assert_eq!(refreshed.access, "a2");
    assert!(session.take_refreshed().is_none(), "once");

    // Saving: whole-number scores; rewatching is "completed" + rewatching.
    let saved = session
        .save(
            52991,
            &Entry {
                status: Status::Rewatching,
                score: 7.5,
                progress: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        saved,
        Entry {
            status: Status::Rewatching,
            score: 8.0,
            progress: 2
        }
    );
    let sent = patches.lock().unwrap().last().unwrap().clone();
    assert_eq!(sent["status"], "completed");
    assert_eq!(sent["is_rewatching"], "true");
    assert_eq!(sent["score"], "8");
    assert_eq!(sent["num_watched_episodes"], "2");
}

#[tokio::test]
async fn mal_signs_out_when_the_tokens_are_dead() {
    let server = mock(|req: &Req| match req.target.split('?').next().unwrap() {
        "/mal/token" => reply(400, json!({"error": "invalid_grant"})),
        _ => reply(401, json!({"error": "invalid_token"})),
    })
    .await;
    let tokens = mal::Tokens {
        access: "a".into(),
        refresh: "r".into(),
        expires_at: chrono::Utc::now().timestamp() + 999_999,
    };
    let session = mal::Session::new(
        reqwest::Client::new(),
        endpoints(&server),
        "abc".into(),
        String::new(),
        tokens,
    );
    let err = session.list().await.unwrap_err();
    assert!(err.is::<SignedOut>(), "{err:#}");
}
