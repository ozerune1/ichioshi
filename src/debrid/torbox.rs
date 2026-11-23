//! TorBox: https://api-docs.torbox.app/
//!
//! Torrents are added with seeding off. When one is downloaded,
//! `requestdl` gives a download URL per file.

use anyhow::Result;
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{
    Account, Client, Gone, Remote, RemoteFile, RemoteState, date, float, id, num, text,
    torrent_part,
};

fn data(c: &Client, status: StatusCode, body: Value) -> Result<Value> {
    if body["success"] == true {
        return Ok(body["data"].clone());
    }
    let code = body["error"].as_str().unwrap_or_default();
    if matches!(code, "BAD_TOKEN" | "NO_AUTH" | "AUTH_ERROR")
        || (status == StatusCode::FORBIDDEN && code.is_empty())
    {
        return Err(c.bad_key());
    }
    if code == "ITEM_NOT_FOUND" {
        return Err(anyhow::Error::new(Gone));
    }
    match body["detail"].as_str().filter(|d| !d.is_empty()) {
        // Its own failures share one message; the code says which.
        Some(detail) if status.is_server_error() && !code.is_empty() => {
            Err(c.error(format!("{detail} ({code})")))
        }
        Some(detail) => Err(c.error(detail)),
        None if !code.is_empty() => Err(c.error(code)),
        None => Err(c.error(format!("HTTP {status}"))),
    }
}

pub(super) async fn account(c: &Client) -> Result<Account> {
    let (status, v) = c.send(c.get("/user/me")).await?;
    let user = data(c, status, v)?;
    let plan = num(&user["plan"]).unwrap_or(0);
    Ok(Account {
        name: text(&user["email"]),
        premium: plan > 0,
        until: date(&user["premium_expires_at"]),
    })
}

pub(super) async fn add(c: &Client, torrent: &[u8], name: &str) -> Result<String> {
    let form = reqwest::multipart::Form::new()
        .part("file", torrent_part(torrent, name))
        // 3: don't seed.
        .text("seed", "3")
        .text("allow_zip", "false")
        .text("name", name.to_owned());
    let (status, v) = c
        .send(c.post("/torrents/createtorrent").multipart(form))
        .await?;
    let d = data(c, status, v)?;
    id(&d["torrent_id"]).ok_or_else(|| c.error("no torrent ID in the reply (queued?)"))
}

pub(super) async fn status(c: &Client, torrent: &str) -> Result<Remote> {
    let req = c
        .get("/torrents/mylist")
        .query(&[("id", torrent), ("bypass_cache", "true")]);
    let (status, v) = c.send(req).await?;
    let t = match data(c, status, v)? {
        Value::Array(list) => list.into_iter().next().unwrap_or(Value::Null),
        other => other,
    };
    if !t.is_object() {
        return Err(anyhow::Error::new(Gone));
    }
    let files = t["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(RemoteFile {
                id: id(&f["id"])?,
                path: text(&f["name"]),
                size: num(&f["size"]).unwrap_or(0),
                selected: true,
                link: None,
            })
        })
        .collect();
    let word = text(&t["download_state"]);
    let state = if t["download_finished"] == true && t["download_present"] == true {
        RemoteState::Ready
    } else if word.starts_with("error") || word == "missingFiles" || word.starts_with("failed") {
        RemoteState::Failed(format!("TorBox reports “{word}”"))
    } else {
        // Documented only as a number; read values over 1 as percentages.
        let progress = float(&t["progress"]).map(|p| if p > 1.0 { p / 100.0 } else { p } as f32);
        RemoteState::Working {
            status: word,
            progress,
            speed: num(&t["download_speed"]),
            seeders: num(&t["seeds"]),
        }
    };
    Ok(Remote { state, files })
}

pub(super) async fn link(c: &Client, torrent: &str, file: &RemoteFile) -> Result<String> {
    let req = c.get("/torrents/requestdl").query(&[
        ("token", c.key.as_str()),
        ("torrent_id", torrent),
        ("file_id", file.id.as_str()),
        ("zip_link", "false"),
    ]);
    let (status, v) = c.send(req).await?;
    data(c, status, v)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| c.error("no download URL in the reply"))
}

