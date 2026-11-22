//! Searching releases.moe (SeaDex).
//!
//! SeaDex entries carry no titles: they are keyed by AniList ID. The
//! releases.moe site itself searches AniList for titles and then looks up
//! which of those IDs have an entry, and so do we.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

const ANILIST_URL: &str = "https://graphql.anilist.co";
const SEADEX_ENTRIES_URL: &str = "https://releases.moe/api/collections/entries/records";

const SEARCH_QUERY: &str = r#"
query ($search: String) {
  Page(perPage: 30) {
    media(search: $search, type: ANIME, sort: SEARCH_MATCH) {
      id
      format
      seasonYear
      episodes
      title { romaji english }
      coverImage { medium large }
    }
  }
}
"#;

/// An AniList search hit, plus its SeaDex entry if it has one.
#[derive(Debug, Clone)]
pub struct Anime {
    pub anilist_id: u64,
    /// The English title if there is one, else romaji.
    pub title: String,
    pub alt_title: Option<String>,
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub format: Option<String>,
    pub year: Option<i32>,
    pub episodes: Option<u32>,
    pub cover_small: Option<String>,
    pub cover_large: Option<String>,
    pub entry: Option<Entry>,
}

impl Anime {
    pub fn anilist_url(&self) -> String {
        format!("https://anilist.co/anime/{}", self.anilist_id)
    }

    pub fn seadex_url(&self) -> String {
        format!("https://releases.moe/{}/", self.anilist_id)
    }

    /// "TV · 2023 · 28 eps"
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(f) = &self.format {
            parts.push(f.replace('_', " "));
        }
        if let Some(y) = self.year {
            parts.push(y.to_string());
        }
        if let Some(e) = self.episodes {
            parts.push(format!("{e} ep{}", if e == 1 { "" } else { "s" }));
        }
        parts.join(" · ")
    }
}

/// A SeaDex entry: the curated releases for one AniList ID.
#[derive(Debug, Clone)]
pub struct Entry {
    pub notes: String,
    pub incomplete: bool,
    pub theoretical_best: String,
    pub releases: Vec<Release>,
}

impl Entry {
    /// The release this app downloads by default (the first one in [`Entry::ranked`]).
    /// `None` if nothing in the entry is on Nyaa.
    pub fn best_nyaa(&self, prefer_dual_audio: bool) -> Option<usize> {
        self.ranked(prefer_dual_audio)
            .into_iter()
            .find(|&i| self.releases[i].nyaa_id().is_some())
    }

    /// Release indices in display order: downloadable (Nyaa) before private
    /// trackers, SeaDex "best" before alternatives, then dual audio first if
    /// preferred. Otherwise SeaDex's own order is kept.
    pub fn ranked(&self, prefer_dual_audio: bool) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.releases.len()).collect();
        order.sort_by_key(|&i| {
            let r = &self.releases[i];
            (
                r.nyaa_id().is_none(),
                !r.is_best,
                prefer_dual_audio && !r.dual_audio,
            )
        });
        order
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(rename = "releaseGroup", default)]
    pub group: String,
    #[serde(rename = "isBest", default)]
    pub is_best: bool,
    #[serde(rename = "dualAudio", default)]
    pub dual_audio: bool,
    #[serde(default)]
    pub tracker: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "infoHash", default)]
    pub info_hash: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub files: Vec<ReleaseFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseFile {
    #[serde(default)]
    pub length: u64,
}

impl Release {
    /// The Nyaa torrent ID, if this release is hosted on Nyaa.
    pub fn nyaa_id(&self) -> Option<u64> {
        if !self.tracker.eq_ignore_ascii_case("nyaa") {
            return None;
        }
        crate::nyaa::view_id(&self.url)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.length).sum()
    }

    /// A 40-character hex info hash, if SeaDex has a real one (private
    /// trackers show `<redacted>`).
    pub fn info_hash_hex(&self) -> Option<&str> {
        let h = self.info_hash.trim();
        (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
    }
}

/// Search AniList for `query` and attach SeaDex entries to the hits.
/// Hits that have an entry come first; AniList's relevance order is kept otherwise.
pub async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<Anime>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = anilist_search(client, query).await?;
    if results.is_empty() {
        return Ok(results);
    }

    let ids: Vec<u64> = results.iter().map(|a| a.anilist_id).collect();
    let mut entries = fetch_entries(client, &ids).await?;
    for anime in &mut results {
        anime.entry = entries.remove(&anime.anilist_id);
    }
    // Stable sort, so relevance order holds within each group.
    results.sort_by_key(|a| a.entry.is_none());
    Ok(results)
}

