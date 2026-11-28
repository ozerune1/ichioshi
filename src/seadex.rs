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

/// AniList's title search. Adult anime are left out unless `adult`: AniList
/// includes them when `isAdult` isn't given (and takes `isAdult: null` as
/// false), so the filter is written in or left out.
fn search_query(adult: bool) -> String {
    let filter = if adult { "" } else { ", isAdult: false" };
    format!(
        "query ($search: String, $page: Int) {{ Page(page: $page, perPage: {SEARCH_PAGE}) {{ \
         pageInfo {{ hasNextPage }} media(search: $search, type: ANIME, \
         sort: SEARCH_MATCH{filter}) {{ id format seasonYear episodes isAdult \
         title {{ romaji english }} coverImage {{ medium large }} }} }} }}"
    )
}

/// Search results a page at a time.
pub const SEARCH_PAGE: u32 = 50;

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
    /// An 18+ title (AniList's `isAdult`): found only with 18+ on, and kept
    /// local (never sent to Sonarr or Jellyfin).
    pub adult: bool,
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
/// Adult anime only with `adult`.
#[cfg(test)]
pub async fn search(client: &reqwest::Client, query: &str, adult: bool) -> Result<Vec<Anime>> {
    Ok(search_page(client, query, adult, 1).await?.0)
}

/// Page `page` (from 1) of [`search`]'s hits, and whether AniList has
/// more after it. A page's hits with an entry come first in it.
pub async fn search_page(
    client: &reqwest::Client,
    query: &str,
    adult: bool,
    page: u32,
) -> Result<(Vec<Anime>, bool)> {
    let query = query.trim();
    if query.is_empty() {
        return Ok((Vec::new(), false));
    }

    let (mut results, more) = anilist_page(client, query, adult, page).await?;
    if results.is_empty() {
        return Ok((results, more));
    }

    let ids: Vec<u64> = results.iter().map(|a| a.anilist_id).collect();
    let mut entries = fetch_entries(client, &ids).await?;
    for anime in &mut results {
        anime.entry = entries.remove(&anime.anilist_id);
    }
    // Stable sort, so relevance order holds within each group.
    results.sort_by_key(|a| a.entry.is_none());
    Ok((results, more))
}

/// Search AniList only (no SeaDex lookup). Adult anime only with `adult`.
pub async fn anilist_search(
    client: &reqwest::Client,
    query: &str,
    adult: bool,
) -> Result<Vec<Anime>> {
    Ok(anilist_page(client, query, adult, 1).await?.0)
}

/// Page `page` of [`anilist_search`]'s hits, and whether there are more.
async fn anilist_page(
    client: &reqwest::Client,
    query: &str,
    adult: bool,
    page: u32,
) -> Result<(Vec<Anime>, bool)> {
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
        #[serde(rename = "pageInfo", default)]
        page_info: PageInfo,
        media: Vec<Media>,
    }
    #[derive(Deserialize, Default)]
    struct PageInfo {
        #[serde(rename = "hasNextPage", default)]
        has_next_page: bool,
    }
    #[derive(Deserialize)]
    struct Media {
        id: u64,
        format: Option<String>,
        #[serde(rename = "seasonYear")]
        season_year: Option<i32>,
        episodes: Option<u32>,
        #[serde(rename = "isAdult", default)]
        is_adult: Option<bool>,
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
        .post(anilist_url())
        .json(&json!({
            "query": search_query(adult),
            "variables": { "search": query, "page": page.max(1) },
        }))
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
    let found = body.data.context("AniList returned no data")?.page;
    let more = found.page_info.has_next_page;
    let media = found.media;

    let list = media
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
                adult: m.is_adult.unwrap_or(false),
            }
        })
        .collect();
    Ok((list, more))
}

