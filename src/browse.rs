//! AniList's anime to discover (trending, by season, popular, top rated,
//! by genre and format) and its airing schedule.

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use serde_json::{Value, json};

use crate::seadex::Anime;

/// An anime as AniList lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub id: u64,
    pub id_mal: Option<u64>,
    pub english: Option<String>,
    pub romaji: Option<String>,
    pub native: Option<String>,
    pub cover: Option<String>,
    pub banner: Option<String>,
    /// TV, MOVIE, ONA, …
    pub format: Option<String>,
    pub episodes: Option<u32>,
    /// FINISHED, RELEASING, NOT_YET_RELEASED, …
    pub status: Option<String>,
    pub season: Option<String>,
    pub year: Option<i32>,
    /// Out of 100.
    pub score: Option<u32>,
    pub popularity: Option<u32>,
    pub genres: Vec<String>,
    /// The next episode, and when it airs (Unix time).
    pub next_airing: Option<(u32, i64)>,
    pub adult: bool,
}

impl Media {
    pub fn title(&self) -> &str {
        self.english
            .as_deref()
            .or(self.romaji.as_deref())
            .or(self.native.as_deref())
            .unwrap_or("Untitled")
    }

    /// As the rest of the app knows anime (its SeaDex entry to be looked up).
    pub fn to_anime(&self) -> Anime {
        Anime {
            anilist_id: self.id,
            title: self.title().to_owned(),
            alt_title: self
                .romaji
                .clone()
                .filter(|r| Some(r) != self.english.as_ref()),
            romaji: self.romaji.clone(),
            english: self.english.clone(),
            format: self.format.clone(),
            year: self.year,
            episodes: self.episodes,
            cover_small: self.cover.clone(),
            cover_large: self.cover.clone(),
            entry: None,
            adult: self.adult,
        }
    }
}

const MEDIA: &str = "id idMal title { english romaji native } coverImage { extraLarge large } \
                     bannerImage format episodes status season seasonYear averageScore \
                     popularity genres isAdult nextAiringEpisode { episode airingAt }";

/// What Discover shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shelf {
    Trending,
    ThisSeason,
    NextSeason,
    Popular,
    TopRated,
}

impl Shelf {
    pub const ALL: [Shelf; 5] = [
        Shelf::Trending,
        Shelf::ThisSeason,
        Shelf::NextSeason,
        Shelf::Popular,
        Shelf::TopRated,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shelf::Trending => "Trending now",
            Shelf::ThisSeason => "This season",
            Shelf::NextSeason => "Next season",
            Shelf::Popular => "All-time popular",
            Shelf::TopRated => "Top rated",
        }
    }
}

/// A shelf, narrowed to a genre and a format (AniList's names).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Filter {
    pub shelf: Shelf,
    pub genre: Option<String>,
    pub format: Option<String>,
}

pub const GENRES: [&str; 18] = [
    "Action",
    "Adventure",
    "Comedy",
    "Drama",
    "Ecchi",
    "Fantasy",
    "Horror",
    "Mahou Shoujo",
    "Mecha",
    "Music",
    "Mystery",
    "Psychological",
    "Romance",
    "Sci-Fi",
    "Slice of Life",
    "Sports",
    "Supernatural",
    "Thriller",
];

/// Formats to pick from: what they're called, and AniList's name.
pub const FORMATS: [(&str, &str); 5] = [
    ("TV", "TV"),
    ("Movie", "MOVIE"),
    ("ONA", "ONA"),
    ("OVA", "OVA"),
    ("TV short", "TV_SHORT"),
];

/// The anime season a day is in (WINTER is January–March), and its year.
pub fn season_of(day: NaiveDate) -> (&'static str, i32) {
    let season = match day.month() {
        1..=3 => "WINTER",
        4..=6 => "SPRING",
        7..=9 => "SUMMER",
        _ => "FALL",
    };
    (season, day.year())
}

pub fn next_season((season, year): (&str, i32)) -> (&'static str, i32) {
    match season {
        "WINTER" => ("SPRING", year),
        "SPRING" => ("SUMMER", year),
        "SUMMER" => ("FALL", year),
        _ => ("WINTER", year + 1),
    }
}

/// "Fall 2026".
pub fn season_name((season, year): (&str, i32)) -> String {
    let mut s = season.to_lowercase();
    if let Some(first) = s.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    format!("{s} {year}")
}

/// AniList's genre of 18+ anime, offered when they're shown.
pub const ADULT_GENRE: &str = "Hentai";

