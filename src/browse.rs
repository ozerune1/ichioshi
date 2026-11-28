//! AniList's anime to discover (trending, by season, popular, top rated,
//! by genre and format) and its airing schedule.

use std::collections::BTreeMap;

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

/// Ready-made views of Discover: a sort and a season.
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

    /// Its sort and season (on `today`).
    fn view(self, today: NaiveDate) -> (Sort, Option<(Season, i32)>) {
        let now = season_of(today);
        let now = (Season::from_anilist(now.0), now.1);
        let next = next_season(season_of(today));
        let next = (Season::from_anilist(next.0), next.1);
        match self {
            Shelf::Trending => (Sort::Trending, None),
            Shelf::ThisSeason => (Sort::Popularity, Some(now)),
            Shelf::NextSeason => (Sort::Popularity, Some(next)),
            Shelf::Popular => (Sort::Popularity, None),
            Shelf::TopRated => (Sort::Score, None),
        }
    }

    /// `from` shown this way (its genres, tags and format kept).
    pub fn apply(self, from: &Filter, today: NaiveDate) -> Filter {
        let (sort, season) = self.view(today);
        Filter {
            sort,
            season: season.map(|s| s.0),
            year: season.map(|s| s.1),
            ..from.clone()
        }
    }

    /// Whether `f` is shown this way.
    pub fn is(self, f: &Filter, today: NaiveDate) -> bool {
        let (sort, season) = self.view(today);
        f.sort == sort && f.season == season.map(|s| s.0) && f.year == season.map(|s| s.1)
    }
}

/// How Discover's anime are ordered: AniList's sorts, and a random order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sort {
    Trending,
    Popularity,
    Score,
    Favourites,
    Newest,
    Oldest,
    Title,
    Random,
}

impl Sort {
    pub const ALL: [Sort; 8] = [
        Sort::Trending,
        Sort::Popularity,
        Sort::Score,
        Sort::Favourites,
        Sort::Newest,
        Sort::Oldest,
        Sort::Title,
        Sort::Random,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Sort::Trending => "Trending",
            Sort::Popularity => "Popularity",
            Sort::Score => "Average score",
            Sort::Favourites => "Favourites",
            Sort::Newest => "Newest",
            Sort::Oldest => "Oldest",
            Sort::Title => "Title",
            Sort::Random => "Random",
        }
    }

    /// AniList's sort (for Random, a stable one its pages are drawn from).
    fn anilist(self) -> &'static str {
        match self {
            Sort::Trending => "TRENDING_DESC",
            Sort::Popularity => "POPULARITY_DESC",
            Sort::Score => "SCORE_DESC",
            Sort::Favourites => "FAVOURITES_DESC",
            Sort::Newest => "START_DATE_DESC",
            Sort::Oldest => "START_DATE",
            Sort::Title => "TITLE_ROMAJI",
            Sort::Random => "ID",
        }
    }
}

/// An anime season (WINTER is January–March).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Season {
    Winter,
    Spring,
    Summer,
    Fall,
}

impl Season {
    pub const ALL: [Season; 4] = [Season::Winter, Season::Spring, Season::Summer, Season::Fall];

    pub fn name(self) -> &'static str {
        match self {
            Season::Winter => "Winter",
            Season::Spring => "Spring",
            Season::Summer => "Summer",
            Season::Fall => "Fall",
        }
    }

    pub fn anilist(self) -> &'static str {
        match self {
            Season::Winter => "WINTER",
            Season::Spring => "SPRING",
            Season::Summer => "SUMMER",
            Season::Fall => "FALL",
        }
    }

    pub fn from_anilist(s: &str) -> Season {
        match s {
            "WINTER" => Season::Winter,
            "SPRING" => Season::Spring,
            "SUMMER" => Season::Summer,
            _ => Season::Fall,
        }
    }
}

/// A genre, tag or list status, wanted or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    Include,
    Exclude,
}

