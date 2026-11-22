//! Uploading to Google Drive.
//!
//! Sign-in is OAuth 2.0 for installed apps: Google's consent page opens in
//! the browser and redirects to a one-off server on 127.0.0.1, with PKCE so
//! an intercepted code is useless. Only the refresh token is kept.
//!
//! Uploads use Drive's resumable protocol: the file goes up in chunks, and
//! after an error the upload asks Drive how much arrived and carries on
//! from there.
//!
//! Google requires an OAuth client ("Desktop app") from the user's own Google
//! Cloud project; the app reads the JSON file Google gives you for it.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Full Drive access: needed to upload into folders the app didn't create.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Upload chunk size; Drive wants multiples of 256 KiB.
const CHUNK: usize = 16 << 20;
/// How many times in a row an upload retries after errors before giving up.
const MAX_RETRIES: u32 = 8;

#[derive(Clone, Debug)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    /// Base for `/drive/v3/...` and `/upload/drive/v3/...`.
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://www.googleapis.com".into(),
        }
    }
}

/// The user's OAuth client, from the JSON file Google Cloud Console gives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OAuthClient {
    pub id: String,
    pub secret: String,
}

impl OAuthClient {
    pub fn from_json(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct File {
            installed: Option<Inner>,
            web: Option<Inner>,
        }
        #[derive(Deserialize)]
        struct Inner {
            client_id: String,
            client_secret: String,
        }
        let file: File = serde_json::from_str(text).context("that isn't a Google OAuth client file")?;
        match (file.installed, file.web) {
            (Some(c), _) => Ok(Self { id: c.client_id, secret: c.client_secret }),
            (None, Some(_)) => bail!("that's a “Web application” client; create a “Desktop app” client instead"),
            (None, None) => bail!("that isn't a Google OAuth client file"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    /// For display, e.g. "My Drive / Anime / Playlists".
    pub path: String,
}

/// What's remembered between runs. Saved readable only by the user, since
/// the refresh token gives access to their Drive.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub client: Option<OAuthClient>,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
    pub folder: Option<Folder>,
    pub upload_after_export: bool,
}

impl Account {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::fsutil::write_private(path, &serde_json::to_vec_pretty(self)?)
    }
}

/// Google no longer accepts the saved sign-in (revoked or expired).
#[derive(Debug)]
pub struct SignInExpired;

impl std::fmt::Display for SignInExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Google no longer accepts this sign-in; sign in again")
    }
}

impl std::error::Error for SignInExpired {}

pub struct SignedIn {
    pub refresh_token: String,
    pub email: String,
}