const DETAILS_QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id, type: ANIME) {
    idMal
    title { romaji english native }
    synonyms
    description(asHtml: false)
    genres
    averageScore
    status
    format
    episodes
    duration
    season
    seasonYear
    source
    studios(isMain: true) { nodes { name } }
    startDate { year month day }
    endDate { year month day }
    nextAiringEpisode { episode airingAt }
    coverImage { large extraLarge }
    bannerImage
    streamingEpisodes { title }
    airingSchedule(perPage: 50) { nodes { episode airingAt } }
    recommendations(sort: RATING_DESC, perPage: 16) {
      nodes {
        rating
        mediaRecommendation {
          id idMal title { english romaji native } coverImage { extraLarge large }
          bannerImage format episodes status season seasonYear averageScore
          popularity genres isAdult nextAiringEpisode { episode airingAt }
        }
      }
    }
  }
}
"#;

/// Where SeaDex requests go instead (tests).
#[cfg(test)]
pub static SEADEX_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Where AniList requests go instead (tests).
#[cfg(test)]
pub static ANILIST_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(crate) fn anilist_url() -> String {
    #[cfg(test)]
    if let Some(base) = ANILIST_BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    ANILIST_URL.to_owned()
}

/// AniList's details about an anime: description, genres, score, studios,
/// airing, and so on.
pub async fn anilist_details(client: &reqwest::Client, id: u64) -> Result<crate::library::Details> {
    let resp = client
        .post(anilist_url())
        .json(&json!({ "query": DETAILS_QUERY, "variables": { "id": id } }))
        .send()
        .await
        .context("could not reach AniList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("AniList is busy; try again in a minute");
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("unexpected response from AniList (HTTP {status})"))?;
    if let Some(e) = body["errors"].get(0) {
        bail!(
            "AniList error: {}",
            e["message"].as_str().unwrap_or("unknown")
        );
    }
    let m = &body["data"]["Media"];
    if !m.is_object() {
        bail!("AniList has no anime {id}");
    }
    let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let list = |v: &serde_json::Value| {
        v.as_array()
            .map(|a| a.iter().filter_map(text).collect())
            .unwrap_or_default()
    };
    let date = |d: &serde_json::Value| {
        let year = d["year"].as_i64()?;
        Some(match (d["month"].as_i64(), d["day"].as_i64()) {
            (Some(m), Some(day)) => format!("{year}-{m:02}-{day:02}"),
            (Some(m), None) => format!("{year}-{m:02}"),
            _ => year.to_string(),
        })
    };
    // "Episode 3 - Killing Magic", as streaming sites list them.
    static STREAMED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^episode\s+(\d+)\s*[-–:]\s*(.+)$").unwrap()
    });
    let mut episodes: std::collections::BTreeMap<u32, crate::library::EpisodeInfo> =
        Default::default();
    for e in m["streamingEpisodes"].as_array().into_iter().flatten() {
        let Some(c) = e["title"]
            .as_str()
            .and_then(|t| STREAMED.captures(t.trim()))
        else {
            continue;
        };
        let Ok(number) = c[1].parse::<u32>() else {
            continue;
        };
        let entry = episodes
            .entry(number)
            .or_insert_with(|| crate::library::EpisodeInfo {
                number,
                ..Default::default()
            });
        entry.title.get_or_insert_with(|| c[2].trim().to_owned());
    }
    // Later seasons are often numbered from the show's start (a second
    // season of 24 as 25–48): count them from 1 when they'd fit that way.
    if let (Some(count), Some(first), Some(last)) = (
        m["episodes"].as_u64().map(|c| c as u32),
        episodes.keys().next().copied(),
        episodes.keys().next_back().copied(),
    ) && first > 1
        && last > count
        && last - first < count
    {
        let shift = first - 1;
        episodes = std::mem::take(&mut episodes)
            .into_values()
            .map(|mut e| {
                e.number -= shift;
                (e.number, e)
            })
            .collect();
    }
    for n in m["airingSchedule"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let (Some(number), Some(at)) = (n["episode"].as_u64(), n["airingAt"].as_i64()) else {
            continue;
        };
        let number = number as u32;
        episodes
            .entry(number)
            .or_insert_with(|| crate::library::EpisodeInfo {
                number,
                ..Default::default()
            })
            .airs = Some(at);
    }
    Ok(crate::library::Details {
        episode_list: episodes.into_values().collect(),
        id_mal: m["idMal"].as_u64(),
        native: text(&m["title"]["native"]),
        synonyms: list(&m["synonyms"]),
        description: text(&m["description"]).map(|d| plain_text(&d)),
        genres: list(&m["genres"]),
        score: m["averageScore"].as_u64().map(|v| v as u32),
        status: text(&m["status"]),
        format: text(&m["format"]),
        episodes: m["episodes"].as_u64().map(|v| v as u32),
        duration: m["duration"].as_u64().map(|v| v as u32),
        season: text(&m["season"]),
        year: m["seasonYear"].as_i64().map(|v| v as i32),
        source: text(&m["source"]),
        studios: m["studios"]["nodes"]
            .as_array()
            .map(|a| a.iter().filter_map(|n| text(&n["name"])).collect())
            .unwrap_or_default(),
        start_date: date(&m["startDate"]),
        end_date: date(&m["endDate"]),
        next_episode: m["nextAiringEpisode"]["episode"]
            .as_u64()
            .zip(m["nextAiringEpisode"]["airingAt"].as_i64())
            .map(|(e, t)| (e as u32, t)),
        cover: text(&m["coverImage"]["extraLarge"]).or_else(|| text(&m["coverImage"]["large"])),
        banner: text(&m["bannerImage"]),
        // Those its users rate up, once each.
        recommendations: {
            let mut seen = std::collections::HashSet::new();
            m["recommendations"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|n| n["rating"].as_i64().unwrap_or(0) > 0)
                .filter(|n| n["mediaRecommendation"].is_object())
                .map(|n| crate::browse::media(&n["mediaRecommendation"]))
                .filter(|r| r.id != 0 && seen.insert(r.id))
                .collect()
        },
        recommended: true,
        fetched: chrono::Utc::now().timestamp(),
    })
}

