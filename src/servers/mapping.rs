//! Which AniList anime is which show and season on TheTVDB (Sonarr's
//! catalogue) or which film on TheMovieDB (Radarr's), from the community
//! "anime-lists" mapping (github.com/Fribb/anime-lists), kept on disk and
//! fetched again weekly.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;

const URL: &str = "https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-full.json";

/// Where the mapping comes from instead (tests).
#[cfg(test)]
pub static BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn url() -> String {
    #[cfg(test)]
    if let Some(base) = BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    URL.to_owned()
}

/// Where an AniList anime is on TheTVDB or TheMovieDB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub tvdb: Option<u64>,
    /// The TVDB season it is (0: specials).
    pub season: u32,
    /// Its episode 1 is the season's episode 1 + this.
    pub offset: u32,
    /// A film on TheMovieDB.
    pub tmdb_movie: Option<u64>,
    pub movie: bool,
}

#[derive(Default)]
pub struct Mapping {
    by_anilist: HashMap<u64, Place>,
    /// AniList anime by TVDB show, and the season (and offset) each is.
    by_tvdb: HashMap<u64, Vec<(u64, u32, u32)>>,
    by_tmdb_movie: HashMap<u64, u64>,
}

#[derive(Deserialize)]
struct Raw {
    anilist_id: Option<u64>,
    #[serde(rename = "type")]
    kind: Option<String>,
    tvdb_id: Option<u64>,
    #[serde(default)]
    themoviedb_id: Option<serde_json::Value>,
    #[serde(default)]
    season: Option<HashMap<String, i64>>,
    /// -1 for specials whose place among TVDB's isn't known.
    #[serde(default)]
    episode_offset: Option<HashMap<String, i64>>,
}

impl Mapping {
    pub fn parse(json: &[u8]) -> Result<Self> {
        let raw: Vec<serde_json::Value> =
            serde_json::from_slice(json).context("not the anime-lists mapping")?;
        let mut m = Mapping::default();
        // An entry that can't be read is left out, not the whole list.
        for r in raw
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Raw>(v).ok())
        {
            let Some(anilist) = r.anilist_id else {
                continue;
            };
            let tvdb = |field: &Option<HashMap<String, i64>>| {
                field.as_ref().and_then(|f| f.get("tvdb").copied())
            };
            let (season, offset) = (
                tvdb(&r.season).unwrap_or(1),
                tvdb(&r.episode_offset).unwrap_or(0),
            );
            // Where on TheTVDB isn't known: not one for Sonarr.
            let placed = season >= 0 && offset >= 0;
            let movie = r.kind.as_deref() == Some("MOVIE");
            let tmdb_movie = r.themoviedb_id.as_ref().and_then(|t| {
                let v = &t["movie"];
                v.as_u64()
                    .or_else(|| v.get(0).and_then(serde_json::Value::as_u64))
            });
            let place = Place {
                tvdb: r.tvdb_id.filter(|_| placed),
                season: season.max(0) as u32,
                offset: offset.max(0) as u32,
                tmdb_movie,
                movie,
            };
            if let Some(tvdb) = place.tvdb
                && !movie
            {
                m.by_tvdb
                    .entry(tvdb)
                    .or_default()
                    .push((anilist, place.season, place.offset));
            }
            if let Some(tmdb) = tmdb_movie
                && movie
            {
                m.by_tmdb_movie.entry(tmdb).or_insert(anilist);
            }
            m.by_anilist.insert(anilist, place);
        }
        Ok(m)
    }

    pub fn place(&self, anilist: u64) -> Option<Place> {
        self.by_anilist.get(&anilist).copied()
    }

    /// The AniList anime that are seasons of a TVDB show: (AniList ID,
    /// season, offset).
    pub fn seasons_of(&self, tvdb: u64) -> &[(u64, u32, u32)] {
        self.by_tvdb.get(&tvdb).map_or(&[], Vec::as_slice)
    }

    pub fn film(&self, tmdb: u64) -> Option<u64> {
        self.by_tmdb_movie.get(&tmdb).copied()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_anilist.len()
    }
}

