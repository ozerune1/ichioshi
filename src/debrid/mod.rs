//! Downloading through a debrid service instead of BitTorrent.
//!
//! The service fetches the torrent on its own servers (often instantly,
//! when someone has fetched it before), and this computer downloads just the
//! files you picked over plain HTTPS. No BitTorrent traffic comes from here.
//!
//! Each service has its own API; [`Client`] hides the differences behind
//! a few calls: add a torrent, check on it, pick its files (only Real-Debrid
//! needs that), get a file's download link, delete it. [`jobs`] drives a
//! download through them and keeps it going across restarts.

mod alldebrid;
mod debridlink;
pub mod jobs;
mod premiumize;
mod realdebrid;
mod torbox;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow};
use reqwest::{RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum Provider {
    #[default]
    RealDebrid,
    AllDebrid,
    Premiumize,
    TorBox,
    DebridLink,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::RealDebrid,
        Provider::AllDebrid,
        Provider::Premiumize,
        Provider::TorBox,
        Provider::DebridLink,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Provider::RealDebrid => "Real-Debrid",
            Provider::AllDebrid => "AllDebrid",
            Provider::Premiumize => "Premiumize",
            Provider::TorBox => "TorBox",
            Provider::DebridLink => "Debrid-Link",
        }
    }

    /// Where the user finds their API key.
    pub fn key_page(self) -> &'static str {
        match self {
            Provider::RealDebrid => "https://real-debrid.com/apitoken",
            Provider::AllDebrid => "https://alldebrid.com/apikeys/",
            Provider::Premiumize => "https://www.premiumize.me/account",
            Provider::TorBox => "https://torbox.app/settings",
            Provider::DebridLink => "https://debrid-link.com/webapp/apikey",
        }
    }

    fn base(self) -> &'static str {
        match self {
            Provider::RealDebrid => "https://api.real-debrid.com/rest/1.0",
            Provider::AllDebrid => "https://api.alldebrid.com",
            Provider::Premiumize => "https://www.premiumize.me/api",
            Provider::TorBox => "https://api.torbox.app/v1/api",
            Provider::DebridLink => "https://debrid-link.com/api/v2",
        }
    }
}

/// The "download with" setting and the API keys, saved readable only by the
/// user since the keys give access to their accounts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Download through the service instead of BitTorrent.
    pub enabled: bool,
    pub provider: Provider,
    /// One key per service, so switching back and forth keeps them.
    pub keys: BTreeMap<Provider, String>,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::fsutil::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn key(&self, provider: Provider) -> Option<&str> {
        self.keys
            .get(&provider)
            .map(|k| k.trim())
            .filter(|k| !k.is_empty())
    }

    /// Whether new downloads go through the service (it's on and has a key).
    pub fn active(&self) -> bool {
        self.enabled && self.key(self.provider).is_some()
    }
}

/// The service turned the API key down.
#[derive(Debug)]
pub struct BadKey(pub Provider);

impl std::fmt::Display for BadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} doesn't accept the API key; check it in Settings",
            self.0.name()
        )
    }
}

impl std::error::Error for BadKey {}

/// The torrent is no longer in the account (deleted on the website, or expired).
#[derive(Debug)]
pub struct Gone;

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the torrent is no longer in the account")
    }
}

impl std::error::Error for Gone {}

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub name: String,
    pub premium: bool,
    /// When premium ends, e.g. "2027-03-01".
    pub until: Option<String>,
}

/// What the service is doing with a torrent.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteState {
    /// Waiting for us to pick files (Real-Debrid).
    NeedsSelection,
    /// Fetching it, or queued. `status` is the service's own word for it.
    Working {
        status: String,
        progress: Option<f32>,
        speed: Option<u64>,
        seeders: Option<u64>,
    },
    Ready,
    Failed(String),
}

/// A file of a torrent as the service lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteFile {
    pub id: String,
    /// Its path inside the torrent, as the service has it (it may or may not
    /// include the torrent's name).
    pub path: String,
    pub size: u64,
    /// Whether the service is fetching it (only Real-Debrid lets you choose).
    pub selected: bool,
    /// The service's link for it, where downloading needs one more step.
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub state: RemoteState,
    pub files: Vec<RemoteFile>,
}

/// An API client for one service, with the user's key.
#[derive(Clone)]
pub struct Client {
    pub provider: Provider,
    key: String,
    http: reqwest::Client,
    base: String,
}

impl Client {
    pub fn new(http: reqwest::Client, provider: Provider, key: &str) -> Self {
        Self {
            provider,
            key: key.trim().to_owned(),
            http,
            base: provider.base().to_owned(),
        }
    }

