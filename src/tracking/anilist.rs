//! AniList: sign-in (OAuth implicit grant), the list, and saving entries,
//! through its GraphQL API.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Endpoints, Entry, SignedOut, Status, callback};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub name: String,
}

pub struct SignedIn {
    pub token: String,
    pub expires_at: i64,
    pub user: User,
}

/// The token arrives in the URL's fragment, which only the page can read:
/// it hands it to the app.
fn page_script() -> String {
    format!(
        "const m=document.getElementById('m');const f=location.hash.slice(1);\
         history.replaceState(null,'',location.pathname);\
         if(!f){{m.textContent=\"AniList didn't send a sign-in. Start again from the app.\";}}\
         else fetch('/anilist/token',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:f}})\
         .then(r=>{{m.textContent=r.ok?{done:?}:'Something went wrong. Start again from the app.';}},\
         ()=>{{m.textContent='The app stopped waiting for this sign-in. Start again from it.';}});",
        done = callback::DONE
    )
}

/// Sign in: `open` gets the page to show in the browser; then wait for
/// AniList to send it back here with a token.
pub async fn sign_in(
    http: &reqwest::Client,
    ep: &Endpoints,
    client_id: &str,
    cancel: &AtomicBool,
    open: impl FnOnce(String),
) -> Result<SignedIn> {
    let listener = callback::listen(ep.port).await?;
    let redirect = format!("http://127.0.0.1:{}/anilist", listener.local_addr()?.port());
    let url = url::Url::parse_with_params(
        &ep.anilist_auth,
        &[
            ("client_id", client_id),
            ("redirect_uri", &redirect),
            ("response_type", "token"),
        ],
    )?;
    open(url.to_string());
    let started = Instant::now();
    let (token, expires_in) = loop {
        let (stream, req) = callback::next(&listener, cancel, started).await?;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/anilist") if req.query.contains_key("error") => {
                callback::respond(
                    stream,
                    "200 OK",
                    "Sign-in was cancelled. You can close this tab.",
                    None,
                )
                .await;
                let why = req
                    .query
                    .get("error_description")
                    .or(req.query.get("error"))
                    .cloned()
                    .unwrap_or_default();
                bail!("AniList sign-in didn't complete ({why})");
            }
            ("GET", "/anilist") => {
                callback::respond(stream, "200 OK", "Signing in…", Some(&page_script())).await
            }
            ("POST", "/anilist/token") => {
                let form: HashMap<String, String> =
                    url::form_urlencoded::parse(req.body.as_bytes())
                        .into_owned()
                        .collect();
                if let Some(error) = form.get("error") {
                    callback::respond(stream, "200 OK", "cancelled", None).await;
                    bail!("AniList sign-in didn't complete ({error})");
                }
                match form.get("access_token").filter(|t| !t.is_empty()) {
                    Some(t) => {
                        callback::respond(stream, "200 OK", "ok", None).await;
                        let expires = form
                            .get("expires_in")
                            .and_then(|e| e.parse::<i64>().ok())
                            .unwrap_or(31_536_000);
                        break (t.clone(), expires);
                    }
                    None => callback::respond(stream, "400 Bad Request", "no token", None).await,
                }
            }
            _ => callback::respond(stream, "404 Not Found", "Not found.", None).await,
        }
    };
    let session = Session::new(http.clone(), ep.anilist_api.clone(), token.clone(), 0);
    let user = session.viewer().await?;
    Ok(SignedIn {
        token,
        expires_at: chrono::Utc::now().timestamp() + expires_in,
        user,
    })
}

fn status_from(s: &str) -> Option<Status> {
    Some(match s {
        "CURRENT" => Status::Watching,
        "PLANNING" => Status::Planning,
        "COMPLETED" => Status::Completed,
        "REPEATING" => Status::Rewatching,
        "PAUSED" => Status::Paused,
        "DROPPED" => Status::Dropped,
        _ => return None,
    })
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Watching => "CURRENT",
        Status::Planning => "PLANNING",
        Status::Completed => "COMPLETED",
        Status::Rewatching => "REPEATING",
        Status::Paused => "PAUSED",
        Status::Dropped => "DROPPED",
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    media_id: Option<u64>,
    status: Option<String>,
    #[serde(default)]
    progress: Option<u32>,
    #[serde(default)]
    score: Option<f32>,
}

impl RawEntry {
    fn entry(&self) -> Option<Entry> {
        Some(Entry {
            status: status_from(self.status.as_deref()?)?,
            score: self.score.unwrap_or(0.0),
            progress: self.progress.unwrap_or(0),
        })
    }
}

/// A signed-in AniList account.
pub struct Session {
    http: reqwest::Client,
    api: String,
    token: String,
    pub user_id: u64,
}