pub(super) async fn delete(c: &Client, torrent: &str) -> Result<()> {
    let torrent_id: u64 = torrent
        .parse()
        .map_err(|_| c.error(format!("bad torrent ID {torrent}")))?;
    let body = json!({"torrent_id": torrent_id, "operation": "delete"});
    let (status, v) = c
        .send(c.post("/torrents/controltorrent").json(&body))
        .await?;
    match data(c, status, v) {
        Ok(_) => Ok(()),
        Err(e) if e.downcast_ref::<Gone>().is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{BadKey, Client, Provider};
    use super::*;
    use crate::mock_http::{mock, reply};

    fn ok(data: Value) -> crate::mock_http::Resp {
        reply(
            200,
            json!({"success": true, "error": null, "detail": "", "data": data}),
        )
    }

    #[tokio::test]
    async fn creates_without_seeding_and_requests_links() {
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let base = mock(move |req| {
            let path = req.target.split('?').next().unwrap();
            if path != "/torrents/requestdl" {
                assert_eq!(req.headers["authorization"], "Bearer key");
            }
            match (req.method.as_str(), path) {
                ("GET", "/user/me") => ok(json!({"email": "me@example.com", "plan": 2, "premium_expires_at": "2027-03-01T00:00:00Z"})),
                ("POST", "/torrents/createtorrent") => {
                    let body = String::from_utf8_lossy(&req.body);
                    assert!(body.contains("name=\"file\"; filename=\"Show.torrent\""));
                    assert!(body.contains("name=\"seed\"\r\n\r\n3\r\n"), "no seeding: {body}");
                    assert!(body.contains("name=\"allow_zip\"\r\n\r\nfalse\r\n"));
                    ok(json!({"hash": "abc", "torrent_id": 5, "auth_id": "x"}))
                }
                ("GET", "/torrents/mylist") => {
                    let q = req.query();
                    assert_eq!((q["id"].as_str(), q["bypass_cache"].as_str()), ("5", "true"));
                    let files = json!([
                        {"id": 0, "name": "Show/Show - 01.mkv", "short_name": "Show - 01.mkv", "size": 100},
                        {"id": 1, "name": "Show/Show - 02.mkv", "short_name": "Show - 02.mkv", "size": 200}
                    ]);
                    if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        ok(json!({"id": 5, "download_state": "downloading", "progress": 0.4, "download_speed": 5000, "seeds": 9,
                            "download_finished": false, "download_present": false, "files": files}))
                    } else {
                        ok(json!({"id": 5, "download_state": "cached", "progress": 1, "download_finished": true, "download_present": true, "files": files}))
                    }
                }
                ("GET", "/torrents/requestdl") => {
                    let q = req.query();
                    assert_eq!((q["token"].as_str(), q["torrent_id"].as_str(), q["zip_link"].as_str()), ("key", "5", "false"));
                    ok(json!(format!("https://cdn.example/{}", q["file_id"])))
                }
                ("POST", "/torrents/controltorrent") => {
                    let body: Value = serde_json::from_slice(&req.body).unwrap();
                    assert_eq!(body, json!({"torrent_id": 5, "operation": "delete"}));
                    ok(Value::Null)
                }
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::TorBox, "key", &base);
        assert_eq!(
            c.account().await.unwrap(),
            Account {
                name: "me@example.com".into(),
                premium: true,
                until: Some("2027-03-01".into())
            }
        );
        let id = c.add(b"t", "Show").await.unwrap();
        assert_eq!(id, "5");
        match c.status(&id).await.unwrap().state {
            RemoteState::Working {
                progress,
                speed,
                seeders,
                ..
            } => assert_eq!((progress, speed, seeders), (Some(0.4), Some(5000), Some(9))),
            other => panic!("{other:?}"),
        }
        let remote = c.status(&id).await.unwrap();
        assert_eq!(remote.state, RemoteState::Ready);
        assert_eq!(
            c.link(&id, &remote.files[1]).await.unwrap(),
            "https://cdn.example/1"
        );
        c.delete(&id).await.unwrap();
    }

    #[tokio::test]
    async fn maps_errors() {
        let base = mock(|req| match req.target.split('?').next().unwrap() {
            "/user/me" => reply(403, json!({"success": false, "error": "BAD_TOKEN", "detail": "The provided token is invalid.", "data": null})),
            "/torrents/mylist" => reply(200, json!({"success": true, "error": null, "detail": "", "data": null})),
            // What TorBox said asked for the film's link (22 Nov 2026).
            "/torrents/requestdl" => reply(500, json!({"success": false, "error": "DATABASE_ERROR",
                "detail": "There was an error processing your request. Please try again later.", "data": null})),
            _ => reply(400, json!({"success": false, "error": "ACTIVE_LIMIT", "detail": "You have reached your active download limit.", "data": null})),
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::TorBox, "k", &base);
        assert!(
            c.account()
                .await
                .unwrap_err()
                .downcast_ref::<BadKey>()
                .is_some()
        );
        assert!(
            c.status("9")
                .await
                .unwrap_err()
                .downcast_ref::<Gone>()
                .is_some()
        );
        assert_eq!(
            c.add(b"t", "x").await.unwrap_err().to_string(),
            "TorBox: You have reached your active download limit."
        );
        let file = RemoteFile {
            id: "0".into(),
            path: "Film.mkv".into(),
            size: 1,
            selected: true,
            link: None,
        };
        assert_eq!(
            c.link("9", &file).await.unwrap_err().to_string(),
            "TorBox: There was an error processing your request. Please try again later. \
             (DATABASE_ERROR)"
        );
    }
}