    /// A client for a mock server (tests).
    #[cfg(test)]
    pub fn with_base(http: reqwest::Client, provider: Provider, key: &str, base: &str) -> Self {
        Self {
            provider,
            key: key.to_owned(),
            http,
            base: base.to_owned(),
        }
    }

    pub async fn account(&self) -> Result<Account> {
        match self.provider {
            Provider::RealDebrid => realdebrid::account(self).await,
            Provider::AllDebrid => alldebrid::account(self).await,
            Provider::Premiumize => premiumize::account(self).await,
            Provider::TorBox => torbox::account(self).await,
            Provider::DebridLink => debridlink::account(self).await,
        }
    }

    /// Add a torrent; returns the service's ID for it.
    pub async fn add(&self, torrent: &[u8], name: &str) -> Result<String> {
        match self.provider {
            Provider::RealDebrid => realdebrid::add(self, torrent).await,
            Provider::AllDebrid => alldebrid::add(self, torrent, name).await,
            Provider::Premiumize => premiumize::add(self, torrent, name).await,
            Provider::TorBox => torbox::add(self, torrent, name).await,
            Provider::DebridLink => debridlink::add(self, torrent, name).await,
        }
    }

    pub async fn status(&self, id: &str) -> Result<Remote> {
        match self.provider {
            Provider::RealDebrid => realdebrid::status(self, id).await,
            Provider::AllDebrid => alldebrid::status(self, id).await,
            Provider::Premiumize => premiumize::status(self, id).await,
            Provider::TorBox => torbox::status(self, id).await,
            Provider::DebridLink => debridlink::status(self, id).await,
        }
    }

    /// Pick which files the service fetches (Real-Debrid; a no-op elsewhere,
    /// where the service always fetches the whole torrent).
    pub async fn select(&self, id: &str, file_ids: &[String]) -> Result<()> {
        match self.provider {
            Provider::RealDebrid => realdebrid::select(self, id, file_ids).await,
            _ => Ok(()),
        }
    }

    /// Whether the service only fetches the files you pick.
    pub fn selects_files(&self) -> bool {
        self.provider == Provider::RealDebrid
    }

    /// A direct HTTPS link for one ready file.
    pub async fn link(&self, id: &str, file: &RemoteFile) -> Result<String> {
        match self.provider {
            Provider::RealDebrid => realdebrid::link(self, file).await,
            Provider::AllDebrid => alldebrid::link(self, file).await,
            Provider::Premiumize | Provider::DebridLink => file
                .link
                .clone()
                .ok_or_else(|| anyhow!("{} gave no link for {}", self.provider.name(), file.path)),
            Provider::TorBox => torbox::link(self, id, file).await,
        }
    }

