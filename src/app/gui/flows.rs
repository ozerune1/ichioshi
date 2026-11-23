//! The app end to end, through the GUI: searching, downloading (torrents,
//! a debrid service, Usenet, behind a VPN), the library, remuxing, the
//! playlist and Google Drive, and your lists. Background work really runs;
//! the sites it talks to are mocked (or live, where a test says so).
//!
//!     LIVE_TEST_DIR=/some/scratch/dir cargo test flows -- --ignored

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use iced_test::selector::{Candidate, Selector};

use super::tests::*;
use super::*;
use crate::app::{AnimePage, EngineState, Origin};

const FRIEREN: u64 = 154587;

/// The app with its state under `base`, and the library and downloads in
/// `base/downloads` (never the real ones); a tall window, so whole pages
/// can be clicked.
fn app_harness(base: &Path) -> Harness {
    set_size(1440.0, 4000.0);
    let h = harness(base, &base.join("state"));
    let mut h = h;
    h.app.settings.library_dir = base.join("downloads");
    h.app.settings.download_dir = base.join("downloads");
    h
}

/// Wait for `done`, then a few more turns so pages settle.
fn wait_until(h: &mut Harness, what: &str, done: impl Fn(&Gui) -> bool) {
    wait(h, what, done);
    for _ in 0..5 {
        let _ = h.update(Message::Tick);
    }
}

fn save(h: &Harness, dir: &Path, name: &str) {
    snapshot(h, &dir.join(name));
}

fn dir(name: &str) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let out = screens_dir(base.path(), name);
    (base, out)
}

/// How many widgets show `label`.
fn count(h: &Gui, label: &str) -> usize {
    let n = Arc::new(AtomicUsize::new(0));
    let (counter, l) = (n.clone(), label.to_owned());
    let _ = simulator(h).find(move |c: Candidate<'_>| -> Option<()> {
        if Selector::select(&mut l.as_str(), c).is_some() {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        None
    });
    n.load(Ordering::Relaxed)
}

/// The anime page has read its release's files.
fn has_release(g: &Gui) -> bool {
    matches!(&g.app.page, Page::Anime(ap)
        if matches!(g.app.coverage(ap), crate::app::library_ui::Coverage::Batch { .. }))
}

/// AniList (details) and Kitsu (episode titles) answers for the mocks:
/// three untitled episodes, finished.
fn details_reply(req: &crate::mock_http::Req) -> Option<crate::mock_http::Resp> {
    use crate::mock_http::reply;
    use serde_json::json;
    let path = req.target.split('?').next().unwrap();
    if path.starts_with("/kitsu") {
        return Some(reply(200, json!({"data": [], "included": []})));
    }
    (path == "/anilist").then(|| {
        reply(
            200,
            json!({"data": {"Media": {"status": "FINISHED", "format": "TV", "episodes": 3,
                "genres": ["Comedy"], "studios": {"nodes": []}, "startDate": {}, "endDate": {}}}}),
        )
    })
}

/// Point AniList and Kitsu at `server` until dropped.
struct DetailsMocked;

impl DetailsMocked {
    fn at(server: &str) -> Self {
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{server}/anilist"));
        *crate::seadex::KITSU_BASE.lock().unwrap() = Some(format!("{server}/kitsu"));
        DetailsMocked
    }
}

impl Drop for DetailsMocked {
    fn drop(&mut self) {
        *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
        *crate::seadex::KITSU_BASE.lock().unwrap() = None;
    }
}

/// Nyaa's downloads go to the real site again when dropped.
struct ResetNyaa;

impl Drop for ResetNyaa {
    fn drop(&mut self) {
        *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
    }
}

/// An anime on a page, with SeaDex's pick being `release` (or nothing).
fn anime_page(id: u64, episodes: u32, release: Option<crate::seadex::Release>) -> Page {
    use crate::seadex::{Anime, Entry};
    Page::Anime(AnimePage {
        anime: Anime {
            anilist_id: id,
            title: "Show".into(),
            alt_title: None,
            romaji: None,
            english: Some("Show".into()),
            format: Some("TV".into()),
            year: Some(2026),
            episodes: Some(episodes),
            cover_small: None,
            cover_large: None,
            entry: release.map(|r| Entry {
                notes: String::new(),
                incomplete: false,
                theoretical_best: String::new(),
                releases: vec![r],
            }),
        },
        chosen: Some(0),
        nyaa: None,
        origin: crate::app::Origin::Results,
        entry_loading: false,
    })
}

fn remux_state(g: &Gui) -> Option<(bool, Option<String>, Option<crate::remux::TrackSig>)> {
    match &g.app.page {
        Page::Remux(rp) => Some((
            rp.analysis.is_some(),
            rp.audio_lang.clone(),
            rp.subs.clone(),
        )),
        _ => None,
    }
}

/// Remux three episodes (choosing tracks), add them to the playlist,
/// reorder it and export it as a dated zip.
#[test]
#[ignore = "needs ffmpeg and mkvmerge; renders screenshots"]
fn remuxes_and_exports() {
    let _net = crate::mock_http::real();
    use crate::engine::SeriesFiles;
    use crate::library::AnimeRef;
    use crate::remux::Kind;

    let (base, out) = dir("remux-screens");
    let base = base.path();
    let mut h = app_harness(base);
    wait_until(&mut h, "engine", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
    });
    let files = crate::remux::make_test_series(&base.join("series"), 3);
    let series = SeriesFiles {
        name: "[Group] Show (1080p)".into(),
        files,
        not_ready: 1,
        info_hashes: vec![],
    };
    let anime = AnimeRef {
        anilist_id: FRIEREN,
        english: Some("Frieren: Beyond Journey’s End".into()),
        romaji: Some("Sousou no Frieren".into()),
    };
    h.app.open_remux_series(series, Some(anime));
    wait_until(&mut h, "tracks", |g| {
        remux_state(g).is_some_and(|(loaded, _, _)| loaded)
    });
    wait_until(
        &mut h,
        "subtitle samples",
        |g| matches!(&g.app.page, Page::Remux(rp) if !rp.samples.is_empty()),
    );
    let (_, audio, subs) = remux_state(&h).unwrap();
    assert_eq!(audio.as_deref(), Some("jpn"));
    assert_eq!(subs.unwrap().name, "English (Full)");
    save(&h, &out, "1-remux-defaults");

    // Switching to the English dub suggests signs-only subtitles.
    send(
        &mut h,
        [super::remux::pick_language(Kind::Audio, Some("eng".into()))],
    );
    wait_until(&mut h, "dub defaults", |g| {
        remux_state(g).is_some_and(|(_, l, _)| l.as_deref() == Some("eng"))
    });
    let (_, _, subs) = remux_state(&h).unwrap();
    assert_eq!(subs.unwrap().name, "English (Signs)");
    save(&h, &out, "2-remux-dub");

    // Back to Japanese with full subtitles, and remux.
    if let Page::Remux(rp) = &mut h.app.page {
        let analysis = rp.analysis.clone().unwrap();
        rp.audio_lang = Some("jpn".into());
        rp.audio = analysis.default_candidate(Kind::Audio, "jpn", None);
        rp.subs = analysis.default_candidate(Kind::Subtitles, "eng", Some("jpn"));
    }
    click(&mut h, "Remux 3 episodes");
    wait_until(
        &mut h,
        "remux",
        |g| matches!(&g.app.page, Page::Remux(rp) if rp.run.as_ref().is_some_and(|r| r.finished)),
    );
    save(&h, &out, "3-remux-done");
    let made_dir = base
        .join("downloads")
        .join("Remuxed")
        .join("Frieren: Beyond Journey’s End");
    let mut made: Vec<String> = std::fs::read_dir(&made_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    made.sort();
    assert_eq!(
        made,
        [
            "Frieren: Beyond Journey’s End - 01.mkv",
            "Frieren: Beyond Journey’s End - 02.mkv",
            "Frieren: Beyond Journey’s End - 03.mkv",
        ]
    );

    // Into the playlist, reorder, export.
    click(&mut h, "Add 3 to playlist");
    assert_eq!(h.app.playlist.items.len(), 3);
    click(&mut h, "Playlist");
    wait_until(&mut h, "playlist page", |g| {
        matches!(g.app.page, Page::Playlist(_))
    });
    // Move episode 3 to the top with its "↑" button (row 3, then row 2).
    for row in [2, 1] {
        click_nth(&mut h, "↑", row);
    }
    let order: Vec<String> = h
        .app
        .playlist
        .items
        .iter()
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(
        order,
        [
            "Frieren: Beyond Journey’s End - 03",
            "Frieren: Beyond Journey’s End - 01",
            "Frieren: Beyond Journey’s End - 02",
        ]
    );
    save(&h, &out, "4-playlist");
    click(&mut h, "Export 3 episodes");
    wait_until(&mut h, "export", |g| {
        matches!(&g.app.page, Page::Playlist(_))
            && g.app
                .status
                .as_ref()
                .is_some_and(|s| s.text.starts_with("Exported"))
    });
    save(&h, &out, "5-playlist-exported");

    let zips: Vec<PathBuf> = std::fs::read_dir(base.join("downloads"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "zip"))
        .collect();
    assert_eq!(zips.len(), 1);
    let stem = zips[0].file_stem().unwrap().to_string_lossy().into_owned();
    assert!(
        chrono::NaiveDateTime::parse_from_str(&stem, "%Y-%m-%d %H-%M-%S").is_ok(),
        "zip named after the date and time: {stem}"
    );
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&zips[0]).unwrap()).unwrap();
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            format!("{stem}/"),
            format!("{stem}/1) Frieren: Beyond Journey’s End - 03.mkv"),
            format!("{stem}/2) Frieren: Beyond Journey’s End - 01.mkv"),
            format!("{stem}/3) Frieren: Beyond Journey’s End - 02.mkv"),
        ]
    );
    let mut first = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_index(1).unwrap(), &mut first).unwrap();
    assert_eq!(
        first,
        std::fs::read(made_dir.join("Frieren: Beyond Journey’s End - 03.mkv")).unwrap()
    );

    // The remuxed copies are listed below; done with them, they're deleted
    // (after asking), and leave the playlist too.
    assert!(shows(&h, "Remuxed copies"));
    assert!(shows(&h, "Frieren: Beyond Journey’s End"));
    assert_eq!(count(&h, "Delete…"), 3);
    save(&h, &out, "6-remuxed-copies");
    click_nth(&mut h, "Delete…", 0);
    assert!(shows(&h, "Delete remuxed copies?"));
    save(&h, &out, "7-delete-remux");
    click(&mut h, "Delete");
    assert!(
        !made_dir
            .join("Frieren: Beyond Journey’s End - 01.mkv")
            .exists()
    );
    assert!(
        made_dir
            .join("Frieren: Beyond Journey’s End - 02.mkv")
            .exists()
    );
    assert_eq!(count(&h, "Delete…"), 2);
    assert_eq!(h.app.playlist.items.len(), 2, "out of the playlist too");
    click(&mut h, "Delete all…");
    click(&mut h, "Delete");
    assert!(!made_dir.exists());
    assert!(
        base.join("downloads").join("Remuxed").exists(),
        "only the series' folder"
    );
    assert!(shows_containing(&h, "None yet."));
}

/// Google Drive on the playlist page: the setup steps, choosing a folder,
/// and an export ready to upload.
#[test]
#[ignore = "renders screenshots"]
fn drive_setup_and_folder() {
    let _net = crate::mock_http::real();
    use crate::app::drive_ui::{SETUP_STEPS, browser_for_test, signed_in_for_test};
    use crate::gdrive::{Entry, Place};
    use crate::playlist::Item;

    let (base, out) = dir("drive-screens");
    let base = base.path();
    let mut h = app_harness(base);
    wait_until(&mut h, "engine", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
    });

    // Not set up yet: how to get a client file.
    click(&mut h, "Playlist");
    for step in SETUP_STEPS {
        assert!(shows(&h, step), "{step}");
    }
    let episodes: Vec<Item> = (1..=4)
        .map(|n| {
            let path = base.join(format!("Show - 0{n}.mkv"));
            std::fs::write(&path, [0u8; 1024]).unwrap();
            Item {
                path,
                title: format!("Show - 0{n}"),
            }
        })
        .collect();
    h.app.playlist.add(episodes);
    assert!(shows(&h, "Export 4 episodes"));
    save(&h, &out, "drive-1-setup");
    h.app.playlist.clear();

    // Signed in, choosing a folder.
    signed_in_for_test(&mut h.app.drive, "me@example.com", None);
    browser_for_test(
        &mut h.app.drive,
        &[
            ("Google Drive", Place::Top),
            ("My Drive", Place::Folder("root".into())),
            ("Anime", Place::Folder("anime".into())),
        ],
        vec![
            Entry {
                name: "Playlists".into(),
                place: Place::Folder("p".into()),
            },
            Entry {
                name: "Seasonal".into(),
                place: Place::Folder("s".into()),
            },
        ],
    );
    save(&h, &out, "drive-2-choose-folder");
    click(&mut h, "Upload to “Anime”");
    let folder = h.app.drive.account.folder.clone().unwrap();
    assert_eq!(
        (folder.id.as_str(), folder.path.as_str()),
        ("anime", "My Drive / Anime")
    );
    assert!(h.app.drive.browser.is_none());

    // Ready to upload an export.
    let mut items = Vec::new();
    for n in 1..=2 {
        let path = base.join(format!("Show - 0{n}.mkv"));
        std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();
        items.push(Item {
            path,
            title: format!("Show - 0{n}"),
        });
    }
    h.app.playlist.add(items);
    if let Page::Playlist(pp) = &mut h.app.page {
        pp.exported_for_test(base.join("downloads/2026-11-21 16-48-35.zip"));
    }
    save(&h, &out, "drive-3-ready");
    assert!(shows(&h, "Upload 2026-11-21 16-48-35.zip"));
}