/// Sign in: `open` gets the consent page URL to show in the browser; then
/// wait for Google's redirect and swap the code for tokens. Gives up after
/// ten minutes, or when `cancel` is set.
pub async fn sign_in(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &OAuthClient,
    cancel: &AtomicBool,
    open: impl FnOnce(String),
) -> Result<SignedIn> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.context("couldn't start the sign-in listener")?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let verifier = random_token(64)?;
    let state = random_token(16)?;
    let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
    let url = url::Url::parse_with_params(
        &endpoints.auth,
        &[
            ("client_id", client.id.as_str()),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )?;
    open(url.to_string());
    let code = wait_for_code(&listener, &state, cancel).await?;
    let tokens: TokenResponse = post_form(
        http,
        &endpoints.token,
        &[
            ("code", &code),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ],
    )
    .await?;
    let refresh_token = tokens.refresh_token.clone().context("Google didn't return a refresh token")?;
    let drive = Drive::new(http.clone(), endpoints.clone(), client.clone(), refresh_token.clone());
    drive.set_access(tokens.access_token, tokens.expires_in);
    let email = drive.email().await?;
    Ok(SignedIn { refresh_token, email })
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
        .map_err(|_| anyhow!("no secure randomness available"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Serve the loopback redirect until a request carries our `state` and a
/// code (or an error). Other requests (a favicon, a stale tab) are ignored.
async fn wait_for_code(listener: &TcpListener, state: &str, cancel: &AtomicBool) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for the sign-in in the browser");
        }
        let Ok(accepted) = tokio::time::timeout(Duration::from_millis(250), listener.accept()).await else { continue };
        let (mut stream, _) = accepted?;
        let mut head = Vec::new();
        let mut buf = [0u8; 4096];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 << 10 {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => head.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head);
        let target = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
        let query: HashMap<String, String> = url::Url::parse(&format!("http://localhost{target}"))
            .map(|u| u.query_pairs().into_owned().collect())
            .unwrap_or_default();
        let (status, message, outcome) = match (query.get("code"), query.get("error"), query.get("state")) {
            (_, Some(error), Some(s)) if s == state => (
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
                Some(Err(anyhow!("Google sign-in didn't complete ({error})"))),
            ),
            (Some(code), None, Some(s)) if s == state => (
                "200 OK",
                "Signed in to SeaDex Downloader. You can close this tab and go back to the app.",
                Some(Ok(code.clone())),
            ),
            (Some(_), _, _) => ("400 Bad Request", "This sign-in doesn't match the app's. Start again from the app.", None),
            _ => ("404 Not Found", "Not found.", None),
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>SeaDex Downloader</title>\
             <body style=\"font:16px sans-serif;margin:3em\"><p>{message}</p>"
        );
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expiry")]
    expires_in: u64,
    refresh_token: Option<String>,
}

fn default_expiry() -> u64 {
    3600
}

/// POST a form to Google's token endpoint and parse the JSON reply.
async fn post_form<T: DeserializeOwned>(http: &reqwest::Client, url: &str, form: &[(&str, &str)]) -> Result<T> {
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("could not reach Google")?;
    let status = resp.status();
    let text = resp.text().await?;
    if status.is_success() {
        return serde_json::from_str(&text).with_context(|| format!("unexpected reply from Google: {text}"));
    }
    #[derive(Deserialize)]
    struct OAuthError {
        error: String,
        error_description: Option<String>,
    }
    match serde_json::from_str::<OAuthError>(&text) {
        Ok(e) if e.error == "invalid_grant" => Err(anyhow::Error::new(SignInExpired)),
        Ok(e) => bail!("Google: {}{}", e.error, e.error_description.map(|d| format!(" ({d})")).unwrap_or_default()),
        Err(_) => bail!("Google returned HTTP {status}"),
    }
}

/// Where to look for folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// "My Drive", "Shared with me" and any shared drives.
    Top,
    SharedWithMe,
    /// A folder; "root" is My Drive, and a shared drive's ID is its root.
    Folder(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub place: Place,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Uploaded {
    pub id: String,
    pub name: String,
    #[serde(rename = "webViewLink")]
    pub link: Option<String>,
}

/// A signed-in Drive. Access tokens are refreshed as needed.
pub struct Drive {
    http: reqwest::Client,
    endpoints: Endpoints,
    client: OAuthClient,
    refresh_token: String,
    access: Mutex<Option<(String, Instant)>>,
    chunk: usize,
}

impl Drive {
    pub fn new(http: reqwest::Client, endpoints: Endpoints, client: OAuthClient, refresh_token: String) -> Self {
        Self { http, endpoints, client, refresh_token, access: Mutex::new(None), chunk: CHUNK }
    }

    fn set_access(&self, token: String, expires_in: u64) {
        let expires = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));
        *self.access.lock().unwrap() = Some((token, expires));
    }

    async fn token(&self) -> Result<String> {
        if let Some((token, expires)) = self.access.lock().unwrap().clone()
            && Instant::now() < expires
        {
            return Ok(token);
        }
        let tokens: TokenResponse = post_form(
            &self.http,
            &self.endpoints.token,
            &[
                ("client_id", &self.client.id),
                ("client_secret", &self.client.secret),
                ("refresh_token", &self.refresh_token),
                ("grant_type", "refresh_token"),
            ],
        )
        .await?;
        self.set_access(tokens.access_token.clone(), tokens.expires_in);
        Ok(tokens.access_token)
    }

    /// GET a Drive API URL (relative to the API base) as JSON; refreshes the
    /// access token once if Drive says it's no longer valid.
    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        for attempt in 0..2 {
            let resp = self
                .http
                .get(format!("{}{path}", self.endpoints.api))
                .query(query)
                .bearer_auth(self.token().await?)
                .send()
                .await
                .context("could not reach Google Drive")?;
            if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.access.lock().unwrap() = None;
                continue;
            }
            return drive_json(resp).await;
        }
        unreachable!()
    }

    pub async fn email(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct About {
            user: User,
        }
        #[derive(Deserialize)]
        struct User {
            #[serde(rename = "emailAddress")]
            email: Option<String>,
            #[serde(rename = "displayName")]
            name: Option<String>,
        }
        let about: About = self.get("/drive/v3/about", &[("fields", "user(emailAddress,displayName)")]).await?;
        Ok(about.user.email.or(about.user.name).unwrap_or_else(|| "your Google account".into()))
    }

    /// The folders in a place, by name.
    pub async fn folders(&self, place: &Place) -> Result<Vec<Entry>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<Item>,
            #[serde(rename = "nextPageToken")]
            next: Option<String>,
        }
        #[derive(Deserialize)]
        struct Item {
            id: String,
            name: String,
        }
        let q = match place {
            Place::Top => {
                #[derive(Deserialize)]
                struct Drives {
                    #[serde(default)]
                    drives: Vec<Item>,
                }
                let mut top = vec![
                    Entry { name: "My Drive".into(), place: Place::Folder("root".into()) },
                    Entry { name: "Shared with me".into(), place: Place::SharedWithMe },
                ];
                // Shared drives (Workspace accounts); none is fine.
                if let Ok(d) = self.get::<Drives>("/drive/v3/drives", &[("pageSize", "100"), ("fields", "drives(id,name)")]).await {
                    top.extend(d.drives.into_iter().map(|d| Entry { name: d.name, place: Place::Folder(d.id) }));
                }
                return Ok(top);
            }
            Place::SharedWithMe => format!("sharedWithMe and mimeType = '{FOLDER_MIME}' and trashed = false"),
            Place::Folder(id) => format!("'{}' in parents and mimeType = '{FOLDER_MIME}' and trashed = false", id.replace('\'', "")),
        };
        let mut out = Vec::new();
        let mut page_token = String::new();
        loop {
            let mut query = vec![
                ("q", q.as_str()),
                ("fields", "nextPageToken,files(id,name)"),
                ("orderBy", "name"),
                ("pageSize", "1000"),
                ("supportsAllDrives", "true"),
                ("includeItemsFromAllDrives", "true"),
            ];
            if !page_token.is_empty() {
                query.push(("pageToken", &page_token));
            }
            let page: Page = self.get("/drive/v3/files", &query).await?;
            out.extend(page.files.into_iter().map(|f| Entry { name: f.name, place: Place::Folder(f.id) }));
            match page.next {
                Some(next) => page_token = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// The folder a Drive link (or ID) points at, checking files can be added to it.
    pub async fn folder_from_link(&self, link: &str) -> Result<Folder> {
        let id = folder_id_from_link(link).context("that doesn't look like a Google Drive folder link")?;
        #[derive(Deserialize)]
        struct File {
            id: String,
            name: String,
            #[serde(rename = "mimeType")]
            mime: String,
            capabilities: Option<Capabilities>,
        }
        #[derive(Deserialize)]
        struct Capabilities {
            #[serde(rename = "canAddChildren")]
            can_add_children: Option<bool>,
        }
        let file: File = self
            .get(
                &format!("/drive/v3/files/{id}"),
                &[("fields", "id,name,mimeType,capabilities(canAddChildren)"), ("supportsAllDrives", "true")],
            )
            .await?;
        if file.mime != FOLDER_MIME {
            bail!("“{}” is a file, not a folder", file.name);
        }
        if file.capabilities.and_then(|c| c.can_add_children) == Some(false) {
            bail!("you can't add files to “{}”", file.name);
        }
        Ok(Folder { id: file.id, path: file.name })
    }

    /// Upload a file into a folder with Drive's resumable protocol. `progress`
    /// gets 0.0–1.0; setting `cancel` stops it.
    pub async fn upload(
        &self,
        path: &Path,
        folder_id: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(f32),
    ) -> Result<Uploaded> {
        let size = tokio::fs::metadata(path).await?.len();
        let name = path.file_name().context("no file name")?.to_string_lossy().into_owned();
        let mime = if name.to_lowercase().ends_with(".zip") { "application/zip" } else { "application/octet-stream" };

        let mut start = None;
        for attempt in 0..2 {
            let resp = self
                .http
                .post(format!(
                    "{}/upload/drive/v3/files?uploadType=resumable&supportsAllDrives=true&fields=id,name,webViewLink",
                    self.endpoints.api
                ))
                .bearer_auth(self.token().await?)
                .header("X-Upload-Content-Type", mime)
                .header("X-Upload-Content-Length", size)
                .json(&json!({ "name": name, "parents": [folder_id] }))
                .send()
                .await
                .context("could not reach Google Drive")?;
            // A rejected access token gets one refresh before giving up.
            if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.access.lock().unwrap() = None;
                continue;
            }
            start = Some(resp);
            break;
        }
        let start = start.expect("the loop always sets it");
        if !start.status().is_success() {
            return Err(drive_error(start).await);
        }
        let session = start
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .context("Google Drive didn't start the upload")?
            .to_owned();

        let mut file = tokio::fs::File::open(path).await?;
        let mut offset = 0u64;
        let mut retries = 0u32;
        let mut buf = vec![0u8; self.chunk];
        loop {
            if cancel.load(Ordering::Relaxed) {
                // Tell Drive to drop what it has so far.
                let _ = self.http.delete(&session).send().await;
                bail!("cancelled");
            }
            let end = (offset + self.chunk as u64).min(size);
            let len = (end - offset) as usize;
            file.seek(SeekFrom::Start(offset)).await?;
            file.read_exact(&mut buf[..len]).await?;
            let range = if size == 0 { "bytes */0".to_owned() } else { format!("bytes {offset}-{}/{size}", end - 1) };
            let sent = self
                .http
                .put(&session)
                .header(reqwest::header::CONTENT_RANGE, range)
                .body(buf[..len].to_vec())
                .send()
                .await;
            match sent {
                Ok(resp) if resp.status().is_success() => {
                    progress(1.0);
                    return drive_json(resp).await;
                }
                Ok(resp) if resp.status().as_u16() == 308 => {
                    offset = received(&resp);
                    retries = 0;
                    progress((offset as f32 / size.max(1) as f32).min(0.99));
                }
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::GONE => {
                    bail!("Google Drive dropped the upload session; start the upload again");
                }
                Ok(resp) if !is_retryable(resp.status()) => return Err(drive_error(resp).await),
                // Server trouble or a network error: wait, ask what arrived, carry on.
                _ => {
                    retries += 1;
                    if retries > MAX_RETRIES {
                        bail!("the upload kept failing; check your connection and try again");
                    }
                    tokio::time::sleep(backoff(retries)).await;
                    if let Ok(resp) = self
                        .http
                        .put(&session)
                        .header(reqwest::header::CONTENT_RANGE, format!("bytes */{size}"))
                        .body(Vec::new())
                        .send()
                        .await
                    {
                        if resp.status().is_success() {
                            progress(1.0);
                            return drive_json(resp).await;
                        }
                        if resp.status().as_u16() == 308 {
                            offset = received(&resp);
                        }
                    }
                }
            }
        }
    }

    /// Upload in smaller chunks (tests: more progress reports).
    #[cfg(test)]
    pub fn with_chunk(mut self, chunk: usize) -> Self {
        self.chunk = chunk;
        self
    }

    /// Revoke the sign-in at Google (best effort, on sign-out).
    pub async fn revoke(&self) {
        let _ = post_form::<serde_json::Value>(&self.http, &self.endpoints.revoke, &[("token", &self.refresh_token)]).await;
    }
}