/// The mapping: from `file` if it's under a week old, else fetched (and
/// kept there); the old copy if fetching fails.
pub async fn load(client: &reqwest::Client, file: &Path) -> Result<Mapping> {
    let fresh = std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < Duration::from_secs(7 * 86_400));
    if fresh && let Ok(bytes) = std::fs::read(file) {
        return Mapping::parse(&bytes);
    }
    let fetched = async {
        let resp = client.get(url()).send().await?.error_for_status()?;
        anyhow::Ok(resp.bytes().await?)
    }
    .await;
    match fetched {
        Ok(bytes) => {
            let mapping = Mapping::parse(&bytes)?;
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, &bytes);
            Ok(mapping)
        }
        Err(e) => match std::fs::read(file) {
            Ok(bytes) => Mapping::parse(&bytes),
            Err(_) => Err(e.context("couldn't fetch the anime mapping (anime-lists)")),
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Entries like the real list's: Frieren, Apothecary Diaries 1 and 2
    /// (one TVDB show), a film.
    pub(crate) const SAMPLE: &str = r#"[
        {"type": "TV", "anilist_id": 154587, "mal_id": 52991, "tvdb_id": 424536,
         "themoviedb_id": {"tv": 209867}, "season": {"tvdb": 1, "tmdb": 1}},
        {"type": "TV", "anilist_id": 161645, "tvdb_id": 431162, "season": {"tvdb": 1}},
        {"type": "TV", "anilist_id": 176301, "tvdb_id": 431162, "season": {"tvdb": 2, "tmdb": 1},
         "episode_offset": {"tmdb": 24}},
        {"type": "TV", "anilist_id": 9, "tvdb_id": 9000, "season": {"tvdb": 1},
         "episode_offset": {"tvdb": 12}},
        {"type": "MOVIE", "anilist_id": 112151, "tvdb_id": 348545,
         "themoviedb_id": {"movie": [635302]}, "season": {"tvdb": 0}},
        {"type": "TV", "mal_id": 1},
        {"type": "OVA", "anilist_id": 13047, "tvdb_id": 81341, "season": {"tvdb": 0},
         "episode_offset": {"tvdb": -1}},
        {"type": "TV", "anilist_id": 77, "tvdb_id": "not a number"}
    ]"#;

    #[test]
    fn maps_both_ways() {
        let m = Mapping::parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.len(), 6, "all but the unreadable one");
        // A special whose place on TheTVDB isn't known (as in the real
        // list): no place for Sonarr.
        assert_eq!(m.place(13047).unwrap().tvdb, None);
        assert!(m.seasons_of(81341).is_empty());
        let s2 = m.place(176301).unwrap();
        assert_eq!(
            (s2.tvdb, s2.season, s2.offset, s2.movie),
            (Some(431162), 2, 0, false)
        );
        assert_eq!(m.place(9).unwrap().offset, 12, "TVDB's offset, not TMDB's");
        let mut seasons = m.seasons_of(431162).to_vec();
        seasons.sort();
        assert_eq!(seasons, [(161645, 1, 0), (176301, 2, 0)]);
        assert_eq!(m.film(635302), Some(112151));
        assert!(m.seasons_of(348545).is_empty(), "films aren't seasons");
        assert_eq!(m.place(112151).unwrap().tmdb_movie, Some(635302));
    }

    /// The real list (fetched from GitHub) reads, as the app reads it.
    #[test]
    #[ignore = "fetches the real anime-lists mapping"]
    fn reads_the_real_mapping() {
        let _net = crate::mock_http::real();
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .user_agent("ichioshi-test")
            .build()
            .unwrap();
        let m = rt
            .block_on(load(&client, &dir.path().join("anime-lists.json")))
            .unwrap();
        assert!(m.len() > 15_000, "{}", m.len());
        let frieren = m.place(154587).unwrap();
        assert_eq!((frieren.tvdb, frieren.season), (Some(424536), 1));
        let mut apothecary = m.seasons_of(431162).to_vec();
        apothecary.sort();
        assert!(apothecary.contains(&(161645, 1, 0)) && apothecary.contains(&(176301, 2, 0)));
        assert!(
            dir.path().join("anime-lists.json").exists(),
            "kept for next time"
        );
    }
}