/// Uploading from the playlist page to a mock Google server; the upload
/// keeps going after leaving the page, and what arrives is the file.
#[test]
#[ignore = "renders screenshots"]
fn uploads_in_the_background() {
    let _net = crate::mock_http::real();
    use crate::app::drive_ui::signed_in_for_test;
    use crate::gdrive::Folder;
    use crate::gdrive::tests::endpoints;
    use crate::mock_http::{Resp, mock, reply};
    use serde_json::json;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    // Made first, dropped last: the mock is released before it stops.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("drive-upload-screens");
    let base = base.path();
    let zip = base.join("2026-11-21 16-48-35.zip");
    let content: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&zip, &content).unwrap();

    // The mock holds the upload until `release` is set (also when the test
    // fails, so the runtime can shut down).
    let release = Arc::new(AtomicBool::new(false));
    struct ReleaseOnDrop(Arc<AtomicBool>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _release = ReleaseOnDrop(release.clone());
    let stored = Arc::new(Mutex::new(Vec::<u8>::new()));
    let session = Arc::new(Mutex::new(String::new()));
    let server = {
        let (release, stored, session) = (release.clone(), stored.clone(), session.clone());
        rt.block_on(mock(move |req| {
            match (req.method.as_str(), req.target.split('?').next().unwrap()) {
                ("POST", "/token") => reply(200, json!({"access_token": "at", "expires_in": 3600})),
                ("POST", "/upload/drive/v3/files") => {
                    let meta: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                    assert_eq!(meta["parents"], json!(["anime"]));
                    Resp {
                        status: 200,
                        headers: vec![("Location".into(), session.lock().unwrap().clone())],
                        body: Vec::new(),
                    }
                }
                ("PUT", "/session") => {
                    // The first chunk goes up quickly; the rest wait for `release`.
                    let first = stored.lock().unwrap().is_empty();
                    if first {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    } else {
                        while !release.load(Ordering::Relaxed) {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                    }
                    let mut stored = stored.lock().unwrap();
                    stored.extend_from_slice(&req.body);
                    if stored.len() < 3_000_000 {
                        let range = format!("bytes=0-{}", stored.len() - 1);
                        return Resp {
                            status: 308,
                            headers: vec![("Range".into(), range)],
                            body: Vec::new(),
                        };
                    }
                    reply(
                        200,
                        json!({"id": "f", "name": "2026-11-21 16-48-35.zip",
                            "webViewLink": "https://drive.google.com/file/d/f/view"}),
                    )
                }
                other => panic!("unexpected {other:?}"),
            }
        }))
    };
    *session.lock().unwrap() = format!("{server}/session");

    let mut h = app_harness(base);
    wait_until(&mut h, "engine", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
    });
    let folder = Folder {
        id: "anime".into(),
        path: "My Drive / Anime".into(),
    };
    signed_in_for_test(&mut h.app.drive, "me@example.com", Some(folder));
    h.app.drive.use_endpoints_for_test(endpoints(&server));
    h.app.drive.use_chunk_for_test(1 << 20);
    let episode = base.join("Show - 01.mkv");
    std::fs::write(&episode, [0u8; 1024]).unwrap();
    h.app.playlist.add(vec![crate::playlist::Item {
        path: episode,
        title: "Show - 01".into(),
    }]);
    click(&mut h, "Playlist");
    if let Page::Playlist(pp) = &mut h.app.page {
        pp.exported_for_test(zip.clone());
    }
    click(&mut h, "Upload 2026-11-21 16-48-35.zip");
    wait_until(&mut h, "first chunk up", |g| {
        g.app.drive.upload_progress().is_some_and(|p| p > 0.3)
    });
    // After a chunk, the time left shows on the page and in the sidebar.
    assert!(
        shows_containing(&h, "Uploading to Drive · "),
        "in the sidebar"
    );
    assert!(shows_containing(&h, "left"), "time estimate shown");
    save(&h, &out, "upload-0-time-left");

    // Leave the page: the upload carries on, shown in the sidebar.
    click(&mut h, "Library");
    assert!(matches!(h.app.page, Page::Home));
    save(&h, &out, "upload-1-in-background");
    assert!(
        h.app.drive.upload_progress().is_some(),
        "leaving the page must not stop the upload"
    );
    assert!(shows_containing(&h, "Uploading to Drive"));

    release.store(true, Ordering::Relaxed);
    wait_until(&mut h, "upload done", |g| {
        g.app.drive.upload_result_for_test().is_some()
    });
    let result = h.app.drive.upload_result_for_test().unwrap().clone();
    assert_eq!(result.map(|u| u.id), Ok("f".to_owned()));
    assert!(
        *stored.lock().unwrap() == content,
        "Drive must get exactly the zip"
    );
    click(&mut h, "Playlist");
    save(&h, &out, "upload-2-done");
    assert!(shows(&h, "Open in Drive ↗"));
    assert!(
        !shows(&h, "Upload 2026-11-21 16-48-35.zip"),
        "already uploaded"
    );
}

/// Downloading through a debrid service (a mock TorBox): the setting, the
/// file picker, the download card through its stages, and Remux from it.
/// BitTorrent never starts.
#[test]
#[ignore = "renders screenshots"]
fn downloads_through_a_debrid_service() {
    let _net = crate::mock_http::real();
    use crate::app::FilesPage;
    use crate::debrid::jobs::Phase;
    use crate::debrid::jobs::tests::{content, make_torrent, serve};
    use crate::debrid::{Config, Provider};
    use crate::mock_http::{mock, reply};
    use serde_json::json;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("debrid-screens");
    let base = base.path();
    let files = [
        ("Show - 01.mkv", 400_000u64),
        ("Show - 02.mkv", 300_000),
        ("Show - 03.mkv", 200_000),
    ];
    let torrent = make_torrent("[Group] Show S01", &files);
    let bodies: Vec<String> = files
        .iter()
        .enumerate()
        .map(|(i, f)| content(i as u8, f.1))
        .collect();

    // TorBox holds the torrent "downloading" until released.
    let release = Arc::new(AtomicBool::new(false));
    let base_cell = Arc::new(Mutex::new(String::new()));
    let server = {
        let (release, base_cell, bodies) = (release.clone(), base_cell.clone(), bodies.clone());
        rt.block_on(mock(move |req| {
            let ok = |data: serde_json::Value| reply(200, json!({"success": true, "data": data}));
            let path = req.target.split('?').next().unwrap();
            match path {
                "/user/me" => ok(json!({"email": "me@example.com", "plan": 2,
                    "premium_expires_at": "2027-03-01T00:00:00Z"})),
                "/torrents/createtorrent" => ok(json!({"torrent_id": 8, "hash": "h"})),
                "/torrents/mylist" => {
                    let ready = release.load(Ordering::SeqCst);
                    let fs: Vec<serde_json::Value> = files
                        .iter()
                        .enumerate()
                        .map(|(i, (p, len))| {
                            json!({"id": i, "name": format!("[Group] Show S01/{p}"), "size": len})
                        })
                        .collect();
                    ok(json!({"id": 8, "download_state": if ready { "cached" } else { "downloading" },
                        "progress": 0.62, "download_speed": 48_000_000, "seeds": 31,
                        "download_finished": ready, "download_present": ready, "files": fs}))
                }
                "/torrents/requestdl" => {
                    let file = req.query()["file_id"].clone();
                    ok(json!(format!("{}/cdn/{file}", base_cell.lock().unwrap())))
                }
                p if p.starts_with("/cdn/") => {
                    serve(req, &bodies[p[5..].parse::<usize>().unwrap()])
                }
                other => panic!("unexpected {other}"),
            }
        }))
    };
    *base_cell.lock().unwrap() = server.clone();

    // Debrid is on from the start, with no service set up yet.
    let config_file = base.join("state").join("debrid.json");
    Config {
        enabled: true,
        ..Default::default()
    }
    .save(&config_file)
    .unwrap();
    let mut h = app_harness(base);
    assert!(
        matches!(h.app.engine, EngineState::Off),
        "no BitTorrent in debrid mode"
    );
    h.app
        .debrid
        .manager
        .use_base_for_test(Provider::TorBox, &server);

    // Set it up in Settings.
    click(&mut h, "Settings");
    click(&mut h, "A debrid service");
    click(&mut h, "TorBox");
    type_into(&mut h, "debrid-key", "tb-key", false);
    click(&mut h, "Check");
    wait_until(&mut h, "account check", |g| g.app.debrid.account.is_some());
    save(&h, &out, "debrid-1-settings");
    assert!(shows(&h, "✓ me@example.com · premium until 2027-03-01"));
    let saved = Config::load(&config_file);
    assert_eq!(
        (saved.provider, saved.key(Provider::TorBox)),
        (Provider::TorBox, Some("tb-key"))
    );
    assert!(shows(&h, "Downloads through TorBox"), "in the sidebar");

    // Pick episodes 1 and 3.
    let listing = crate::engine::listing_from_bytes(torrent.clone()).unwrap();
    let mut fp = FilesPage::new(listing, None, "test".into());
    fp.checked = vec![true, false, true];
    h.app.page = Page::Files(fp);
    save(&h, &out, "debrid-2-files");
    click(&mut h, "↓  Download via TorBox");
    wait_until(&mut h, "TorBox fetching", |g| {
        g.app
            .debrid
            .manager
            .views()
            .first()
            .is_some_and(|v| matches!(v.live.phase, Phase::Remote { .. }))
    });
    click(&mut h, "Downloads");
    save(&h, &out, "debrid-3-service-fetching");
    assert!(shows(&h, "62%"));
    assert!(shows_containing(&h, "TorBox is getting it"));

    release.store(true, Ordering::SeqCst);
    wait_until(&mut h, "done", |g| {
        g.app
            .debrid
            .manager
            .views()
            .first()
            .is_some_and(|v| v.live.phase == Phase::Done)
    });
    click(&mut h, "Files (2)");
    save(&h, &out, "debrid-4-done");
    let got = base.join("downloads").join("[Group] Show S01");
    assert_eq!(
        std::fs::read_to_string(got.join("Show - 01.mkv")).unwrap(),
        bodies[0]
    );
    assert_eq!(
        std::fs::read_to_string(got.join("Show - 03.mkv")).unwrap(),
        bodies[2]
    );
    assert!(!got.join("Show - 02.mkv").exists());
    assert!(
        matches!(h.app.engine, EngineState::Off),
        "still no BitTorrent"
    );

    // Episodes… reopens the picker with the job's selection.
    click(&mut h, "Episodes…");
    let Page::Files(fp) = &h.app.page else {
        panic!("files page")
    };
    assert_eq!(fp.checked, vec![true, false, true]);
    assert!(shows(&h, "Update download"));

    // Remux… opens the remux page with the downloaded episodes.
    click(&mut h, "Downloads");
    click(&mut h, "Remux…");
    let Page::Remux(rp) = &h.app.page else {
        panic!("remux page")
    };
    assert_eq!(rp.series.files.len(), 2);
}

/// Torrents over a VPN (PIA, through a stand-in `piactl`), with loopback
/// standing in for the VPN's interface: the engine waits while PIA is
/// disconnected, starts bound to the interface and listening on the
/// forwarded port once connected, stops when the VPN drops and restarts
/// when it's back. Turning the VPN off puts torrents back on the normal
/// connection.
#[test]
#[ignore = "renders screenshots"]
fn torrents_wait_for_the_vpn() {
    let _net = crate::mock_http::real();
    use crate::vpn::tests::{FakeProbe, iface};
    use crate::vpn::{Config, Env, Network, Service, pia};
    use std::time::{Duration, Instant};

    let (base, out) = dir("vpn-screens");
    let base = base.path();
    let pia_dir = base.join("pia");
    let piactl = pia::tests::fake_piactl(&pia_dir, "Disconnected", "Inactive", true);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listening = || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
    let config = Config {
        enabled: true,
        service: Service::Pia,
        interface: Some("lo".into()),
        ..Default::default()
    };
    config.save(&base.join("state").join("vpn.json")).unwrap();
    let mut h = app_harness(base);
    let probe = Arc::new(FakeProbe::default());
    let tunnel = |index| crate::vpn::Interface {
        tunnel: true,
        loopback: false,
        ..iface("lo", index, [127, 0, 0, 1], true)
    };
    *probe.interfaces.lock().unwrap() = vec![tunnel(1)];
    h.app.use_vpn_env_for_test(Env {
        probe: probe.clone(),
        piactl: Some(piactl),
        natpmp: None,
        poll: Duration::from_millis(100),
        port_wait: Duration::from_secs(2),
    });

    // PIA is disconnected: torrents wait, offline (no DHT, no trackers), and
    // the app offers to connect.
    let offline = |g: &Gui| matches!(g.app.engine, EngineState::Ready(_)) && g.app.engine_offline();
    wait_until(&mut h, "waiting", |g| {
        offline(g) && g.app.vpn.waiting.as_deref() == Some("PIA is disconnected")
    });
    assert!(
        h.app.engine().unwrap().session().get_dht().is_none(),
        "no DHT while offline"
    );
    click(&mut h, "Downloads");
    save(&h, &out, "vpn-1-waiting");
    assert!(shows(&h, "Torrents are waiting for the VPN"));
    assert!(shows(&h, "Torrents wait for the VPN"), "in the sidebar");
    assert!(
        h.app
            .download_ready()
            .unwrap_err()
            .contains("waiting for the VPN")
    );
    click(&mut h, "Connect PIA");
    wait_until(&mut h, "connect ran", |_| {
        std::fs::read_to_string(pia_dir.join("calls")).is_ok_and(|c| c == "connect\n")
    });

    // Connected, with a forwarded port.
    pia::tests::set(&pia_dir, "connectionstate", "Connected");
    pia::tests::set(&pia_dir, "portforward", &port.to_string());
    wait_until(&mut h, "engine on the VPN", |g| {
        matches!(g.app.engine, EngineState::Ready(_)) && g.app.vpn.waiting.is_none()
    });
    let on_vpn = Network {
        interface: Some("lo".into()),
        index: 1,
        port: Some(port),
        offline: false,
    };
    wait_until(&mut h, "the forwarded port", |g| {
        g.app.engine_net == Some(on_vpn.clone())
    });
    assert!(listening(), "listens on the forwarded port");
    // The listening socket is bound to the interface (ss shows it as addr%device).
    if let Ok(out) = std::process::Command::new("ss").args(["-Htln"]).output() {
        let ss = String::from_utf8_lossy(&out.stdout);
        assert!(
            ss.contains(&format!("%lo:{port}")),
            "not bound to the interface:\n{ss}"
        );
    }
    assert!(shows(&h, &format!("VPN: lo · port {port}")));
    click(&mut h, "Settings");
    save(&h, &out, "vpn-2-settings");
    assert!(shows(&h, &format!("✓ Port {port} forwarded")));

    // The VPN drops: the engine stops, so nothing goes out.
    pia::tests::set(&pia_dir, "connectionstate", "Reconnecting");
    probe.interfaces.lock().unwrap().clear();
    wait_until(&mut h, "offline", |g| {
        offline(g) && g.app.vpn.waiting.as_deref() == Some("PIA is reconnecting")
    });
    click(&mut h, "Downloads");
    assert!(!shows(&h, "Connect PIA"), "already reconnecting");
    save(&h, &out, "vpn-3-dropped");
    let deadline = Instant::now() + Duration::from_secs(10);
    while listening() {
        assert!(
            Instant::now() < deadline,
            "still listening after the VPN dropped"
        );
        let _ = h.update(Message::Tick);
        std::thread::sleep(Duration::from_millis(100));
    }

    // Back, as a new interface: the engine restarts on it.
    *probe.interfaces.lock().unwrap() = vec![tunnel(7)];
    pia::tests::set(&pia_dir, "connectionstate", "Connected");
    wait_until(&mut h, "restarted", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
            && g.app.engine_net.as_ref().is_some_and(|n| n.index == 7)
    });
    assert!(listening());

    // VPN off: torrents go back to the normal connection.
    click(&mut h, "Settings");
    click(&mut h, "Only download torrents over a VPN");
    wait_until(&mut h, "off the VPN", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
            && g.app.engine_net == Some(Network::default())
    });
    assert!(!h.app.vpn.config.enabled);
    assert!(
        !Config::load(&base.join("state").join("vpn.json")).enabled,
        "saved"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while listening() {
        assert!(
            Instant::now() < deadline,
            "the forwarded port stays open without the VPN"
        );
        let _ = h.update(Message::Tick);
        std::thread::sleep(Duration::from_millis(100));
    }

    // Downloading through a debrid service instead: torrents from before wait
    // offline, quietly, and nothing about the VPN is in the way.
    {
        let a = &mut h.app;
        a.debrid.config.enabled = true;
        a.debrid.config.provider = crate::debrid::Provider::TorBox;
        a.debrid
            .config
            .keys
            .insert(crate::debrid::Provider::TorBox, "key".into());
        a.vpn.config.enabled = true;
        a.vpn.monitor.set_config(a.vpn.config.clone());
    }
    pia::tests::set(&pia_dir, "connectionstate", "Disconnected");
    wait_until(&mut h, "offline again", |g| {
        offline(g) && g.app.vpn.waiting.is_some()
    });
    click(&mut h, "Downloads");
    save(&h, &out, "vpn-4-debrid-mode");
    assert!(
        !shows(&h, "Torrents are waiting for the VPN"),
        "no banner without local torrenting"
    );
    assert!(!shows(&h, "Torrents wait for the VPN"));
    assert!(!shows_containing(&h, "VPN:"), "no VPN status either");
    assert!(
        h.app.download_ready().is_ok(),
        "the debrid service doesn't need the VPN"
    );
}

