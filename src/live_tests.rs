//! End-to-end checks against the real services. They need the network, so
//! they're ignored by default:
//!
//!     LIVE_TEST_DIR=/some/scratch/dir cargo test -- --ignored --test-threads=1

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::{Engine, Source};
use crate::{nyaa, seadex};

/// Frieren: Beyond Journey's End (AniList 154587), which has a SeaDex entry.
const FRIEREN: u64 = 154587;

fn scratch_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("LIVE_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("seadex-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every file under `dir`, hidden ones included.
fn files_on_disk(dir: &std::path::Path) -> Vec<String> {
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

async fn sintel_torrent() -> Vec<u8> {
    client()
        .get("https://webtorrent.io/torrents/sintel.torrent")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec()
}

async fn wait_finished(handle: &crate::engine::TorrentHandle) {
    let deadline = Instant::now() + Duration::from_secs(240);
    while !handle.stats().finished {
        assert!(
            Instant::now() < deadline,
            "download timed out: {}",
            handle.stats()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("seadex-downloader-tests")
        .build()
        .unwrap()
}

#[tokio::test]
#[ignore = "network"]
#[allow(
    clippy::await_holding_lock,
    reason = "the lock orders whole tests, each on its own runtime"
)]
async fn search_finds_seadex_entry_with_nyaa_release() {
    let _net = crate::mock_http::real();
    let results = seadex::search(&client(), "Sousou no Frieren")
        .await
        .unwrap();
    let frieren = results
        .iter()
        .find(|a| a.anilist_id == FRIEREN)
        .expect("Frieren in AniList results");
    let entry = frieren.entry.as_ref().expect("Frieren has a SeaDex entry");
    let best = entry.best_nyaa(false).expect("a Nyaa release");
    let r = &entry.releases[best];
    println!(
        "best on Nyaa: {} (nyaa {:?}, {} files)",
        r.group,
        r.nyaa_id(),
        r.files.len()
    );
    assert!(r.nyaa_id().is_some());
    // Results with an entry are listed first.
    let first_without = results
        .iter()
        .position(|a| a.entry.is_none())
        .unwrap_or(results.len());
    assert!(results[first_without..].iter().all(|a| a.entry.is_none()));
}

#[tokio::test]
#[ignore = "network"]
#[allow(
    clippy::await_holding_lock,
    reason = "the lock orders whole tests, each on its own runtime"
)]
async fn nyaa_torrent_lists_the_files_seadex_describes() {
    let _net = crate::mock_http::real();
    let http = client();
    let entries = seadex::fetch_entries(&http, &[FRIEREN]).await.unwrap();
    let entry = &entries[&FRIEREN];
    let release = &entry.releases[entry.best_nyaa(false).unwrap()];

    let bytes = nyaa::fetch_torrent(&http, release.nyaa_id().unwrap())
        .await
        .unwrap();
    let dir = scratch_dir("list");
    let engine = Engine::start(&dir.join("state"), dir.join("dl"), &Default::default())
        .await
        .unwrap();
    let listing = engine.inspect(Source::TorrentFile(bytes)).await.unwrap();

    println!(
        "{} ({} files, multi_file={})",
        listing.name,
        listing.files.len(),
        listing.multi_file
    );
    for f in listing.files.iter().take(5) {
        println!("  [{}] {} ({} bytes)", f.index, f.path, f.len);
    }
    assert_eq!(listing.files.len(), release.files.len());
    assert_eq!(
        listing.files.iter().map(|f| f.len).sum::<u64>(),
        release.total_size()
    );
    assert!(listing.existing.is_none());
    // Listing must not leave anything downloading.
    assert!(engine.torrents().is_empty());
    engine.stop().await;
}