/// A shelf's anime (adult ones only with `adult`), most relevant first.
pub async fn browse(
    client: &reqwest::Client,
    filter: &Filter,
    today: NaiveDate,
    adult: bool,
) -> Result<Vec<Media>> {
    let now = season_of(today);
    let (sort, season) = match filter.shelf {
        Shelf::Trending => ("TRENDING_DESC", None),
        Shelf::ThisSeason => ("POPULARITY_DESC", Some(now)),
        Shelf::NextSeason => ("POPULARITY_DESC", Some(next_season(now))),
        Shelf::Popular => ("POPULARITY_DESC", None),
        Shelf::TopRated => ("SCORE_DESC", None),
    };
    // Left out, AniList's filter takes in adult anime too.
    let adult_filter = if adult { "" } else { "isAdult: false, " };
    let q = format!(
        "query ($sort: [MediaSort], $season: MediaSeason, $year: Int, $genre: String, \
         $format: MediaFormat) {{ Page(perPage: 50) {{ media(type: ANIME, {adult_filter}\
         sort: $sort, season: $season, seasonYear: $year, genre: $genre, format: $format) \
         {{ {MEDIA} }} }} }}"
    );
    // Filters left out, not null: AniList takes `genre: null` as "no genre".
    let mut vars = serde_json::Map::new();
    vars.insert("sort".into(), json!([sort]));
    if let Some((s, y)) = season {
        vars.insert("season".into(), json!(s));
        vars.insert("year".into(), json!(y));
    }
    if let Some(g) = &filter.genre {
        vars.insert("genre".into(), json!(g));
    }
    if let Some(f) = &filter.format {
        vars.insert("format".into(), json!(f));
    }
    let data = query(client, &q, Value::Object(vars)).await?;
    Ok(data["Page"]["media"]
        .as_array()
        .into_iter()
        .flatten()
        .map(media)
        .filter(|m| adult || !m.adult)
        .collect())
}

/// An episode airing.
#[derive(Clone, Debug, PartialEq)]
pub struct Airing {
    /// Unix time.
    pub at: i64,
    pub episode: u32,
    pub media: Media,
}

/// Every episode airing from `from` to `to` (Unix times), in order; adult
/// anime only with `adult`.
pub async fn schedule(
    client: &reqwest::Client,
    from: i64,
    to: i64,
    adult: bool,
) -> Result<Vec<Airing>> {
    let q = format!(
        "query ($page: Int, $from: Int, $to: Int) {{ Page(page: $page, perPage: 50) {{ \
         pageInfo {{ hasNextPage }} airingSchedules(airingAt_greater: $from, \
         airingAt_lesser: $to, sort: TIME) {{ airingAt episode media {{ {MEDIA} }} }} }} }}"
    );
    let mut out = Vec::new();
    // A week is a few hundred episodes; stop at 20 pages anyway.
    for page in 1..=20 {
        let data = query(
            client,
            &q,
            json!({ "page": page, "from": from - 1, "to": to }),
        )
        .await?;
        let p = &data["Page"];
        for a in p["airingSchedules"].as_array().into_iter().flatten() {
            let (Some(at), Some(episode)) = (a["airingAt"].as_i64(), a["episode"].as_u64()) else {
                continue;
            };
            let media = media(&a["media"]);
            if (adult || !media.adult) && media.id != 0 {
                out.push(Airing {
                    at,
                    episode: episode as u32,
                    media,
                });
            }
        }
        if p["pageInfo"]["hasNextPage"] != true {
            break;
        }
    }
    Ok(out)
}

/// How long an anime is, and what follows it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Part {
    pub episodes: Option<u32>,
    /// Its sequels (series, not films or specials).
    pub sequels: Vec<u64>,
}