/// Downloading from Usenet: a release on SeaDex that both indexers (mocks)
/// have, a fake provider (TLS, with a login) serving its articles, Nyaa (a
/// mock) for its .torrent. The release list shows it's on Usenet, and the
/// picked episodes arrive intact; the providers dialog checks the login.
#[test]
#[ignore = "renders screenshots"]
fn downloads_from_usenet() {
    let _net = crate::mock_http::mocked();
    use crate::debrid::jobs::tests::make_torrent;
    use crate::mock_http::{Resp, mock, reply};
    use crate::usenet::fake::{FakeProvider, Options, upload};
    use crate::usenet::indexers::Indexers;
    use serde_json::json;
    use std::time::{Duration, Instant};

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("usenet-screens");
    let base = base.path();
    let episodes = [
        ("Show - 01.mkv", 400_000usize),
        ("Show - 02.mkv", 300_000),
        ("Show - 03.mkv", 200_000),
    ];
    let data: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            (0..episodes[i].1)
                .map(|n| ((n * (i + 5)) % 251) as u8)
                .collect()
        })
        .collect();
    let sizes: Vec<(&str, u64)> = episodes.iter().map(|(n, l)| (*n, *l as u64)).collect();
    let torrent = make_torrent("[Group] Show S01", &sizes);
    let hash = crate::engine::info_hash_of(&torrent).unwrap();
    let posted: Vec<(&str, &[u8])> = episodes
        .iter()
        .zip(&data)
        .map(|((n, _), d)| (*n, d.as_slice()))
        .collect();
    let up = upload("ui", &posted, 48_000, 16_384, 12);
    let fake = rt.block_on(FakeProvider::start(
        up.articles.clone(),
        Options {
            login: Some(("me".to_owned(), "pw".to_owned())),
            tls: true,
            delay: Duration::from_millis(300),
        },
    ));

    // Nyaa and both indexers.
    let nzb = up.nzb().into_bytes();
    let base_url = Arc::new(std::sync::Mutex::new(String::new()));
    let server = {
        let (hash, base_url) = (hash.clone(), base_url.clone());
        rt.block_on(mock(move |req| {
            if let Some(r) = details_reply(req) {
                return r;
            }
            let path = req.target.split('?').next().unwrap().to_owned();
            let url = base_url.lock().unwrap().clone();
            match path.as_str() {
                "/download/777.torrent" => Resp::binary(200, torrent.clone()),
                "/at/json" => reply(
                    200,
                    json!({"id": 1, "info_hash": hash, "status": "complete",
                        "nzb_url": format!("{url}/at/nzb/1")}),
                ),
                "/at/nzb/1" => Resp::binary(200, nzb.clone()),
                p if p == format!("/tk/v1/torrents/btih/{hash}") => reply(
                    200,
                    json!({"id": 5, "btih": hash, "state": "completed", "has_nzb": 1}),
                ),
                p if p.starts_with("/tks/nzbs/5/") => Resp::binary(200, nzb.clone()),
                other => panic!("unexpected {other}"),
            }
        }))
    };
    *base_url.lock().unwrap() = server.clone();
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = ResetNyaa;
    let _details = DetailsMocked::at(&server);

    let mut usenet = crate::usenet::Config::default();
    usenet.add(fake.provider("Fake News"));
    usenet
        .save(&base.join("state").join("usenet.json"))
        .unwrap();
    let mut h = app_harness(base);
    h.app.usenet.use_indexers_for_test(Indexers::with_bases(
        reqwest::Client::new(),
        &format!("{server}/at"),
        &format!("{server}/tk"),
        &format!("{server}/tks"),
    ));

    // An anime whose SeaDex pick is this release.
    let release = crate::seadex::Release {
        group: "Group".into(),
        is_best: true,
        dual_audio: false,
        tracker: "Nyaa".into(),
        url: "https://nyaa.si/view/777".into(),
        info_hash: hash.clone(),
        tags: vec![],
        files: vec![],
    };
    h.app.page = anime_page(1, 3, Some(release));
    // Every episode listed, none downloaded yet; the release (folded) says
    // it's on Usenet.
    wait_until(&mut h, "episodes and NZB lookup", |g| {
        g.app.usenet.links(&hash).is_some_and(|l| l.len() == 2) && has_release(g)
    });
    save(&h, &out, "usenet-1-episodes");
    assert_eq!(count(&h, "↓ Download"), 3, "three episodes to download");
    assert!(shows(&h, "↓  Download 3 missing"));
    click(&mut h, "Change");
    assert!(shows(&h, "NZB"), "the release list says it's on Usenet");
    save(&h, &out, "usenet-2-release");

    // Episode 1 from its row; then episode 3, which joins the same download.
    click_nth(&mut h, "↓ Download", 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    while count(&h, "↓ Download") != 2 {
        assert!(Instant::now() < deadline, "episode 1 never started");
        let _ = h.update(Message::Tick);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.app.usenet.manager.views().len(), 1);
    click_nth(&mut h, "↓ Download", 1);
    wait(&mut h, "episodes 1 and 3", |g| {
        g.app
            .usenet
            .manager
            .views()
            .first()
            .is_some_and(|v| v.job.files.iter().filter(|f| f.wanted).count() == 2)
    });
    // Caught part way: progress bars on the episodes.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !h
        .app
        .pages
        .stats
        .get(&1)
        .is_some_and(|s| s.progress.values().any(|p| *p > 0.1))
    {
        assert!(Instant::now() < deadline, "timed out waiting for progress");
        let _ = h.update(Message::Tick);
        std::thread::sleep(Duration::from_millis(10));
    }
    save(&h, &out, "usenet-3-progress");
    assert!(shows_containing(&h, "%"), "progress bars on the episodes");
    assert_eq!(
        count(&h, "↓ Download"),
        1,
        "only episode 2 left to download"
    );

    // Done, it's imported: off the downloads list, in the series' folder.
    let folder = base.join("downloads").join("Show");
    wait_until(&mut h, "imported", |g| {
        g.app.usenet.manager.views().is_empty() && folder.join("Show - 03.mkv").exists()
    });
    wait_until(&mut h, "counted", |g| {
        g.app
            .pages
            .stats
            .get(&1)
            .is_some_and(|s| s.files.len() == 2)
    });
    save(&h, &out, "usenet-4-imported");
    assert!(shows_containing(&h, "2 of 3 in your library"));
    assert!(
        shows(&h, "[Group] Show S01 is in your library"),
        "the note says it's imported"
    );
    assert!(std::fs::read(folder.join("Show - 01.mkv")).unwrap() == data[0]);
    assert!(std::fs::read(folder.join("Show - 03.mkv")).unwrap() == data[2]);
    assert!(!folder.join("Show - 02.mkv").exists());
    assert!(
        h.app.engine().is_none_or(|e| e.torrents().is_empty()),
        "nothing went to BitTorrent"
    );
    let series = h.app.library.find(1).expect("added to the library");
    assert_eq!(series.folder, folder);
    assert!(series.downloads.contains(&hash));

    // The providers dialog: check the login.
    click(&mut h, "Settings");
    assert!(shows(&h, "Fake News") && shows(&h, "4 connections"));
    click(&mut h, "Providers…");
    click(&mut h, "Check");
    wait_until(&mut h, "the login check", |g| {
        shows(g, "✓ Connected and logged in")
    });
    save(&h, &out, "usenet-5-providers");
    click(&mut h, "Done");
    assert!(h.app.usenet.dialog.is_none());
}

/// A release with one torrent per episode, downloading through a debrid
/// service, with a Usenet provider set up: episodes on Usenet come from
/// there, and the picker says so.
#[test]
#[ignore = "renders screenshots"]
fn per_episode_releases_prefer_usenet() {
    let _net = crate::mock_http::mocked();
    use crate::app::FilesPage;
    use crate::debrid::jobs::tests::make_torrent;
    use crate::mock_http::{Resp, mock, reply};
    use crate::nyaa::NyaaItem;
    use crate::usenet::fake::{FakeProvider, Options, upload};
    use crate::usenet::indexers::Indexers;
    use crate::usenet::jobs::Phase;
    use serde_json::json;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("usenet-episodes-screens");
    let base = base.path();
    // Episodes 1 and 2 are on Usenet; 3 isn't.
    let names = ["Show - 01.mkv", "Show - 02.mkv", "Show - 03.mkv"];
    let data: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            (0..150_000usize)
                .map(|n| ((n * (i + 7)) % 249) as u8)
                .collect()
        })
        .collect();
    let torrents: Vec<Vec<u8>> = names
        .iter()
        .map(|n| make_torrent(&n.replace(".mkv", ""), &[(n, 150_000)]))
        .collect();
    let hashes: Vec<String> = torrents
        .iter()
        .map(|t| crate::engine::info_hash_of(t).unwrap())
        .collect();
    let uploads: Vec<_> = (0..2)
        .map(|i| upload(&format!("ep{i}"), &[(names[i], &data[i])], 40_000, 8192, 6))
        .collect();
    let mut articles = uploads[0].articles.clone();
    articles.extend(uploads[1].articles.clone());
    let fake = rt.block_on(FakeProvider::start(articles, Options::default()));

    let nzbs: Vec<Vec<u8>> = uploads.iter().map(|u| u.nzb().into_bytes()).collect();
    let url_cell = Arc::new(std::sync::Mutex::new(String::new()));
    let server = {
        let (hashes, url_cell) = (hashes.clone(), url_cell.clone());
        rt.block_on(mock(move |req| {
            let path = req.target.split('?').next().unwrap().to_owned();
            let url = url_cell.lock().unwrap().clone();
            if let Some(id) = path
                .strip_prefix("/download/")
                .and_then(|p| p.strip_suffix(".torrent"))
            {
                return Resp::binary(200, torrents[id.parse::<usize>().unwrap() - 901].clone());
            }
            match path.as_str() {
                "/at/json" => match hashes.iter().position(|h| *h == req.query()["btih"]) {
                    Some(i) if i < 2 => reply(
                        200,
                        json!({"info_hash": hashes[i], "status": "complete",
                            "nzb_url": format!("{url}/at/nzb/{i}")}),
                    ),
                    _ => reply(200, json!({})),
                },
                p if p.starts_with("/at/nzb/") => {
                    Resp::binary(200, nzbs[p[8..].parse::<usize>().unwrap()].clone())
                }
                p if p.starts_with("/tk/") => reply(404, json!({"detail": "not found"})),
                other => panic!("unexpected {other}"),
            }
        }))
    };
    *url_cell.lock().unwrap() = server.clone();
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = ResetNyaa;

    // TorBox is the download method (no key, so the real TorBox is never
    // asked), and a Usenet provider is set up.
    let state = base.join("state");
    crate::debrid::Config {
        enabled: true,
        provider: crate::debrid::Provider::TorBox,
        ..Default::default()
    }
    .save(&state.join("debrid.json"))
    .unwrap();
    let mut usenet = crate::usenet::Config::default();
    usenet.add(fake.provider("Fake News"));
    usenet.save(&state.join("usenet.json")).unwrap();
    let mut h = app_harness(base);
    h.app.usenet.use_indexers_for_test(Indexers::with_bases(
        reqwest::Client::new(),
        &format!("{server}/at"),
        &format!("{server}/tk"),
        &format!("{server}/tks"),
    ));
    let items: Vec<(u32, NyaaItem)> = (0..3)
        .map(|i| {
            let item = NyaaItem {
                id: 901 + i as u64,
                title: format!("[Group] Show - 0{} [1080p]", i + 1),
                seeders: 50,
                size: 150_000,
                info_hash: hashes[i].clone(),
                trusted: true,
                remake: false,
            };
            (i as u32 + 1, item)
        })
        .collect();
    h.app.page = Page::Files(FilesPage::episodes(
        items,
        "Show · Group".into(),
        "Show [Group]".into(),
        None,
        "Group · per episode".into(),
    ));
    wait_until(&mut h, "lookups", |g| {
        hashes.iter().all(|x| g.app.usenet.links(x).is_some())
    });
    save(&h, &out, "episodes-1-mixed");
    assert!(shows(
        &h,
        "On Usenet (AnimeTosho): 2 of 3 episodes download from your Usenet providers"
    ));
    assert!(shows(&h, "↓  Download: 2 from Usenet, 1 via TorBox"));

    // Just the two on Usenet: all from there.
    if let Page::Files(fp) = &mut h.app.page {
        fp.checked = vec![true, true, false];
    }
    save(&h, &out, "episodes-2-usenet");
    click(&mut h, "↓  Download from Usenet");
    wait_until(&mut h, "both done", |g| {
        let v = g.app.usenet.manager.views();
        v.len() == 2 && v.iter().all(|v| v.live.phase == Phase::Done)
    });
    let folder = base.join("downloads").join("Show [Group]");
    for i in 0..2 {
        assert!(
            std::fs::read(folder.join(names[i])).unwrap() == data[i],
            "episode {}",
            i + 1
        );
    }
    assert!(
        h.app.debrid.manager.views().is_empty(),
        "nothing went to TorBox"
    );
}