impl Choice {
    /// The next of a three-way chip: off, included, excluded, off.
    pub fn cycle(now: Option<Choice>) -> Option<Choice> {
        match now {
            None => Some(Choice::Include),
            Some(Choice::Include) => Some(Choice::Exclude),
            Some(Choice::Exclude) => None,
        }
    }
}

/// What Discover asks AniList for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Filter {
    pub sort: Sort,
    pub season: Option<Season>,
    pub year: Option<i32>,
    /// AniList's name ("TV").
    pub format: Option<String>,
    /// Genres and tags (AniList's names), each wanted or not.
    pub tags: BTreeMap<String, Choice>,
    /// Your lists' statuses (None: on neither list), each wanted or not,
    /// in [`crate::app::discover_ui::LIST_CHOICES`]' order.
    pub lists: Vec<(Option<crate::tracking::Status>, Choice)>,
    /// For a random order: another number is another order.
    pub shuffle: u64,
}

/// Which anime your lists' statuses chosen leave: those of some AniList
/// (or, signed in only there, MyAnimeList) IDs, or all but some.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListIds {
    Any,
    /// AniList IDs (`mal`: MyAnimeList's), only these (`not`: all but these).
    Ids {
        ids: Vec<u64>,
        not: bool,
        mal: bool,
    },
    /// None at all (only lists' anime wanted, and none are on them).
    Nothing,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            sort: Sort::Trending,
            season: None,
            year: None,
            format: None,
            tags: BTreeMap::new(),
            lists: Vec::new(),
            shuffle: 0,
        }
    }
}

impl Filter {
    /// Whether `name` is one of AniList's genres (else it's a tag).
    pub fn is_genre(name: &str) -> bool {
        GENRES.contains(&name) || name == ADULT_GENRE
    }

    /// Genres and tags it wants (`Include`) or doesn't, as AniList's four
    /// lists: genres in, genres not in, tags in, tags not in.
    fn tag_lists(&self) -> [Vec<&str>; 4] {
        let mut out: [Vec<&str>; 4] = Default::default();
        for (name, choice) in &self.tags {
            let i = match (Self::is_genre(name), choice) {
                (true, Choice::Include) => 0,
                (true, Choice::Exclude) => 1,
                (false, Choice::Include) => 2,
                (false, Choice::Exclude) => 3,
            };
            out[i].push(name);
        }
        out
    }
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

/// Anime a shelf shows a page at a time.
pub const PER_PAGE: u32 = 50;

/// A page of Discover's anime.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub items: Vec<Media>,
    /// Whether there are more pages.
    pub more: bool,
    /// How many pages there are (asked for a random order).
    pub last_page: Option<u32>,
}

/// AniList gives at most this many pages (of [`PER_PAGE`]: 5000 anime),
/// and says this many whenever there are more: a random order's pages are
/// drawn from at most these.
const MAX_PAGES: u32 = 100;

/// Page `page` (from 1) of `filter`'s anime (adult ones only with `adult`),
/// in its order. A random order is pages taken in a shuffled order (known
/// to be `last_page` long, or asked first), each shuffled too.
pub async fn browse(
    client: &reqwest::Client,
    filter: &Filter,
    ids: &ListIds,
    adult: bool,
    page: u32,
    last_page: Option<u32>,
) -> Result<Found> {
    let page = page.max(1);
    if *ids == ListIds::Nothing {
        return Ok(Found {
            items: Vec::new(),
            more: false,
            last_page: None,
        });
    }
    if filter.sort != Sort::Random {
        let (items, more, _) = browse_page(client, filter, ids, adult, page, MEDIA).await?;
        return Ok(Found {
            items,
            more,
            last_page: None,
        });
    }
    let last = match last_page {
        Some(l) => l,
        None => count_pages(client, filter, ids, adult).await?,
    }
    .clamp(1, MAX_PAGES);
    let drawn = shuffled_page(filter.shuffle, page, last);
    let (mut items, _, _) = browse_page(client, filter, ids, adult, drawn, MEDIA).await?;
    shuffle(
        &mut items,
        filter.shuffle ^ u64::from(page).wrapping_mul(0x9e37_79b9_7f4a_7c15),
    );
    Ok(Found {
        items,
        more: page < last,
        last_page: Some(last),
    })
}

