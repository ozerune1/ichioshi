//! MyAnimeList: sign-in (OAuth with PKCE, which MyAnimeList only does
//! "plain"), the list, and saving entries, through its v2 API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::{Endpoints, Entry, SignedOut, Status, callback};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Unix time.
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: String,
    #[serde(default = "month")]
    expires_in: i64,
}

fn month() -> i64 {
    31 * 24 * 3600
}

fn random(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
        .map_err(|_| anyhow!("no secure randomness available"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Sign in: `open` gets the page to show in the browser; then wait for
/// MyAnimeList to send it back here with a code, and swap that for tokens.
/// Returns them and the user's name.
pub async fn sign_in(
    http: &reqwest::Client,
    ep: &Endpoints,
    client_id: &str,
    client_secret: &str,
    cancel: &AtomicBool,
    open: impl FnOnce(String),
) -> Result<(Tokens, String)> {
    let listener = callback::listen(ep.port).await?;
    let redirect = format!("http://127.0.0.1:{}/mal", listener.local_addr()?.port());
    // Plain PKCE: the challenge is the verifier (43–128 URL-safe characters).
    let verifier = random(48)?;
    let state = random(16)?;
    let url = url::Url::parse_with_params(
        &ep.mal_auth,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("code_challenge", &verifier),
            ("code_challenge_method", "plain"),
            ("state", &state),
            ("redirect_uri", &redirect),
        ],
    )?;
    open(url.to_string());
    let started = Instant::now();
    let code = loop {
        let (stream, req) = callback::next(&listener, cancel, started).await?;
        if req.path != "/mal" {
            callback::respond(stream, "404 Not Found", "Not found.", None).await;
            continue;
        }
        if req.query.get("state") != Some(&state) {
            callback::respond(
                stream,
                "400 Bad Request",
                "This sign-in doesn't match the app's. Start again from the app.",
                None,
            )
            .await;
            continue;
        }
        if let Some(error) = req.query.get("error") {
            callback::respond(
                stream,
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
                None,
            )
            .await;
            bail!("MyAnimeList sign-in didn't complete ({error})");
        }
        match req.query.get("code") {
            Some(code) => {
                callback::respond(stream, "200 OK", callback::DONE, None).await;
                break code.clone();
            }
            None => callback::respond(stream, "400 Bad Request", "No sign-in code.", None).await,
        }
    };
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect),
        ("code_verifier", &verifier),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }
    let tokens = token_request(http, &ep.mal_token, &form).await?;
    let session = Session::new(
        http.clone(),
        ep.clone(),
        client_id.into(),
        client_secret.into(),
        tokens.clone(),
    );
    let name = session.me().await?;
    Ok((tokens, name))
}

async fn token_request(http: &reqwest::Client, url: &str, form: &[(&str, &str)]) -> Result<Tokens> {
    let resp = http
        .post(url)
        .form(form)
        .send()
        .await
        .context("couldn't reach MyAnimeList")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        if text.contains("invalid_grant") || status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SignedOut.into());
        }
        bail!(
            "MyAnimeList answered {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let r: TokenReply = resp
        .json()
        .await
        .context("unexpected answer from MyAnimeList")?;
    Ok(Tokens {
        access: r.access_token,
        refresh: r.refresh_token,
        expires_at: chrono::Utc::now().timestamp() + r.expires_in,
    })
}

fn status_from(s: &str, rewatching: bool) -> Option<Status> {
    Some(match s {
        "watching" => Status::Watching,
        "plan_to_watch" => Status::Planning,
        "completed" if rewatching => Status::Rewatching,
        "completed" => Status::Completed,
        "on_hold" => Status::Paused,
        "dropped" => Status::Dropped,
        _ => return None,
    })
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Watching => "watching",
        Status::Planning => "plan_to_watch",
        // MyAnimeList: rewatching is "completed", rewatching.
        Status::Completed | Status::Rewatching => "completed",
        Status::Paused => "on_hold",
        Status::Dropped => "dropped",
    }
}

#[derive(Deserialize)]
struct ListStatus {
    status: Option<String>,
    #[serde(default)]
    score: u32,
    #[serde(default)]
    num_episodes_watched: u32,
    #[serde(default)]
    is_rewatching: bool,
}

impl ListStatus {
    fn entry(&self) -> Option<Entry> {
        Some(Entry {
            status: status_from(self.status.as_deref()?, self.is_rewatching)?,
            score: self.score as f32,
            progress: self.num_episodes_watched,
        })
    }
}

