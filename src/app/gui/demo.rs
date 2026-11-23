//! A demo library of real shows (AniList's details, covers and banners),
//! with episodes "on disk" (empty sparse files), some watched, and list
//! entries: for screenshots of the GUI (see `tests::library_screens`).
//!
//!     DEMO_DIR=/some/dir cargo test make_demo_library -- --ignored

use std::collections::HashMap;
use std::path::Path;

use crate::library::{Library, NewSeries, Watch};
use crate::tracking::{Entry, Status};

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
    let now = chrono::Utc::now().timestamp();
    for (i, (search, on_disk, watched, status, score, _downloading)) in SHOWS.iter().enumerate() {
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
    }
    library.save_or_warn();
    std::fs::write(
        state.join("lists-cache.json"),
        serde_json::to_vec_pretty(&lists)?,
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
    let library = Library::load(
        Path::new(&dir).join("state").join("library.json"),
        Path::new(&dir),
    );
    assert_eq!(library.series.len(), SHOWS.len());
}