/// A film: its release is one file with no episode number in its name
/// (SeaDex's pick for Sword Art Online Progressive: Scherzo of Deep Night).
/// It's the film's one episode: it can be downloaded, and once it's in the
/// series' folder it plays.
#[test]
#[ignore = "renders screenshots"]
fn downloads_and_plays_a_film() {
    let _net = crate::mock_http::mocked();
    use crate::mock_http::{Resp, mock, reply};
    use serde_json::json;

    let film = "Sword.Art.Online.Progressive.Scherzo.of.Deep.Night.2022.1080p.BluRay.Opus5.1.x265-Legion.mkv";
    let torrent = crate::debrid::jobs::tests::make_torrent(
        "Sword.Art.Online.Progressive.Scherzo.of.Deep.Night.2022.1080p.BluRay.Opus5.1.x265-Legion",
        &[(film, 240_000)],
    );
    let hash = crate::engine::info_hash_of(&torrent).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(mock(move |req| {
        let path = req.target.split('?').next().unwrap();
        match path {
            "/download/1892585.torrent" => Resp::binary(200, torrent.clone()),
            p if p.starts_with("/kitsu") => reply(200, json!({"data": [], "included": []})),
            _ => reply(
                200,
                json!({"data": {"Media": {"status": "FINISHED", "format": "MOVIE",
                    "episodes": 1, "duration": 100, "genres": [], "studios": {"nodes": []},
                    "startDate": {}, "endDate": {}}}}),
            ),
        }
    }));
    let _details = DetailsMocked::at(&server);
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _nyaa = ResetNyaa;

    let (base, out) = dir("film-screens");
    let base = base.path();
    let mut h = app_harness(base);
    wait_until(&mut h, "engine", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
    });
    let release = crate::seadex::Release {
        group: "Legion".into(),
        is_best: true,
        dual_audio: true,
        tracker: "Nyaa".into(),
        url: "https://nyaa.si/view/1892585".into(),
        info_hash: hash.clone(),
        tags: vec![],
        files: vec![],
    };
    let mut page = anime_page(140999, 1, Some(release));
    if let Page::Anime(ap) = &mut page {
        ap.anime.format = Some("MOVIE".into());
        ap.anime.title = "Sword Art Online the Movie -Progressive- Scherzo of Deep Night".into();
        ap.anime.english = Some(ap.anime.title.clone());
    }
    h.app.page = page;
    wait_until(
        &mut h,
        "the release's file",
        |g| matches!(&g.app.page, Page::Anime(ap) if g.app.coverage(ap).has(1)),
    );
    save(&h, &out, "film-1-page");
    assert!(shows(&h, "↓  Download 1 missing"));

    // Downloading it adds it to the library, into its own folder.
    click(&mut h, "↓  Download 1 missing");
    wait_until(&mut h, "downloading", |g| {
        g.app.engine().is_some_and(|e| e.torrents().len() == 1)
    });
    let folder = h.app.library.find(140999).unwrap().folder.clone();
    let (_, t) = h
        .app
        .engine()
        .unwrap()
        .torrents()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(t.only_files(), Some(vec![0]));
    assert_eq!(t.output_folder(), folder);
    wait_until(&mut h, "its progress", |g| {
        g.app
            .pages
            .stats
            .get(&140999)
            .is_some_and(|s| s.progress.contains_key(&1))
    });

    // Once it's there (the torrent stopped; its file put in place), it's
    // the film's episode, and plays.
    // Removed as the Downloads page removes what's picked (keeping its
    // files).
    let (id, _) = h
        .app
        .engine()
        .unwrap()
        .torrents()
        .into_iter()
        .next()
        .unwrap();
    send(
        &mut h,
        [act(Action::RemoveMany {
            items: vec![crate::app::Picked::Torrent(id)],
            delete_files: false,
            blocklist: false,
        })],
    );
    wait_until(&mut h, "removed", |g| {
        g.app.engine().is_some_and(|e| e.torrents().is_empty())
    });
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(film),
        crate::mkv::tests::with_tracks(&[(1, 1, "und", "Video", None)]),
    )
    .unwrap();
    h.app.pages.last_stats = None;
    wait_until(&mut h, "the film on disk", |g| {
        g.app
            .pages
            .stats
            .get(&140999)
            .is_some_and(|s| s.episodes.contains_key(&1))
    });
    save(&h, &out, "film-2-in-library");
    assert!(shows(&h, "▶  Play"), "the film plays");
    assert!(shows_containing(&h, "1 of 1 in your library"));
    let files = crate::app::watching::episode_files(
        h.app.library.find(140999).unwrap(),
        &Default::default(),
    );
    assert_eq!(files.get(&1), Some(&folder.join(film)));
}

/// The library on your servers (all mocks): set up Jellyfin, Sonarr and
/// Radarr in Settings; the library as they have it; a series' page with
/// what's there, what's downloading and what to search for; handing
/// Sonarr SeaDex's release; adding an anime; streaming an episode from
/// Jellyfin with its subtitles, and telling Jellyfin how far it got; and
/// back to the library on this computer, untouched.
#[test]
#[ignore = "renders screenshots; needs GStreamer and ffmpeg"]
fn library_on_your_servers() {
    let _net = crate::mock_http::mocked();
    use crate::mock_http::{Req, Resp, mock, reply};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    let (base, out) = dir("servers-screens");
    let base = base.path();
    let Some(video) = crate::gst_video::tests::typeset_sample(&base.join("sample")) else {
        return;
    };
    let video_bytes = std::fs::read(&video).unwrap();
    let ass = std::fs::read(base.join("sample").join("typeset.ass")).unwrap();
    let torrent = crate::debrid::jobs::tests::make_torrent(
        "[Group] Frieren S01",
        &[
            ("Frieren - 01.mkv", 1000),
            ("Frieren - 02.mkv", 1000),
            ("Frieren - 03.mkv", 1000),
        ],
    );
    let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
    let log = seen.clone();
    const LINK_CLICK: u64 = 126403;
    let server = rt_block(mock(move |req| {
        log.lock().unwrap().push(req.clone());
        let path = req.target.split('?').next().unwrap();
        let q = req.query();
        let ep = |id: u64, n: u32, file: u64| {
            json!({"id": id, "seasonNumber": 1, "episodeNumber": n, "hasFile": file > 0,
                   "monitored": n != 2, "episodeFileId": file})
        };
        let v: Value = match (req.method.as_str(), path) {
            ("GET", "/mapping") => json!([
                {"type": "TV", "anilist_id": FRIEREN, "tvdb_id": 424536, "season": {"tvdb": 1}},
                {"type": "TV", "anilist_id": LINK_CLICK, "tvdb_id": 404070, "season": {"tvdb": 1}},
                {"type": "MOVIE", "anilist_id": 112151, "themoviedb_id": {"movie": [635302]}}]),
            ("GET", "/sonarr/api/v3/system/status") => {
                json!({"appName": "Sonarr", "version": "4.0.15"})
            }
            ("GET", "/radarr/api/v3/system/status") => {
                json!({"appName": "Radarr", "version": "5.26.2"})
            }
            ("GET", "/sonarr/api/v3/qualityprofile" | "/radarr/api/v3/qualityprofile") => {
                json!([{"id": 4, "name": "HD-1080p"}, {"id": 6, "name": "Remux"}])
            }
            ("GET", "/sonarr/api/v3/rootfolder") => {
                json!([{"path": "/tv/anime", "freeSpace": 4_000_000_000_000_u64}])
            }
            ("GET", "/radarr/api/v3/rootfolder") => {
                json!([{"path": "/films/anime", "freeSpace": 4_000_000_000_000_u64}])
            }
            ("GET", "/sonarr/api/v3/series") => {
                json!([{"id": 1, "title": "Frieren: Beyond Journey's End",
                "tvdbId": 424536, "titleSlug": "frieren", "path": "/tv/anime/Frieren", "monitored": true,
                "seasons": [{"seasonNumber": 1, "monitored": true}]}])
            }
            ("GET", "/sonarr/api/v3/episode") => {
                json!([ep(11, 1, 100), ep(12, 2, 0), ep(13, 3, 0)])
            }
            ("GET", "/sonarr/api/v3/episodefile") => json!([{"id": 100,
                "relativePath": "Season 01/Frieren - S01E01.mkv", "size": video_bytes.len()}]),
            ("GET", "/sonarr/api/v3/queue") => json!({"records": [
                {"id": 5, "seriesId": 1, "episodeId": 13, "size": 100.0, "sizeleft": 40.0,
                 "title": "[Group] Frieren - 03 (1080p)", "status": "downloading",
                 "trackedDownloadStatus": "ok", "trackedDownloadState": "downloading",
                 "timeleft": "00:02:10", "downloadClient": "qBittorrent", "protocol": "torrent",
                 "quality": {"quality": {"name": "WEBDL-1080p"}},
                 "episode": {"seasonNumber": 1, "episodeNumber": 3, "title": "Killing Magic"}},
                {"id": 6, "seriesId": 99, "episodeId": 990, "size": 50.0, "sizeleft": 0.0,
                 "title": "Other.Show.S01E01.1080p", "status": "completed",
                 "trackedDownloadStatus": "warning", "trackedDownloadState": "importPending",
                 "series": {"title": "Other Show"},
                 "episode": {"seasonNumber": 1, "episodeNumber": 1},
                 "statusMessages": [{"title": "Other.Show.S01E01.1080p.mkv",
                    "messages": ["Not an upgrade for existing episode file(s)"]}]}]}),
            ("DELETE", "/sonarr/api/v3/queue/5") => json!({}),
            ("DELETE", "/sonarr/api/v3/queue/bulk") => json!({}),
            ("GET", "/radarr/api/v3/movie") => json!([]),
            ("GET", "/radarr/api/v3/queue") => json!({"records": []}),
            ("PUT", "/sonarr/api/v3/episode/monitor") => json!([]),
            ("POST", "/sonarr/api/v3/command") => json!({"id": 1}),
            ("POST", "/sonarr/api/v3/release/push") => {
                json!([{"approved": true, "rejections": []}])
            }
            ("GET", "/sonarr/api/v3/series/lookup") => {
                json!([{"title": "Link Click", "tvdbId": 404070,
                "seasons": [{"seasonNumber": 1}, {"seasonNumber": 2}]}])
            }
            ("POST", "/sonarr/api/v3/series") => json!({"id": 2}),
            ("POST", "/jf/Users/AuthenticateByName") => {
                json!({"AccessToken": "jtok", "User": {"Id": "u1"}})
            }
            ("GET", "/jf/System/Info/Public") => json!({"ServerName": "nas"}),
            ("GET", "/jf/Items") => json!({"Items": [{"Id": "js1", "Type": "Series",
                "ProviderIds": {"Tvdb": "424536"}, "Path": "/media/anime/Frieren"}]}),
            ("GET", "/jf/Shows/js1/Episodes") => json!({"Items": [{"Id": "je1",
                "ParentIndexNumber": 1, "IndexNumber": 1,
                "Path": "/media/anime/Frieren/Season 01/Frieren - S01E01.mkv", "RunTimeTicks": 50_000_000,
                "UserData": {"Played": false, "PlaybackPositionTicks": 0}}]}),
            ("GET", "/jf/Items/je1") => json!({"RunTimeTicks": 50_000_000,
                "Chapters": [{"StartPositionTicks": 0, "Name": "Opening"},
                             {"StartPositionTicks": 20_000_000, "Name": "Part A"}],
                "MediaSources": [{"Id": "src1", "MediaStreams": [
                    {"Index": 0, "Type": "Video", "Codec": "h264"},
                    {"Index": 1, "Type": "Audio", "Codec": "flac", "Language": "jpn", "Channels": 1,
                     "IsDefault": true},
                    {"Index": 2, "Type": "Subtitle", "Codec": "ass", "Language": "eng",
                     "Title": "Full Subtitles", "IsDefault": true}]}]}),
            ("GET", "/jf/Videos/je1/stream") => return Resp::binary(200, video_bytes.clone()),
            ("GET", "/jf/Videos/je1/src1/Subtitles/2/Stream.ass") => {
                return Resp::binary(200, ass.clone());
            }
            ("POST", "/jf/UserItems/je1/UserData") => json!({}),
            ("GET", "/download/777.torrent") => return Resp::binary(200, torrent.clone()),
            (_, "/seadex") => {
                if q.get("filter")
                    .is_some_and(|f| f.contains(&FRIEREN.to_string()))
                {
                    json!({"items": [{"alID": FRIEREN, "notes": "", "expand": {"trs": [{
                        "releaseGroup": "Group", "isBest": true, "dualAudio": false, "tracker": "Nyaa",
                        "url": "https://nyaa.si/view/777", "infoHash": "a".repeat(40),
                        "files": [{"length": 3000}]}]}}]})
                } else {
                    json!({"items": []})
                }
            }
            (_, p) if p.starts_with("/kitsu") => json!({"data": [], "included": []}),
            ("POST", "/anilist") => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                if let Some(ids) = body["variables"]["ids"].as_array() {
                    json!({"data": {"Page": {"media": ids.iter().map(|id| json!({"id": id,
                        "title": {"english": if *id == FRIEREN { "Frieren: Beyond Journey’s End" } else { "Link Click" },
                                  "romaji": "Sousou no Frieren"},
                        "format": "TV", "episodes": 28, "seasonYear": 2023})).collect::<Vec<_>>()}}})
                } else {
                    json!({"data": {"Media": {"status": "FINISHED", "format": "TV", "episodes": 28,
                        "genres": ["Fantasy"], "studios": {"nodes": []}, "startDate": {}, "endDate": {}}}})
                }
            }
            other => panic!("unexpected {other:?}"),
        };
        reply(200, v)
    }));
    *crate::servers::mapping::BASE.lock().unwrap() = Some(format!("{server}/mapping"));
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _details = DetailsMocked::at(&server);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *crate::servers::mapping::BASE.lock().unwrap() = None;
            *crate::seadex::SEADEX_BASE.lock().unwrap() = None;
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }
    let _reset = Reset;
    let sent = |method: &str, path: &str| -> Vec<Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.target.split('?').next() == Some(path))
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect()
    };

    // A series in the library on this computer, which stays as it was.
    let mut h = app_harness(base);
    let local = crate::library::NewSeries {
        anilist_id: 1,
        english: Some("On this computer".into()),
        ..Default::default()
    };
    h.app.library.add(&local, &base.join("downloads"));

    // Settings: the servers.
    click(&mut h, "Settings");
    click(&mut h, "Jellyfin, Sonarr and Radarr");
    assert!(h.app.servers_mode());
    assert!(h.app.library.series.is_empty(), "the servers' own library");
    type_into(&mut h, "sonarr-url", &format!("{server}/sonarr"), false);
    type_into(&mut h, "sonarr-key", "skey", true);
    type_into(&mut h, "radarr-url", &format!("{server}/radarr/"), false);
    type_into(&mut h, "radarr-key", "rkey", true);
    wait_until(&mut h, "Sonarr and Radarr checked", |g| {
        g.app.servers.config.sonarr.root_folder.is_some()
            && g.app.servers.config.radarr.root_folder.is_some()
    });
    assert!(shows(&h, "Sonarr 4.0.15") && shows(&h, "Radarr 5.26.2"));
    assert_eq!(
        h.app.servers.config.sonarr.quality_profile,
        Some(4),
        "the first, until changed"
    );
    type_into(&mut h, "jellyfin-url", &format!("{server}/jf"), false);
    type_into(&mut h, "jellyfin-user", "me", false);
    type_into(&mut h, "jellyfin-password", "secret", true);
    wait_until(&mut h, "signed in to Jellyfin", |g| {
        g.app.servers.config.jellyfin.signed_in()
    });
    assert!(shows(&h, "Signed in as me on nas"));
    assert!(h.app.servers.password.is_empty(), "the password isn't kept");
    let saved = std::fs::read_to_string(base.join("state").join("servers.json")).unwrap();
    assert!(!saved.contains("secret"));
    save(&h, &out, "1-settings");

    // The library, as the servers have it.
    click(&mut h, "Library");
    wait_until(&mut h, "the servers read", |g| {
        g.app
            .library
            .find(FRIEREN)
            .is_some_and(|s| s.remote.is_some())
            && !g.app.pages.stats.is_empty()
    });
    assert!(shows(&h, "Frieren: Beyond Journey’s End"));
    assert!(shows_containing(&h, "Sonarr · Radarr · Jellyfin"));
    save(&h, &out, "2-library");

    // Its page: episode 1 plays from Jellyfin; 2 is missing; 3 downloads.
    click(&mut h, "Frieren: Beyond Journey’s End");
    wait_until(&mut h, "its release read", has_release);
    assert!(shows(&h, "▶ Play"));
    assert!(shows(&h, "60%"), "Sonarr's download of episode 3");
    assert!(shows(&h, "On Sonarr"));
    save(&h, &out, "3-series");
    click_nth(&mut h, "⌕ Search", 0);
    wait_until(&mut h, "Sonarr searching", |g| {
        g.app
            .status
            .as_ref()
            .is_some_and(|s| s.text.contains("searching"))
    });
    assert_eq!(
        sent("POST", "/sonarr/api/v3/command"),
        [json!({"name": "EpisodeSearch", "episodeIds": [12]})]
    );
    assert_eq!(
        sent("PUT", "/sonarr/api/v3/episode/monitor"),
        [json!({"episodeIds": [12], "monitored": true})],
        "watched for, so it's looked for"
    );
    click(&mut h, "↓  Send the release to Sonarr");
    wait_until(&mut h, "Sonarr downloading", |g| {
        g.app
            .status
            .as_ref()
            .is_some_and(|s| s.text == "Sonarr is downloading it")
    });
    let pushed = &sent("POST", "/sonarr/api/v3/release/push")[0];
    assert_eq!(pushed["title"], "[Group] Frieren S01");
    assert_eq!(
        pushed["downloadUrl"],
        format!("{server}/download/777.torrent")
    );

    // Downloads: Sonarr's queue, and Radarr's (empty).
    click(&mut h, "Downloads");
    wait(&mut h, "the queue", |g| {
        shows_containing(g, "Killing Magic")
    });
    assert!(shows_containing(&h, "Episode 3"));
    assert!(shows_containing(&h, "60% · Downloading · 3 min left"));
    assert!(shows_containing(&h, "WEBDL-1080p · qBittorrent (torrent)"));
    assert!(
        shows(&h, "Other Show"),
        "not in the library: as Sonarr calls it"
    );
    assert!(shows(&h, "Not an upgrade for existing episode file(s)"));
    assert!(shows(&h, "Nothing in Radarr's queue."));
    save(&h, &out, "3b-downloads");
    click_nth(&mut h, "Remove…", 0);
    assert!(shows(&h, "Remove from Sonarr's queue?"));
    save(&h, &out, "3c-remove");
    click(&mut h, "Remove and blocklist");
    wait_until(&mut h, "removed", |g| {
        g.app
            .status
            .as_ref()
            .is_some_and(|s| s.text.contains("blocklisted"))
    });
    let removed: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.target.clone())
        .collect();
    assert_eq!(
        removed,
        ["/sonarr/api/v3/queue/5?removeFromClient=true&blocklist=true"]
    );
    // Several at once: picked with their boxes (here, all of them).
    click(&mut h, "Select all");
    assert!(shows(&h, "2 selected"));
    save(&h, &out, "3d-picked");
    click(&mut h, "Remove selected…");
    assert!(shows(&h, "Remove 2 downloads from the queue?"));
    click(&mut h, "Remove");
    wait_until(&mut h, "removed both", |g| {
        g.app
            .status
            .as_ref()
            .is_some_and(|s| s.text == "Removed 2 from Sonarr's queue")
    });
    let bulk: Vec<Value> = sent("DELETE", "/sonarr/api/v3/queue/bulk");
    assert_eq!(bulk, [json!({"ids": [5, 6]})]);
    assert!(h.picked.is_empty(), "nothing left picked");
    click(&mut h, "Library");
    click(&mut h, "Frieren: Beyond Journey’s End");
    wait_until(&mut h, "its page again", has_release);

    // Streaming episode 1 from Jellyfin (the simulator doesn't run tasks:
    // what Jellyfin says about the file is read here).
    let jelly = h.app.jellyfin().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let playable = rt.block_on(jelly.playable("je1")).unwrap();
    let _ = h.open_stream(FRIEREN, 1, playable.clone());
    assert!(
        h.player.is_some(),
        "{:?}",
        h.app.status.as_ref().map(|s| &s.text)
    );
    let subs = rt.block_on(jelly.text_subtitles(&playable));
    h.player.as_mut().unwrap().subs_loaded(Arc::new(subs));
    let p = h.player.as_ref().unwrap();
    let labels: Vec<String> = p.tracks.iter().map(crate::player::Track::label).collect();
    assert_eq!(labels, ["Japanese · FLAC 1.0", "English · Full Subtitles"]);
    assert_eq!((p.audio, p.subs), (Some(0), Some(0)), "your languages");
    wait(&mut h, "playing", |g| {
        g.player.as_ref().unwrap().position() > 0.5
    });
    send(&mut h, [Message::Player(player::Msg::Frame)]);
    assert!(
        h.player.as_ref().unwrap().overlay.is_some(),
        "Jellyfin's subtitles, by libass"
    );
    save(&h, &out, "4-streaming");
    send(&mut h, [Message::Player(player::Msg::TogglePause)]);
    wait(&mut h, "paused", |g| g.player.as_ref().unwrap().paused());
    send(&mut h, [Message::Player(player::Msg::Back)]);
    wait_until(&mut h, "Jellyfin told", |_| {
        !sent("POST", "/jf/UserItems/je1/UserData").is_empty()
    });
    let told = &sent("POST", "/jf/UserItems/je1/UserData")[0];
    assert!(
        told["PlaybackPositionTicks"].as_i64().unwrap() > 5_000_000,
        "{told}"
    );

    // An anime not on the servers yet.
    let link_click = crate::browse::Media {
        id: LINK_CLICK,
        english: Some("Link Click".into()),
        format: Some("ONA".into()),
        episodes: Some(11),
        ..Default::default()
    };
    send(
        &mut h,
        [act(Action::OpenMedia(
            Box::new(link_click),
            Origin::Discover,
        ))],
    );
    wait_for(&mut h, "+  Add to Sonarr");
    assert!(shows(&h, "Not on your servers"));
    save(&h, &out, "5-not-on-servers");
    click(&mut h, "+  Add to Sonarr");
    wait_until(&mut h, "added", |g| {
        g.app
            .status
            .as_ref()
            .is_some_and(|s| s.text == "Added Link Click to Sonarr")
    });
    let added = &sent("POST", "/sonarr/api/v3/series")[0];
    assert_eq!(added["seriesType"], "anime");
    assert_eq!(added["rootFolderPath"], "/tv/anime");
    assert_eq!(added["seasons"][0]["monitored"], true);
    assert_eq!(
        added["seasons"][1]["monitored"], false,
        "just the anime's season"
    );

    // Back to the library on this computer, as it was.
    click(&mut h, "Settings");
    click(&mut h, "On this computer");
    assert!(!h.app.servers_mode());
    let titles: Vec<&str> = h.app.library.series.iter().map(|s| s.title()).collect();
    assert_eq!(titles, ["On this computer"]);
    assert!(base.join("state").join("servers-library.json").exists());
}