    /// Delete the torrent from the account.
    pub async fn delete(&self, id: &str) -> Result<()> {
        match self.provider {
            Provider::RealDebrid => realdebrid::delete(self, id).await,
            Provider::AllDebrid => alldebrid::delete(self, id).await,
            Provider::Premiumize => premiumize::delete(self, id).await,
            Provider::TorBox => torbox::delete(self, id).await,
            Provider::DebridLink => debridlink::delete(self, id).await,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.http.get(self.url(path)).bearer_auth(&self.key)
    }

    fn post(&self, path: &str) -> RequestBuilder {
        self.http.post(self.url(path)).bearer_auth(&self.key)
    }

    fn put(&self, path: &str) -> RequestBuilder {
        self.http.put(self.url(path)).bearer_auth(&self.key)
    }

    fn delete_req(&self, path: &str) -> RequestBuilder {
        self.http.delete(self.url(path)).bearer_auth(&self.key)
    }

    /// Send a request; returns the HTTP status and the body parsed as JSON
    /// (`Null` for an empty or non-JSON body).
    async fn send(&self, req: RequestBuilder) -> Result<(StatusCode, Value)> {
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("could not reach {}: {e}", self.provider.name()))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    fn bad_key(&self) -> anyhow::Error {
        anyhow::Error::new(BadKey(self.provider))
    }

    fn error(&self, what: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("{}: {what}", self.provider.name())
    }
}

/// A JSON number that may also come as a string (services aren't consistent).
fn num(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// An ID that may come as a number or a string.
fn id(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

/// A date for display, from a Unix time or an ISO string.
fn date(v: &Value) -> Option<String> {
    match v {
        Value::Number(_) | Value::String(_) if num(v).is_some_and(|t| t > 0) => {
            chrono::DateTime::from_timestamp(num(v)? as i64, 0)
                .map(|d| d.format("%Y-%m-%d").to_string())
        }
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.format("%Y-%m-%d").to_string()),
        _ => None,
    }
}

/// A multipart part holding a .torrent file.
fn torrent_part(torrent: &[u8], name: &str) -> reqwest::multipart::Part {
    let file_name = format!("{}.torrent", crate::engine::sanitize_folder_name(name));
    reqwest::multipart::Part::bytes(torrent.to_vec())
        .file_name(file_name)
        .mime_str("application/x-bittorrent")
        .expect("valid MIME type")
}

/// For each of `wanted` (path inside the torrent, size), the index of the
/// service's file that is it: same size and a matching tail of the path
/// (services differ in whether they include the torrent's name). The
/// closest matches are paired first, so "Specials/E02.mkv" can't take the
/// file meant for "E02.mkv".
pub fn match_files(wanted: &[(&str, u64)], remote: &[RemoteFile]) -> Vec<Option<usize>> {
    let parts = |p: &str| -> Vec<String> {
        p.split(['/', '\\'])
            .filter(|c| !c.is_empty())
            .map(|c| c.to_lowercase())
            .collect()
    };
    let remote_parts: Vec<Vec<String>> = remote.iter().map(|f| parts(&f.path)).collect();
    // (matching components, extra remote components, wanted, remote)
    let mut pairs = Vec::new();
    for (w, (path, size)) in wanted.iter().enumerate() {
        let want = parts(path);
        for (r, file) in remote.iter().enumerate() {
            if file.size != *size {
                continue;
            }
            let common = want
                .iter()
                .rev()
                .zip(remote_parts[r].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            if common > 0 {
                pairs.push((common, remote_parts[r].len() - common, w, r));
            }
        }
    }
    pairs.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    let mut out = vec![None; wanted.len()];
    let mut taken = vec![false; remote.len()];
    for (_, _, w, r) in pairs {
        if out[w].is_none() && !taken[r] {
            out[w] = Some(r);
            taken[r] = true;
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn rf(id: &str, path: &str, size: u64) -> RemoteFile {
        RemoteFile {
            id: id.into(),
            path: path.into(),
            size,
            selected: true,
            link: None,
        }
    }

    #[test]
    fn matches_files_by_size_and_path_tail() {
        let remote = vec![
            rf("1", "/Show S01/Extras/NCOP.mkv", 100),
            rf("2", "/Show S01/Show - 01.mkv", 500),
            rf("3", "Show S01/Show - 02.mkv", 500),
            rf("4", "Show - 03.mkv", 700),
            rf("5", "Show S01/Specials/Show - 02.mkv", 500),
        ];
        let wanted = [
            ("Show - 02.mkv", 500),
            ("Specials/Show - 02.mkv", 500),
            ("Show - 03.mkv", 700),
            ("Extras/NCOP.mkv", 100),
            ("Show - 04.mkv", 500), // not on the service
            ("Show - 03.mkv", 701), // wrong size
        ];
        assert_eq!(
            match_files(&wanted, &remote),
            vec![Some(2), Some(4), Some(3), Some(0), None, None]
        );
    }

    #[test]
    fn config_keeps_a_key_per_service() {
        let dir =
            std::env::temp_dir().join(format!("ichioshi-debrid-config-{}", std::process::id()));
        let path = dir.join("debrid.json");
        let mut config = Config {
            enabled: true,
            provider: Provider::TorBox,
            ..Default::default()
        };
        assert!(!config.active(), "no key yet");
        config.keys.insert(Provider::TorBox, " tb-key ".into());
        config.keys.insert(Provider::RealDebrid, "rd-key".into());
        assert!(config.active());
        config.save(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let loaded = Config::load(&path);
        assert_eq!(loaded, config);
        assert_eq!(loaded.key(Provider::TorBox), Some("tb-key"));
        assert_eq!(loaded.key(Provider::AllDebrid), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lenient_json_values() {
        assert_eq!(num(&serde_json::json!("123")), Some(123));
        assert_eq!(num(&serde_json::json!(12.7)), Some(12));
        assert_eq!(id(&serde_json::json!(42)), Some("42".into()));
        assert_eq!(
            date(&serde_json::json!(1_800_000_000)),
            Some("2027-01-15".into())
        );
        assert_eq!(
            date(&serde_json::json!("2032-06-06T04:42:42.000Z")),
            Some("2032-06-06".into())
        );
        assert_eq!(date(&serde_json::json!(0)), None);
    }
}