/// How many pages `filter`'s anime fill (at most [`MAX_PAGES`]). AniList's
/// own count is capped, so it's found by asking: pages 2, 4, 8… until one
/// says it's the last or is empty, then between (only IDs are asked for).
async fn count_pages(
    client: &reqwest::Client,
    filter: &Filter,
    ids: &ListIds,
    adult: bool,
) -> Result<u32> {
    // (anime on it, more after it)
    let probe = async |page: u32| -> Result<(bool, bool)> {
        let (items, more, _) = browse_page(client, filter, ids, adult, page, "id").await?;
        Ok((!items.is_empty(), more))
    };
    let (any, more) = probe(1).await?;
    if !any || !more {
        return Ok(1);
    }
    // `full`: a page known to have anime and more after it; `past`: one
    // known to be past the end.
    let (mut full, mut past) = (1u32, None::<u32>);
    let mut p = 2;
    while p <= MAX_PAGES {
        match probe(p).await? {
            (true, false) => return Ok(p),
            (true, true) => full = p,
            (false, _) => {
                past = Some(p);
                break;
            }
        }
        if p == MAX_PAGES {
            return Ok(MAX_PAGES);
        }
        p = (p * 2).min(MAX_PAGES);
    }
    let mut past = past.unwrap_or(MAX_PAGES + 1);
    while past - full > 1 {
        let mid = full + (past - full) / 2;
        match probe(mid).await? {
            (true, false) => return Ok(mid),
            (true, true) => full = mid,
            (false, _) => past = mid,
        }
    }
    Ok(full)
}

/// One of AniList's pages (each anime's `fields`): its anime, whether
/// there are more, how many pages AniList says there are.
async fn browse_page(
    client: &reqwest::Client,
    filter: &Filter,
    ids: &ListIds,
    adult: bool,
    page: u32,
    fields: &str,
) -> Result<(Vec<Media>, bool, u32)> {
    // Left out, AniList's filter takes in adult anime too.
    let adult_filter = if adult { "" } else { "isAdult: false, " };
    let q = format!(
        "query ($page: Int, $sort: [MediaSort], $season: MediaSeason, $year: Int, \
         $genreIn: [String], $genreNotIn: [String], $tagIn: [String], $tagNotIn: [String], \
         $format: MediaFormat, $idIn: [Int], $idNotIn: [Int], $idMalIn: [Int], \
         $idMalNotIn: [Int]) {{ Page(page: $page, perPage: {PER_PAGE}) {{ \
         pageInfo {{ hasNextPage lastPage }} media(type: ANIME, {adult_filter}sort: $sort, \
         season: $season, seasonYear: $year, genre_in: $genreIn, genre_not_in: $genreNotIn, \
         tag_in: $tagIn, tag_not_in: $tagNotIn, format: $format, id_in: $idIn, \
         id_not_in: $idNotIn, idMal_in: $idMalIn, idMal_not_in: $idMalNotIn) {{ {fields} }} }} }}"
    );
    // Filters left out, not null: AniList takes `genre: null` as "no genre".
    let mut vars = serde_json::Map::new();
    vars.insert("page".into(), json!(page));
    vars.insert("sort".into(), json!([filter.sort.anilist()]));
    if let Some(s) = filter.season {
        vars.insert("season".into(), json!(s.anilist()));
    }
    if let Some(y) = filter.year {
        vars.insert("year".into(), json!(y));
    }
    let names = ["genreIn", "genreNotIn", "tagIn", "tagNotIn"];
    for (name, list) in names.into_iter().zip(filter.tag_lists()) {
        if !list.is_empty() {
            vars.insert(name.into(), json!(list));
        }
    }
    if let Some(f) = &filter.format {
        vars.insert("format".into(), json!(f));
    }
    if let ListIds::Ids { ids, not, mal } = ids {
        let name = match (mal, not) {
            (false, false) => "idIn",
            (false, true) => "idNotIn",
            (true, false) => "idMalIn",
            (true, true) => "idMalNotIn",
        };
        vars.insert(name.into(), json!(ids));
    }
    let data = query(client, &q, Value::Object(vars)).await?;
    let list = data["Page"]["media"]
        .as_array()
        .into_iter()
        .flatten()
        .map(media)
        .filter(|m| adult || !m.adult)
        .collect();
    let info = &data["Page"]["pageInfo"];
    let last = info["lastPage"].as_u64().map_or(page, |l| l as u32);
    Ok((list, info["hasNextPage"] == true, last))
}