fn rt_block<T>(f: impl std::future::Future<Output = T>) -> T {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
        .block_on(f)
}

/// Old downloads move into the library: a finished Usenet download in the
/// old download folder, and an old remux folder named after the series.
#[test]
#[ignore = "renders screenshots"]
fn moves_old_downloads_into_the_library() {
    let _net = crate::mock_http::real();
    use crate::debrid::jobs::tests::make_torrent;
    use crate::library::NewSeries;
    use crate::mock_http::{Resp, mock};
    use crate::usenet::fake::{FakeProvider, Options, upload};
    use crate::usenet::indexers::{Indexer, NzbLink};

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("library-move-screens");
    let base = base.path();
    let episode: Vec<u8> = (0..120_000u32).map(|i| (i % 241) as u8).collect();
    let torrent = make_torrent("[Group] Show", &[("Show - 01.mkv", 120_000)]);
    let hash = crate::engine::info_hash_of(&torrent).unwrap();
    let listing = crate::engine::listing_from_bytes(torrent).unwrap();
    let up = upload("mv", &[("Show - 01.mkv", &episode)], 40_000, 8192, 4);
    let fake = rt.block_on(FakeProvider::start(up.articles.clone(), Options::default()));
    let nzb = up.nzb().into_bytes();
    let server = rt.block_on(mock(move |_| Resp::binary(200, nzb.clone())));
    let links = vec![NzbLink {
        indexer: Indexer::TsukiHime,
        url: format!("{server}/nzb/0"),
    }];
    let mut usenet = crate::usenet::Config::default();
    usenet.add(fake.provider("Fake News"));
    usenet
        .save(&base.join("state").join("usenet.json"))
        .unwrap();

    let mut h = app_harness(base);
    let downloads = base.join("downloads");
    let library = base.join("Anime");
    {
        let a = &mut h.app;
        a.settings.library_dir = library.clone();
        let new = NewSeries {
            anilist_id: 5,
            english: Some("Show".into()),
            ..Default::default()
        };
        a.library.record([hash.clone()], &new, &library);
        // An old remux, in a folder named after the series.
        std::fs::create_dir_all(downloads.join("Show")).unwrap();
        std::fs::write(
            downloads.join("Show").join("Show - 02.mkv"),
            b"remuxed episode 2",
        )
        .unwrap();
        // An old download, finishing outside the library.
        a.usenet
            .manager
            .add(&listing, &[0], downloads.join("[Group] Show"), links)
            .unwrap();
    }
    let folder = library.join("Show");
    wait_until(&mut h, "moved", |g| {
        g.app.usenet.manager.views().is_empty()
            && folder.join("Show - 01.mkv").exists()
            && folder.join("Show - 02.mkv").exists()
    });
    save(&h, &out, "library-moved");
    assert!(std::fs::read(folder.join("Show - 01.mkv")).unwrap() == episode);
    assert_eq!(
        std::fs::read(folder.join("Show - 02.mkv")).unwrap(),
        b"remuxed episode 2"
    );
    assert!(
        !downloads.join("Show").exists() && !downloads.join("[Group] Show").exists(),
        "emptied folders go"
    );
    assert!(downloads.exists(), "the downloads folder stays");
    assert!(h.app.settings.migrated);
}

/// Episodes in the library get default tracks from the language
/// preferences (the library's, then the series' own), with every track
/// kept; files that aren't Matroska are noted, not retried.
#[test]
#[ignore = "renders screenshots"]
fn library_episodes_get_default_tracks() {
    let _net = crate::mock_http::real();
    use crate::library::NewSeries;
    use crate::prefs::{LangPrefs, SubType};

    let (base, _out) = dir("library-defaults-screens");
    let base = base.path();
    let mut h = app_harness(base);
    let library = base.join("downloads");
    let folder = {
        let new = NewSeries {
            anilist_id: 9,
            english: Some("Show".into()),
            ..Default::default()
        };
        h.app.library.add(&new, &library).folder.clone()
    };
    std::fs::create_dir_all(&folder).unwrap();
    // Like a real WEB-DL: everything "default", languages partly in names.
    let tracks = [
        (1, 1, "und", "Video", None),
        (2, 2, "jpn", "Japanese", None),
        (3, 2, "und", "English", None),
        (4, 17, "und", "English (Full Subtitle)", None),
        (5, 17, "und", "English (Signs)", Some(0)),
        (6, 17, "spa", "Spanish", None),
    ];
    for ep in ["Show - 01.mkv", "Show - 02.mkv"] {
        std::fs::write(folder.join(ep), crate::mkv::tests::with_tracks(&tracks)).unwrap();
    }
    std::fs::write(folder.join("Show - 03.mp4"), b"not matroska").unwrap();
    let defaults = |name: &str| -> Vec<u64> {
        let info = crate::mkv::probe(&folder.join(name)).unwrap();
        assert_eq!(info.tracks.len(), 6, "every track kept");
        info.tracks
            .iter()
            .filter(|t| t.default)
            .map(|t| t.number)
            .collect()
    };
    let done = |g: &Gui| {
        g.app
            .library
            .find(9)
            .is_some_and(|s| g.app.pending_defaults(s).is_empty())
    };

    // The library's preferences: Japanese audio, full English subtitles.
    wait_until(&mut h, "defaults set", done);
    assert_eq!(defaults("Show - 01.mkv"), [1, 2, 4]);
    assert_eq!(defaults("Show - 02.mkv"), [1, 2, 4]);
    let applied = h.app.library.find(9).unwrap().applied.clone();
    assert!(
        applied["Show - 03.mp4"].error.is_some(),
        "not Matroska: noted"
    );

    // This series as an English dub: English audio, signs only.
    h.app.library.find_mut(9).unwrap().prefs = Some(LangPrefs {
        audio: "eng".into(),
        subtitles: Some("eng".into()),
        sub_type: SubType::Auto,
    });
    h.app.recheck_defaults();
    wait_until(&mut h, "defaults changed", done);
    assert_eq!(defaults("Show - 01.mkv"), [1, 3, 5]);
    assert_eq!(defaults("Show - 02.mkv"), [1, 3, 5]);
}