/// A signed-in MyAnimeList account; tokens are refreshed as needed.
pub struct Session {
    http: reqwest::Client,
    ep: Endpoints,
    client_id: String,
    client_secret: String,
    tokens: tokio::sync::Mutex<Tokens>,
    refreshed: AtomicBool,
}

impl Session {
    pub fn new(
        http: reqwest::Client,
        ep: Endpoints,
        client_id: String,
        client_secret: String,
        tokens: Tokens,
    ) -> Self {
        Self {
            http,
            ep,
            client_id,
            client_secret,
            tokens: tokio::sync::Mutex::new(tokens),
            refreshed: AtomicBool::new(false),
        }
    }

    /// New tokens, once, after a refresh (to save).
    pub fn take_refreshed(&self) -> Option<Tokens> {
        if !self.refreshed.load(Ordering::Relaxed) {
            return None;
        }
        let tokens = self.tokens.try_lock().ok()?;
        self.refreshed.store(false, Ordering::Relaxed);
        Some(tokens.clone())
    }

    async fn refresh(&self, tokens: &mut Tokens) -> Result<()> {
        let mut form = vec![
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", tokens.refresh.as_str()),
        ];
        if !self.client_secret.is_empty() {
            form.push(("client_secret", &self.client_secret));
        }
        *tokens = token_request(&self.http, &self.ep.mal_token, &form).await?;
        self.refreshed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Send a request with the access token (refreshed first if it's about
    /// to run out, and once more if it's refused).
    async fn send(
        &self,
        build: impl Fn(&str) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut tokens = self.tokens.lock().await;
        if tokens.expires_at - chrono::Utc::now().timestamp() < 24 * 3600 {
            self.refresh(&mut tokens).await?;
        }
        let resp = build(&tokens.access)
            .send()
            .await
            .context("couldn't reach MyAnimeList")?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        self.refresh(&mut tokens).await?;
        let resp = build(&tokens.access)
            .send()
            .await
            .context("couldn't reach MyAnimeList")?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SignedOut.into());
        }
        Ok(resp)
    }

    async fn json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "MyAnimeList answered {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        resp.json()
            .await
            .context("unexpected answer from MyAnimeList")
    }

    /// The user's name.
    pub async fn me(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Me {
            name: String,
        }
        let url = format!("{}/users/@me", self.ep.mal_api);
        let me: Me = Self::json(self.send(|t| self.http.get(&url).bearer_auth(t)).await?).await?;
        Ok(me.name)
    }

    /// The whole anime list, by MyAnimeList ID.
    pub async fn list(&self) -> Result<HashMap<u64, Entry>> {
        #[derive(Deserialize)]
        struct Node {
            id: u64,
        }
        #[derive(Deserialize)]
        struct Item {
            node: Node,
            list_status: ListStatus,
        }
        #[derive(Deserialize, Default)]
        struct Paging {
            next: Option<String>,
        }
        #[derive(Deserialize)]
        struct Page {
            data: Vec<Item>,
            #[serde(default)]
            paging: Paging,
        }
        let mut out = HashMap::new();
        let mut next = Some(format!(
            "{}/users/@me/animelist?fields=list_status&limit=1000&nsfw=true",
            self.ep.mal_api
        ));
        let mut pages = 0;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > 100 {
                break;
            }
            let page: Page =
                Self::json(self.send(|t| self.http.get(&url).bearer_auth(t)).await?).await?;
            for item in page.data {
                if let Some(entry) = item.list_status.entry() {
                    out.insert(item.node.id, entry);
                }
            }
            next = page.paging.next;
        }
        Ok(out)
    }

    /// Save an entry (adding the anime to the list if it isn't on it).
    pub async fn save(&self, mal_id: u64, e: &Entry) -> Result<Entry> {
        let url = format!("{}/anime/{mal_id}/my_list_status", self.ep.mal_api);
        let score = (e.score.round() as u32).min(10).to_string();
        let progress = e.progress.to_string();
        let rewatching = if e.status == Status::Rewatching {
            "true"
        } else {
            "false"
        };
        let form = [
            ("status", status_name(e.status)),
            ("is_rewatching", rewatching),
            ("score", score.as_str()),
            ("num_watched_episodes", progress.as_str()),
        ];
        let saved: ListStatus = Self::json(
            self.send(|t| self.http.patch(&url).bearer_auth(t).form(&form))
                .await?,
        )
        .await?;
        saved
            .entry()
            .ok_or_else(|| anyhow!("MyAnimeList didn't say what it saved"))
    }
}
