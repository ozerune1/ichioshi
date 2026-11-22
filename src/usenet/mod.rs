//! Usenet downloads.
//!
//! Releases are found as torrents on Nyaa, as always. Anime indexers that
//! mirror those torrents to Usenet (AnimeTosho and TsukiHime) are asked for
//! the same release by its info hash, so a release found on several of them
//! is one entry with several NZBs ([`indexers`]). When you have a Usenet
//! provider set up and a release has an NZB, it downloads from Usenet:
//!
//! - [`nzb`] reads the NZB: which articles make up which file.
//! - [`nntp`] fetches articles from your providers, in priority order; backup
//!   providers are only asked for articles the main ones don't have.
//! - Articles are yEnc-decoded (each part's CRC checked) and written in place.
//! - If articles are missing everywhere, the release's PAR2 recovery files
//!   are fetched and the files repaired ([`repair`]).
//!
//! [`jobs`] runs it all, one release at a time, and resumes after a restart.

#[cfg(test)]
pub mod fake;
pub mod indexers;
pub mod jobs;
pub mod nntp;
pub mod nzb;
pub mod repair;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A Usenet provider (news server) account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Provider {
    /// Stable identifier.
    pub id: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: String,
    pub password: String,
    /// Simultaneous connections the account allows.
    pub connections: u16,
    /// Only asked for articles the main providers don't have (a block account).
    pub backup: bool,
    pub enabled: bool,
    /// SHA-256 of a self-signed server certificate to trust (instead of the
    /// usual certificate checks).
    pub fingerprint: Option<String>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            host: String::new(),
            port: 563,
            tls: true,
            username: String::new(),
            password: String::new(),
            connections: 10,
            backup: false,
            enabled: true,
            fingerprint: None,
        }
    }
}

impl Provider {
    /// The name to show: the one given, or the host.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() { &self.host } else { &self.name }
    }

    pub fn usable(&self) -> bool {
        self.enabled && !self.host.trim().is_empty()
    }
}

/// The Usenet settings, in `<state>/usenet.json` (private: it holds passwords).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    /// In priority order.
    pub providers: Vec<Provider>,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::fsutil::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Whether downloads can go through Usenet.
    pub fn active(&self) -> bool {
        self.providers.iter().any(Provider::usable)
    }

    /// Add a provider with a fresh ID.
    pub fn add(&mut self, mut provider: Provider) -> u64 {
        provider.id = self.providers.iter().map(|p| p.id + 1).max().unwrap_or(1);
        let id = provider.id;
        self.providers.push(provider);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usenet.json");
        assert_eq!(Config::load(&path), Config::default());
        let mut c = Config::default();
        assert!(!c.active());
        let id = c.add(Provider { host: "news.example.com".into(), username: "me".into(), password: "pw".into(), ..Default::default() });
        c.add(Provider { host: "block.example.com".into(), backup: true, ..Default::default() });
        assert_eq!((id, c.providers[1].id), (1, 2));
        assert!(c.active());
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        c.providers.iter_mut().for_each(|p| p.enabled = false);
        assert!(!c.active());
    }
}