/// The library as the front page, a series' page with its AniList details
/// (AniList and SeaDex are mocks), its own languages, the library's
/// languages dialog, episodes to the playlist and to download, remuxing
/// one, and taking the series out of the library.
#[test]
#[ignore = "renders screenshots"]
fn library_and_series_pages() {
    let _net = crate::mock_http::mocked();
    use crate::library::NewSeries;
    use crate::mock_http::{mock, reply};
    use serde_json::json;

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
            *crate::seadex::SEADEX_BASE.lock().unwrap() = None;
            *crate::seadex::KITSU_BASE.lock().unwrap() = None;
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("library-pages-screens");
    let base = base.path();
    // SeaDex's pick for Frieren: a batch with episodes 1–4.
    let names = [
        "Frieren - 01.mkv",
        "Frieren - 02.mkv",
        "Frieren - 03.mkv",
        "Frieren - 04.mkv",
    ];
    let torrent =
        crate::debrid::jobs::tests::make_torrent("[Group] Frieren", &names.map(|n| (n, 1000u64)));
    let server = rt.block_on(mock(move |req| {
        if req.target.starts_with("/kitsu") {
            return reply(200, json!({"data": [], "included": []}));
        }
        if req.target == "/download/777.torrent" {
            return crate::mock_http::Resp::binary(200, torrent.clone());
        }
        if req.target.starts_with("/seadex") {
            return reply(
                200,
                json!({"items": [{"alID": 1, "notes": "", "expand": {"trs": [{
                "releaseGroup": "Group", "isBest": true, "dualAudio": false, "tracker": "Nyaa",
                "url": "https://nyaa.si/view/777", "infoHash": "a".repeat(40),
                "files": [{"length": 1000}]}]}}]}),
            );
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        let airing = body["variables"]["id"] == 1;
        reply(
            200,
            json!({"data": {"Media": {
            "title": {"native": if airing { "葬送のフリーレン" } else { "時光代理人" }},
            "description": "An elf mage outlives her party.<br><br>\n(Source: Crunchyroll)",
            "genres": ["Adventure", "Drama", "Fantasy"], "averageScore": 90,
            "status": if airing { "RELEASING" } else { "FINISHED" }, "format": "TV",
            "episodes": if airing { 28 } else { 11 },
            "duration": 24, "season": "FALL", "seasonYear": 2023, "source": "MANGA",
            "studios": {"nodes": [{"name": "Madhouse"}]},
            "startDate": {"year": 2023, "month": 9, "day": 29},
            "nextAiringEpisode": if airing { json!({"episode": 5, "airingAt": 1_800_000_000}) }
                else { json!(null) }}}}),
        )
    }));
    *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{server}/anilist"));
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::seadex::KITSU_BASE.lock().unwrap() = Some(format!("{server}/kitsu"));
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = Reset;

    let mut h = app_harness(base);
    let library = base.join("downloads");
    let tracks = [
        (1, 1, "und", "Video", None),
        (2, 2, "jpn", "Japanese", None),
        (3, 17, "eng", "English", None),
    ];
    for (id, title, eps) in [(1u64, "Frieren", 2), (2, "Link Click", 1)] {
        let new = NewSeries {
            anilist_id: id,
            english: Some(title.into()),
            format: Some("TV".into()),
            year: Some(2023),
            ..Default::default()
        };
        let folder = h.app.library.add(&new, &library).folder.clone();
        std::fs::create_dir_all(&folder).unwrap();
        for ep in 1..=eps {
            std::fs::write(
                folder.join(format!("{title} - 0{ep}.mkv")),
                crate::mkv::tests::with_tracks(&tracks),
            )
            .unwrap();
        }
    }
    wait_until(&mut h, "details", |g| {
        g.app.library.series.iter().all(|s| s.details.is_some()) && g.app.pages.stats.len() == 2
    });
    save(&h, &out, "library-1-home");
    assert!(shows_containing(&h, "2 series"));
    assert!(shows(&h, "2 of 28 episodes"));
    assert!(shows(&h, "Finished airing"));

    // The library's languages.
    click(&mut h, "Change…");
    save(&h, &out, "library-2-languages");
    assert!(
        !shows(&h, "Signs & songs only, for watching a dub"),
        "only the chosen kind's help shows"
    );
    click(&mut h, "Cancel");
    assert!(h.app.pages.languages.is_none());

    // A series' page: its card is the button.
    click(&mut h, "Frieren");
    wait_until(
        &mut h,
        "series page",
        |g| matches!(&g.app.page, Page::Anime(ap) if ap.opened_from_library() && ap.anime.entry.is_some()),
    );
    save(&h, &out, "library-3-series");
    assert!(shows(&h, "In your library"));
    assert!(shows_containing(&h, "Airing: episode 5"));
    assert!(shows(&h, "Adventure") && shows(&h, "Fantasy"));
    assert!(shows_containing(&h, "An elf mage outlives her party."));

    // Its own languages.
    click(&mut h, "Use different languages for this series");
    assert!(h.app.library.find(1).unwrap().prefs.is_some());
    save(&h, &out, "library-4-own-languages");

    // Every episode is listed: 2 of 28 here; the release has 3 and 4.
    wait_until(
        &mut h,
        "the release's files",
        |g| matches!(&g.app.page, Page::Anime(ap) if g.app.coverage(ap).has(4)),
    );
    assert!(shows_containing(&h, "2 of 28 in your library"));
    assert!(shows(&h, "↓  Download 2 missing"));
    save(&h, &out, "library-5-episodes");

    // Episodes to the playlist: one from its row, then the rest.
    assert!(h.app.playlist.items.is_empty());
    click_nth(&mut h, "+ Playlist", 0);
    let titles = |g: &Gui| {
        g.app
            .playlist
            .items
            .iter()
            .map(|i| i.title.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(&h), ["Frieren - 01"]);
    assert!(shows(&h, "In playlist"));
    assert!(shows(&h, "Added 1 episode to the playlist"));
    click(&mut h, "+ Playlist (1)");
    assert_eq!(titles(&h), ["Frieren - 01", "Frieren - 02"]);
    assert_eq!(
        h.app.playlist.items[1].path,
        library.join("Frieren").join(names[1])
    );
    assert_eq!(count(&h, "In playlist"), 2);
    assert!(shows(&h, "All in the playlist"), "nothing left to add");
    save(&h, &out, "library-5b-playlist");

    // One episode: a torrent with just that file, into the series' folder.
    click_nth(&mut h, "↓ Download", 0);
    wait_until(&mut h, "episode 3 downloading", |g| {
        g.app.engine().is_some_and(|e| e.torrents().len() == 1)
    });
    let engine = h.app.engine().unwrap();
    let (_, t) = engine.torrents().into_iter().next().unwrap();
    assert_eq!(t.only_files(), Some(vec![2]), "just episode 3");
    assert_eq!(t.output_folder(), library.join("Frieren"));
    wait_until(&mut h, "a progress bar", |g| {
        g.app
            .pages
            .stats
            .get(&1)
            .is_some_and(|s| s.progress.contains_key(&3))
    });
    save(&h, &out, "library-6-downloading");
    assert!(
        shows_containing(&h, "2 of 28 in your library"),
        "a file still downloading isn't in the library yet"
    );
    assert!(shows(&h, "0%"), "its progress instead");
    assert!(shows(&h, "Not out yet"), "episodes 5–28 haven't aired");
    assert_eq!(count(&h, "↓ Download"), 1, "just episode 4 to download");
    // The rest of what the release has joins it.
    click(&mut h, "↓  Download 1 missing");
    wait_until(&mut h, "episodes 3 and 4", |g| {
        g.app
            .engine()
            .unwrap()
            .torrents()
            .first()
            .is_some_and(|(_, t)| t.only_files() == Some(vec![2, 3]))
    });

    // Remux one episode from the library, and come back.
    click_nth(&mut h, "Remux…", 0);
    wait_until(
        &mut h,
        "remux page",
        |g| matches!(&g.app.page, Page::Remux(rp) if rp.analysis.is_some()),
    );
    let Page::Remux(rp) = &h.app.page else {
        panic!("remux page")
    };
    assert_eq!(
        rp.series.files,
        [library.join("Frieren").join("Frieren - 01.mkv")]
    );
    save(&h, &out, "library-7-remux-episode");
    click(&mut h, "← Back");
    assert!(
        matches!(&h.app.page, Page::Anime(ap) if ap.anime.anilist_id == 1),
        "back to the series"
    );

    // Out of the library, files kept.
    click(&mut h, "Remove");
    assert!(h.app.library.find(1).is_none());
    assert!(
        library.join("Frieren").join("Frieren - 01.mkv").exists(),
        "files kept"
    );
    assert!(matches!(h.app.page, Page::Home));
}

/// A download that can't finish one way is handed on: Usenet (its NZB is
/// gone) to the debrid service (which rejects the key), then to BitTorrent
/// here, which is started for it.
#[test]
#[ignore = "renders screenshots"]
fn failed_downloads_fall_back_to_torrents() {
    let _net = crate::mock_http::real();
    use crate::debrid::jobs::tests::make_torrent;
    use crate::debrid::{Config, Provider};
    use crate::mock_http::{mock, reply};
    use crate::usenet::fake::{FakeProvider, Options};
    use crate::usenet::indexers::{Indexer, NzbLink};
    use serde_json::json;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("handover-screens");
    let base = base.path();
    let torrent = make_torrent("[Group] Show S01", &[("Show - 01.mkv", 50_000)]);
    let hash = crate::engine::info_hash_of(&torrent).unwrap();
    let listing = crate::engine::listing_from_bytes(torrent).unwrap();
    let server = rt.block_on(mock(|req| match req.target.split('?').next().unwrap() {
        "/nzb/gone" => reply(404, json!({"error": "not found"})),
        _ => reply(
            403,
            json!({"success": false, "error": "BAD_TOKEN", "detail": "The token is invalid.",
                "data": null}),
        ),
    }));
    let fake = rt.block_on(FakeProvider::start(Default::default(), Options::default()));
    let mut usenet = crate::usenet::Config::default();
    usenet.add(fake.provider("Fake News"));
    usenet
        .save(&base.join("state").join("usenet.json"))
        .unwrap();
    // TorBox mode, so BitTorrent isn't running (the key is added below, for the mock).
    Config {
        enabled: true,
        provider: Provider::TorBox,
        ..Default::default()
    }
    .save(&base.join("state").join("debrid.json"))
    .unwrap();

    let mut h = app_harness(base);
    assert!(matches!(h.app.engine, EngineState::Off));
    {
        let a = &mut h.app;
        a.debrid
            .manager
            .use_base_for_test(Provider::TorBox, &server);
        a.debrid.config.keys.insert(Provider::TorBox, "key".into());
        a.debrid.manager.set_keys(a.debrid.config.keys.clone());
        let gone = vec![NzbLink {
            indexer: Indexer::AnimeTosho,
            url: format!("{server}/nzb/gone"),
        }];
        a.usenet
            .manager
            .add(&listing, &[0], base.join("downloads").join("Show"), gone)
            .unwrap();
    }
    wait_until(&mut h, "a torrent here", |g| {
        g.app.usenet.manager.views().is_empty()
            && g.app.debrid.manager.views().is_empty()
            && g.app.engine().is_some_and(|e| {
                e.torrents()
                    .iter()
                    .any(|(_, t)| t.info_hash().as_string() == hash)
            })
    });
    click(&mut h, "Downloads");
    save(&h, &out, "handover");
    let engine = h.app.engine().unwrap();
    let (_, t) = engine.torrents().into_iter().next().unwrap();
    assert_eq!(
        t.output_folder(),
        base.join("downloads").join("Show"),
        "same folder"
    );
    assert_eq!(t.only_files(), Some(vec![0]), "same files");
}

/// Your lists (mock AniList and MyAnimeList): signing in to both from
/// Settings, the entry on the library card, changing the score there and
/// the status on the series page (to both), and a failed save put back.
#[test]
#[ignore = "renders screenshots"]
fn tracks_lists_on_anilist_and_mal() {
    let _net = crate::mock_http::mocked();
    use super::library::change;
    use crate::library::NewSeries;
    use crate::mock_http::{mock, reply};
    use crate::tracking::{Change, Endpoints, Service, Status};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *crate::seadex::SEADEX_BASE.lock().unwrap() = None;
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }
    #[derive(Default)]
    struct Sites {
        anilist_saves: Vec<Value>,
        mal_patches: Vec<HashMap<String, String>>,
        fail_mal: bool,
    }
    let sites: Arc<Mutex<Sites>> = Arc::default();
    let log = sites.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("tracking-screens");
    let base = base.path();
    let server = rt.block_on(mock(move |req| {
        if let Some(r) = details_reply(req) {
            return r;
        }
        let path = req.target.split('?').next().unwrap().to_owned();
        let auth = req
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_default();
        match (req.method.as_str(), path.as_str()) {
            (_, p) if p.starts_with("/seadex") => reply(
                200,
                json!({"items": [{"alID": 1, "notes": "", "expand": {"trs": [{
                "releaseGroup": "Group", "isBest": true, "dualAudio": false, "tracker": "Nyaa",
                "url": "https://nyaa.si/view/777", "infoHash": "a".repeat(40),
                "files": [{"length": 1000}]}]}}]}),
            ),
            ("POST", "/graphql") => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let (q, vars) = (
                    body["query"].as_str().unwrap().to_owned(),
                    body["variables"].clone(),
                );
                if q.contains("idMal") {
                    return reply(
                        200,
                        json!({"data": {"Page": {"media": [{"id": 1, "idMal": 52991}]}}}),
                    );
                }
                assert_eq!(auth, "Bearer al-token");
                if q.contains("Viewer") {
                    reply(200, json!({"data": {"Viewer": {"id": 77, "name": "jake"}}}))
                } else if q.contains("MediaListCollection") {
                    reply(
                        200,
                        json!({"data": {"MediaListCollection": {"hasNextChunk": false, "lists": [
                        {"entries": [{"mediaId": 1, "status": "CURRENT", "progress": 2,
                            "score": 7.5}]}]}}}),
                    )
                } else {
                    log.lock().unwrap().anilist_saves.push(vars.clone());
                    let score = vars["score"].as_f64().unwrap() / 10.0;
                    reply(
                        200,
                        json!({"data": {"SaveMediaListEntry": {"mediaId": 1,
                            "status": vars["status"], "progress": vars["progress"],
                            "score": score}}}),
                    )
                }
            }
            ("POST", "/anilist/token") => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(
                    (body["code"].as_str(), body["client_secret"].as_str()),
                    (Some("c0de"), Some("s3cret"))
                );
                reply(
                    200,
                    json!({"access_token": "al-token", "expires_in": 31536000}),
                )
            }
            ("POST", "/mal/token") => reply(
                200,
                json!({"access_token": "mal-token", "refresh_token": "r", "expires_in": 2678400}),
            ),
            ("GET", "/mal/api/users/@me") => reply(200, json!({"name": "jake_mal"})),
            ("GET", "/mal/api/users/@me/animelist") => reply(
                200,
                json!({"data": [{"node": {"id": 52991}, "list_status": {"status": "watching",
                    "score": 8, "num_episodes_watched": 2, "is_rewatching": false}}],
                    "paging": {}}),
            ),
            ("PATCH", "/mal/api/anime/52991/my_list_status") => {
                let f = req.form();
                let mut sites = log.lock().unwrap();
                sites.mal_patches.push(f.clone());
                if std::mem::take(&mut sites.fail_mal) {
                    return reply(500, json!({"error": "oops"}));
                }
                reply(
                    200,
                    json!({"status": f["status"], "score": f["score"].parse::<u32>().unwrap(),
                    "num_episodes_watched": f["num_watched_episodes"].parse::<u32>().unwrap(),
                    "is_rewatching": f["is_rewatching"] == "true"}),
                )
            }
            _ => reply(404, json!({})),
        }
    }));
    let _details = DetailsMocked::at(&server);
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = Reset;

    let mut h = app_harness(base);
    h.app.tracking.endpoints = Endpoints {
        anilist_auth: format!("{server}/anilist/authorize"),
        anilist_token: format!("{server}/anilist/token"),
        anilist_api: format!("{server}/graphql"),
        mal_auth: format!("{server}/mal/authorize"),
        mal_token: format!("{server}/mal/token"),
        mal_api: format!("{server}/mal/api"),
        port: 0,
    };
    let library = base.join("downloads");
    let new = NewSeries {
        anilist_id: 1,
        english: Some("Frieren".into()),
        format: Some("TV".into()),
        episodes: Some(3),
        ..Default::default()
    };
    h.app.library.add(&new, &library);
    send(&mut h, [Message::Tick]);
    assert!(!h.app.tracking.any(), "nothing to show before signing in");

    // Sign in to both, the browser played here.
    click(&mut h, "Settings");
    save(&h, &out, "tracking-1-settings");
    let sign_in_url = |h: &mut Harness| {
        wait_until(h, "the sign-in page", |g| {
            g.app
                .tracking
                .signing_in
                .as_ref()
                .is_some_and(|j| j.url.is_some())
        });
        let url = h
            .app
            .tracking
            .signing_in
            .as_ref()
            .unwrap()
            .url
            .clone()
            .unwrap();
        url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect::<HashMap<String, String>>()
    };
    h.app.tracking.config.anilist.client_id = "1234".into();
    // No secret: AniList's sign-in needs one.
    click(&mut h, "Sign in with AniList");
    assert!(h.app.tracking.signing_in.is_none());
    assert!(shows(&h, "Paste the AniList client secret too"));
    type_into(&mut h, "anilist-secret", "s3cret", false);
    assert_eq!(h.app.tracking.config.anilist.client_secret, "s3cret");
    click(&mut h, "Sign in with AniList");
    let q = sign_in_url(&mut h);
    assert_eq!(q["response_type"], "code");
    let back = format!("{}?code=c0de", q["redirect_uri"]);
    rt.spawn(async move {
        reqwest::get(&back).await.unwrap();
    });
    wait_until(&mut h, "AniList", |g| {
        g.app.tracking.signed_in(Service::AniList)
    });
    h.app.tracking.config.mal.client_id = "abc".into();
    click(&mut h, "Sign in with MyAnimeList");
    let q = sign_in_url(&mut h);
    let back = format!("{}?code=c1&state={}", q["redirect_uri"], q["state"]);
    rt.spawn(async move {
        reqwest::get(&back).await.unwrap();
    });
    wait_until(&mut h, "MyAnimeList", |g| {
        g.app.tracking.signed_in(Service::Mal)
    });
    save(&h, &out, "tracking-2-signed-in");
    assert!(shows(&h, "Signed in as jake") && shows(&h, "Signed in as jake_mal"));
    let saved = std::fs::read_to_string(base.join("state").join("tracking.json")).unwrap();
    assert!(
        saved.contains("al-token") && saved.contains("mal-token"),
        "kept for next time"
    );

    // The library card: AniList's entry (watching, 7.5).
    let entry = |g: &Gui, s: Service| g.app.tracking.entry(s, 1).cloned();
    wait_until(&mut h, "both lists", |g| {
        entry(g, Service::AniList).is_some() && entry(g, Service::Mal).is_some()
    });
    click(&mut h, "Library");
    save(&h, &out, "tracking-3-library");
    assert!(shows(&h, "Watching"), "the card's badge");
    send(&mut h, [change(1, Change::Score(9.0))]);
    wait_until(&mut h, "saved", |g| {
        g.app.tracking.saving.is_empty() && entry(g, Service::Mal).is_some_and(|e| e.score == 9.0)
    });
    assert_eq!(entry(&h, Service::AniList).unwrap().score, 9.0);
    assert_eq!(
        sites.lock().unwrap().anilist_saves.last().unwrap()["score"],
        90
    );
    assert_eq!(
        sites.lock().unwrap().mal_patches.last().unwrap()["score"],
        "9"
    );

    // The series page: completed, on both (all 3 episodes watched).
    click(&mut h, "Frieren");
    wait_until(&mut h, "details", |g| g.app.details(1).is_some());
    send(&mut h, [change(1, Change::Status(Status::Completed))]);
    wait_until(&mut h, "completed", |g| {
        g.app.tracking.saving.is_empty()
            && entry(g, Service::Mal).is_some_and(|e| e.status == Status::Completed)
    });
    let a = entry(&h, Service::AniList).unwrap();
    assert_eq!((a.status, a.progress), (Status::Completed, 3));
    let mal_sent = sites.lock().unwrap().mal_patches.last().unwrap().clone();
    assert_eq!(
        (
            mal_sent["status"].as_str(),
            mal_sent["num_watched_episodes"].as_str()
        ),
        ("completed", "3")
    );
    assert!(shows(&h, "AniList: Completed · ★ 9 · 3/3"));
    save(&h, &out, "tracking-4-series");

    // MyAnimeList fails to save: its entry is put back, and it says so.
    sites.lock().unwrap().fail_mal = true;
    send(&mut h, [change(1, Change::Status(Status::Rewatching))]);
    wait_until(&mut h, "saved and failed", |g| {
        g.app.tracking.saving.is_empty() && g.app.tracking.errors.contains_key(&Service::Mal)
    });
    assert_eq!(
        entry(&h, Service::AniList).unwrap().status,
        Status::Rewatching
    );
    assert_eq!(
        entry(&h, Service::Mal).unwrap().status,
        Status::Completed,
        "put back"
    );
    assert!(shows(&h, "⚠ Not the same on both"));
    save(&h, &out, "tracking-5-mismatch");
}