impl Session {
    pub fn new(http: reqwest::Client, api: String, token: String, user_id: u64) -> Self {
        Self {
            http,
            api,
            token,
            user_id,
        }
    }

    async fn graphql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        graphql(&self.http, &self.api, Some(&self.token), query, variables).await
    }

    pub async fn viewer(&self) -> Result<User> {
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "Viewer")]
            viewer: User,
        }
        let d: Data = self
            .graphql("query { Viewer { id name } }", json!({}))
            .await?;
        Ok(d.viewer)
    }

    /// The whole anime list, by AniList ID.
    pub async fn list(&self) -> Result<HashMap<u64, Entry>> {
        const QUERY: &str = "query ($user: Int, $chunk: Int) { MediaListCollection(userId: $user, type: ANIME, chunk: $chunk, perChunk: 500) \
            { hasNextChunk lists { entries { mediaId status progress score(format: POINT_10_DECIMAL) } } } }";
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Collection {
            #[serde(default)]
            has_next_chunk: bool,
            #[serde(default)]
            lists: Vec<List>,
        }
        #[derive(Deserialize)]
        struct List {
            #[serde(default)]
            entries: Vec<RawEntry>,
        }
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "MediaListCollection")]
            collection: Collection,
        }
        let mut out = HashMap::new();
        for chunk in 1..=200 {
            let d: Data = self
                .graphql(QUERY, json!({"user": self.user_id, "chunk": chunk}))
                .await?;
            for e in d.collection.lists.iter().flat_map(|l| &l.entries) {
                if let (Some(id), Some(entry)) = (e.media_id, e.entry()) {
                    out.insert(id, entry);
                }
            }
            if !d.collection.has_next_chunk {
                break;
            }
        }
        Ok(out)
    }

    /// Save an entry (adding the anime to the list if it isn't on it).
    pub async fn save(&self, media_id: u64, e: &Entry) -> Result<Entry> {
        const MUTATION: &str = "mutation ($media: Int, $status: MediaListStatus, $score: Int, $progress: Int) \
            { SaveMediaListEntry(mediaId: $media, status: $status, scoreRaw: $score, progress: $progress) \
            { mediaId status progress score(format: POINT_10_DECIMAL) } }";
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "SaveMediaListEntry")]
            saved: RawEntry,
        }
        let vars = json!({
            "media": media_id,
            "status": status_name(e.status),
            "score": (e.score * 10.0).round() as i64,
            "progress": e.progress,
        });
        let d: Data = self.graphql(MUTATION, vars).await?;
        d.saved
            .entry()
            .ok_or_else(|| anyhow!("AniList didn't say what it saved"))
    }
}

/// MyAnimeList's IDs for AniList's (no sign-in needed).
pub async fn mal_ids(http: &reqwest::Client, api: &str, ids: &[u64]) -> Result<HashMap<u64, u64>> {
    const QUERY: &str = "query ($ids: [Int]) { Page(perPage: 50) { media(id_in: $ids, type: ANIME) { id idMal } } }";
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Media {
        id: u64,
        id_mal: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Page {
        media: Vec<Media>,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "Page")]
        page: Page,
    }
    let mut out = HashMap::new();
    for chunk in ids.chunks(50) {
        let d: Data = graphql(http, api, None, QUERY, json!({ "ids": chunk })).await?;
        out.extend(
            d.page
                .media
                .into_iter()
                .filter_map(|m| Some((m.id, m.id_mal?))),
        );
    }
    Ok(out)
}

async fn graphql<T: DeserializeOwned>(
    http: &reqwest::Client,
    api: &str,
    token: Option<&str>,
    query: &str,
    variables: Value,
) -> Result<T> {
    let mut req = http
        .post(api)
        .header("Accept", "application/json")
        .json(&json!({ "query": query, "variables": variables }));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.context("couldn't reach AniList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("AniList is busy; try again in a minute");
    }
    let body: Value = resp
        .json()
        .await
        .with_context(|| format!("AniList answered {status}"))?;
    let errors: Vec<&str> = body
        .get("errors")
        .and_then(Value::as_array)
        .map(|e| {
            e.iter()
                .filter_map(|e| e.get("message")?.as_str())
                .collect()
        })
        .unwrap_or_default();
    let auth = errors.iter().any(|m| {
        let m = m.to_ascii_lowercase();
        m.contains("invalid token") || m.contains("unauthorized")
    });
    if status == reqwest::StatusCode::UNAUTHORIZED || (token.is_some() && auth) {
        return Err(SignedOut.into());
    }
    if !errors.is_empty() {
        bail!("AniList: {}", errors.join("; "));
    }
    if !status.is_success() {
        bail!("AniList answered {status}");
    }
    serde_json::from_value(body.get("data").cloned().unwrap_or(Value::Null))
        .context("unexpected answer from AniList")
}
