//! Drives the real app offscreen against the live services and saves a PNG
//! of each screen, for checking the layout by eye:
//!
//!     LIVE_TEST_DIR=/some/scratch/dir cargo test render_screens -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;

const FRIEREN: u64 = 154587;

/// Step the UI until `done` holds, then a little longer so images and
/// layout settle.
fn wait_until(h: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Click the widget labelled `label`, stepping until it appears (popups and
/// new pages show up a frame after the click that opens them).
fn click(h: &mut Harness<'_, App>, label: &str) {
    for _ in 0..100 {
        if let Some(node) = h.query_by_label(label) {
            node.click();
            h.step();
            return;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no widget labelled {label:?}");
}

/// The release section of an anime page is folded once a release is
/// picked: open it.
fn open_release_section(h: &mut Harness<'_, App>) {
    for _ in 0..200 {
        if let Some(node) = h.query_by_label_contains("Downloads come from") {
            node.click();
            h.run_steps(3);
            return;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no release section");
}

/// The anime page has read its release's files.
fn h_has_release(a: &App) -> bool {
    matches!(&a.page, Page::Anime(ap) if matches!(a.coverage(ap), super::library_ui::Coverage::Batch { .. }))
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
        reply(200, json!({"data": {"Media": {"status": "FINISHED", "format": "TV", "episodes": 3, "genres": ["Comedy"],
            "studios": {"nodes": []}, "startDate": {}, "endDate": {}}}}))
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

fn save(h: &mut Harness<'_, App>, dir: &Path, name: &str) {
    let path = dir.join(name);
    h.render().expect("render").save(&path).expect("save png");
    println!("saved {}", path.display());
}

fn screens_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("LIVE_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(name);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// The real app, offscreen, with its state and downloads under `base`.
fn harness(base: &Path, handle: tokio::runtime::Handle) -> Harness<'static, App> {
    let state_dir = base.join("state");
    let downloads = base.join("downloads");
    Harness::builder()
        .with_size([1280.0, 800.0])
        .wgpu()
        // Like eframe's default `system_fonts`: titles can contain symbols
        // and scripts that egui's bundled fonts lack.
        .with_context_setup(|ctx| {
            ctx.add_font_provider(std::sync::Arc::new(
                egui_system_font_provider::SystemFontProvider::new(),
            ))
        })
        .build_eframe(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            let mut app = App::new(cc, handle, state_dir);
            // Never the real library or Downloads.
            app.settings.download_dir = downloads.clone();
            app.settings.library_dir = downloads;
            app
        })
}

#[test]
#[ignore = "network; renders screenshots"]
fn render_screens() {
    let _net = crate::mock_http::real();
    let base = screens_dir("screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut h = harness(&base, rt.handle().clone());

    wait_until(&mut h, "engine", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });
    save(&mut h, &base, "1-home.png");

    // Search by typing and pressing the button.
    h.get_by_role(Role::TextInput).click();
    h.step();
    h.get_by_role(Role::TextInput)
        .type_text("Sousou no Frieren");
    h.step();
    h.get_by_label("Search").click();
    wait_until(&mut h, "search results", |a| {
        !a.searching && !a.results.is_empty()
    });
    save(&mut h, &base, "2-results.png");

    // Open Frieren by clicking its card.
    let title = h
        .state()
        .results
        .iter()
        .find(|a| a.anilist_id == FRIEREN)
        .expect("Frieren in results")
        .title
        .clone();
    h.get_by_label(&title).click();
    wait_until(&mut h, "anime page", |a| matches!(a.page, Page::Anime(_)));
    save(&mut h, &base, "3-anime.png");

    // Fetch the torrent and list its files.
    open_release_section(&mut h);
    h.get_by_label("Choose files ⏵").click();
    wait_until(&mut h, "file list", |a| matches!(a.page, Page::Files(_)));
    save(&mut h, &base, "4-files.png");

    // Narrow down to a few episodes with the filter.
    h.get_by_label("None").click();
    h.step();
    let Page::Files(fp) = &mut h.state_mut().page else {
        unreachable!()
    };
    fp.filter = "S01E0".into();
    h.step();
    let Page::Files(fp) = &h.state().page else {
        unreachable!()
    };
    let shown = fp.visible().len();
    h.get_by_label(&format!("Select {shown} shown")).click();
    wait_until(
        &mut h,
        "selection",
        |a| matches!(&a.page, Page::Files(fp) if fp.checked.iter().filter(|c| **c).count() == shown),
    );
    save(&mut h, &base, "5-files-filtered.png");

    // An anime SeaDex doesn't cover: the third season of The Apothecary Diaries.
    click(&mut h, "⏴ Back");
    click(&mut h, "⏴ Results");
    wait_until(&mut h, "results page", |a| matches!(a.page, Page::Results));
    h.state_mut().query = "Kusuriya no Hitorigoto".into();
    click(&mut h, "Search");
    wait_until(&mut h, "search results", |a| {
        !a.searching && a.last_query == "Kusuriya no Hitorigoto"
    });
    save(&mut h, &base, "6-results-mixed.png");
    let title = h
        .state()
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
        |a| matches!(&a.page, Page::Anime(AnimePage { nyaa: Some(s), .. }) if s.searched && !s.loading),
    );
    save(&mut h, &base, "7-nyaa-fallback.png");
    open_release_section(&mut h);
    click(&mut h, "Choose files ⏵");
    wait_until(&mut h, "episode picker", |a| {
        matches!(a.page, Page::Files(_))
    });
    save(&mut h, &base, "8-nyaa-episodes.png");
    click(&mut h, "⏴ Back");
    wait_until(&mut h, "anime page", |a| matches!(a.page, Page::Anime(_)));

    // Put something in the downloads panel without downloading anime: one
    // small file from the Sintel open movie torrent.
    let engine = h.state().engine().unwrap();
    let downloads = base.join("downloads");
    rt.block_on(async {
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
    wait_until(&mut h, "download to finish and stop", |a| {
        a.engine().is_some_and(|e| {
            e.torrents()
                .iter()
                .all(|(_, t)| t.stats().finished && t.is_paused())
        })
    });
    click(&mut h, "Downloads");
    wait_until(&mut h, "downloads page", |a| {
        matches!(a.page, Page::Downloads)
    });
    h.get_by_label("Files").click();
    wait_until(&mut h, "expanded", |_| true);
    save(&mut h, &base, "9-downloads.png");

    // The delete confirmation.
    click(&mut h, "Remove");
    click(&mut h, "Remove and delete files…");
    wait_until(&mut h, "modal", |a| a.confirm_delete.is_some());
    save(&mut h, &base, "10-delete-modal.png");
    click(&mut h, "Cancel");
    wait_until(&mut h, "modal closed", |a| a.confirm_delete.is_none());

    rt.block_on(engine.stop());
}

#[test]
#[ignore = "needs ffmpeg and mkvmerge; renders screenshots"]
fn render_remux_page() {
    let _net = crate::mock_http::real();
    use crate::engine::SeriesFiles;
    use crate::library::AnimeRef;
    use crate::remux::Kind;

    let base = screens_dir("remux-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut h = harness(&base, rt.handle().clone());
    wait_until(&mut h, "engine", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });

    let files = crate::remux::make_test_series(&base.join("series"), 3);
    let series = SeriesFiles {
        name: "[Group] Show (1080p)".into(),
        files,
        not_ready: 1,
        info_hashes: vec![],
    };
    let anime = AnimeRef {
        anilist_id: 154587,
        english: Some("Frieren: Beyond Journey’s End".into()),
        romaji: Some("Sousou no Frieren".into()),
    };
    h.state_mut().open_remux_series(series, Some(anime));
    let remux_page = |a: &App| match &a.page {
        Page::Remux(rp) => Some((
            rp.analysis.is_some(),
            rp.audio_lang.clone(),
            rp.subs.clone(),
        )),
        _ => None,
    };
    wait_until(&mut h, "tracks", |a| {
        remux_page(a).is_some_and(|(loaded, _, _)| loaded)
    });
    wait_until(
        &mut h,
        "subtitle samples",
        |a| matches!(&a.page, Page::Remux(rp) if !rp.samples.is_empty()),
    );
    let (_, audio, subs) = remux_page(h.state()).unwrap();
    assert_eq!(audio.as_deref(), Some("jpn"));
    assert_eq!(subs.unwrap().name, "English (Full)");
    save(&mut h, &base, "1-remux-defaults.png");

    // Switching to the English dub suggests signs-only subtitles.
    h.get_all_by_role(Role::ComboBox).next().unwrap().click();
    h.run_steps(3);
    let english = h
        .query_all_by_label("English")
        .last()
        .unwrap()
        .rect()
        .center();
    h.event(egui::Event::PointerButton {
        pos: english,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.event(egui::Event::PointerButton {
        pos: english,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    wait_until(&mut h, "dub defaults", |a| {
        remux_page(a).is_some_and(|(_, l, _)| l.as_deref() == Some("eng"))
    });
    let (_, _, subs) = remux_page(h.state()).unwrap();
    assert_eq!(subs.unwrap().name, "English (Signs)");
    save(&mut h, &base, "2-remux-dub.png");

    // Back to Japanese with full subtitles, and remux.
    if let Page::Remux(rp) = &mut h.state_mut().page {
        let analysis = rp.analysis.clone().unwrap();
        rp.audio_lang = Some("jpn".into());
        rp.audio = analysis.default_candidate(Kind::Audio, "jpn", None);
        rp.subs = analysis.default_candidate(Kind::Subtitles, "eng", Some("jpn"));
    }
    click(&mut h, "Remux 3 episodes");
    wait_until(
        &mut h,
        "remux",
        |a| matches!(&a.page, Page::Remux(rp) if rp.run.as_ref().is_some_and(|r| r.finished)),
    );
    save(&mut h, &base, "3-remux-done.png");
    let out = base
        .join("downloads")
        .join("Remuxed")
        .join("Frieren: Beyond Journey’s End");
    let mut made: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    made.sort();
    assert_eq!(
        made,
        vec![
            "Frieren: Beyond Journey’s End - 01.mkv",
            "Frieren: Beyond Journey’s End - 02.mkv",
            "Frieren: Beyond Journey’s End - 03.mkv",
        ]
    );

    // Into the playlist, reorder, export.
    click(&mut h, "Add 3 to playlist");
    assert_eq!(h.state().playlist.items.len(), 3);
    click(&mut h, "Playlist (3)");
    wait_until(&mut h, "playlist page", |a| {
        matches!(a.page, Page::Playlist(_))
    });
    // Move episode 3 to the top with its "Move up" button (row 3, then row 2).
    for row in [2, 1] {
        h.query_all_by_label("⏶").nth(row).unwrap().click();
        h.run_steps(3);
    }
    let order: Vec<String> = h
        .state()
        .playlist
        .items
        .iter()
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(
        order,
        vec![
            "Frieren: Beyond Journey’s End - 03",
            "Frieren: Beyond Journey’s End - 01",
            "Frieren: Beyond Journey’s End - 02",
        ]
    );
    save(&mut h, &base, "4-playlist.png");
    click(&mut h, "Export 3 episodes");
    wait_until(&mut h, "export", |a| {
        matches!(&a.page, Page::Playlist(_))
            && a.status
                .as_ref()
                .is_some_and(|s| s.text.starts_with("Exported"))
    });
    save(&mut h, &base, "5-playlist-exported.png");

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
        vec![
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
        std::fs::read(out.join("Frieren: Beyond Journey’s End - 03.mkv")).unwrap()
    );
}

/// `REMUX_SAMPLE_DIR=<folder of real episodes>`: the remux page for them.
#[test]
#[ignore = "needs REMUX_SAMPLE_DIR; renders a screenshot"]
fn render_remux_page_for_real_files() {
    let _net = crate::mock_http::real();
    use crate::engine::SeriesFiles;
    use crate::library::AnimeRef;

    let Some(sample) = std::env::var_os("REMUX_SAMPLE_DIR").map(PathBuf::from) else {
        return;
    };
    let base = screens_dir("remux-real-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut h = harness(&base, rt.handle().clone());
    wait_until(&mut h, "engine", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });
    let mut files: Vec<PathBuf> = std::fs::read_dir(&sample)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let name = sample.file_name().unwrap().to_string_lossy().into_owned();
    let anime = AnimeRef {
        anilist_id: 0,
        english: Some("Sample".into()),
        romaji: None,
    };
    h.state_mut().open_remux_series(
        SeriesFiles {
            name,
            files,
            not_ready: 0,
            info_hashes: vec![],
        },
        Some(anime),
    );
    wait_until(
        &mut h,
        "subtitle samples",
        |a| matches!(&a.page, Page::Remux(rp) if !rp.samples.is_empty()),
    );
    save(&mut h, &base, "remux-real.png");
    // Switch to the English dub: the signs-only track should be suggested.
    if let Page::Remux(rp) = &mut h.state_mut().page {
        rp.audio_lang = Some("eng".into());
        let analysis = rp.analysis.clone().unwrap();
        rp.audio = analysis.default_candidate(crate::remux::Kind::Audio, "eng", None);
        rp.subs = analysis.default_candidate(crate::remux::Kind::Subtitles, "eng", Some("eng"));
        println!("suggested for the English dub: {:?}", rp.subs);
    }
    h.run_steps(5);
    save(&mut h, &base, "remux-real-dub.png");
}

/// The Google Drive section and folder chooser in their states (no network:
/// the states are set directly; the Drive logic has its own mock tests).
#[test]
#[ignore = "renders screenshots"]
fn render_drive_ui() {
    let _net = crate::mock_http::real();
    use super::drive_ui::{browser_for_test, signed_in_for_test};
    use crate::gdrive::{Entry, Place};
    use crate::playlist::Item;

    let base = screens_dir("drive-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut h = harness(&base, rt.handle().clone());
    wait_until(&mut h, "engine", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });

    // Not set up yet: how to get a client file.
    click(&mut h, "Playlist (0)");
    h.run_steps(5); // the bottom panel settles to its content's height
    click(&mut h, "How to get the client file");
    h.run_steps(5);
    save(&mut h, &base, "drive-1-setup.png");

    // A short window with episodes listed: the steps scroll instead of
    // running off the bottom, and Export stays on screen.
    let short = base.join("short");
    std::fs::create_dir_all(&short).unwrap();
    let episodes: Vec<Item> = (1..=4)
        .map(|n| {
            let path = short.join(format!("Show - 0{n}.mkv"));
            std::fs::write(&path, [0u8; 1024]).unwrap();
            Item {
                path,
                title: format!("Show - 0{n}"),
            }
        })
        .collect();
    h.state_mut().playlist.add(episodes);
    h.set_size(egui::vec2(1280.0, 560.0));
    h.run_steps(5);
    save(&mut h, &base, "drive-1b-short-window.png");
    let bottom = h.state().ctx.content_rect().bottom();
    let export = h.get_by_label("Export 4 episodes").rect();
    assert!(
        export.bottom() <= bottom,
        "Export button cut off: {export:?} vs window bottom {bottom}"
    );
    let last = super::drive_ui::SETUP_STEPS[6];
    h.get_by_label(last).scroll_to_me();
    h.run_steps(5);
    save(&mut h, &base, "drive-1c-short-window-scrolled.png");
    let rect = h.get_by_label(last).rect();
    assert!(
        rect.bottom() <= bottom,
        "last setup step unreachable: {rect:?} vs window bottom {bottom}"
    );

    // Wider windows: every step fits in the window.
    for (w, hgt) in [
        (1920.0, 1080.0),
        (1920.0, 560.0),
        (1600.0, 900.0),
        (2560.0, 1440.0),
    ] {
        h.set_size(egui::vec2(w, hgt));
        h.run_steps(8);
        save(&mut h, &base, &format!("drive-1d-{w}x{hgt}.png"));
        let right = h.state().ctx.content_rect().right();
        for step in super::drive_ui::SETUP_STEPS {
            let node = h.get_by_label(step);
            node.scroll_to_me();
            let rect = node.rect();
            assert!(
                rect.right() <= right,
                "{w}x{hgt}: “{step}” runs off the window: {rect:?}"
            );
        }
    }
    h.state_mut().playlist.clear();
    h.set_size(egui::vec2(1280.0, 800.0));

    // Signed in, choosing a folder.
    signed_in_for_test(&mut h.state_mut().drive, "me@example.com", None);
    browser_for_test(
        &mut h.state_mut().drive,
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
    h.run_steps(5);
    save(&mut h, &base, "drive-2-choose-folder.png");
    click(&mut h, "Upload to “Anime”");
    let folder = h.state().drive.account.folder.clone().unwrap();
    assert_eq!(
        (folder.id.as_str(), folder.path.as_str()),
        ("anime", "My Drive / Anime")
    );
    assert!(h.state().drive.browser.is_none());

    // Ready to upload an export.
    let files = base.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let mut items = Vec::new();
    for n in 1..=2 {
        let path = files.join(format!("Show - 0{n}.mkv"));
        std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();
        items.push(Item {
            path,
            title: format!("Show - 0{n}"),
        });
    }
    h.state_mut().playlist.add(items);
    if let Page::Playlist(pp) = &mut h.state_mut().page {
        pp.exported_for_test(base.join("downloads/2026-11-21 16-48-35.zip"));
    }
    h.run_steps(5);
    save(&mut h, &base, "drive-3-ready.png");
    assert!(
        h.query_by_label("⬆ Upload 2026-11-21 16-48-35.zip")
            .is_some()
    );
}

/// Uploading from the playlist page against a mock Google server; the upload
/// keeps going after leaving the page, and what arrives is the file.
#[test]
#[ignore = "renders screenshots"]
fn uploads_in_the_background() {
    let _net = crate::mock_http::real();
    use super::drive_ui::signed_in_for_test;
    use crate::gdrive::Folder;
    use crate::gdrive::tests::endpoints;
    use crate::mock_http::{Resp, mock, reply};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let base = screens_dir("drive-upload-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
        rt.block_on(mock(move |req| match (req.method.as_str(), req.target.split('?').next().unwrap()) {
            ("POST", "/token") => reply(200, json!({"access_token": "at", "expires_in": 3600})),
            ("POST", "/upload/drive/v3/files") => {
                let meta: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(meta["parents"], json!(["anime"]));
                Resp { status: 200, headers: vec![("Location".into(), session.lock().unwrap().clone())], body: Vec::new() }
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
                    return Resp { status: 308, headers: vec![("Range".into(), range)], body: Vec::new() };
                }
                reply(200, json!({"id": "f", "name": "2026-11-21 16-48-35.zip", "webViewLink": "https://drive.google.com/file/d/f/view"}))
            }
            other => panic!("unexpected {other:?}"),
        }))
    };
    *session.lock().unwrap() = format!("{server}/session");

    let mut h = harness(&base, rt.handle().clone());
    wait_until(&mut h, "engine", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });
    let folder = Folder {
        id: "anime".into(),
        path: "My Drive / Anime".into(),
    };
    signed_in_for_test(&mut h.state_mut().drive, "me@example.com", Some(folder));
    h.state_mut()
        .drive
        .use_endpoints_for_test(endpoints(&server));
    h.state_mut().drive.use_chunk_for_test(1 << 20);
    let episode = base.join("Show - 01.mkv");
    std::fs::write(&episode, [0u8; 1024]).unwrap();
    h.state_mut().playlist.add(vec![crate::playlist::Item {
        path: episode,
        title: "Show - 01".into(),
    }]);
    click(&mut h, "Playlist (1)");
    if let Page::Playlist(pp) = &mut h.state_mut().page {
        pp.exported_for_test(zip.clone());
    }
    click(&mut h, "⬆ Upload 2026-11-21 16-48-35.zip");
    wait_until(&mut h, "first chunk up", |a| {
        a.drive.upload_progress().is_some_and(|p| p > 0.3)
    });
    // After a chunk, the time left shows on the page and in the top bar.
    assert_eq!(
        h.query_all_by_label_contains(" left · ").count(),
        2,
        "time estimate shown"
    );
    save(&mut h, &base, "upload-0-time-left.png");

    // Leave the page: the upload carries on, shown in the top bar.
    click(&mut h, "⏴ Back");
    assert!(matches!(h.state().page, Page::Home));
    h.run_steps(5);
    save(&mut h, &base, "upload-1-in-background.png");
    assert!(
        h.state().drive.upload_progress().is_some(),
        "leaving the page must not stop the upload"
    );

    release.store(true, Ordering::Relaxed);
    wait_until(&mut h, "upload done", |a| {
        a.drive.upload_result_for_test().is_some()
    });
    let result = h.state().drive.upload_result_for_test().unwrap().clone();
    assert_eq!(result.map(|u| u.id), Ok("f".to_owned()));
    assert!(
        *stored.lock().unwrap() == content,
        "Drive must get exactly the zip"
    );

    click(&mut h, "Playlist (1)");
    h.run_steps(5);
    save(&mut h, &base, "upload-2-done.png");
    assert!(h.query_by_label("Open in Drive ↗").is_some());
    assert!(
        h.query_by_label("⬆ Upload 2026-11-21 16-48-35.zip")
            .is_none(),
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
    use crate::debrid::jobs::Phase;
    use crate::debrid::jobs::tests::{content, make_torrent, serve};
    use crate::debrid::{Config, Provider};
    use crate::mock_http::{mock, reply};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let base = screens_dir("debrid-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
                "/user/me" => ok(json!({"email": "me@example.com", "plan": 2, "premium_expires_at": "2027-03-01T00:00:00Z"})),
                "/torrents/createtorrent" => ok(json!({"torrent_id": 8, "hash": "h"})),
                "/torrents/mylist" => {
                    let ready = release.load(Ordering::SeqCst);
                    let fs: Vec<serde_json::Value> = files
                        .iter()
                        .enumerate()
                        .map(|(i, (p, len))| json!({"id": i, "name": format!("[Group] Show S01/{p}"), "size": len}))
                        .collect();
                    ok(json!({"id": 8, "download_state": if ready { "cached" } else { "downloading" }, "progress": 0.62,
                        "download_speed": 48_000_000, "seeds": 31, "download_finished": ready, "download_present": ready, "files": fs}))
                }
                "/torrents/requestdl" => {
                    let file = req.query()["file_id"].clone();
                    ok(json!(format!("{}/cdn/{file}", base_cell.lock().unwrap())))
                }
                p if p.starts_with("/cdn/") => serve(req, &bodies[p[5..].parse::<usize>().unwrap()]),
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
    let mut h = harness(&base, rt.handle().clone());
    h.run_steps(5);
    assert!(
        matches!(h.state().engine, EngineState::Off),
        "no BitTorrent in debrid mode"
    );
    h.state()
        .debrid
        .manager
        .use_base_for_test(Provider::TorBox, &server);

    // Set it up in Settings, which stays open through every click.
    click(&mut h, "⚙ Settings");
    click(&mut h, "A debrid service");
    click(&mut h, "TorBox");
    h.run_steps(3);
    h.get_by_role(Role::PasswordInput).click();
    h.run_steps(2);
    h.get_by_role(Role::PasswordInput).type_text("tb-key");
    h.run_steps(2);
    click(&mut h, "Check");
    wait_until(&mut h, "account check", |a| a.debrid.account.is_some());
    save(&mut h, &base, "debrid-1-settings.png");
    assert!(
        h.query_by_label("✔ me@example.com · premium until 2027-03-01")
            .is_some(),
        "settings still open"
    );
    let saved = Config::load(&config_file);
    assert_eq!(
        (saved.provider, saved.key(Provider::TorBox)),
        (Provider::TorBox, Some("tb-key"))
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(
        h.query_by_label("A debrid service").is_none(),
        "Esc closes settings"
    );
    assert!(
        h.query_by_label("TorBox · me@example.com").is_some(),
        "status bar shows the account"
    );

    // Pick episodes 1 and 3.
    let listing = crate::engine::listing_from_bytes(torrent.clone()).unwrap();
    let mut fp = FilesPage::new(listing, None, "test".into());
    fp.checked = vec![true, false, true];
    h.state_mut().page = Page::Files(fp);
    h.run_steps(5);
    save(&mut h, &base, "debrid-2-files.png");
    click(&mut h, "⬇ Download via TorBox");
    wait_until(&mut h, "TorBox fetching", |a| {
        a.debrid
            .manager
            .views()
            .first()
            .is_some_and(|v| matches!(v.live.phase, Phase::Remote { .. }))
    });
    click(&mut h, "Downloads (1)");
    h.run_steps(3);
    save(&mut h, &base, "debrid-3-service-fetching.png");
    assert!(h.query_by_label("TorBox: 62%").is_some());

    release.store(true, Ordering::SeqCst);
    wait_until(&mut h, "done", |a| {
        a.debrid
            .manager
            .views()
            .first()
            .is_some_and(|v| v.live.phase == Phase::Done)
    });
    click(&mut h, "Files");
    h.run_steps(5);
    save(&mut h, &base, "debrid-4-done.png");
    let out = base.join("downloads").join("[Group] Show S01");
    assert_eq!(
        std::fs::read_to_string(out.join("Show - 01.mkv")).unwrap(),
        bodies[0]
    );
    assert_eq!(
        std::fs::read_to_string(out.join("Show - 03.mkv")).unwrap(),
        bodies[2]
    );
    assert!(!out.join("Show - 02.mkv").exists());
    assert!(
        matches!(h.state().engine, EngineState::Off),
        "still no BitTorrent"
    );

    // Episodes… reopens the picker with the job's selection.
    click(&mut h, "Episodes…");
    h.run_steps(3);
    let Page::Files(fp) = &h.state().page else {
        panic!("files page")
    };
    assert_eq!(fp.checked, vec![true, false, true]);
    assert!(h.query_by_label("Update download").is_some());

    // Remux… opens the remux page with the downloaded episodes.
    click(&mut h, "Downloads");
    h.run_steps(3);
    click(&mut h, "Remux…");
    h.run_steps(3);
    let Page::Remux(rp) = &h.state().page else {
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

    let base = screens_dir("vpn-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
    let mut h = harness(&base, rt.handle().clone());
    let probe = std::sync::Arc::new(FakeProbe::default());
    let tunnel = |index| crate::vpn::Interface {
        tunnel: true,
        loopback: false,
        ..iface("lo", index, [127, 0, 0, 1], true)
    };
    *probe.interfaces.lock().unwrap() = vec![tunnel(1)];
    h.state_mut().use_vpn_env_for_test(Env {
        probe: probe.clone(),
        piactl: Some(piactl),
        natpmp: None,
        poll: Duration::from_millis(100),
        port_wait: Duration::from_secs(2),
    });

    // PIA is disconnected: torrents wait, and the app offers to connect.
    // PIA is disconnected: torrents wait, offline (no DHT, no trackers), and
    // the app offers to connect.
    let offline = |a: &App| matches!(a.engine, EngineState::Ready(_)) && a.engine_offline();
    wait_until(&mut h, "waiting", |a| {
        offline(a) && a.vpn.waiting.as_deref() == Some("PIA is disconnected")
    });
    assert!(
        h.state().engine().unwrap().session().get_dht().is_none(),
        "no DHT while offline"
    );
    click(&mut h, "Downloads");
    h.run_steps(3);
    save(&mut h, &base, "vpn-1-waiting.png");
    assert!(
        h.query_by_label("⏸ Torrents are waiting for the VPN")
            .is_some()
    );
    assert!(
        h.state()
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
    wait_until(&mut h, "engine on the VPN", |a| {
        matches!(a.engine, EngineState::Ready(_))
    });
    let on_vpn = Network {
        interface: Some("lo".into()),
        index: 1,
        port: Some(port),
        offline: false,
    };
    assert_eq!(h.state().engine_net, Some(on_vpn));
    assert!(listening(), "listens on the forwarded port");
    // The listening socket is bound to the interface (ss shows it as addr%device).
    if let Ok(out) = std::process::Command::new("ss").args(["-Htln"]).output() {
        let ss = String::from_utf8_lossy(&out.stdout);
        assert!(
            ss.contains(&format!("%lo:{port}")),
            "not bound to the interface:\n{ss}"
        );
    }
    assert!(
        h.query_by_label(&format!("VPN: lo · port {port}"))
            .is_some()
    );
    click(&mut h, "⚙ Settings");
    h.run_steps(5);
    save(&mut h, &base, "vpn-2-settings.png");
    assert!(
        h.query_by_label(&format!("✔ Port {port} forwarded"))
            .is_some()
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);

    // The VPN drops: the engine stops, so nothing goes out.
    pia::tests::set(&pia_dir, "connectionstate", "Reconnecting");
    probe.interfaces.lock().unwrap().clear();
    wait_until(&mut h, "offline", |a| {
        offline(a) && a.vpn.waiting.as_deref() == Some("PIA is reconnecting")
    });
    assert!(
        h.query_by_label("Connect PIA").is_none(),
        "already reconnecting"
    );
    save(&mut h, &base, "vpn-3-dropped.png");
    let deadline = Instant::now() + Duration::from_secs(10);
    while listening() {
        assert!(
            Instant::now() < deadline,
            "still listening after the VPN dropped"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(100));
    }

    // Back, as a new interface: the engine restarts on it.
    *probe.interfaces.lock().unwrap() = vec![tunnel(7)];
    pia::tests::set(&pia_dir, "connectionstate", "Connected");
    wait_until(&mut h, "restarted", |a| {
        matches!(a.engine, EngineState::Ready(_))
            && a.engine_net.as_ref().is_some_and(|n| n.index == 7)
    });
    assert!(listening());

    // VPN off: torrents go back to the normal connection.
    click(&mut h, "⚙ Settings");
    click(&mut h, "Only download torrents over a VPN");
    wait_until(&mut h, "off the VPN", |a| {
        matches!(a.engine, EngineState::Ready(_)) && a.engine_net == Some(Network::default())
    });
    assert!(!h.state().vpn.config.enabled);
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
        h.step();
        std::thread::sleep(Duration::from_millis(100));
    }

    // Downloading through a debrid service instead: torrents from before wait
    // offline, quietly, and nothing about the VPN is in the way.
    {
        let a = h.state_mut();
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
    wait_until(&mut h, "offline again", |a| {
        offline(a) && a.vpn.waiting.is_some()
    });
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    save(&mut h, &base, "vpn-4-debrid-mode.png");
    assert!(
        h.query_by_label("⏸ Torrents are waiting for the VPN")
            .is_none(),
        "no banner without local torrenting"
    );
    assert!(h.query_by_label("Torrents waiting for the VPN").is_none());
    assert!(
        h.query_by_label_contains("VPN:").is_none(),
        "no VPN status either"
    );
    assert!(
        h.state().download_ready().is_ok(),
        "the debrid service doesn't need the VPN"
    );
}

/// Downloading from Usenet: a release on SeaDex that both indexers (mocks)
/// have, a fake provider (TLS, with a login) serving its articles, Nyaa (a
/// mock) for its .torrent. The release list shows it's on Usenet, the
/// picker offers Usenet, and the picked episodes arrive intact; the
/// providers dialog checks the login.
#[test]
#[ignore = "renders screenshots"]
fn downloads_from_usenet() {
    let _net = crate::mock_http::mocked();
    use crate::debrid::jobs::tests::make_torrent;
    use crate::mock_http::{Resp, mock, reply};
    use crate::usenet::fake::{FakeProvider, Options, upload};
    use crate::usenet::indexers::Indexers;
    use serde_json::json;

    struct ResetNyaa;
    impl Drop for ResetNyaa {
        fn drop(&mut self) {
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }

    let base = screens_dir("usenet-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
    let hash = info_hash_of(&torrent).unwrap();
    let posted: Vec<(&str, &[u8])> = episodes
        .iter()
        .zip(&data)
        .map(|((n, _), d)| (*n, d.as_slice()))
        .collect();
    let up = upload("ui", &posted, 48_000, 16_384, 12);
    let login = Some(("me".to_owned(), "pw".to_owned()));
    let fake = rt.block_on(FakeProvider::start(
        up.articles.clone(),
        Options {
            login,
            tls: true,
            delay: Duration::from_millis(300),
        },
    ));

    // Nyaa and both indexers.
    let nzb = up.nzb().into_bytes();
    let base_url = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
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
                "/at/json" => reply(200, json!({"id": 1, "info_hash": hash, "status": "complete", "nzb_url": format!("{url}/at/nzb/1")})),
                "/at/nzb/1" => Resp::binary(200, nzb.clone()),
                p if p == format!("/tk/v1/torrents/btih/{hash}") => {
                    reply(200, json!({"id": 5, "btih": hash, "state": "completed", "has_nzb": 1}))
                }
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
    let mut h = harness(&base, rt.handle().clone());
    h.state_mut()
        .usenet
        .use_indexers_for_test(Indexers::with_bases(
            reqwest::Client::new(),
            &format!("{server}/at"),
            &format!("{server}/tk"),
            &format!("{server}/tks"),
        ));

    // An anime whose SeaDex pick is this release.
    let release = Release {
        group: "Group".into(),
        is_best: true,
        dual_audio: false,
        tracker: "Nyaa".into(),
        url: "https://nyaa.si/view/777".into(),
        info_hash: hash.clone(),
        tags: vec![],
        files: vec![],
    };
    let anime = Anime {
        anilist_id: 1,
        title: "Show".into(),
        alt_title: None,
        romaji: None,
        english: Some("Show".into()),
        format: Some("TV".into()),
        year: Some(2026),
        episodes: Some(3),
        cover_small: None,
        cover_large: None,
        entry: Some(seadex::Entry {
            notes: String::new(),
            incomplete: false,
            theoretical_best: String::new(),
            releases: vec![release],
        }),
    };
    h.state_mut().page = Page::Anime(AnimePage {
        anime,
        chosen: Some(0),
        nyaa: None,
        from_library: false,
        entry_loading: false,
    });
    // Every episode listed, none downloaded yet; the release (folded) says
    // it's on Usenet.
    wait_until(&mut h, "episodes and NZB lookup", |a| {
        a.usenet.links(&hash).is_some_and(|l| l.len() == 2) && h_has_release(a)
    });
    save(&mut h, &base, "usenet-1-episodes.png");
    assert_eq!(
        h.get_all_by_label("⬇ Download").count(),
        3,
        "three episodes to download"
    );
    assert!(h.query_by_label("⬇ Download series (3 missing)").is_some());
    open_release_section(&mut h);
    assert!(
        h.query_by_label("NZB").is_some(),
        "the release list says it's on Usenet"
    );
    save(&mut h, &base, "usenet-2-release.png");

    // Episode 1 from its row; then episode 3, which joins the same download.
    h.get_all_by_label("⬇ Download").next().unwrap().click();
    // No settling: episode 3 is clicked while episode 1 is still downloading.
    let deadline = Instant::now() + Duration::from_secs(30);
    while h.get_all_by_label("⬇ Download").count() != 2 {
        assert!(Instant::now() < deadline, "episode 1 never started");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.state().usenet.manager.views().len(), 1);
    let buttons: Vec<_> = h.get_all_by_label("⬇ Download").collect();
    assert_eq!(buttons.len(), 2, "episodes 2 and 3 left");
    buttons[1].click();
    h.step();
    wait_until(&mut h, "episodes 1 and 3", |a| {
        a.usenet
            .manager
            .views()
            .first()
            .is_some_and(|v| v.job.files.iter().filter(|f| f.wanted).count() == 2)
    });
    // Caught part way (no settling: it's quick): progress bars on the episodes.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !h
        .state()
        .pages
        .stats
        .get(&1)
        .is_some_and(|s| s.progress.values().any(|p| *p > 0.1))
    {
        assert!(Instant::now() < deadline, "timed out waiting for progress");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.run_steps(2);
    save(&mut h, &base, "usenet-3-progress.png");
    assert!(
        h.query_all_by_label_contains("%").count() >= 1,
        "progress bars on the episodes"
    );
    assert_eq!(
        h.get_all_by_label("⬇ Download").count(),
        1,
        "only episode 2 left to download"
    );
    // Done, it's imported: off the downloads list, in the series' folder.
    let out = base.join("downloads").join("Show");
    wait_until(&mut h, "imported", |a| {
        a.usenet.manager.views().is_empty() && out.join("Show - 03.mkv").exists()
    });
    h.run_steps(5);
    save(&mut h, &base, "usenet-4-imported.png");
    assert!(h.query_by_label("2 of 3 in your library").is_some());
    assert!(
        h.query_by_label("[Group] Show S01 is in your library")
            .is_some(),
        "status says it's imported"
    );
    assert!(std::fs::read(out.join("Show - 01.mkv")).unwrap() == data[0]);
    assert!(std::fs::read(out.join("Show - 03.mkv")).unwrap() == data[2]);
    assert!(!out.join("Show - 02.mkv").exists());
    assert!(
        h.state().engine().is_none_or(|e| e.torrents().is_empty()),
        "nothing went to BitTorrent"
    );
    let series = h.state().library.find(1).expect("added to the library");
    assert_eq!(series.folder, out);
    assert!(series.downloads.contains(&hash));

    // The providers dialog: check the login.
    click(&mut h, "⚙ Settings");
    h.run_steps(3);
    assert!(h.query_by_label("Fake News · 4 connections").is_some());
    click(&mut h, "Usenet providers…");
    click(&mut h, "Check");
    for _ in 0..200 {
        if h.query_by_label("✔ Connected and logged in").is_some() {
            break;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    save(&mut h, &base, "usenet-5-providers.png");
    assert!(h.query_by_label("✔ Connected and logged in").is_some());
    click(&mut h, "Close");
    h.run_steps(3);
    assert!(h.state().usenet.dialog.is_none());
}

/// A release with one torrent per episode, downloading through a debrid
/// service, with a Usenet provider set up: episodes on Usenet come from
/// there, and the picker says so (it used to say "via TorBox").
#[test]
#[ignore = "renders screenshots"]
fn per_episode_releases_prefer_usenet() {
    let _net = crate::mock_http::mocked();
    use crate::debrid::jobs::tests::make_torrent;
    use crate::mock_http::{Resp, mock, reply};
    use crate::usenet::fake::{FakeProvider, Options, upload};
    use crate::usenet::indexers::Indexers;
    use crate::usenet::jobs::Phase;
    use serde_json::json;

    struct ResetNyaa;
    impl Drop for ResetNyaa {
        fn drop(&mut self) {
            *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = None;
        }
    }

    let base = screens_dir("usenet-episodes-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
    let hashes: Vec<String> = torrents.iter().map(|t| info_hash_of(t).unwrap()).collect();
    let uploads: Vec<_> = (0..2)
        .map(|i| upload(&format!("ep{i}"), &[(names[i], &data[i])], 40_000, 8192, 6))
        .collect();
    let mut articles = uploads[0].articles.clone();
    articles.extend(uploads[1].articles.clone());
    let fake = rt.block_on(FakeProvider::start(articles, Options::default()));

    let nzbs: Vec<Vec<u8>> = uploads.iter().map(|u| u.nzb().into_bytes()).collect();
    let url_cell = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let server = {
        let (hashes, url_cell) = (hashes.clone(), url_cell.clone());
        rt.block_on(mock(move |req| {
            let path = req.target.split('?').next().unwrap().to_owned();
            let url = url_cell.lock().unwrap().clone();
            if let Some(id) = path.strip_prefix("/download/").and_then(|p| p.strip_suffix(".torrent")) {
                return Resp::binary(200, torrents[id.parse::<usize>().unwrap() - 901].clone());
            }
            match path.as_str() {
                "/at/json" => match hashes.iter().position(|h| *h == req.query()["btih"]) {
                    Some(i) if i < 2 => reply(200, json!({"info_hash": hashes[i], "status": "complete", "nzb_url": format!("{url}/at/nzb/{i}")})),
                    _ => reply(200, json!({})),
                },
                p if p.starts_with("/at/nzb/") => Resp::binary(200, nzbs[p[8..].parse::<usize>().unwrap()].clone()),
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
    let debrid = crate::debrid::Config {
        enabled: true,
        provider: crate::debrid::Provider::TorBox,
        ..Default::default()
    };
    debrid.save(&state.join("debrid.json")).unwrap();
    let mut usenet = crate::usenet::Config::default();
    usenet.add(fake.provider("Fake News"));
    usenet.save(&state.join("usenet.json")).unwrap();
    let mut h = harness(&base, rt.handle().clone());
    h.state_mut()
        .usenet
        .use_indexers_for_test(Indexers::with_bases(
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
    let page = FilesPage::episodes(
        items,
        "Show · Group".into(),
        "Show [Group]".into(),
        None,
        "Group · per episode".into(),
    );
    h.state_mut().page = Page::Files(page);
    wait_until(&mut h, "lookups", |a| {
        hashes.iter().all(|h| a.usenet.links(h).is_some())
    });
    save(&mut h, &base, "episodes-1-mixed.png");
    assert!(
        h.query_by_label(
            "On Usenet (AnimeTosho): 2 of 3 episodes download from your Usenet providers"
        )
        .is_some()
    );
    assert!(
        h.query_by_label("⬇ Download: 2 from Usenet, 1 via TorBox")
            .is_some()
    );

    // Just the two on Usenet: all from there.
    if let Page::Files(fp) = &mut h.state_mut().page {
        fp.checked = vec![true, true, false];
    }
    h.run_steps(3);
    save(&mut h, &base, "episodes-2-usenet.png");
    click(&mut h, "⬇ Download from Usenet");
    wait_until(&mut h, "both done", |a| {
        let v = a.usenet.manager.views();
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
        h.state().debrid.manager.views().is_empty(),
        "nothing went to TorBox"
    );
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

    let base = screens_dir("library-move-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let episode: Vec<u8> = (0..120_000u32).map(|i| (i % 241) as u8).collect();
    let torrent = make_torrent("[Group] Show", &[("Show - 01.mkv", 120_000)]);
    let hash = info_hash_of(&torrent).unwrap();
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

    let mut h = harness(&base, rt.handle().clone());
    let downloads = base.join("downloads");
    let library = base.join("Anime");
    {
        let a = h.state_mut();
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
    wait_until(&mut h, "moved", |a| {
        a.usenet.manager.views().is_empty()
            && folder.join("Show - 01.mkv").exists()
            && folder.join("Show - 02.mkv").exists()
    });
    save(&mut h, &base, "library-moved.png");
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
    assert!(h.state().settings.migrated);
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

    let base = screens_dir("library-defaults-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut h = harness(&base, rt.handle().clone());
    let library = base.join("downloads");
    let folder = {
        let a = h.state_mut();
        let new = NewSeries {
            anilist_id: 9,
            english: Some("Show".into()),
            ..Default::default()
        };
        a.library.add(&new, &library).folder.clone()
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
    let done = |a: &App| {
        a.library
            .find(9)
            .is_some_and(|s| a.pending_defaults(s).is_empty())
    };

    // The library's preferences: Japanese audio, full English subtitles.
    wait_until(&mut h, "defaults set", done);
    assert_eq!(defaults("Show - 01.mkv"), [1, 2, 4]);
    assert_eq!(defaults("Show - 02.mkv"), [1, 2, 4]);
    let applied = h.state().library.find(9).unwrap().applied.clone();
    assert!(
        applied["Show - 03.mp4"].error.is_some(),
        "not Matroska: noted"
    );

    // This series as an English dub: English audio, signs only.
    {
        let a = h.state_mut();
        a.library.find_mut(9).unwrap().prefs = Some(LangPrefs {
            audio: "eng".into(),
            subtitles: Some("eng".into()),
            sub_type: SubType::Auto,
        });
        a.recheck_defaults();
    }
    wait_until(&mut h, "defaults changed", done);
    assert_eq!(defaults("Show - 01.mkv"), [1, 3, 5]);
    assert_eq!(defaults("Show - 02.mkv"), [1, 3, 5]);
}

/// The library as the front page, a series' page with its AniList details
/// (AniList and SeaDex are mocks), its own languages, the library's
/// languages dialog, and taking a series out of the library.
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
    let base = screens_dir("library-pages-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
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
            return reply(200, json!({"items": [{"alID": 1, "notes": "", "expand": {"trs": [{
                "releaseGroup": "Group", "isBest": true, "dualAudio": false, "tracker": "Nyaa",
                "url": "https://nyaa.si/view/777", "infoHash": "a".repeat(40), "files": [{"length": 1000}]}]}}]}));
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        let airing = body["variables"]["id"] == 1;
        reply(200, json!({"data": {"Media": {
            "title": {"native": if airing { "葬送のフリーレン" } else { "時光代理人" }},
            "description": "An elf mage outlives her party.<br><br>\n(Source: Crunchyroll)",
            "genres": ["Adventure", "Drama", "Fantasy"], "averageScore": 90,
            "status": if airing { "RELEASING" } else { "FINISHED" }, "format": "TV", "episodes": if airing { 28 } else { 11 },
            "duration": 24, "season": "FALL", "seasonYear": 2023, "source": "MANGA", "studios": {"nodes": [{"name": "Madhouse"}]},
            "startDate": {"year": 2023, "month": 9, "day": 29},
            "nextAiringEpisode": if airing { json!({"episode": 5, "airingAt": 1_800_000_000}) } else { json!(null) }}}}))
    }));
    *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{server}/anilist"));
    *crate::seadex::SEADEX_BASE.lock().unwrap() = Some(format!("{server}/seadex"));
    *crate::seadex::KITSU_BASE.lock().unwrap() = Some(format!("{server}/kitsu"));
    *crate::nyaa::DOWNLOAD_BASE.lock().unwrap() = Some(server.clone());
    let _reset = Reset;

    let mut h = harness(&base, rt.handle().clone());
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
        let folder = h.state_mut().library.add(&new, &library).folder.clone();
        std::fs::create_dir_all(&folder).unwrap();
        for ep in 1..=eps {
            std::fs::write(
                folder.join(format!("{title} - 0{ep}.mkv")),
                crate::mkv::tests::with_tracks(&tracks),
            )
            .unwrap();
        }
    }
    wait_until(&mut h, "details", |a| {
        a.library.series.iter().all(|s| s.details.is_some()) && a.pages.stats.len() == 2
    });
    save(&mut h, &base, "library-1-home.png");
    assert!(h.query_by_label("2 series").is_some());
    assert!(h.query_by_label_contains("2 of 28 episodes").is_some());
    assert!(h.query_by_label("Finished airing").is_some());

    // The library's languages.
    click(&mut h, "Change…");
    h.run_steps(3);
    save(&mut h, &base, "library-2-languages.png");
    assert!(
        h.query_by_label("Signs & songs only, for watching a dub")
            .is_none(),
        "only the chosen kind's help shows"
    );
    click(&mut h, "Cancel");
    h.run_steps(3);
    assert!(h.state().pages.languages.is_none());

    // A series' page: its card is the button.
    h.get_all_by_label("Frieren").next().unwrap().click();
    h.step();
    wait_until(
        &mut h,
        "series page",
        |a| matches!(&a.page, Page::Anime(ap) if ap.from_library && ap.anime.entry.is_some()),
    );
    save(&mut h, &base, "library-3-series.png");
    assert!(h.query_by_label("In your library").is_some());
    assert!(h.query_by_label_contains("Airing: episode 5").is_some());
    assert!(h.query_by_label("Adventure · Drama · Fantasy").is_some());
    assert!(
        h.query_by_label_contains("An elf mage outlives her party.")
            .is_some()
    );
    assert!(h.query_by_label("⏴ Library").is_some());

    // Its own languages.
    click(&mut h, "Use different languages for this series");
    h.run_steps(3);
    assert!(h.state().library.find(1).unwrap().prefs.is_some());
    save(&mut h, &base, "library-4-own-languages.png");

    // Every episode is listed: 2 of 28 here; the release has 3 and 4.
    wait_until(
        &mut h,
        "the release's files",
        |a| matches!(&a.page, Page::Anime(ap) if a.coverage(ap).has(4)),
    );
    assert!(h.query_by_label("2 of 28 in your library").is_some());
    assert!(h.query_by_label("⬇ Download series (2 missing)").is_some());
    save(&mut h, &base, "library-5-episodes.png");

    // Episodes to the playlist: one from its row, then the rest.
    assert!(h.state().playlist.items.is_empty());
    h.get_all_by_label("+ Playlist").next().unwrap().click();
    h.run_steps(3);
    let titles = |a: &App| {
        a.playlist
            .items
            .iter()
            .map(|i| i.title.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(h.state()), ["Frieren - 01"]);
    assert!(h.query_by_label("✔ In playlist").is_some());
    click(&mut h, "+ Playlist (1)");
    assert_eq!(titles(h.state()), ["Frieren - 01", "Frieren - 02"]);
    assert_eq!(
        h.state().playlist.items[1].path,
        library.join("Frieren").join(names[1])
    );
    h.run_steps(3);
    assert_eq!(h.get_all_by_label("✔ In playlist").count(), 2);
    assert!(
        h.get_by_label("+ Playlist").accesskit_node().is_disabled(),
        "nothing left to add"
    );
    assert!(
        h.query_by_label("Added 1 episode to the playlist")
            .is_some()
    );
    save(&mut h, &base, "library-5b-playlist.png");

    // One episode: a torrent with just that file, into the series' folder.
    h.get_all_by_label("⬇ Download")
        .find(|b| !b.accesskit_node().is_disabled())
        .unwrap()
        .click();
    h.step();
    wait_until(&mut h, "episode 3 downloading", |a| {
        a.engine().is_some_and(|e| e.torrents().len() == 1)
    });
    let engine = h.state().engine().unwrap();
    let (_, t) = engine.torrents().into_iter().next().unwrap();
    assert_eq!(t.only_files(), Some(vec![2]), "just episode 3");
    assert_eq!(t.output_folder(), library.join("Frieren"));
    wait_until(&mut h, "a progress bar", |a| {
        a.pages
            .stats
            .get(&1)
            .is_some_and(|s| s.progress.contains_key(&3))
    });
    save(&mut h, &base, "library-6-downloading.png");
    assert!(
        h.query_by_label("2 of 28 in your library").is_some(),
        "a file still downloading isn't in the library yet"
    );
    assert!(h.query_by_label("0%").is_some(), "its progress instead");
    // Episode 5 on haven't aired (the rows on screen, that is).
    assert!(
        h.query_all_by_label("Not out yet").count() >= 1,
        "episodes 5–28 haven't aired"
    );
    assert_eq!(
        h.get_all_by_label("⬇ Download").count(),
        1,
        "just episode 4 to download"
    );
    // The rest of what the release has joins it.
    click(&mut h, "⬇ Download series (1 missing)");
    wait_until(&mut h, "episodes 3 and 4", |a| {
        a.engine()
            .unwrap()
            .torrents()
            .first()
            .is_some_and(|(_, t)| t.only_files() == Some(vec![2, 3]))
    });

    // Remux one episode from the library, and come back.
    h.get_all_by_label("Remux…").next().unwrap().click();
    h.step();
    wait_until(
        &mut h,
        "remux page",
        |a| matches!(&a.page, Page::Remux(rp) if rp.analysis.is_some()),
    );
    let Page::Remux(rp) = &h.state().page else {
        panic!("remux page")
    };
    assert_eq!(
        rp.series.files,
        [library.join("Frieren").join("Frieren - 01.mkv")]
    );
    save(&mut h, &base, "library-5-remux-episode.png");
    click(&mut h, "⏴ Back");
    h.run_steps(3);
    assert!(
        matches!(&h.state().page, Page::Anime(ap) if ap.anime.anilist_id == 1),
        "back to the series"
    );

    // Out of the library, files kept.
    click(&mut h, "Remove");
    click(&mut h, "Remove from library (keep files)");
    h.run_steps(3);
    assert!(h.state().library.find(1).is_none());
    assert!(
        library.join("Frieren").join("Frieren - 01.mkv").exists(),
        "files kept"
    );
    assert!(matches!(h.state().page, Page::Home));
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
    use crate::mock_http::mock;
    use crate::mock_http::reply;
    use crate::usenet::fake::{FakeProvider, Options};
    use crate::usenet::indexers::{Indexer, NzbLink};
    use serde_json::json;

    let base = screens_dir("handover-screens");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let torrent = make_torrent("[Group] Show S01", &[("Show - 01.mkv", 50_000)]);
    let hash = info_hash_of(&torrent).unwrap();
    let listing = crate::engine::listing_from_bytes(torrent).unwrap();
    let server = rt.block_on(mock(|req| match req.target.split('?').next().unwrap() {
        "/nzb/gone" => reply(404, json!({"error": "not found"})),
        _ => reply(403, json!({"success": false, "error": "BAD_TOKEN", "detail": "The token is invalid.", "data": null})),
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

    let mut h = harness(&base, rt.handle().clone());
    h.run_steps(3);
    assert!(matches!(h.state().engine, EngineState::Off));
    {
        let a = h.state_mut();
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
    wait_until(&mut h, "a torrent here", |a| {
        a.usenet.manager.views().is_empty()
            && a.debrid.manager.views().is_empty()
            && a.engine().is_some_and(|e| {
                e.torrents()
                    .iter()
                    .any(|(_, t)| t.info_hash().as_string() == hash)
            })
    });
    save(&mut h, &base, "handover.png");
    let engine = h.state().engine().unwrap();
    let (_, t) = engine.torrents().into_iter().next().unwrap();
    assert_eq!(
        t.output_folder(),
        base.join("downloads").join("Show"),
        "same folder"
    );
    assert_eq!(t.only_files(), Some(vec![0]), "same files");
}