/// Episodes finished in the player go on the lists (mock AniList and
/// MyAnimeList): plan to watch becomes watching at episode 1 (the score
/// kept), and the last episode completes it.
#[test]
#[ignore = "needs GStreamer and ffmpeg"]
fn syncs_watched_episodes_to_the_lists() {
    let _net = crate::mock_http::mocked();
    use crate::library::NewSeries;
    use crate::mock_http::{mock, reply};
    use crate::tracking::{Config, Endpoints, Service, Status};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Mutex;
    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
            *crate::seadex::SEADEX_BASE.lock().unwrap() = None;
            *crate::seadex::KITSU_BASE.lock().unwrap() = None;
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }
    #[derive(Default)]
    struct Sites {
        anilist: Vec<Value>,
        mal: Vec<HashMap<String, String>>,
    }
    let sites: Arc<Mutex<Sites>> = Arc::default();
    let log = sites.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("sync-screens");
    let base = base.path();
    let server = rt.block_on(mock(move |req| {
        let path = req.target.split('?').next().unwrap().to_owned();
        match (req.method.as_str(), path.as_str()) {
            (_, "/kitsu/mappings") => reply(200, json!({"data": [], "included": []})),
            // The show's details: two episodes.
            ("POST", "/anilist") => reply(
                200,
                json!({"data": {"Media": {"status": "FINISHED", "format": "TV", "episodes": 2,
                "genres": [], "studios": {"nodes": []}, "startDate": {}, "endDate": {}}}}),
            ),
            (_, p) if p.starts_with("/seadex") => reply(
                200,
                json!({"items": [{"alID": 9, "notes": "", "expand": {"trs": [{
                "releaseGroup": "Group", "isBest": true, "dualAudio": false, "tracker": "Nyaa",
                "url": "https://nyaa.si/view/777", "infoHash": "a".repeat(40),
                "files": [{"length": 1000}]}]}}]}),
            ),
            ("POST", "/graphql") => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let (q, vars) = (
                    body["query"].as_str().unwrap().to_owned(),
                    body["variables"].clone(),
                );
                if q.contains("idMal") {
                    reply(
                        200,
                        json!({"data": {"Page": {"media": [{"id": 9, "idMal": 909}]}}}),
                    )
                } else if q.contains("MediaListCollection") {
                    reply(
                        200,
                        json!({"data": {"MediaListCollection": {"hasNextChunk": false, "lists": [
                        {"entries": [{"mediaId": 9, "status": "PLANNING", "progress": 0,
                            "score": 8}]}]}}}),
                    )
                } else {
                    log.lock().unwrap().anilist.push(vars.clone());
                    reply(
                        200,
                        json!({"data": {"SaveMediaListEntry": {"mediaId": 9,
                        "status": vars["status"], "progress": vars["progress"],
                        "score": vars["score"].as_f64().unwrap() / 10.0}}}),
                    )
                }
            }
            ("GET", "/mal/api/users/@me/animelist") => reply(
                200,
                json!({"data": [{"node": {"id": 909}, "list_status": {"status": "plan_to_watch",
                    "score": 8, "num_episodes_watched": 0, "is_rewatching": false}}],
                    "paging": {}}),
            ),
            ("PATCH", "/mal/api/anime/909/my_list_status") => {
                let f = req.form();
                log.lock().unwrap().mal.push(f.clone());
                reply(
                    200,
                    json!({"status": f["status"], "score": f["score"].parse::<u32>().unwrap(),
                    "num_episodes_watched": f["num_watched_episodes"].parse::<u32>().unwrap(),
                    "is_rewatching": false}),
                )
            }
            _ => reply(404, json!({})),
        }
    }));
    *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{server}/anilist"));
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::seadex::KITSU_BASE.lock().unwrap() = Some(format!("{server}/kitsu"));
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = Reset;

    let Some(sample) = crate::gst_video::tests::plain_sample(&base.join("sample"), 6) else {
        return;
    };
    let mut h = app_harness(base);
    // Signed in to both (the sign-in itself is tested elsewhere).
    h.app.tracking.endpoints = Endpoints {
        anilist_auth: format!("{server}/x"),
        anilist_token: format!("{server}/x"),
        anilist_api: format!("{server}/graphql"),
        mal_auth: format!("{server}/x"),
        mal_token: format!("{server}/mal/token"),
        mal_api: format!("{server}/mal/api"),
        port: 0,
    };
    let mut config = Config::default();
    config.anilist.token = Some("al".into());
    config.anilist.user = Some(crate::tracking::anilist::User {
        id: 77,
        name: "jake".into(),
    });
    config.mal.client_id = "abc".into();
    config.mal.tokens = Some(crate::tracking::mal::Tokens {
        access: "m".into(),
        refresh: "r".into(),
        expires_at: chrono::Utc::now().timestamp() + 999_999,
    });
    config.mal.user = Some("jake".into());
    h.app.tracking.config = config;
    let library = base.join("downloads");
    let new = NewSeries {
        anilist_id: 9,
        english: Some("Show".into()),
        format: Some("TV".into()),
        ..Default::default()
    };
    let folder = h.app.library.add(&new, &library).folder.clone();
    std::fs::create_dir_all(&folder).unwrap();
    for ep in ["Show - 01.mkv", "Show - 02.mkv"] {
        std::fs::copy(&sample, folder.join(ep)).unwrap();
    }
    let entry = |g: &Gui, s: Service| g.app.tracking.entry(s, 9).cloned();

    h.app.open_series(9);
    wait_until(&mut h, "both lists and the details", |g| {
        g.app.details(9).is_some()
            && entry(g, Service::AniList).is_some()
            && entry(g, Service::Mal).is_some()
    });
    click(&mut h, "▶  Play episode 1");
    let near_end = |h: &mut Harness| {
        wait(h, "playing", |g| {
            g.player
                .as_ref()
                .is_some_and(|p| p.position() > 0.2 && p.duration() > 0.0)
        });
        let p = h.player.as_mut().unwrap();
        let to = p.duration() - 0.4;
        p.seek_to(to, true);
    };

    // Episode 1 done: watching, 1 of 2, score kept; then episode 2 plays.
    near_end(&mut h);
    wait_until(&mut h, "episode 1 on both lists", |g| {
        g.app.tracking.saving.is_empty()
            && entry(g, Service::Mal).is_some_and(|e| e.progress == 1)
            && entry(g, Service::AniList).is_some_and(|e| e.progress == 1)
    });
    {
        let s = sites.lock().unwrap();
        assert_eq!(
            s.anilist.last().unwrap(),
            &json!({"media": 9, "status": "CURRENT", "score": 80, "progress": 1})
        );
        let m = s.mal.last().unwrap();
        assert_eq!(
            (
                m["status"].as_str(),
                m["score"].as_str(),
                m["num_watched_episodes"].as_str()
            ),
            ("watching", "8", "1")
        );
    }
    assert!(shows_containing(
        &h,
        "Episode 1 watched: updated AniList and MyAnimeList"
    ));
    save(&h, &out, "sync-1-episode-1");

    // Episode 2, the last: completed.
    wait_until(&mut h, "episode 2 playing", |g| {
        g.player.as_ref().is_some_and(|p| p.episode == 2)
    });
    near_end(&mut h);
    wait_until(&mut h, "completed on both", |g| {
        g.app.tracking.saving.is_empty()
            && entry(g, Service::Mal).is_some_and(|e| e.status == Status::Completed)
            && entry(g, Service::AniList).is_some_and(|e| e.status == Status::Completed)
    });
    let s = sites.lock().unwrap();
    assert_eq!(s.anilist.len(), 2, "one save per episode");
    assert_eq!(
        s.anilist[1],
        json!({"media": 9, "status": "COMPLETED", "score": 80, "progress": 2})
    );
    assert_eq!(s.mal[1]["status"], "completed");
    assert_eq!(s.mal[1]["num_watched_episodes"], "2");
}

/// `REMUX_SAMPLE_DIR=<folder of real episodes>`: the remux page for them,
/// and what it suggests for the English dub.
#[test]
#[ignore = "needs REMUX_SAMPLE_DIR; renders a screenshot"]
fn remux_page_for_real_files() {
    let _net = crate::mock_http::real();
    use crate::engine::SeriesFiles;
    use crate::library::AnimeRef;

    let Some(sample) = std::env::var_os("REMUX_SAMPLE_DIR").map(PathBuf::from) else {
        return;
    };
    let (base, out) = dir("remux-real-screens");
    let mut h = app_harness(base.path());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&sample)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let name = sample.file_name().unwrap().to_string_lossy().into_owned();
    h.app.open_remux_series(
        SeriesFiles {
            name,
            files,
            not_ready: 0,
            info_hashes: vec![],
        },
        Some(AnimeRef {
            anilist_id: 0,
            english: Some("Sample".into()),
            romaji: None,
        }),
    );
    wait_until(
        &mut h,
        "subtitle samples",
        |g| matches!(&g.app.page, Page::Remux(rp) if !rp.samples.is_empty()),
    );
    save(&h, &out, "remux-real");
    // The English dub: the signs-only track should be suggested.
    send(
        &mut h,
        [super::remux::pick_language(
            crate::remux::Kind::Audio,
            Some("eng".into()),
        )],
    );
    if let Page::Remux(rp) = &h.app.page {
        println!("suggested for the English dub: {:?}", rp.subs);
    }
    save(&h, &out, "remux-real-dub");
}