/// From a 308 reply's `Range: bytes=0-N`: the next byte Drive wants.
fn received(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get(reqwest::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.rsplit('-').next())
        .and_then(|n| n.parse::<u64>().ok())
        .map_or(0, |last| last + 1)
}

fn is_retryable(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::REQUEST_TIMEOUT
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis((500u64 << attempt.min(6)).min(30_000))
}

async fn drive_json<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    if !resp.status().is_success() {
        return Err(drive_error(resp).await);
    }
    let text = resp.text().await?;
    serde_json::from_str(&text).with_context(|| format!("unexpected reply from Google Drive: {text}"))
}

/// Drive's error message, e.g. "File not found: abc".
async fn drive_error(resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned));
    match (status, message) {
        (StatusCode::UNAUTHORIZED, _) => anyhow::Error::new(SignInExpired),
        (_, Some(m)) => anyhow!("Google Drive: {m}"),
        _ => anyhow!("Google Drive returned HTTP {status}"),
    }
}

/// The folder ID in a Drive link (`.../folders/<id>`, `...?id=<id>`) or a bare ID.
pub fn folder_id_from_link(link: &str) -> Option<String> {
    let link = link.trim();
    let is_id = |s: &str| s.len() >= 10 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if is_id(link) {
        return Some(link.to_owned());
    }
    let url = url::Url::parse(link).ok()?;
    if !url.host_str().is_some_and(|h| h.ends_with("google.com")) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    let from_path = segments.iter().position(|s| *s == "folders").and_then(|i| segments.get(i + 1)).map(|s| s.to_string());
    from_path
        .or_else(|| url.query_pairs().find(|(k, _)| k == "id").map(|(_, v)| v.into_owned()))
        .filter(|id| is_id(id))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) use crate::mock_http::{Resp, mock, reply};

    pub(crate) fn endpoints(base: &str) -> Endpoints {
        Endpoints {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: base.to_owned(),
        }
    }

    fn client() -> OAuthClient {
        OAuthClient { id: "id.apps.googleusercontent.com".into(), secret: "secret".into() }
    }

    #[test]
    fn reads_client_files_and_folder_links() {
        let installed = r#"{"installed":{"client_id":"a.apps.googleusercontent.com","client_secret":"s","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(OAuthClient::from_json(installed).unwrap().id, "a.apps.googleusercontent.com");
        let web = r#"{"web":{"client_id":"a","client_secret":"s"}}"#;
        assert!(OAuthClient::from_json(web).unwrap_err().to_string().contains("Desktop app"));

        let id = "1AbCdEfGhIjKlMnOp_qrs-TUV";
        for link in [
            format!("https://drive.google.com/drive/folders/{id}"),
            format!("https://drive.google.com/drive/u/1/folders/{id}?usp=sharing"),
            format!("https://drive.google.com/open?id={id}"),
            id.to_owned(),
        ] {
            assert_eq!(folder_id_from_link(&link).as_deref(), Some(id), "{link}");
        }
        assert_eq!(folder_id_from_link("https://example.com/folders/1AbCdEfGhIjKlMnOp"), None);
        assert_eq!(folder_id_from_link("my folder"), None);
    }

    #[tokio::test]
    async fn signs_in_with_pkce_and_gets_the_account() {
        let challenge = Arc::new(Mutex::new(String::new()));
        let seen = challenge.clone();
        let base = mock(move |req| match req.target.as_str() {
            "/token" => {
                let form = req.form();
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["code"], "the-code");
                assert_eq!(form["client_secret"], "secret");
                // The verifier must hash to the challenge the browser was given.
                let hashed = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, form["code_verifier"].as_bytes()));
                assert_eq!(hashed, *seen.lock().unwrap());
                reply(200, json!({"access_token": "at", "expires_in": 3599, "refresh_token": "rt"}))
            }
            t if t.starts_with("/drive/v3/about") => {
                assert_eq!(req.headers["authorization"], "Bearer at");
                reply(200, json!({"user": {"emailAddress": "me@example.com"}}))
            }
            other => panic!("unexpected {other}"),
        })
        .await;

        let http = reqwest::Client::new();
        let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
        let browser = tokio::spawn(async move {
            // Play the browser: follow the consent URL's redirect with a code.
            let url = url::Url::parse(&url_rx.await.unwrap()).unwrap();
            let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
            assert_eq!(q["scope"], SCOPE);
            assert_eq!(q["code_challenge_method"], "S256");
            assert_eq!(q["access_type"], "offline");
            *challenge.lock().unwrap() = q["code_challenge"].clone();
            let back = reqwest::Client::new();
            // A stray request and a forged state are ignored...
            let stray = back.get(format!("{}/favicon.ico", q["redirect_uri"])).send().await.unwrap();
            assert_eq!(stray.status(), 404);
            let forged = back.get(format!("{}/?code=evil&state=wrong", q["redirect_uri"])).send().await.unwrap();
            assert_eq!(forged.status(), 400);
            // ...the real redirect completes the sign-in.
            let page = back
                .get(format!("{}/?code=the-code&state={}&scope=x", q["redirect_uri"], q["state"]))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert!(page.contains("You can close this tab"));
        });
        let signed_in = sign_in(&http, &endpoints(&base), &client(), &AtomicBool::new(false), |url| {
            url_tx.send(url).unwrap();
        })
        .await
        .unwrap();
        browser.await.unwrap();
        assert_eq!((signed_in.refresh_token.as_str(), signed_in.email.as_str()), ("rt", "me@example.com"));
    }

    #[tokio::test]
    async fn refreshes_tokens_and_reports_revoked_sign_ins() {
        let base = mock(|req| match req.target.as_str() {
            "/token" if req.form()["refresh_token"] == "good" => {
                reply(200, json!({"access_token": "fresh", "expires_in": 3600}))
            }
            "/token" => reply(400, json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."})),
            t if t.starts_with("/drive/v3/about") => {
                assert_eq!(req.headers["authorization"], "Bearer fresh");
                reply(200, json!({"user": {"displayName": "Me"}}))
            }
            other => panic!("unexpected {other}"),
        })
        .await;
        let good = Drive::new(reqwest::Client::new(), endpoints(&base), client(), "good".into());
        assert_eq!(good.email().await.unwrap(), "Me");
        let revoked = Drive::new(reqwest::Client::new(), endpoints(&base), client(), "old".into());
        let err = revoked.email().await.unwrap_err();
        assert!(err.downcast_ref::<SignInExpired>().is_some(), "{err:#}");
    }

    #[tokio::test]
    async fn lists_folders_across_pages() {
        let base = mock(|req| {
            let q = req.query();
            match q.get("pageToken").map(String::as_str) {
                _ if req.target.starts_with("/token") => reply(200, json!({"access_token": "at", "expires_in": 3600})),
                _ if req.target.starts_with("/drive/v3/drives") => reply(200, json!({"drives": [{"id": "0AShared", "name": "Team"}]})),
                None => {
                    assert_eq!(q["q"], "'root' in parents and mimeType = 'application/vnd.google-apps.folder' and trashed = false");
                    reply(200, json!({"files": [{"id": "a", "name": "Anime"}], "nextPageToken": "p2"}))
                }
                Some("p2") => reply(200, json!({"files": [{"id": "b", "name": "Backups"}]})),
                Some(other) => panic!("page {other}"),
            }
        })
        .await;
        let drive = Drive::new(reqwest::Client::new(), endpoints(&base), client(), "rt".into());
        let top: Vec<String> = drive.folders(&Place::Top).await.unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(top, ["My Drive", "Shared with me", "Team"]);
        let folders = drive.folders(&Place::Folder("root".into())).await.unwrap();
        assert_eq!(
            folders,
            vec![
                Entry { name: "Anime".into(), place: Place::Folder("a".into()) },
                Entry { name: "Backups".into(), place: Place::Folder("b".into()) },
            ]
        );
    }

    /// The upload survives a server error and a chunk Drive only partly
    /// kept, and what arrives is exactly the file.
    #[tokio::test]
    async fn resumable_upload_recovers_from_errors() {
        let dir = std::env::temp_dir().join(format!("seadex-gdrive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-11-21 16-48-35.zip");
        let content: Vec<u8> = (0..1_300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &content).unwrap();

        #[derive(Default)]
        struct Server {
            stored: Vec<u8>,
            puts: usize,
            failed_once: bool,
            tokens: usize,
            starts: usize,
        }
        let server = Arc::new(Mutex::new(Server::default()));
        let state = server.clone();
        let base_cell = Arc::new(Mutex::new(String::new()));
        let base_for_handler = base_cell.clone();
        let base = mock(move |req| {
            let mut s = state.lock().unwrap();
            let total = 1_300_000usize;
            match (req.method.as_str(), req.target.as_str()) {
                ("POST", "/token") => {
                    s.tokens += 1;
                    reply(200, json!({"access_token": format!("at{}", s.tokens), "expires_in": 3600}))
                }
                ("POST", t) if t.starts_with("/upload/drive/v3/files?uploadType=resumable") => {
                    s.starts += 1;
                    // The first access token is rejected, as after a password change.
                    if s.starts == 1 {
                        assert_eq!(req.headers["authorization"], "Bearer at1");
                        return reply(401, json!({"error": {"message": "Invalid Credentials"}}));
                    }
                    assert_eq!(req.headers["authorization"], "Bearer at2");
                    let meta: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                    assert_eq!(meta, json!({"name": "2026-11-21 16-48-35.zip", "parents": ["folder-id"]}));
                    assert_eq!(req.headers["x-upload-content-length"], total.to_string());
                    let session = format!("{}/session", base_for_handler.lock().unwrap());
                    Resp { status: 200, headers: vec![("Location".into(), session)], body: Vec::new() }
                }
                ("PUT", "/session") => {
                    s.puts += 1;
                    let range = &req.headers["content-range"];
                    let acked = |stored: usize| {
                        let headers = if stored == 0 { vec![] } else { vec![("Range".into(), format!("bytes=0-{}", stored - 1))] };
                        Resp { status: 308, headers, body: Vec::new() }
                    };
                    if range == &format!("bytes */{total}") {
                        return acked(s.stored.len()); // "how much do you have?"
                    }
                    let (start, _) = range.trim_start_matches("bytes ").split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    assert_eq!(start, s.stored.len(), "chunks must continue where Drive left off");
                    if s.puts == 2 && !s.failed_once {
                        s.failed_once = true;
                        return reply(503, json!({"error": {"message": "Backend Error"}}));
                    }
                    // Keep only half of the third chunk, as Drive sometimes does.
                    let keep = if s.puts == 4 { req.body.len() / 2 } else { req.body.len() };
                    s.stored.extend_from_slice(&req.body[..keep]);
                    if s.stored.len() == total {
                        reply(200, json!({"id": "file-id", "name": "2026-11-21 16-48-35.zip", "webViewLink": "https://drive.google.com/file/d/file-id/view"}))
                    } else {
                        acked(s.stored.len())
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        *base_cell.lock().unwrap() = base.clone();

        let mut drive = Drive::new(reqwest::Client::new(), endpoints(&base), client(), "rt".into());
        drive.chunk = 256 << 10;
        let mut last = 0.0;
        let uploaded = drive.upload(&path, "folder-id", &AtomicBool::new(false), |p| last = p).await.unwrap();
        assert_eq!(uploaded.id, "file-id");
        assert_eq!(uploaded.link.as_deref(), Some("https://drive.google.com/file/d/file-id/view"));
        assert_eq!(last, 1.0);
        let s = server.lock().unwrap();
        assert!(s.failed_once);
        assert_eq!((s.tokens, s.starts), (2, 2));
        assert!(s.stored == content, "the uploaded bytes must be the file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