/// The page shown `n`th (from 1) of `last`, in the order `seed` makes:
/// each page once.
fn shuffled_page(seed: u64, n: u32, last: u32) -> u32 {
    let last = u64::from(last.max(1));
    let n = u64::from(n.max(1) - 1) % last;
    // A step that's coprime with the count visits every page once.
    let step = [7919u64, 6007, 4999, 3001, 1009, 101, 13, 1]
        .into_iter()
        .find(|p| gcd(*p, last) == 1)
        .unwrap_or(1);
    ((n * step + seed % last) % last + 1) as u32
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `items` in an order `seed` makes.
fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut x = seed | 1;
    for i in (1..items.len()).rev() {
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        items.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// One of AniList's tags (genres are [`GENRES`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    /// e.g. "Theme-Romance".
    pub category: String,
    pub adult: bool,
}

/// AniList's tags, for Discover's filter.
pub async fn tags(client: &reqwest::Client) -> Result<Vec<Tag>> {
    let data = query(
        client,
        "query { MediaTagCollection { name category isAdult } }",
        json!({}),
    )
    .await?;
    let mut out: Vec<Tag> = data["MediaTagCollection"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(Tag {
                name: t["name"].as_str()?.to_owned(),
                category: t["category"].as_str().unwrap_or_default().to_owned(),
                adult: t["isAdult"] == true,
            })
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    Ok(out)
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

    #[test]
    fn counts_pages_by_asking() {
        let _net = crate::mock_http::mocked();
        let rt = tokio::runtime::Runtime::new().unwrap();
        // 7 real pages; AniList says 100 (its cap), as it does.
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        let server = rt.block_on(mock(move |req| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let page = body["variables"]["page"].as_u64().unwrap();
            log.lock().unwrap().push(page);
            let media: Vec<Value> = if page <= 7 {
                vec![json!({"id": page})]
            } else {
                vec![]
            };
            reply(
                200,
                json!({"data": {"Page": {"pageInfo": {"hasNextPage": page < 7, "lastPage": 100},
                    "media": media}}}),
            )
        }));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(server);
        let client = reqwest::Client::new();
        let filter = Filter::default();
        let n = rt.block_on(count_pages(&client, &filter, &ListIds::Any, false));
        let probes = asked.lock().unwrap().clone();
        let random = Filter {
            sort: Sort::Random,
            shuffle: 3,
            ..Filter::default()
        };
        let first = rt.block_on(browse(&client, &random, &ListIds::Any, false, 1, None));
        reset();
        assert_eq!(n.unwrap(), 7);
        assert_eq!(probes, [1, 2, 4, 8, 6, 7], "doubling, then between");
        let first = first.unwrap();
        assert_eq!(first.last_page, Some(7));
        assert_eq!(first.items.len(), 1, "a real page");
        assert!(first.more);
    }

    #[test]
    fn random_order_visits_each_page_once() {
        for last in [1u32, 2, 7, 13, 100, 200] {
            for seed in [0u64, 1, 42, u64::MAX] {
                let mut seen: Vec<u32> = (1..=last).map(|n| shuffled_page(seed, n, last)).collect();
                assert!(seen.iter().all(|p| (1..=last).contains(p)));
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(seen.len(), last as usize, "last {last}, seed {seed}");
            }
        }
        assert_ne!(
            (1..=10)
                .map(|n| shuffled_page(1, n, 10))
                .collect::<Vec<_>>(),
            (1..=10)
                .map(|n| shuffled_page(2, n, 10))
                .collect::<Vec<_>>(),
            "another shuffle, another order"
        );
        let mut a: Vec<u32> = (0..20).collect();
        shuffle(&mut a, 7);
        assert_ne!(a, (0..20).collect::<Vec<_>>());
        a.sort_unstable();
        assert_eq!(a, (0..20).collect::<Vec<_>>());
        assert_eq!(Choice::cycle(None), Some(Choice::Include));
        assert_eq!(Choice::cycle(Some(Choice::Include)), Some(Choice::Exclude));
        assert_eq!(Choice::cycle(Some(Choice::Exclude)), None);
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
            assert_eq!(v["genreIn"], json!(["Drama"]));
            assert_eq!(v["genreNotIn"], json!(["Ecchi"]));
            assert_eq!(v["tagIn"], json!(["Time Skip", "Twins"]));
            assert!(
                v.get("format").is_none() && v.get("tagNotIn").is_none(),
                "unset filters left out, not null"
            );
            assert!(q.contains("isAdult: false"));
            if v["page"] == 2 {
                return reply(
                    200,
                    json!({"data": {"Page": {"pageInfo": {"hasNextPage": false}, "media": [
                        {"id": 9, "title": {"romaji": "Nine"}}]}}}),
                );
            }
            assert_eq!(v["page"], 1);
            reply(
                200,
                json!({"data": {"Page": {"pageInfo": {"hasNextPage": true}, "media": [
                    {"id": 7, "idMal": 70, "title": {"english": "Seven", "romaji": "Nana"},
                     "coverImage": {"large": "c7"}, "format": "TV", "episodes": 12,
                     "averageScore": 81, "genres": ["Drama"],
                     "nextAiringEpisode": {"episode": 2, "airingAt": 99}},
                    {"id": 8, "title": {"romaji": "Eight"}, "isAdult": true}]}}}),
            )
        }));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(server);
        let client = reqwest::Client::new();
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        let mut filter = Shelf::NextSeason.apply(&Filter::default(), today);
        assert!(Shelf::NextSeason.is(&filter, today));
        assert!(!Shelf::ThisSeason.is(&filter, today));
        for (tag, choice) in [
            ("Drama", Choice::Include),
            ("Ecchi", Choice::Exclude),
            ("Time Skip", Choice::Include),
            ("Twins", Choice::Include),
        ] {
            filter.tags.insert(tag.into(), choice);
        }
        let found = rt.block_on(browse(&client, &filter, &ListIds::Any, false, 1, None));
        let second = rt.block_on(browse(&client, &filter, &ListIds::Any, false, 2, None));
        let nothing = rt.block_on(browse(&client, &filter, &ListIds::Nothing, false, 1, None));
        assert!(
            nothing.unwrap().items.is_empty(),
            "asked nothing of AniList"
        );
        let airing = rt.block_on(schedule(&client, 0, 2_000_000_000, false));
        reset();
        let Found {
            items: found, more, ..
        } = found.unwrap();
        assert!(more, "AniList has a page after it");
        let Found {
            items: second,
            more,
            ..
        } = second.unwrap();
        assert_eq!(second.iter().map(|m| m.id).collect::<Vec<_>>(), [9]);
        assert!(!more);
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