/// Downloads one small file from a multi-file torrent of the Sintel open
/// movie (CC BY 3.0) and checks the big video next to it was not fetched.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "network"]
async fn downloads_only_the_selected_file() {
    let http = client();
    let bytes = http
        .get("https://webtorrent.io/torrents/sintel.torrent")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();

    let dir = scratch_dir("download");
    let engine = Engine::start(&dir.join("state"), dir.join("dl"), &Default::default())
        .await
        .unwrap();
    let listing = engine.inspect(Source::TorrentFile(bytes)).await.unwrap();
    for f in &listing.files {
        println!("  [{}] {} ({} bytes)", f.index, f.path, f.len);
    }
    let small = listing.files.iter().min_by_key(|f| f.len).unwrap().clone();
    let big = listing.files.iter().max_by_key(|f| f.len).unwrap().clone();
    let out = listing.output_folder(&dir.join("dl"));
    engine
        .download(
            listing.torrent_bytes.clone(),
            Some(vec![small.index]),
            out.clone(),
        )
        .await
        .unwrap();

    let (_, handle) = engine.torrents().pop().expect("torrent was added");
    assert_eq!(handle.only_files(), Some(vec![small.index]));
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let stats = handle.stats();
        if stats.finished {
            println!(
                "finished: {} of {} selected bytes",
                stats.progress_bytes, stats.total_bytes
            );
            break;
        }
        assert!(Instant::now() < deadline, "timed out; last stats: {stats}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let got = std::fs::metadata(out.join(&small.path)).expect("selected file exists");
    assert_eq!(got.len(), small.len);

    // Nothing but the selected file is on disk, hidden or not: the slivers of
    // its neighbours in the shared piece were deleted once it was verified,
    // and none of the big video was downloaded.
    assert_eq!(files_on_disk(&out), vec![small.path.clone()]);
    println!("only {} on disk; {} not downloaded", small.path, big.path);
    engine.stop().await;
}

/// Selecting a file later that shares a piece with one already downloaded:
/// its sliver of that piece was deleted, so it must be downloaded again
/// rather than left as a hole.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "network"]
async fn selecting_a_neighbour_later_downloads_its_sliver_again() {
    let bytes = sintel_torrent().await;
    let dir = scratch_dir("reselect");
    let engine = Engine::start(&dir.join("state"), dir.join("dl"), &Default::default())
        .await
        .unwrap();
    let listing = engine
        .inspect(Source::TorrentFile(bytes.clone()))
        .await
        .unwrap();
    let index = |name: &str| listing.files.iter().find(|f| f.path == name).unwrap().index;
    let (en, es) = (index("Sintel.en.srt"), index("Sintel.es.srt"));
    let out = listing.output_folder(&dir.join("dl"));

    engine
        .download(bytes.clone(), Some(vec![en]), out.clone())
        .await
        .unwrap();
    let (_, handle) = engine.torrents().pop().unwrap();
    wait_finished(&handle).await;
    assert_eq!(
        files_on_disk(&out),
        vec!["Sintel.en.srt"],
        "es.srt's sliver was deleted"
    );

    // Now also select es.srt, which shares en.srt's piece.
    engine
        .download(bytes.clone(), Some(vec![en, es]), out.clone())
        .await
        .unwrap();
    let (_, handle) = engine.torrents().pop().unwrap();
    wait_finished(&handle).await;
    assert_eq!(files_on_disk(&out), vec!["Sintel.en.srt", "Sintel.es.srt"]);
    let reselected = std::fs::read(out.join("Sintel.es.srt")).unwrap();
    engine.stop().await;

    // Compare with a fresh download of just es.srt.
    let fresh_dir = scratch_dir("reselect-fresh");
    let fresh = Engine::start(
        &fresh_dir.join("state"),
        fresh_dir.join("dl"),
        &Default::default(),
    )
    .await
    .unwrap();
    let fresh_out = listing.output_folder(&fresh_dir.join("dl"));
    fresh
        .download(bytes, Some(vec![es]), fresh_out.clone())
        .await
        .unwrap();
    let (_, fresh_handle) = fresh.torrents().pop().unwrap();
    wait_finished(&fresh_handle).await;
    let expected = std::fs::read(fresh_out.join("Sintel.es.srt")).unwrap();
    assert!(
        reselected == expected,
        "re-selected file differs from a fresh download"
    );
    assert!(
        String::from_utf8_lossy(&reselected).contains("-->"),
        "looks like an .srt"
    );
    fresh.stop().await;
}

/// Torrents (and their file selection) come back when the app restarts.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "network"]
async fn session_restores_torrents_after_restart() {
    let bytes = client()
        .get("https://webtorrent.io/torrents/sintel.torrent")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    let dir = scratch_dir("restore");
    let state = dir.join("state");

    let engine = Engine::start(&state, dir.join("dl"), &Default::default())
        .await
        .unwrap();
    let listing = engine.inspect(Source::TorrentFile(bytes)).await.unwrap();
    let pick = vec![listing.files[0].index, listing.files[1].index];
    let out = listing.output_folder(&dir.join("dl"));
    engine
        .download(
            listing.torrent_bytes.clone(),
            Some(pick.clone()),
            out.clone(),
        )
        .await
        .unwrap();
    let (_, handle) = engine.torrents().pop().unwrap();
    wait_finished(&handle).await;
    engine.stop().await;
    drop(engine);

    let engine = Engine::start(&state, dir.join("dl"), &Default::default())
        .await
        .unwrap();
    let torrents = engine.torrents();
    assert_eq!(torrents.len(), 1, "torrent was not restored");
    let (_, handle) = &torrents[0];
    let mut restored = handle.only_files().expect("file selection restored");
    restored.sort();
    assert_eq!(restored, pick);
    assert_eq!(handle.output_folder(), out.as_path());
    // Still complete after the restart: librqbit's quick resume check passed
    // even though the neighbours' slivers were deleted (a failure here would
    // mean a full re-hash and re-download on every start).
    let deadline = Instant::now() + Duration::from_secs(30);
    while matches!(
        handle.stats().state,
        librqbit::TorrentStatsState::Initializing { .. }
    ) {
        assert!(Instant::now() < deadline, "resume check took too long");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        handle.stats().finished,
        "lost progress across a restart: {}",
        handle.stats()
    );
    let mut picked: Vec<String> = pick
        .iter()
        .map(|&i| {
            listing
                .files
                .iter()
                .find(|f| f.index == i)
                .unwrap()
                .path
                .clone()
        })
        .collect();
    picked.sort();
    assert_eq!(files_on_disk(&out), picked);
    engine.stop().await;
}

/// Nothing is ever uploaded: no listening port, zero bytes sent while
/// downloading, and the torrent stops by itself once the selection is done.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "network"]
async fn never_seeds() {
    let bytes = client()
        .get("https://webtorrent.io/torrents/sintel.torrent")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    let dir = scratch_dir("noseed");
    let engine = Engine::start(&dir.join("state"), dir.join("dl"), &Default::default())
        .await
        .unwrap();
    assert_eq!(
        engine.session().listen_addr(),
        None,
        "must not accept incoming connections"
    );

    let listing = engine.inspect(Source::TorrentFile(bytes)).await.unwrap();
    let poster = listing
        .files
        .iter()
        .find(|f| f.path == "poster.jpg")
        .unwrap()
        .index;
    let out = listing.output_folder(&dir.join("dl"));
    engine
        .download(listing.torrent_bytes.clone(), Some(vec![poster]), out)
        .await
        .unwrap();
    let (_, handle) = engine.torrents().pop().unwrap();

    let deadline = Instant::now() + Duration::from_secs(240);
    while !handle.stats().finished {
        assert!(Instant::now() < deadline, "download timed out");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // The watcher stops finished torrents within a couple of seconds.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !handle.is_paused() {
        assert!(
            Instant::now() < deadline,
            "finished torrent kept running (seeding)"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let stats = handle.stats();
    println!(
        "downloaded {} bytes, uploaded {}",
        stats.progress_bytes, stats.uploaded_bytes
    );
    assert_eq!(stats.uploaded_bytes, 0);
    engine.stop().await;
}

/// For airing shows SeaDex doesn't cover, the Nyaa fallback finds a usable,
/// ranked release for the right season.
#[tokio::test]
#[ignore = "network"]
#[allow(
    clippy::await_holding_lock,
    reason = "the lock orders whole tests, each on its own runtime"
)]
async fn nyaa_fallback_ranks_shows_without_seadex_entries() {
    let _net = crate::mock_http::real();
    use crate::fallback::{self, ReleaseKind, SearchPlan};

    let http = client();
    // Popular shows from the current season, which SeaDex rarely covers yet.
    let body: serde_json::Value = http
        .post("https://graphql.anilist.co")
        .json(&serde_json::json!({
            "query": "query { Page(perPage: 15) { media(season: FALL, seasonYear: 2026, type: ANIME, format: TV, sort: POPULARITY_DESC) { title { romaji english } } } }"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles: Vec<String> = body["data"]["Page"]["media"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["title"]["romaji"].as_str().map(str::to_owned))
        .collect();

    let mut checked = 0;
    for romaji in titles {
        let found = seadex::search(&http, &romaji).await.unwrap();
        let Some(anime) = found
            .into_iter()
            .find(|a| a.romaji.as_deref() == Some(romaji.as_str()))
        else {
            continue;
        };
        if anime
            .entry
            .as_ref()
            .is_some_and(|e| e.best_nyaa(false).is_some())
        {
            continue;
        }
        let plan = SearchPlan::for_anime(&anime);
        let releases = fallback::find_releases(&http, &plan, false).await.unwrap();
        println!(
            "\n{} (queries {:?}, season {:?}): {} releases",
            anime.title,
            plan.queries,
            plan.season,
            releases.len()
        );
        for r in releases.iter().take(6) {
            let kind = match &r.kind {
                ReleaseKind::Torrent { .. } => "torrent",
                ReleaseKind::Episodes(_) => "per-episode",
            };
            println!(
                "  {:<14} {:<18} {:<26} {:<10} {:>9} seeds {:<8} {kind} | {}",
                r.group,
                r.rank.label,
                r.episodes_label(),
                r.quality_label(),
                crate::app::fmt_bytes_pub(r.size),
                r.seeders_label(),
                r.title()
            );
        }
        let best = releases
            .first()
            .expect("Nyaa has releases for a popular airing show");
        assert!(best.usable(), "best release is unusable: {best:?}");
        assert!(
            releases
                .iter()
                .all(|r| !r.title().to_lowercase().ends_with(".zip"))
        );
        checked += 1;
        if checked == 3 {
            break;
        }
    }
    assert!(
        checked > 0,
        "no current-season show without a SeaDex Nyaa release found"
    );
}