/// Anime's episode counts and sequels, by AniList ID.
pub async fn parts(
    client: &reqwest::Client,
    ids: &[u64],
) -> Result<std::collections::HashMap<u64, Part>> {
    const Q: &str = "query ($ids: [Int], $page: Int) { Page(page: $page, perPage: 50) { \
        media(id_in: $ids, type: ANIME) { id episodes relations { edges { relationType \
        node { id type format } } } } } }";
    let mut out = std::collections::HashMap::new();
    for chunk in ids.chunks(50) {
        let data = query(client, Q, json!({ "ids": chunk, "page": 1 })).await?;
        for m in data["Page"]["media"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_u64() else { continue };
            let sequels = m["relations"]["edges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["relationType"] == "SEQUEL" && e["node"]["type"] == "ANIME")
                .filter(|e| {
                    !matches!(
                        e["node"]["format"].as_str(),
                        Some("MOVIE" | "SPECIAL" | "MUSIC")
                    )
                })
                .filter_map(|e| e["node"]["id"].as_u64())
                .collect();
            out.insert(
                id,
                Part {
                    episodes: m["episodes"].as_u64().map(|n| n as u32),
                    sequels,
                },
            );
        }
    }
    Ok(out)
}

/// Anime by AniList ID (50 a request), in no particular order.
pub async fn by_ids(client: &reqwest::Client, ids: &[u64]) -> Result<Vec<Media>> {
    let q = format!(
        "query ($ids: [Int], $page: Int) {{ Page(page: $page, perPage: 50) {{ \
         pageInfo {{ hasNextPage }} media(id_in: $ids, type: ANIME) {{ {MEDIA} }} }} }}"
    );
    let mut out = Vec::new();
    for chunk in ids.chunks(50) {
        let data = query(client, &q, json!({ "ids": chunk, "page": 1 })).await?;
        if let Some(list) = data["Page"]["media"].as_array() {
            out.extend(list.iter().map(media).filter(|m| m.id != 0));
        }
    }
    Ok(out)
}

async fn query(client: &reqwest::Client, q: &str, variables: Value) -> Result<Value> {
    let resp = client
        .post(crate::seadex::anilist_url())
        .json(&json!({ "query": q, "variables": variables }))
        .send()
        .await
        .context("could not reach AniList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("AniList is busy; try again in a minute");
    }
    let mut body: Value = resp
        .json()
        .await
        .with_context(|| format!("unexpected response from AniList (HTTP {status})"))?;
    if let Some(e) = body["errors"].get(0) {
        bail!(
            "AniList error: {}",
            e["message"].as_str().unwrap_or("unknown")
        );
    }
    Ok(body["data"].take())
}

fn media(m: &Value) -> Media {
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let num = |v: &Value| v.as_u64().map(|n| n as u32);
    Media {
        id: m["id"].as_u64().unwrap_or(0),
        id_mal: m["idMal"].as_u64(),
        english: text(&m["title"]["english"]),
        romaji: text(&m["title"]["romaji"]),
        native: text(&m["title"]["native"]),
        cover: text(&m["coverImage"]["extraLarge"]).or_else(|| text(&m["coverImage"]["large"])),
        banner: text(&m["bannerImage"]),
        format: text(&m["format"]),
        episodes: num(&m["episodes"]),
        status: text(&m["status"]),
        season: text(&m["season"]),
        year: m["seasonYear"].as_i64().map(|y| y as i32),
        score: num(&m["averageScore"]),
        popularity: num(&m["popularity"]),
        genres: m["genres"]
            .as_array()
            .map(|a| a.iter().filter_map(text).collect())
            .unwrap_or_default(),
        next_airing: num(&m["nextAiringEpisode"]["episode"])
            .zip(m["nextAiringEpisode"]["airingAt"].as_i64()),
        adult: m["isAdult"] == true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_http::{mock, reply};

    #[test]
    fn seasons() {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(season_of(day(2026, 11, 22)), ("FALL", 2026));
        assert_eq!(season_of(day(2026, 1, 1)), ("WINTER", 2026));
        assert_eq!(next_season(("FALL", 2026)), ("WINTER", 2027));
        assert_eq!(next_season(("SPRING", 2026)), ("SUMMER", 2026));
        assert_eq!(season_name(("FALL", 2026)), "Fall 2026");
    }

    fn reset() {
        *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
    }

    /// The season's shelf asks for the season and genre; adult anime are
    /// left out; the schedule reads every page.
    #[test]
    fn browses_and_reads_the_schedule() {
        let _net = crate::mock_http::mocked();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(mock(|req| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let (q, v) = (body["query"].as_str().unwrap(), &body["variables"]);
            if q.contains("airingSchedules") {
                let page = v["page"].as_u64().unwrap();
                let show = |id: u64, ep: u64, adult: bool| {
                    json!({"airingAt": 1_800_000_000 + id, "episode": ep, "media": {
                        "id": id, "idMal": id + 1000, "title": {"romaji": format!("Show {id}")},
                        "isAdult": adult}})
                };
                return reply(
                    200,
                    json!({"data": {"Page": {"pageInfo": {"hasNextPage": page == 1},
                    "airingSchedules": if page == 1 {
                        vec![show(1, 5, false), show(2, 1, true)]
                    } else {
                        vec![show(3, 12, false)]
                    }}}}),
                );
            }
            assert_eq!(v["sort"], json!(["POPULARITY_DESC"]));
            assert_eq!(
                (v["season"].as_str(), v["year"].as_i64()),
                (Some("WINTER"), Some(2027))
            );
            assert_eq!(v["genre"], "Drama");
            assert!(
                v.get("format").is_none(),
                "unset filters left out, not null"
            );
            assert!(q.contains("isAdult: false"));
            reply(
                200,
                json!({"data": {"Page": {"media": [
                    {"id": 7, "idMal": 70, "title": {"english": "Seven", "romaji": "Nana"},
                     "coverImage": {"large": "c7"}, "format": "TV", "episodes": 12,
                     "averageScore": 81, "genres": ["Drama"],
                     "nextAiringEpisode": {"episode": 2, "airingAt": 99}},
                    {"id": 8, "title": {"romaji": "Eight"}, "isAdult": true}]}}}),
            )
        }));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(server);
        let client = reqwest::Client::new();
        let filter = Filter {
            shelf: Shelf::NextSeason,
            genre: Some("Drama".into()),
            format: None,
        };
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        let found = rt.block_on(browse(&client, &filter, today, false));
        let airing = rt.block_on(schedule(&client, 0, 2_000_000_000, false));
        reset();
        let found = found.unwrap();
        assert_eq!(found.len(), 1, "no adult anime");
        let m = &found[0];
        assert_eq!(
            (m.id, m.id_mal, m.title(), m.score, m.next_airing),
            (7, Some(70), "Seven", Some(81), Some((2, 99)))
        );
        assert_eq!(m.to_anime().alt_title.as_deref(), Some("Nana"));
        let airing = airing.unwrap();
        let got: Vec<(u64, u32)> = airing.iter().map(|a| (a.media.id, a.episode)).collect();
        assert_eq!(got, [(1, 5), (3, 12)], "both pages, no adult anime");
    }
}