/// Search AniList only (no SeaDex lookup).
pub async fn anilist_search(client: &reqwest::Client, query: &str) -> Result<Vec<Anime>> {
    #[derive(Deserialize)]
    struct Resp {
        data: Option<Data>,
        #[serde(default)]
        errors: Vec<GqlError>,
    }
    #[derive(Deserialize)]
    struct GqlError {
        message: String,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "Page")]
        page: Page,
    }
    #[derive(Deserialize)]
    struct Page {
        media: Vec<Media>,
    }
    #[derive(Deserialize)]
    struct Media {
        id: u64,
        format: Option<String>,
        #[serde(rename = "seasonYear")]
        season_year: Option<i32>,
        episodes: Option<u32>,
        title: Title,
        #[serde(rename = "coverImage")]
        cover_image: Option<Cover>,
    }
    #[derive(Deserialize)]
    struct Title {
        romaji: Option<String>,
        english: Option<String>,
    }
    #[derive(Deserialize)]
    struct Cover {
        medium: Option<String>,
        large: Option<String>,
    }

    let resp = client
        .post(ANILIST_URL)
        .json(&json!({ "query": SEARCH_QUERY, "variables": { "search": query } }))
        .send()
        .await
        .context("could not reach AniList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("AniList is rate limiting searches; wait a minute and try again");
    }
    let body: Resp = resp
        .json()
        .await
        .with_context(|| format!("unexpected response from AniList (HTTP {status})"))?;
    if let Some(e) = body.errors.first() {
        bail!("AniList error: {}", e.message);
    }
    let media = body.data.context("AniList returned no data")?.page.media;

    Ok(media
        .into_iter()
        .map(|m| {
            let romaji = m.title.romaji.filter(|s| !s.is_empty());
            let english = m.title.english.filter(|s| !s.is_empty());
            let (title, alt_title) = match (&english, &romaji) {
                (Some(en), Some(ro)) if en != ro => (en.clone(), Some(ro.clone())),
                (Some(en), _) => (en.clone(), None),
                (None, Some(ro)) => (ro.clone(), None),
                (None, None) => (format!("AniList #{}", m.id), None),
            };
            let cover = m.cover_image;
            Anime {
                anilist_id: m.id,
                title,
                alt_title,
                romaji,
                english,
                format: m.format,
                year: m.season_year,
                episodes: m.episodes,
                cover_small: cover.as_ref().and_then(|c| c.medium.clone()),
                cover_large: cover.and_then(|c| c.large.or(c.medium)),
                entry: None,
            }
        })
        .collect())
}

/// Fetch SeaDex entries (with their torrents) for the given AniList IDs.
pub async fn fetch_entries(client: &reqwest::Client, ids: &[u64]) -> Result<HashMap<u64, Entry>> {
    #[derive(Deserialize)]
    struct Resp {
        items: Vec<RawEntry>,
    }
    #[derive(Deserialize)]
    struct RawEntry {
        #[serde(rename = "alID")]
        al_id: u64,
        #[serde(default)]
        notes: String,
        #[serde(default)]
        incomplete: bool,
        #[serde(rename = "theoreticalBest", default)]
        theoretical_best: String,
        #[serde(default)]
        expand: Option<Expand>,
    }
    #[derive(Deserialize)]
    struct Expand {
        #[serde(default)]
        trs: Vec<Release>,
    }

    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let filter = ids
        .iter()
        .map(|id| format!("alID={id}"))
        .collect::<Vec<_>>()
        .join("||");
    let per_page = ids.len().to_string();

    let resp = client
        .get(SEADEX_ENTRIES_URL)
        .query(&[
            ("filter", format!("({filter})").as_str()),
            ("expand", "trs"),
            ("perPage", per_page.as_str()),
            ("skipTotal", "true"),
        ])
        .send()
        .await
        .context("could not reach releases.moe")?
        .error_for_status()
        .context("releases.moe returned an error")?;
    let body: Resp = resp
        .json()
        .await
        .context("unexpected response from releases.moe")?;

    Ok(body
        .items
        .into_iter()
        .map(|e| {
            (
                e.al_id,
                Entry {
                    notes: e.notes.trim().to_owned(),
                    incomplete: e.incomplete,
                    theoretical_best: e.theoretical_best.trim().to_owned(),
                    releases: e.expand.map(|x| x.trs).unwrap_or_default(),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(group: &str, tracker: &str, url: &str, best: bool, dual: bool) -> Release {
        Release {
            group: group.into(),
            is_best: best,
            dual_audio: dual,
            tracker: tracker.into(),
            url: url.into(),
            info_hash: String::new(),
            tags: vec![],
            files: vec![],
        }
    }

    fn entry(releases: Vec<Release>) -> Entry {
        Entry {
            notes: String::new(),
            incomplete: false,
            theoretical_best: String::new(),
            releases,
        }
    }

    #[test]
    fn best_prefers_nyaa_best_over_private_best() {
        let e = entry(vec![
            release("A", "AB", "/torrents.php?id=1", true, true),
            release("B", "Nyaa", "https://nyaa.si/view/2", false, true),
            release("C", "Nyaa", "https://nyaa.si/view/3", true, false),
        ]);
        assert_eq!(e.best_nyaa(false), Some(2));
        assert_eq!(e.ranked(false), vec![2, 1, 0]);
    }

    #[test]
    fn dual_audio_preference_only_breaks_ties() {
        let e = entry(vec![
            release("Sub", "Nyaa", "https://nyaa.si/view/1", true, false),
            release("Dub", "Nyaa", "https://nyaa.si/view/2", true, true),
            release("Alt", "Nyaa", "https://nyaa.si/view/3", false, true),
        ]);
        assert_eq!(e.best_nyaa(false), Some(0));
        assert_eq!(e.best_nyaa(true), Some(1));
        // A non-best dual-audio release never beats a best one.
        assert_eq!(e.ranked(true), vec![1, 0, 2]);
    }

    #[test]
    fn no_nyaa_release_means_no_best() {
        let e = entry(vec![release("A", "AB", "/torrents.php?id=1", true, false)]);
        assert_eq!(e.best_nyaa(false), None);
    }

    #[test]
    fn parses_seadex_release_json() {
        let r: Release = serde_json::from_str(
            r#"{"dualAudio":true,"infoHash":"<redacted>","isBest":true,"releaseGroup":"PMR",
                "tags":["Best"],"tracker":"AB","url":"/torrents.php?id=1","files":[{"length":5,"name":"a.mkv"}]}"#,
        )
        .unwrap();
        assert_eq!(r.group, "PMR");
        assert!(r.info_hash_hex().is_none());
        assert_eq!(r.total_size(), 5);
        assert!(r.nyaa_id().is_none());
    }
}
