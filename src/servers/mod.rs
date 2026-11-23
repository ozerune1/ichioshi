//! A library kept on your own servers instead of this computer: Sonarr
//! (series) and Radarr (films) say what's there and download what's wanted
//! with their own download client; Jellyfin streams it and keeps what's been
//! watched. Anime are matched to AniList through the anime-lists mapping.

pub mod arr;
pub mod build;
pub mod jellyfin;
pub mod mapping;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Where the library is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryMode {
    /// Folders on this computer (the app downloads into them).
    #[default]
    Local,
    /// Jellyfin, Sonarr and Radarr.
    Servers,
}

/// Sonarr or Radarr: where, and its API key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArrAccount {
    pub url: String,
    pub api_key: String,
    /// What anime are added with.
    pub quality_profile: Option<u64>,
    pub root_folder: Option<String>,
}

impl ArrAccount {
    pub fn is_set(&self) -> bool {
        !self.url.trim().is_empty() && !self.api_key.trim().is_empty()
    }
}

/// Jellyfin: where, and who's signed in (its token; the password isn't
/// kept).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinAccount {
    pub url: String,
    pub username: String,
    pub token: Option<String>,
    pub user_id: Option<String>,
    pub server_name: Option<String>,
    /// This app, to Jellyfin (made once).
    pub device_id: String,
}

impl JellyfinAccount {
    pub fn signed_in(&self) -> bool {
        self.token.is_some() && self.user_id.is_some() && !self.url.trim().is_empty()
    }
}

/// The servers, saved (privately: it holds API keys) in `servers.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: LibraryMode,
    pub jellyfin: JellyfinAccount,
    pub sonarr: ArrAccount,
    pub radarr: ArrAccount,
}

impl Config {
    pub fn file(state_dir: &Path) -> PathBuf {
        state_dir.join("servers.json")
    }

    pub fn load(state_dir: &Path) -> Self {
        let mut c: Config = std::fs::read(Self::file(state_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if c.jellyfin.device_id.is_empty() {
            let mut buf = [0u8; 8];
            let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf);
            c.jellyfin.device_id = format!("ichioshi-{:016x}", u64::from_le_bytes(buf));
        }
        c
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        crate::fsutil::write_private(&Self::file(state_dir), &serde_json::to_vec_pretty(self)?)
    }

    /// Something to show: Sonarr or Radarr is set up.
    pub fn any(&self) -> bool {
        self.sonarr.is_set() || self.radarr.is_set()
    }
}

/// A server's base URL, without a trailing slash (and with a scheme).
pub fn base(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    }
}

/// `f` over `items`, `at_once` at a time, in their order.
pub(crate) async fn each<T, R, F, Fut>(items: Vec<T>, at_once: usize, f: F) -> Vec<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = R> + Send + 'static,
{
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(at_once.max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    for (i, item) in items.into_iter().enumerate() {
        let gate = gate.clone();
        let fut = f(item);
        tasks.spawn(async move {
            let _turn = gate.acquire_owned().await;
            (i, fut.await)
        });
    }
    let mut out: Vec<(usize, R)> = Vec::new();
    while let Some(done) = tasks.join_next().await {
        if let Ok(r) = done {
            out.push(r);
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// What went wrong talking to a server, readably.
pub(crate) fn describe(e: &reqwest::Error, what: &str) -> String {
    if e.is_connect() {
        format!("couldn't reach {what} (is the address right, and is it running?)")
    } else if e.is_timeout() {
        format!("{what} didn't answer in time")
    } else if let Some(s) = e.status() {
        match s.as_u16() {
            401 | 403 => format!("{what} refused the API key or sign-in (HTTP {s})"),
            404 => format!("{what} doesn't have that (HTTP 404; is the address right?)"),
            _ => format!("{what} answered HTTP {s}"),
        }
    } else {
        format!("{what}: {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases() {
        assert_eq!(base(" nas:8989/ "), "http://nas:8989");
        assert_eq!(
            base("https://x.example/sonarr/"),
            "https://x.example/sonarr"
        );
    }

    #[test]
    fn saves_privately_and_makes_a_device_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::load(dir.path());
        assert!(c.jellyfin.device_id.starts_with("ichioshi-"));
        c.sonarr.url = "nas:8989".into();
        c.sonarr.api_key = "k".into();
        c.save(dir.path()).unwrap();
        let again = Config::load(dir.path());
        assert_eq!(again, c);
        assert!(again.any());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(Config::file(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "only you can read it");
    }
}
