//! What a GUI shows, as plain data built from the library: library cards,
//! series pages with every episode, and downloads. Any frontend (egui,
//! iced, a web view) can draw it.
//!
//! List entries and downloads come from the live services in the app; the
//! design prototypes read them from files the demo library writes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::library::{Details, Library, Series};
use crate::tracking::{Entry, Status};

#[derive(Clone, Debug, Serialize)]
pub struct Image {
    pub url: String,
    /// A copy on disk, when there is one.
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ListView {
    pub status: Status,
    pub status_name: String,
    /// "8", "7.5", or "–".
    pub score: String,
    pub progress: u32,
}

impl ListView {
    fn of(e: &Entry) -> Self {
        Self {
            status: e.status,
            status_name: e.status.name().into(),
            score: e.score_text(),
            progress: e.progress,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Card {
    pub id: u64,
    pub title: String,
    pub cover: Option<Image>,
    /// "TV · 2023".
    pub kind: String,
    pub have: usize,
    pub total: Option<u32>,
    pub airing: Option<String>,
    pub list: Option<ListView>,
    /// Episodes watched, of those there are (0–1).
    pub watched: f32,
    /// Something downloading for it, how far along.
    pub downloading: Option<f32>,
    /// Added to the library (Unix time).
    pub added: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpisodeState {
    OnDisk { size: String },
    Downloading { progress: f32 },
    Missing,
    NotAired,
}

#[derive(Clone, Debug, Serialize)]
pub struct EpisodeView {
    pub number: u32,
    pub title: String,
    pub airs: Option<String>,
    pub state: EpisodeState,
    /// How much has been watched (0–1).
    pub watched: f32,
    pub done: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SeriesView {
    pub id: u64,
    pub title: String,
    pub native: Option<String>,
    pub cover: Option<Image>,
    pub banner: Option<Image>,
    /// "★ 90%", "Fall 2023", "Madhouse", "24 min episodes".
    pub facts: Vec<String>,
    pub genres: Vec<String>,
    pub description: Option<String>,
    pub airing: Option<String>,
    pub list: Option<ListView>,
    /// The episode to watch next, and whether it's part-watched.
    pub up_next: Option<(u32, bool)>,
    pub episodes: Vec<EpisodeView>,
    pub have: usize,
    pub missing: usize,
    pub size: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadView {
    pub name: String,
    pub series: Option<u64>,
    #[serde(default)]
    pub episodes: Vec<u32>,
    /// "Usenet", "TorBox", "BitTorrent".
    pub source: String,
    pub progress: f32,
    /// "18.4 MB/s · 3 min left".
    pub detail: String,
    pub size: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Model {
    pub cards: Vec<Card>,
    pub series: Vec<SeriesView>,
    pub downloads: Vec<DownloadView>,
    /// "AniList · jake".
    pub accounts: Vec<String>,
}

impl Model {
    pub fn series(&self, id: u64) -> Option<&SeriesView> {
        self.series.iter().find(|s| s.id == id)
    }
}

pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

fn title_case(s: &str) -> String {
    let lower = s.to_lowercase().replace('_', " ");
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn airing(d: &Details) -> Option<String> {
    if let Some((episode, at)) = d.next_episode {
        let when = chrono::DateTime::from_timestamp(at, 0)?.with_timezone(&chrono::Local);
        return Some(format!(
            "Episode {episode} airs {}",
            when.format("%a %-d %b, %H:%M")
        ));
    }
    Some(match d.status.as_deref()? {
        "FINISHED" => "Finished airing".into(),
        "RELEASING" => "Airing".into(),
        "NOT_YET_RELEASED" => "Not out yet".into(),
        other => title_case(other),
    })
}

/// An image's URL, and its copy in `cache` (`<stem>.jpg`, `.png` or `.webp`).
fn image(url: Option<&str>, cache: &Path, stem: String) -> Option<Image> {
    let url = url?.to_owned();
    let path = ["jpg", "png", "webp"]
        .iter()
        .map(|ext| cache.join(format!("{stem}.{ext}")))
        .find(|p| p.exists());
    Some(Image { url, path })
}

/// A series' episode files by number.
pub(crate) fn episode_files(series: &Series) -> BTreeMap<u32, PathBuf> {
    let mut out = BTreeMap::new();
    for path in series.video_files() {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(ep) = crate::title::episode_of_file(&name) {
            out.entry(ep).or_insert(path);
        }
    }
    out
}

/// Build the model from the app's state folder.
/// The demo library's model: list entries and downloads from the files the
/// demo writes (`lists-cache.json`, `downloads-cache.json`).
pub fn load(state_dir: &Path) -> Model {
    let library = Library::load(state_dir.join("library.json"), state_dir);
    let lists: HashMap<u64, Entry> = std::fs::read_to_string(state_dir.join("lists-cache.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let downloads: Vec<DownloadView> =
        std::fs::read_to_string(state_dir.join("downloads-cache.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
    let tracking = crate::tracking::Config::load(&state_dir.join("tracking.json"));
    build(
        &library,
        &state_dir.join("images"),
        &lists,
        downloads,
        accounts(&tracking),
    )
}

/// "AniList · jake", for the accounts signed in to.
pub fn accounts(tracking: &crate::tracking::Config) -> Vec<String> {
    let mut accounts = Vec::new();
    if let Some(u) = &tracking.anilist.user {
        accounts.push(format!("AniList · {}", u.name));
    }
    if let Some(u) = &tracking.mal.user {
        accounts.push(format!("MyAnimeList · {u}"));
    }
    accounts
}

/// The model of `library`: `lists` are list entries by AniList ID; images
/// are looked for in `images` (else drawn from their URLs).
pub fn build(
    library: &Library,
    images: &Path,
    lists: &HashMap<u64, Entry>,
    downloads: Vec<DownloadView>,
    accounts: Vec<String>,
) -> Model {
    let images = images.to_owned();
    let now = chrono::Utc::now().timestamp();

    let mut cards = Vec::new();
    let mut series_views = Vec::new();
    for s in &library.series {
        let d = s.details.clone().unwrap_or_default();
        let files = episode_files(s);
        let downloading: HashMap<u32, f32> = downloads
            .iter()
            .filter(|dl| dl.series == Some(s.anilist_id))
            .flat_map(|dl| dl.episodes.iter().map(|e| (*e, dl.progress)))
            .collect();
        let total = d.episodes.or(s.episodes);
        let last = [
            total.unwrap_or(0),
            d.episode_list.iter().map(|e| e.number).max().unwrap_or(0),
            files.keys().max().copied().unwrap_or(0),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let aired = d.aired();
        let mut episodes = Vec::new();
        let mut bytes = 0u64;
        for n in 1..=last {
            let info = d.episode(n);
            let airs_at = info.and_then(|e| e.airs);
            let out = match airs_at {
                Some(t) => t <= now,
                None => aired.is_none_or(|a| n <= a),
            };
            let state = if let Some(p) = downloading.get(&n) {
                EpisodeState::Downloading { progress: *p }
            } else if let Some(path) = files.get(&n) {
                let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                bytes += len;
                EpisodeState::OnDisk {
                    size: size_text(len),
                }
            } else if out {
                EpisodeState::Missing
            } else {
                EpisodeState::NotAired
            };
            let w = s.watched.get(&n);
            episodes.push(EpisodeView {
                number: n,
                title: info
                    .and_then(|e| e.title.clone())
                    .unwrap_or_else(|| format!("Episode {n}")),
                airs: airs_at
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%-d %b %Y")
                            .to_string()
                    }),
                state,
                watched: w.map_or(0.0, |w| {
                    if w.done {
                        1.0
                    } else {
                        (w.position / w.duration.max(1.0)) as f32
                    }
                }),
                done: w.is_some_and(|w| w.done),
            });
        }
        let have = files.len();
        let missing = episodes
            .iter()
            .filter(|e| matches!(e.state, EpisodeState::Missing))
            .count();
        let done = episodes.iter().filter(|e| e.done).count();
        let up_next = {
            let last_done = episodes.iter().filter(|e| e.done).map(|e| e.number).max();
            episodes
                .iter()
                .filter(|e| matches!(e.state, EpisodeState::OnDisk { .. }))
                .find(|e| !e.done && last_done.is_none_or(|d| e.number > d))
                .map(|e| (e.number, e.watched > 0.0))
        };
        let mut facts = Vec::new();
        if let Some(score) = d.score {
            facts.push(format!("★ {score}%"));
        }
        if let (Some(season), Some(year)) = (&d.season, d.year) {
            facts.push(format!("{} {year}", title_case(season)));
        }
        if !d.studios.is_empty() {
            facts.push(d.studios.join(", "));
        }
        if let Some(m) = d.duration {
            facts.push(format!("{m} min episodes"));
        }
        let kind = [
            d.format.clone().or(s.format.clone()),
            d.year.or(s.year).map(|y| y.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let cover = image(
            d.cover.as_deref().or(s.cover.as_deref()),
            &images,
            format!("{}-cover", s.anilist_id),
        );
        let list = lists.get(&s.anilist_id).map(ListView::of);
        cards.push(Card {
            id: s.anilist_id,
            title: s.title().to_owned(),
            cover: cover.clone(),
            kind,
            have,
            total,
            airing: airing(&d),
            list: list.clone(),
            watched: if last == 0 {
                0.0
            } else {
                done as f32 / last as f32
            },
            downloading: downloads
                .iter()
                .find(|dl| dl.series == Some(s.anilist_id))
                .map(|dl| dl.progress),
            added: s.added,
        });
        series_views.push(SeriesView {
            id: s.anilist_id,
            title: s.title().to_owned(),
            native: d.native.clone(),
            cover,
            banner: image(
                d.banner.as_deref(),
                &images,
                format!("{}-banner", s.anilist_id),
            ),
            facts,
            genres: d.genres.clone(),
            description: d.description.clone(),
            airing: airing(&d),
            list,
            up_next,
            episodes,
            have,
            missing,
            size: size_text(bytes),
        });
    }
    cards.sort_by_key(|c| std::cmp::Reverse(c.added));
    Model {
        cards,
        series: series_views,
        downloads,
        accounts,
    }
}

/// A demo library of real shows (AniList's details, covers and banners),
/// with episodes "on disk" (empty sparse files), some watched, list
/// entries and downloads: for showing GUI designs.
#[cfg(test)]
pub mod demo {
    use super::*;
    use crate::library::{NewSeries, Watch};

    /// Search, episodes on disk, episodes watched, list status, score, downloading.
    type Show = (&'static str, u32, u32, Status, f32, &'static [u32]);

    const SHOWS: &[Show] = &[
        ("Sousou no Frieren", 28, 28, Status::Completed, 10.0, &[]),
        (
            "Kusuriya no Hitorigoto 2nd Season",
            18,
            14,
            Status::Watching,
            9.0,
            &[19, 20],
        ),
        ("Dandadan", 12, 7, Status::Watching, 8.5, &[]),
        ("Bocchi the Rock!", 12, 12, Status::Completed, 9.5, &[]),
        (
            "Ore dake Level Up na Ken",
            12,
            12,
            Status::Completed,
            7.0,
            &[],
        ),
        (
            "Kaijuu 8-gou",
            6,
            0,
            Status::Planning,
            0.0,
            &[7, 8, 9, 10, 11, 12],
        ),
        ("Chainsaw Man", 12, 3, Status::Paused, 8.0, &[]),
        ("Kimetsu no Yaiba", 26, 26, Status::Completed, 8.0, &[]),
        ("Oshi no Ko", 11, 11, Status::Completed, 8.5, &[]),
        ("Spy x Family", 12, 2, Status::Watching, 0.0, &[]),
    ];

    async fn find(
        http: &reqwest::Client,
        search: &str,
    ) -> anyhow::Result<(u64, Option<String>, Option<String>, Option<String>)> {
        let q = "query ($s: String) { Media(search: $s, type: ANIME) { id title { english romaji } coverImage { extraLarge } } }";
        let v: serde_json::Value = http
            .post("https://graphql.anilist.co")
            .json(&serde_json::json!({"query": q, "variables": {"s": search}}))
            .send()
            .await?
            .json()
            .await?;
        let m = &v["data"]["Media"];
        let s = |p: &serde_json::Value| p.as_str().map(str::to_owned);
        Ok((
            m["id"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("no {search}: {v}"))?,
            s(&m["title"]["english"]),
            s(&m["title"]["romaji"]),
            s(&m["coverImage"]["extraLarge"]),
        ))
    }

    /// Save an image as `<stem>.<its real type>` (AniList serves some PNGs
    /// under .jpg URLs).
    async fn fetch_image(http: &reqwest::Client, url: &str, dir: &Path, stem: &str) {
        if ["jpg", "png", "webp"]
            .iter()
            .any(|e| dir.join(format!("{stem}.{e}")).exists())
        {
            return;
        }
        if let Ok(r) = http.get(url).send().await
            && let Ok(bytes) = r.bytes().await
        {
            let ext = if bytes.starts_with(b"\x89PNG") {
                "png"
            } else if bytes.get(8..12) == Some(b"WEBP") {
                "webp"
            } else {
                "jpg"
            };
            let _ = std::fs::write(dir.join(format!("{stem}.{ext}")), bytes);
        }
    }

    pub async fn make(dir: &Path) -> anyhow::Result<()> {
        let http = reqwest::Client::new();
        let state = dir.join("state");
        let root = dir.join("Anime");
        let images = state.join("images");
        std::fs::create_dir_all(&images)?;
        let mut library = Library::load(state.join("library.json"), &root);
        let mut lists = HashMap::new();
        let mut downloads = Vec::new();
        let now = chrono::Utc::now().timestamp();
        for (i, (search, on_disk, watched, status, score, downloading)) in SHOWS.iter().enumerate()
        {
            let (id, english, romaji, cover) = find(&http, search).await?;
            let mut details = crate::seadex::anilist_details(&http, id).await?;
            if details.episode_list.iter().all(|e| e.title.is_none())
                && let Ok(more) = crate::seadex::kitsu_episodes(&http, id).await
            {
                crate::seadex::merge_episodes(&mut details.episode_list, more);
            }
            if let Some(url) = details.cover.clone().or(cover.clone()) {
                fetch_image(&http, &url, &images, &format!("{id}-cover")).await;
            }
            if let Some(url) = &details.banner {
                fetch_image(&http, url, &images, &format!("{id}-banner")).await;
            }
            let new = NewSeries {
                anilist_id: id,
                english,
                romaji,
                cover,
                format: details.format.clone(),
                year: details.year,
                ..Default::default()
            };
            let series = library.add(&new, &root);
            series.added = now - i as i64 * 86_400;
            series.episodes = details.episodes;
            std::fs::create_dir_all(&series.folder)?;
            let title = series.title().replace(['/', ':'], " ");
            for n in 1..=*on_disk {
                let f = std::fs::File::create(series.folder.join(format!("{title} - {n:02}.mkv")))?;
                // Sparse: a realistic size without the disk space.
                f.set_len(1_150_000_000 + u64::from(n * 7_919 % 400) * 1_000_000)?;
            }
            for n in 1..=*watched {
                series.watched.insert(
                    n,
                    Watch {
                        position: 1420.0,
                        duration: 1420.0,
                        done: true,
                        at: now,
                    },
                );
            }
            if *watched < *on_disk && *watched > 0 {
                series.watched.insert(
                    watched + 1,
                    Watch {
                        position: 610.0,
                        duration: 1420.0,
                        done: false,
                        at: now,
                    },
                );
            }
            series.details = Some(details);
            lists.insert(
                id,
                Entry {
                    status: *status,
                    score: *score,
                    progress: *watched,
                },
            );
            if !downloading.is_empty() {
                let (source, progress, detail) = if downloading.len() > 2 {
                    ("BitTorrent", 0.37, "12.6 MB/s · 41 peers · 6 min left")
                } else {
                    ("Usenet", 0.72, "48.0 MB/s · 1 min left")
                };
                downloads.push(DownloadView {
                    name: format!("[SeaDex best] {title} ({} episodes)", downloading.len()),
                    series: Some(id),
                    episodes: downloading.to_vec(),
                    source: source.into(),
                    progress,
                    detail: detail.into(),
                    size: size_text(u64::from(downloading.len() as u32) * 1_300_000_000),
                });
            }
        }
        downloads.push(DownloadView {
            name: "[Group] Sintel (open movie)".into(),
            series: None,
            episodes: Vec::new(),
            source: "TorBox".into(),
            progress: 0.91,
            detail: "Fetching from TorBox · 96.2 MB/s".into(),
            size: "1.2 GB".into(),
        });
        library.save_or_warn();
        std::fs::write(
            state.join("lists-cache.json"),
            serde_json::to_vec_pretty(&lists)?,
        )?;
        std::fs::write(
            state.join("downloads-cache.json"),
            serde_json::to_vec_pretty(&downloads)?,
        )?;
        let mut tracking = crate::tracking::Config::default();
        tracking.anilist.user = Some(crate::tracking::anilist::User {
            id: 1,
            name: "jake".into(),
        });
        tracking.mal.user = Some("jake".into());
        tracking.save(&state.join("tracking.json"))?;
        Ok(())
    }

    /// `DEMO_DIR=<folder>`: make the demo library there (uses AniList).
    #[tokio::test]
    #[ignore = "needs DEMO_DIR and the network"]
    async fn make_demo_library() {
        let Some(dir) = std::env::var_os("DEMO_DIR") else {
            return;
        };
        make(Path::new(&dir)).await.unwrap();
        let model = load(&Path::new(&dir).join("state"));
        assert_eq!(model.cards.len(), SHOWS.len());
        std::fs::write(
            Path::new(&dir).join("model.json"),
            serde_json::to_vec_pretty(&model).unwrap(),
        )
        .unwrap();
    }
}