/// Where Kitsu requests go instead (tests).
#[cfg(test)]
pub static KITSU_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn kitsu_url() -> String {
    #[cfg(test)]
    if let Some(base) = KITSU_BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    "https://kitsu.io/api/edge".to_owned()
}

/// Episode titles and air dates from Kitsu (which has them for many shows
/// AniList doesn't), for an AniList ID. Up to 200 episodes.
pub async fn kitsu_episodes(
    client: &reqwest::Client,
    anilist_id: u64,
) -> Result<Vec<crate::library::EpisodeInfo>> {
    let base = kitsu_url();
    let mapping: serde_json::Value = client
        .get(format!("{base}/mappings"))
        .query(&[
            ("filter[externalSite]", "anilist/anime"),
            ("filter[externalId]", &anilist_id.to_string()),
            ("include", "item"),
        ])
        .send()
        .await
        .context("could not reach Kitsu")?
        .error_for_status()?
        .json()
        .await?;
    let Some(kitsu_id) = mapping["included"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["type"] == "anime")
        .and_then(|i| i["id"].as_str())
        .map(str::to_owned)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for page in 0..10 {
        let resp: serde_json::Value = client
            .get(format!("{base}/anime/{kitsu_id}/episodes"))
            .query(&[
                ("page[limit]", "20"),
                ("page[offset]", &(page * 20).to_string()),
                ("sort", "number"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let data = resp["data"].as_array().cloned().unwrap_or_default();
        for e in &data {
            let a = &e["attributes"];
            let Some(number) = a["number"].as_u64() else {
                continue;
            };
            let title = a["canonicalTitle"]
                .as_str()
                .or_else(|| a["titles"]["en"].as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let airs = a["airdate"]
                .as_str()
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .map(|t| t.and_utc().timestamp());
            out.push(crate::library::EpisodeInfo {
                number: number as u32,
                title: title.map(str::to_owned),
                airs,
            });
        }
        if data.len() < 20 {
            break;
        }
    }
    Ok(out)
}

/// Fill in titles and air times AniList doesn't have from `more`.
pub fn merge_episodes(
    list: &mut Vec<crate::library::EpisodeInfo>,
    more: Vec<crate::library::EpisodeInfo>,
) {
    for m in more {
        match list.iter_mut().find(|e| e.number == m.number) {
            Some(e) => {
                e.title = e.title.take().or(m.title);
                e.airs = e.airs.or(m.airs);
            }
            None => list.push(m),
        }
    }
    list.sort_by_key(|e| e.number);
}

/// AniList descriptions are light HTML: line breaks, italics, entities.
fn plain_text(html: &str) -> String {
    static TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)<br\s*/?>\r?\n?|<[^>]+>").unwrap());
    let text = TAG.replace_all(html, |c: &regex::Captures| {
        if c[0].to_lowercase().starts_with("<br") {
            "\n"
        } else {
            ""
        }
    });
    let text = text
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&nbsp;", " ");
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines().map(str::trim) {
        blank = if line.is_empty() { blank + 1 } else { 0 };
        if blank <= 1 && !(out.is_empty() && line.is_empty()) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_owned()
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

    #[cfg(test)]
    let url = SEADEX_BASE
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| SEADEX_ENTRIES_URL.to_owned());
    #[cfg(not(test))]
    let url = SEADEX_ENTRIES_URL;
    let resp = client
        .get(url)
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

    #[test]
    fn descriptions_become_plain_text() {
        let html = "Frieren &amp; co. go <i>north</i>.<br><br>\n<br>\nIt&#039;s &quot;slow&quot;.<br>\n(Source: Crunchyroll)";
        assert_eq!(
            plain_text(html),
            "Frieren & co. go north.\n\nIt's \"slow\".\n(Source: Crunchyroll)"
        );
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn reads_anilist_details() {
        use crate::mock_http::{mock, reply};
        let base = mock(|req| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["variables"]["id"], 154587);
            reply(200, serde_json::json!({"data": {"Media": {
                "title": {"romaji": "Sousou no Frieren", "english": "Frieren", "native": "葬送のフリーレン"},
                "synonyms": ["Frieren at the Funeral"], "description": "An elf <i>mage</i>.", "genres": ["Adventure", "Drama"],
                "averageScore": 90, "status": "RELEASING", "format": "TV", "episodes": 28, "duration": 24, "season": "FALL",
                "seasonYear": 2023, "source": "MANGA", "studios": {"nodes": [{"name": "Madhouse"}]},
                "startDate": {"year": 2023, "month": 9, "day": 29}, "endDate": {"year": null, "month": null, "day": null},
                "nextAiringEpisode": {"episode": 5, "airingAt": 1800000000},
                "coverImage": {"large": "l.png", "extraLarge": "xl.png"}, "bannerImage": "b.png",
                "streamingEpisodes": [{"title": "Episode 2 - It Didn't Have to Be Magic..."}, {"title": "Episode 1 - The Journey's End"}, {"title": "Trailer"}],
                "airingSchedule": {"nodes": [{"episode": 1, "airingAt": 1695999600}, {"episode": 5, "airingAt": 1800000000}]},
                "recommendations": {"nodes": [
                    {"rating": 120, "mediaRecommendation": {"id": 101, "title": {"english": "Dungeon Meal"}, "format": "TV", "isAdult": false}},
                    {"rating": -3, "mediaRecommendation": {"id": 102, "title": {"romaji": "Rated Down"}}},
                    {"rating": 40, "mediaRecommendation": null},
                    {"rating": 30, "mediaRecommendation": {"id": 101, "title": {"english": "Dungeon Meal"}}}]}}}}))
        })
        .await;
        let _net = crate::mock_http::mocked();
        *ANILIST_BASE.lock().unwrap() = Some(base);
        let d = anilist_details(&reqwest::Client::new(), 154587).await;
        *ANILIST_BASE.lock().unwrap() = None;
        let d = d.unwrap();
        assert_eq!(d.description.as_deref(), Some("An elf mage."));
        assert_eq!(
            (d.score, d.episodes, d.year),
            (Some(90), Some(28), Some(2023))
        );
        assert_eq!(d.studios, ["Madhouse"]);
        assert_eq!(d.start_date.as_deref(), Some("2023-09-29"));
        assert_eq!(d.end_date, None);
        assert_eq!(d.next_episode, Some((5, 1_800_000_000)));
        assert_eq!(d.cover.as_deref(), Some("xl.png"));
        let eps: Vec<(u32, Option<&str>, Option<i64>)> = d
            .episode_list
            .iter()
            .map(|e| (e.number, e.title.as_deref(), e.airs))
            .collect();
        assert_eq!(
            eps,
            [
                (1, Some("The Journey's End"), Some(1695999600)),
                (2, Some("It Didn't Have to Be Magic..."), None),
                (5, None, Some(1800000000))
            ]
        );
        assert_eq!(d.aired(), Some(4), "next is episode 5");
        // Rated up, once each.
        let recs: Vec<(u64, &str)> = d
            .recommendations
            .iter()
            .map(|m| (m.id, m.title()))
            .collect();
        assert_eq!(recs, [(101, "Dungeon Meal")]);
        assert!(d.recommended);
    }

    /// A second season listed as 25–48 by the streaming sites is 1–24.
    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn renumbers_later_seasons_from_one() {
        let base = crate::mock_http::mock(|_| {
            crate::mock_http::reply(200, serde_json::json!({"data": {"Media": {"episodes": 24, "status": "FINISHED",
                "streamingEpisodes": [{"title": "Episode 26 - Two"}, {"title": "Episode 25 - One"}, {"title": "Episode 48 - Last"}],
                "airingSchedule": {"nodes": [{"episode": 1, "airingAt": 1736500000}]}}}}))
        })
        .await;
        let _net = crate::mock_http::mocked();
        *ANILIST_BASE.lock().unwrap() = Some(base);
        let d = anilist_details(&reqwest::Client::new(), 1).await;
        *ANILIST_BASE.lock().unwrap() = None;
        let d = d.unwrap();
        let eps: Vec<(u32, Option<&str>, bool)> = d
            .episode_list
            .iter()
            .map(|e| (e.number, e.title.as_deref(), e.airs.is_some()))
            .collect();
        assert_eq!(
            eps,
            [
                (1, Some("One"), true),
                (2, Some("Two"), false),
                (24, Some("Last"), false)
            ]
        );
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn reads_kitsu_episodes() {
        use crate::mock_http::{mock, reply};
        let base = mock(|req| match req.target.split('?').next().unwrap() {
            "/mappings" => reply(200, serde_json::json!({"data": [{"id": "1"}], "included": [{"type": "anime", "id": "46474"}]})),
            "/anime/46474/episodes" => reply(200, serde_json::json!({"data": [
                {"attributes": {"number": 1, "canonicalTitle": "The Journey's End", "airdate": "2023-09-29"}},
                {"attributes": {"number": 2, "canonicalTitle": null, "titles": {"en": "Magic"}, "airdate": null}}]})),
            other => panic!("unexpected {other}"),
        })
        .await;
        let _net = crate::mock_http::mocked();
        *KITSU_BASE.lock().unwrap() = Some(base);
        let eps = kitsu_episodes(&reqwest::Client::new(), 154587).await;
        *KITSU_BASE.lock().unwrap() = None;
        let eps = eps.unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(
            (eps[0].title.as_deref(), eps[1].title.as_deref()),
            (Some("The Journey's End"), Some("Magic"))
        );
        assert!(eps[0].airs.is_some() && eps[1].airs.is_none());
        let mut list = vec![crate::library::EpisodeInfo {
            number: 2,
            title: None,
            airs: Some(5),
        }];
        merge_episodes(&mut list, eps);
        assert_eq!(
            list.iter()
                .map(|e| (e.number, e.title.clone(), e.airs.is_some()))
                .collect::<Vec<_>>(),
            [
                (1, Some("The Journey's End".into()), true),
                (2, Some("Magic".into()), true)
            ]
        );
    }

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
