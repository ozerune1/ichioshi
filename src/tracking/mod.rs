//! Your anime lists on AniList and MyAnimeList: signing in, reading the
//! lists, and changing an entry's status, score and progress.
//!
//! Both sites need an API client registered by the user (they don't let
//! apps sign people in otherwise). Sign-in sends the browser back to a
//! server on 127.0.0.1 at a fixed port, the redirect URL given when
//! registering the client.

pub mod anilist;
mod callback;
pub mod mal;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The port in the redirect URLs registered with AniList and MyAnimeList.
pub const PORT: u16 = 47341;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Service {
    AniList,
    Mal,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Service::AniList => "AniList",
            Service::Mal => "MyAnimeList",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Service::AniList => "anilist",
            Service::Mal => "mal",
        }
    }
}

/// Where a list entry stands (the same on both sites).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    Watching,
    Planning,
    Completed,
    Rewatching,
    Paused,
    Dropped,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Watching,
        Status::Planning,
        Status::Completed,
        Status::Rewatching,
        Status::Paused,
        Status::Dropped,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Status::Watching => "Watching",
            Status::Planning => "Plan to watch",
            Status::Completed => "Completed",
            Status::Rewatching => "Rewatching",
            Status::Paused => "On hold",
            Status::Dropped => "Dropped",
        }
    }
}

/// An anime on a list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub status: Status,
    /// Out of 10 (AniList keeps tenths); 0 is no score.
    pub score: f32,
    /// Episodes watched.
    pub progress: u32,
}

impl Entry {
    /// "8", "7.5", or "–".
    pub fn score_text(&self) -> String {
        score_text(self.score)
    }
}

pub fn score_text(score: f32) -> String {
    if score <= 0.0 {
        "–".to_owned()
    } else if (score - score.round()).abs() < 0.05 {
        format!("{:.0}", score.round())
    } else {
        format!("{score:.1}")
    }
}

/// MyAnimeList's words for its scores.
pub fn score_word(score: u32) -> &'static str {
    match score {
        10 => "Masterpiece",
        9 => "Great",
        8 => "Very good",
        7 => "Good",
        6 => "Fine",
        5 => "Average",
        4 => "Bad",
        3 => "Very bad",
        2 => "Horrible",
        1 => "Appalling",
        _ => "No score",
    }
}

/// One change to an entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Status(Status),
    Score(f32),
    Progress(u32),
    /// Episode n was watched (in the player): progress up to it (never
    /// back), and the status follows: watching, or completed at the end.
    Watched(u32),
}

impl Change {
    /// The entry after the change. Completing sets progress to the episode
    /// count (when known), as both sites do.
    pub fn apply(&self, entry: Option<&Entry>, episodes: Option<u32>) -> Entry {
        let mut e = entry.cloned().unwrap_or(Entry {
            status: Status::Watching,
            score: 0.0,
            progress: 0,
        });
        match *self {
            Change::Status(s) => {
                e.status = s;
                if s == Status::Completed
                    && let Some(n) = episodes.filter(|n| *n > 0)
                {
                    e.progress = n;
                }
            }
            Change::Score(s) => e.score = s.clamp(0.0, 10.0),
            Change::Progress(p) => e.progress = episodes.filter(|n| *n > 0).map_or(p, |n| p.min(n)),
            Change::Watched(n) => {
                let n = episodes.filter(|t| *t > 0).map_or(n, |t| n.min(t));
                if n <= e.progress && entry.is_some() {
                    return e;
                }
                e.progress = n;
                let last = episodes.is_some_and(|t| t > 0 && n >= t);
                e.status = match e.status {
                    _ if last && e.status != Status::Completed => Status::Completed,
                    Status::Planning | Status::Paused | Status::Dropped => Status::Watching,
                    other => other,
                };
            }
        }
        e
    }
}

/// The token stopped working: sign in again.
#[derive(Debug)]
pub struct SignedOut;

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("signed out: sign in again")
    }
}

impl std::error::Error for SignedOut {}

/// The sites' addresses (tests point them at mock servers).
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub anilist_auth: String,
    pub anilist_api: String,
    pub mal_auth: String,
    pub mal_token: String,
    pub mal_api: String,
    /// The sign-in callback's port: `PORT`, or 0 for any (tests).
    pub port: u16,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            anilist_auth: "https://anilist.co/api/v2/oauth/authorize".into(),
            anilist_api: "https://graphql.anilist.co".into(),
            mal_auth: "https://myanimelist.net/v1/oauth2/authorize".into(),
            mal_token: "https://myanimelist.net/v1/oauth2/token".into(),
            mal_api: "https://api.myanimelist.net/v2".into(),
            port: PORT,
        }
    }
}

/// The redirect URL to register for a service.
pub fn redirect_url(service: Service) -> String {
    format!("http://127.0.0.1:{PORT}/{}", service.path())
}

/// The accounts, saved in `tracking.json` (only you can read it).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub anilist: AniListAccount,
    pub mal: MalAccount,
    /// MyAnimeList's IDs for AniList's, as AniList gives them.
    pub mal_ids: BTreeMap<u64, u64>,
    /// Update the lists when an episode is finished in the player.
    pub sync_watched: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            anilist: Default::default(),
            mal: Default::default(),
            mal_ids: BTreeMap::new(),
            sync_watched: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AniListAccount {
    pub client_id: String,
    pub token: Option<String>,
    /// Unix time.
    pub expires_at: i64,
    pub user: Option<anilist::User>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MalAccount {
    pub client_id: String,
    /// Only for clients MyAnimeList gave one.
    pub client_secret: String,
    pub tokens: Option<mal::Tokens>,
    pub user: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| match serde_json::from_str(&t) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!("ignoring unreadable {}: {e}", path.display());
                    None
                }
            })
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::fsutil::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn signed_in(&self, service: Service) -> bool {
        match service {
            Service::AniList => self.anilist.token.is_some() && self.anilist.user.is_some(),
            Service::Mal => self.mal.tokens.is_some(),
        }
    }
}

#[cfg(test)]
mod tests;