/// Against the live sites: search, open an anime, list its release's
/// files and filter them; an anime SeaDex doesn't cover (Nyaa's releases,
/// one torrent per episode); a small real download, and deleting it.
#[test]
#[ignore = "network; renders screenshots"]
fn searches_and_downloads_live() {
    let _net = crate::mock_http::real();
    use crate::engine::Source;

    let (base, out) = dir("screens");
    let base = base.path();
    let mut h = app_harness(base);
    wait_until(&mut h, "engine", |g| {
        matches!(g.app.engine, EngineState::Ready(_))
    });
    save(&h, &out, "1-home");

    // Search by typing and pressing Enter.
    click(&mut h, "Search");
    type_into(&mut h, "search", "Sousou no Frieren", true);
    wait_until(&mut h, "search results", |g| {
        !g.app.searching && !g.app.results.is_empty()
    });
    save(&h, &out, "2-results");

    // Open Frieren by clicking its card.
    let title = h
        .app
        .results
        .iter()
        .find(|a| a.anilist_id == FRIEREN)
        .expect("Frieren in results")
        .title
        .clone();
    click(&mut h, &title);
    wait_until(&mut h, "anime page", |g| {
        matches!(g.app.page, Page::Anime(_))
    });
    save(&h, &out, "3-anime");

    // Fetch the torrent and list its files.
    wait_until(
        &mut h,
        "a release chosen",
        |g| matches!(&g.app.page, Page::Anime(ap) if ap.chosen.is_some()),
    );
    click(&mut h, "Change");
    click(&mut h, "Choose files…");
    wait_until(&mut h, "file list", |g| {
        matches!(g.app.page, Page::Files(_))
    });
    save(&h, &out, "4-files");

    // Narrow down to a few episodes with the filter.
    click(&mut h, "None");
    type_into(&mut h, "filter", "S01E0", false);
    let Page::Files(fp) = &h.app.page else {
        unreachable!()
    };
    let shown = fp.visible().len();
    click(&mut h, &format!("Select {shown} shown"));
    assert!(
        matches!(&h.app.page, Page::Files(fp) if fp.checked.iter().filter(|c| **c).count() == shown)
    );
    save(&h, &out, "5-files-filtered");

    // An anime SeaDex doesn't cover: the third season of The Apothecary Diaries.
    click(&mut h, "← Back");
    click(&mut h, "Results");
    wait_until(&mut h, "results page", |g| {
        matches!(g.app.page, Page::Results)
    });
    send(&mut h, [edit(|g| g.app.query.clear())]);
    type_into(&mut h, "search", "Kusuriya no Hitorigoto", true);
    wait_until(&mut h, "search results", |g| {
        !g.app.searching && g.app.last_query == "Kusuriya no Hitorigoto"
    });
    save(&h, &out, "6-results-mixed");
    let title = h
        .app
        .results
        .iter()
        .find(|a| a.entry.as_ref().and_then(|e| e.best_nyaa(false)).is_none())
        .expect("a result without a SeaDex Nyaa release")
        .title
        .clone();
    click(&mut h, &title);
    wait_until(
        &mut h,
        "Nyaa results",
        |g| matches!(&g.app.page, Page::Anime(AnimePage { nyaa: Some(s), .. }) if s.searched && !s.loading),
    );
    save(&h, &out, "7-nyaa-fallback");
    if shows(&h, "Change") {
        click(&mut h, "Change");
    }
    click(&mut h, "Choose files…");
    wait_until(&mut h, "episode picker", |g| {
        matches!(g.app.page, Page::Files(_))
    });
    save(&h, &out, "8-nyaa-episodes");
    click(&mut h, "← Back");
    wait_until(&mut h, "anime page", |g| {
        matches!(g.app.page, Page::Anime(_))
    });

    // Something in the downloads without downloading anime: one small file
    // from the Sintel open movie torrent.
    let engine = h.app.engine().unwrap();
    let downloads = base.join("downloads");
    h._rt_handle().block_on(async {
        let bytes = reqwest::get("https://webtorrent.io/torrents/sintel.torrent")
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .to_vec();
        let listing = engine.inspect(Source::TorrentFile(bytes)).await.unwrap();
        let poster = listing
            .files
            .iter()
            .find(|f| f.path == "poster.jpg")
            .unwrap()
            .index;
        let out = listing.output_folder(&downloads);
        engine
            .download(listing.torrent_bytes, Some(vec![poster]), out)
            .await
            .unwrap();
    });
    // Finished, then stopped by the engine: nothing seeds.
    wait_until(&mut h, "download to finish and stop", |g| {
        g.app.engine().is_some_and(|e| {
            e.torrents()
                .iter()
                .all(|(_, t)| t.stats().finished && t.is_paused())
        })
    });
    click(&mut h, "Downloads");
    click_with(&mut h, "Files", || containing("Files (".into()));
    save(&h, &out, "9-downloads");

    // The delete confirmation.
    click(&mut h, "Delete…");
    assert!(h.app.confirm_delete.is_some());
    save(&h, &out, "10-delete-dialog");
    click(&mut h, "Cancel");
    assert!(h.app.confirm_delete.is_none());
    h._rt_handle().block_on(engine.stop());
}

/// Discover (AniList's lists), the calendar (your lists' anime airing this
/// week; everything when asked), and on a series page MyAnimeList's
/// episode scores and each episode's discussion on both sites. AniList
/// and MyAnimeList are mocks.
#[test]
#[ignore = "renders screenshots"]
fn discovers_schedules_and_discusses() {
    let _net = crate::mock_http::mocked();
    use crate::discuss::tests::{episode_row, topic_page};
    use crate::mock_http::{Resp, mock, reply};
    use crate::tracking::{Entry, Status};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
            *crate::seadex::SEADEX_BASE.lock().unwrap() = None;
            *crate::seadex::KITSU_BASE.lock().unwrap() = None;
            *crate::discuss::MAL_BASE.lock().unwrap() = None;
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, out) = dir("discover-screens");
    let base = base.path();
    let asked: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = asked.clone();
    let show = |id: u64, title: &str| {
        json!({"id": id, "idMal": id + 100, "title": {"english": title}, "format": "TV",
            "episodes": 12, "averageScore": 80 + id, "genres": ["Drama", "Fantasy"],
            "season": "FALL", "seasonYear": 2026})
    };
    let server = rt.block_on(mock(move |req| {
        let path = req.target.split('?').next().unwrap().to_owned();
        if path.starts_with("/kitsu") {
            return reply(200, json!({"data": [], "included": []}));
        }
        if path.starts_with("/seadex") {
            return reply(200, json!({"items": []}));
        }
        // MyAnimeList's pages.
        if path == "/anime/101/_/episode" {
            let rows = episode_row(1, "4.28", 9001, 791, "")
                + &episode_row(2, "3.90", 9002, 12, r#"<span>Filler</span>"#);
            return Resp::html(200, &format!("<table>{rows}</table>"));
        }
        if path == "/forum/" {
            let offset = req.query().get("show").cloned().unwrap_or_default();
            assert_eq!(req.query()["topicid"], "9001");
            return Resp::html(
                200,
                &topic_page(
                    "Show One Episode 1 Discussion",
                    &[(1, "malfan", "What an <b>opening</b>!"), (2, "other", "Agreed")],
                    if offset == "0" { 50 } else { 0 },
                ),
            );
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let (q, v) = (body["query"].as_str().unwrap().to_owned(), body["variables"].clone());
        log.lock().unwrap().push(v.clone());
        if q.contains("airingSchedules") {
            // On your list (watching), dropped, and not on it: Monday 20:00.
            let at = v["from"].as_i64().unwrap() + 1 + 20 * 3600;
            return reply(
                200,
                json!({"data": {"Page": {"pageInfo": {"hasNextPage": false}, "airingSchedules": [
                    {"airingAt": at, "episode": 3, "media": show(1, "Show One")},
                    {"airingAt": at + 60, "episode": 7, "media": show(2, "Dropped Show")},
                    {"airingAt": at + 120, "episode": 1, "media": show(3, "Someone Else's")}]}}}),
            );
        }
        if q.contains("media(type: ANIME") {
            return reply(
                200,
                json!({"data": {"Page": {"media": [show(1, "Show One"), show(3, "Someone Else's")]}}}),
            );
        }
        if q.contains("threads(") {
            return reply(
                200,
                json!({"data": {"Page": {"pageInfo": {"hasNextPage": false}, "threads": [
                    {"id": 600, "title": "[Spoilers] Show One - Episode 2 Discussion"},
                    {"id": 500, "title": "[Spoilers] Show One - Episode 1 Discussion"}]}}}),
            );
        }
        if q.contains("Thread(id") {
            assert_eq!(v["id"], 500);
            let page = v["page"].as_u64().unwrap();
            let comments = if page == 1 {
                json!([{"id": 11, "comment": "Great ~!the twist!~ though", "likeCount": 4,
                    "createdAt": 1_700_000_000, "user": {"name": "fan"},
                    "childComments": [{"id": 12, "comment": "Agreed!", "likeCount": 1,
                        "createdAt": 1_700_000_100, "user": {"name": "pal"}, "childComments": null}]}])
            } else {
                json!([{"id": 13, "comment": "Late to this.", "likeCount": 0,
                    "createdAt": 1_700_100_000, "user": {"name": "late"}, "childComments": null}])
            };
            return reply(
                200,
                json!({"data": {"Thread": {"id": 500, "title": "[Spoilers] Show One - Episode 1 Discussion",
                    "body": "Discuss!", "replyCount": 3, "siteUrl": "https://anilist.co/forum/thread/500",
                    "createdAt": 1_699_999_000, "likeCount": 2, "user": {"name": "op"}},
                    "Page": {"pageInfo": {"hasNextPage": page == 1}, "threadComments": comments}}}),
            );
        }
        if q.contains("Media(id") {
            return reply(
                200,
                json!({"data": {"Media": {"idMal": 101, "status": "RELEASING", "format": "TV",
                    "episodes": 2, "genres": [], "studios": {"nodes": []}, "startDate": {},
                    "endDate": {}}}}),
            );
        }
        panic!("unexpected query {q}")
    }));
    *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(server.clone());
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::seadex::KITSU_BASE.lock().unwrap() = Some(format!("{server}/kitsu"));
    *crate::discuss::MAL_BASE.lock().unwrap() = Some(server.clone());
    let _reset = Reset;

    let mut h = app_harness(base);
    let entry = |status| Entry {
        status,
        score: 0.0,
        progress: 0,
    };
    h.app.tracking.pretend_lists(HashMap::from([
        (1, entry(Status::Watching)),
        (2, entry(Status::Dropped)),
    ]));

    // Discover: AniList's trending list; next season's, of a genre.
    click(&mut h, "Discover");
    wait_for(&mut h, "Someone Else's");
    assert!(shows(&h, "Watching"), "your list's status on the card");
    save(&h, &out, "1-discover");
    click_with(&mut h, "Next season", || {
        containing("Next season · ".into())
    });
    wait(&mut h, "next season's list", |g| {
        g.app.discover.shelves.len() == 2
            && matches!(
                g.app.discover.shelves.get(&g.app.discover.filter),
                Some((crate::app::discover_ui::Loaded::Ready(_), _))
            )
    });
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["sort"] == json!(["POPULARITY_DESC"]) && v["season"].is_string()),
        "a season asked for"
    );
    // An anime from it: its page, and Back to Discover.
    click(&mut h, "Someone Else's");
    wait_until(
        &mut h,
        "its page",
        |g| matches!(&g.app.page, Page::Anime(ap) if ap.anime.anilist_id == 3 && !ap.entry_loading),
    );
    click(&mut h, "Discover");
    assert!(matches!(h.app.page, Page::Discover));
    click(&mut h, "Someone Else's");
    click(&mut h, "←");
    assert!(
        matches!(h.app.page, Page::Discover),
        "Back goes to Discover"
    );

    // The calendar: your list's anime only (not the dropped one)...
    click(&mut h, "Calendar");
    wait_for(&mut h, "Show One");
    assert!(!shows(&h, "Dropped Show") && !shows(&h, "Someone Else's"));
    save(&h, &out, "2-calendar");
    // ...or everything.
    click(&mut h, "Show full schedule");
    assert!(shows(&h, "Dropped Show") && shows(&h, "Someone Else's"));
    save(&h, &out, "3-calendar-full");
    click(&mut h, "Next week →");
    wait(&mut h, "next week", |g| g.app.calendar.weeks.len() == 2);
    click(&mut h, "← Previous week");

    // Its page, from the calendar: MyAnimeList's episode scores.
    click(&mut h, "Show One");
    wait_for(&mut h, "★ 4.28");
    assert!(shows(&h, "★ 3.90") && shows(&h, "Filler"));
    save(&h, &out, "4-series-scores");

    // Episode 1's discussion: AniList's thread, spoilers hidden...
    click_nth(&mut h, "Discuss", 0);
    wait_for(&mut h, "[Spoilers] Show One - Episode 1 Discussion");
    assert!(shows(&h, "Great") && shows(&h, "though") && !shows(&h, "the twist"));
    assert!(shows(&h, "Agreed!"), "replies under their comment");
    save(&h, &out, "5-discussion-anilist");
    click(&mut h, "Show spoiler");
    assert!(shows(&h, "the twist"));
    click(&mut h, "Load more");
    wait_for(&mut h, "Late to this.");
    // ...and MyAnimeList's topic.
    click_with(&mut h, "MyAnimeList tab", || {
        containing("MyAnimeList · 791".into())
    });
    wait_for(&mut h, "Show One Episode 1 Discussion");
    assert!(shows(&h, "malfan") && shows(&h, "What an opening!"));
    save(&h, &out, "6-discussion-mal");
    click(&mut h, "Load more");
    wait(&mut h, "page two", |g| {
        g.app.discussion.as_ref().is_some_and(|d| {
            matches!(d.threads.get(&crate::discuss::Site::Mal),
                Some(crate::app::discover_ui::Loaded::Ready(Some(t))) if t.posts.len() == 4)
        })
    });
    click(&mut h, "Close");
    assert!(h.app.discussion.is_none());
}

/// Against the live sites, with covers: Discover, the full calendar, a
/// series page with MyAnimeList's episode ratings, and an episode's
/// discussion on both sites. For looking at.
#[test]
#[ignore = "network; renders screenshots"]
fn discover_and_discuss_live() {
    let _net = crate::mock_http::real();
    let (base, out) = dir("discover-live");
    let mut h = app_harness(base.path());
    set_size(1440.0, 900.0);
    click(&mut h, "Discover");
    wait(&mut h, "the list", |g| {
        matches!(
            g.app.discover.shelves.get(&g.app.discover.filter),
            Some((crate::app::discover_ui::Loaded::Ready(_), _))
        )
    });
    fetch_images(&mut h);
    save(&h, &out, "1-discover");
    click(&mut h, "Calendar");
    click(&mut h, "Show full schedule");
    wait(&mut h, "the schedule", |g| {
        matches!(
            g.app.calendar.weeks.get(&g.app.calendar.week),
            Some((crate::app::discover_ui::Loaded::Ready(_), _))
        )
    });
    fetch_images(&mut h);
    save(&h, &out, "2-calendar");
    // Frieren (finished; 28 episodes, each discussed on both sites).
    let frieren = crate::browse::Media {
        id: FRIEREN,
        english: Some("Frieren: Beyond Journey’s End".into()),
        romaji: Some("Sousou no Frieren".into()),
        episodes: Some(28),
        ..Default::default()
    };
    send(
        &mut h,
        [act(Action::OpenMedia(Box::new(frieren), Origin::Discover))],
    );
    wait(&mut h, "episode ratings", |g| {
        g.app.episode_scores.episode(FRIEREN, 1).is_some()
    });
    set_size(1440.0, 1800.0);
    fetch_images(&mut h);
    save(&h, &out, "3-series");
    set_size(1440.0, 900.0);
    send(
        &mut h,
        [act(Action::OpenDiscussion {
            id: FRIEREN,
            episode: 28,
        })],
    );
    wait(&mut h, "both threads", |g| {
        g.app.discussion.as_ref().is_some_and(|d| {
            d.threads
                .values()
                .all(|t| !matches!(t, crate::app::discover_ui::Loaded::Loading))
        })
    });
    fetch_images(&mut h);
    snapshot_after(&h, &out.join("4-discussion-anilist"), 40);
    click_with(&mut h, "MyAnimeList tab", || {
        containing("MyAnimeList ·".into())
    });
    fetch_images(&mut h);
    snapshot_after(&h, &out.join("5-discussion-mal"), 40);
}
