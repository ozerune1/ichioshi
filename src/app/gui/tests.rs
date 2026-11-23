//! The GUI, driven like a person would: clicking what's drawn, typing,
//! waiting for background work; and screenshots of each page.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Theme;
use iced_test::Simulator;
use iced_test::selector::{Bounded, Candidate, Selector, Text};

use super::*;
use crate::library::{Library, NewSeries};

pub(super) struct Harness {
    pub gui: Gui,
    /// Runs the app's background work (dropped last).
    _rt: tokio::runtime::Runtime,
}

impl Harness {
    /// The runtime the app's background work runs on.
    pub(super) fn _rt_handle(&self) -> tokio::runtime::Handle {
        self._rt.handle().clone()
    }
}

impl std::ops::Deref for Harness {
    type Target = Gui;
    fn deref(&self) -> &Gui {
        &self.gui
    }
}

impl std::ops::DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Gui {
        &mut self.gui
    }
}

/// The GUI on `state_dir`, with the library and downloads under `base`
/// (never the real ones).
pub(super) fn harness(base: &Path, state_dir: &Path) -> Harness {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let wake = Arc::new(Notify::new());
    let waker = {
        let wake = wake.clone();
        Waker::new(move || wake.notify_one(), |_| {})
    };
    let mut app = App::create(rt.handle().clone(), state_dir.to_owned(), waker);
    app.settings.library_dir = base.join("library");
    app.settings.download_dir = base.join("downloads");
    let mut gui = Gui::with(app, wake);
    gui.duties();
    Harness { gui, _rt: rt }
}

fn settings() -> iced::Settings {
    iced::Settings {
        default_font: INTER,
        ..Default::default()
    }
}

thread_local! {
    /// The window's size in this test (end-to-end tests use a tall one, so
    /// whole pages are on screen to click).
    static SIZE: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((1440.0, 900.0)) };
}

pub(super) fn set_size(w: f32, h: f32) {
    SIZE.with(|s| s.set((w, h)));
}

pub(super) fn simulator(gui: &Gui) -> Simulator<'_, Message> {
    Simulator::with_size(settings(), SIZE.with(std::cell::Cell::get), gui.view())
}

/// Run messages as the app would (tasks they return aren't run).
pub(super) fn send(gui: &mut Gui, messages: impl IntoIterator<Item = Message>) {
    for m in messages {
        let _ = gui.update(m);
    }
}

/// Click what `selector` finds, waiting (up to a few seconds) for it to
/// show up, and run what it sends.
pub(super) fn click_with<S>(gui: &mut Gui, what: &str, mut selector: impl FnMut() -> S)
where
    S: iced_test::selector::Selector + Send,
    S::Output: Bounded + Clone + Send + Sync + 'static,
{
    for _ in 0..100 {
        let mut ui = simulator(gui);
        if ui.click(selector()).is_ok() {
            let messages: Vec<Message> = ui.into_messages().collect();
            send(gui, messages);
            return;
        }
        drop(ui);
        std::thread::sleep(Duration::from_millis(30));
        let _ = gui.update(Message::Tick);
    }
    panic!("nothing to click: {what}");
}

/// Click the widget showing `label`.
pub(super) fn click(gui: &mut Gui, label: &str) {
    let l = label.to_owned();
    click_with(gui, &format!("{label:?}"), move || l.clone());
}

/// Click the `n`th (from 0) widget showing `label`.
pub(super) fn click_nth(gui: &mut Gui, label: &str, n: usize) {
    let l = label.to_owned();
    click_with(gui, &format!("{label:?} #{n}"), move || nth(l.clone(), n));
}

fn nth(label: String, n: usize) -> impl FnMut(Candidate<'_>) -> Option<Text> + Send {
    let mut seen = 0;
    move |c| {
        let t = Selector::select(&mut label.as_str(), c)?;
        seen += 1;
        (seen == n + 1).then_some(t)
    }
}

/// Text that contains `part`.
pub(super) fn containing(part: String) -> impl FnMut(Candidate<'_>) -> Option<Text> + Send {
    move |c| {
        let text = match &c {
            Candidate::Text { content, .. } if content.contains(&part) => (*content).to_owned(),
            _ => return None,
        };
        Selector::select(&mut text.as_str(), c)
    }
}

/// Type into the field with `id` (replacing what's there), then press
/// Enter if `submit`.
pub(super) fn type_into(gui: &mut Gui, id: &'static str, text: &str, submit: bool) {
    let mut ui = simulator(gui);
    ui.click(iced::widget::Id::new(id))
        .unwrap_or_else(|e| panic!("no field {id:?}: {e:?}"));
    ui.typewrite(text);
    if submit {
        ui.tap_key(iced::keyboard::key::Named::Enter);
    }
    let messages: Vec<Message> = ui.into_messages().collect();
    send(gui, messages);
}

pub(super) fn shows(gui: &Gui, label: &str) -> bool {
    simulator(gui).find(label).is_ok()
}

pub(super) fn shows_containing(gui: &Gui, part: &str) -> bool {
    simulator(gui).find(containing(part.to_owned())).is_ok()
}

/// Let background work run until `done`.
pub(super) fn wait(gui: &mut Gui, what: &str, done: impl Fn(&Gui) -> bool) {
    let until = Instant::now() + Duration::from_secs(90);
    while !done(gui) {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
        let _ = gui.update(Message::Tick);
    }
}

/// Wait until the app draws `label`.
pub(super) fn wait_for(gui: &mut Gui, label: &str) {
    let l = label.to_owned();
    wait(gui, &format!("{label:?} to show"), move |g| shows(g, &l));
}

/// A screenshot, once images are in.
pub(super) fn snapshot(gui: &Gui, path: &Path) {
    let mut ui = simulator(gui);
    for _ in 0..4 {
        let _ = ui.snapshot(&Theme::Dark).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        ui.snapshot(&Theme::Dark)
            .unwrap()
            .matches_image(path)
            .unwrap()
    );
}

/// Where screenshots go: `$LIVE_TEST_DIR/<name>`, else a temporary folder.
pub(super) fn screens_dir(base: &Path, name: &str) -> PathBuf {
    let out = std::env::var_os("LIVE_TEST_DIR")
        .map_or(base.join("screens"), |d| PathBuf::from(d).join(name));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    out
}

/// A library with one series ("Show", AniList 9) and `files` in its folder.
pub(super) fn one_series(base: &Path, files: &[(&str, &Path)]) -> PathBuf {
    let state = base.join("state");
    let mut library = Library::load(state.join("library.json"), &base.join("library"));
    let new = NewSeries {
        anilist_id: 9,
        english: Some("Show".into()),
        episodes: Some(2),
        ..Default::default()
    };
    let folder = library.add(&new, &base.join("library")).folder.clone();
    std::fs::create_dir_all(&folder).unwrap();
    for (name, from) in files {
        std::fs::copy(from, folder.join(name)).unwrap();
    }
    library.save_or_warn();
    state
}

/// The demo library (`DEMO_DIR`, made by `demo::make`), copied so nothing
/// in it changes; its covers and lists shown as if fetched.
fn demo(base: &Path) -> Option<Harness> {
    let dir = PathBuf::from(std::env::var_os("DEMO_DIR")?);
    let state = base.join("state");
    std::fs::create_dir_all(&state).unwrap();
    for name in ["library.json", "lists-cache.json"] {
        std::fs::copy(dir.join("state").join(name), state.join(name)).unwrap();
    }
    let mut h = harness(base, &state);
    let images = dir.join("state/images");
    let series: Vec<_> = h.app.library.series.clone();
    for s in &series {
        let d = s.details.clone().unwrap_or_default();
        for (url, stem) in [
            (s.cover.clone().or(d.cover.clone()), "cover"),
            (d.cover.clone(), "cover"),
            (d.banner.clone(), "banner"),
        ] {
            let Some(url) = url else { continue };
            for ext in ["jpg", "png", "webp"] {
                let file = images.join(format!("{}-{stem}.{ext}", s.anilist_id));
                if file.exists() {
                    h.images.loaded(
                        url.clone(),
                        Some(iced::widget::image::Handle::from_path(file)),
                    );
                }
            }
        }
    }
    let lists: std::collections::HashMap<u64, crate::tracking::Entry> =
        serde_json::from_slice(&std::fs::read(state.join("lists-cache.json")).unwrap()).unwrap();
    h.app.tracking.pretend_lists(lists);
    // The stats (episodes on disk) are counted in the background.
    wait(&mut h, "the library's stats", |g| {
        g.app.pages.stats.len() == g.app.library.series.len()
    });
    Some(h)
}

/// `DEMO_DIR=<demo library>`: screenshots of the library and a series.
#[test]
#[ignore = "needs DEMO_DIR"]
fn library_screens() {
    let base = tempfile::tempdir().unwrap();
    let Some(mut h) = demo(base.path()) else {
        return;
    };
    let out = screens_dir(base.path(), "iced-library");
    snapshot(&h, &out.join("1-library"));
    let id = h
        .app
        .library
        .series
        .iter()
        .find(|s| s.title().contains("Apothecary"))
        .map_or(h.app.library.series[0].anilist_id, |s| s.anilist_id);
    send(&mut h, [act(Action::OpenSeries(id))]);
    snapshot(&h, &out.join("2-series"));

    // Search results: the library's series, as if found.
    let mut results = Vec::new();
    for s in h.app.library.series.clone().iter().take(5) {
        h.app.open_series(s.anilist_id);
        if let Page::Anime(ap) = &h.app.page {
            results.push(ap.anime.clone());
        }
    }
    h.app.results = results;
    h.app.query = "demo".into();
    h.app.last_query = "demo".into();
    h.app.page = Page::Results;
    snapshot(&h, &out.join("3-search"));

    send(&mut h, [act(Action::GoDownloads)]);
    snapshot(&h, &out.join("4-downloads"));

    h.app.page = Page::Settings;
    snapshot(&h, &out.join("5-settings"));
    send(&mut h, [act(Action::OpenLanguages)]);
    snapshot(&h, &out.join("6-languages"));
    send(
        &mut h,
        [
            edit(|g| g.app.pages.languages = None),
            act(Action::UsenetOpenProviders),
        ],
    );
    click(&mut h, "Add provider");
    snapshot(&h, &out.join("7-usenet-provider"));
    send(&mut h, [edit(|g| g.app.usenet.dialog = None)]);

    let files: Vec<PathBuf> = h
        .app
        .library
        .find(id)
        .unwrap()
        .video_files()
        .into_iter()
        .take(6)
        .collect();
    send(
        &mut h,
        [
            act(Action::AddToPlaylist { id, files }),
            act(Action::OpenPlaylist),
        ],
    );
    snapshot(&h, &out.join("8-playlist"));
}

// ── The player ────────────────────────────────────────────────────────

/// Read the playing episode's subtitles now (the app does it in the
/// background, which the simulator doesn't run).
fn load_subs(gui: &mut Gui) {
    let p = gui.player.as_mut().unwrap();
    let subs = crate::mkv::text_subtitles(&p.path).unwrap();
    p.subs_loaded(Arc::new(subs));
}

fn play(id: u64, episode: u32) -> Message {
    act(Action::Player(PlayerAction::Play { id, episode }))
}

/// A line that began before a seek is on screen right after it (the
/// bug GStreamer's own subtitles had: resuming and skipping lost lines).
#[test]
#[ignore = "needs GStreamer, libass and ffmpeg"]
fn shows_the_line_a_seek_lands_in() {
    if !crate::gst_video::available() || !crate::subs::available() {
        eprintln!("skipping: no GStreamer or libass");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let Some(sample) = crate::gst_video::tests::one_line(&base.path().join("sample"), "srt") else {
        return;
    };
    let state = one_series(base.path(), &[("Show - 01.mkv", &sample)]);
    let mut h = harness(base.path(), &state);
    send(&mut h, [play(9, 1)]);
    load_subs(&mut h);
    let overlay = |h: &Harness| h.player.as_ref().unwrap().overlay.is_some();
    assert!(!overlay(&h), "nothing before 2 s");
    h.player.as_mut().unwrap().seek_to(3.5, true);
    assert!(overlay(&h), "the 2–6 s line, at 3.5 s");
    h.player.as_mut().unwrap().seek_to(7.0, true);
    assert!(!overlay(&h), "gone after 6 s");
    // A delay moves it: 3 seconds later, it's on at 7 s.
    send(&mut h, [Message::Player(player::Msg::SubDelay(3000))]);
    assert!(overlay(&h));
}

/// Without chapters, "Skip 85s" jumps an opening's usual length, and
/// goes away when there's less than that left.
#[test]
#[ignore = "needs GStreamer and ffmpeg"]
fn skips_85_seconds_without_chapters() {
    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let Some(sample) = crate::gst_video::tests::plain_sample(&base.path().join("sample"), 120)
    else {
        return;
    };
    let state = one_series(base.path(), &[("Show - 01.mkv", &sample)]);
    let mut h = harness(base.path(), &state);
    send(&mut h, [play(9, 1)]);
    assert!(
        h.player.is_some(),
        "didn't play: {:?}",
        h.app.status.as_ref().map(|s| &s.text)
    );
    wait(&mut h, "playing", |g| {
        g.player.as_ref().unwrap().position() > 0.3
    });
    assert!(h.player.as_ref().unwrap().chapters.is_empty());
    assert!(!shows(&h, "Skip opening"));
    click(&mut h, "Skip 85s");
    wait(&mut h, "85 seconds on", |g| {
        g.player.as_ref().unwrap().position() >= 85.0
    });
    send(&mut h, [Message::Player(player::Msg::Moved)]);
    assert!(!shows(&h, "Skip 85s"), "less than 85 seconds left");
}

/// The player: plays an episode from the library with its subtitles,
/// skips the opening, saves how far it got, and goes on to the next
/// episode at the end.
#[test]
#[ignore = "renders screenshots; needs GStreamer and ffmpeg"]
fn plays_episodes() {
    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let Some(sample) = crate::gst_video::tests::typeset_sample(&base.path().join("sample")) else {
        return;
    };
    let state = one_series(
        base.path(),
        &[("Show - 01.mkv", &sample), ("Show - 02.mkv", &sample)],
    );
    let mut h = harness(base.path(), &state);
    let out = screens_dir(base.path(), "iced-player");
    send(&mut h, [act(Action::OpenSeries(9))]);
    wait_for(&mut h, "▶  Play episode 1");
    click(&mut h, "▶  Play episode 1");
    assert!(h.player.is_some());
    load_subs(&mut h);
    let p = h.player.as_ref().unwrap();
    let labels: Vec<String> = p.tracks.iter().map(crate::player::Track::label).collect();
    assert_eq!(labels, ["Japanese · FLAC 1.0", "English · Full Subtitles"]);
    assert_eq!(
        (p.audio, p.subs),
        (Some(0), Some(0)),
        "the file's default tracks"
    );

    // In the opening: skip it from the button (chapters say where it is,
    // so no 85-second skip).
    wait(&mut h, "playing", |g| {
        g.player.as_ref().unwrap().position() > 0.5
    });
    assert!(!shows(&h, "Skip 85s"));
    send(&mut h, [Message::Player(player::Msg::Frame)]);
    assert!(
        h.player.as_ref().unwrap().overlay.is_some(),
        "the typeset subtitles, by libass"
    );
    snapshot(&h, &out.join("1-opening"));
    click(&mut h, "Skip opening");
    wait(&mut h, "past the opening", |g| {
        g.player.as_ref().unwrap().position() >= 2.0
    });

    // Pause (space): progress saved.
    let space = || Message::Key(Key::Named(Named::Space), keyboard::Modifiers::empty());
    send(&mut h, [space()]);
    wait(&mut h, "paused", |g| g.player.as_ref().unwrap().paused());
    snapshot(&h, &out.join("2-paused"));
    let w = h.app.library.find(9).unwrap().watched[&1].clone();
    assert!(w.position >= 2.0 && !w.done, "{w:?}");
    let saved = Library::load(state.join("library.json"), base.path());
    assert!(
        saved.find(9).unwrap().watched.contains_key(&1),
        "saved to disk"
    );

    // On to the end: episode 1 watched, episode 2 playing.
    send(&mut h, [space()]);
    wait(&mut h, "playing again", |g| {
        !g.player.as_ref().unwrap().paused()
    });
    h.player.as_mut().unwrap().seek_to(4.6, true);
    wait(&mut h, "episode 2", |g| {
        g.player.as_ref().is_some_and(|p| p.episode == 2)
    });
    assert!(h.app.library.find(9).unwrap().watched[&1].done);

    // Back to the series page, which says what's next.
    send(
        &mut h,
        [Message::Key(
            Key::Named(Named::Escape),
            keyboard::Modifiers::empty(),
        )],
    );
    assert!(h.player.is_none());
    assert!(matches!(&h.app.page, Page::Anime(ap) if ap.anime.anilist_id == 9));
    snapshot(&h, &out.join("3-series"));
}

/// `PLAYER_SAMPLE=<episode>`: the player over a real episode (linked,
/// not copied), paused at `PLAYER_AT`, with its panels.
#[test]
#[ignore = "needs PLAYER_SAMPLE"]
fn player_screens() {
    let Some(file) = std::env::var_os("PLAYER_SAMPLE").map(PathBuf::from) else {
        return;
    };
    let at: f64 = std::env::var("PLAYER_AT")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(122.5);
    let base = tempfile::tempdir().unwrap();
    let state = base.path().join("state");
    let mut library = Library::load(state.join("library.json"), &base.path().join("library"));
    let new = NewSeries {
        anilist_id: 9,
        english: Some("The Way of the Househusband".into()),
        ..Default::default()
    };
    let folder = library
        .add(&new, &base.path().join("library"))
        .folder
        .clone();
    std::fs::create_dir_all(&folder).unwrap();
    // A link, not a copy: the episode isn't touched.
    std::os::unix::fs::symlink(&file, folder.join("The Way of the Househusband - 01.mkv")).unwrap();
    library.save_or_warn();
    let out = screens_dir(base.path(), "player-design");
    let mut h = harness(base.path(), &state);
    // The simulator draws at twice the size.
    send(&mut h, [Message::Scale(2.0), play(9, 1)]);
    assert!(h.player.is_some(), "didn't play");
    load_subs(&mut h);
    h.player.as_mut().unwrap().name = Some("Okay, not too shabby".into());
    h.player.as_mut().unwrap().seek_to(at, true);
    wait(&mut h, "there", |g| {
        g.player.as_ref().unwrap().position() >= at + 0.3
    });
    // Each snapshot is a new renderer, which only gets the picture from
    // a new frame: panels are shown playing.
    let frame = |h: &mut Harness| send(h, [Message::Player(player::Msg::Frame)]);
    send(
        &mut h,
        [Message::Player(player::Msg::Panel(player::Panel::Tracks))],
    );
    frame(&mut h);
    snapshot(&h, &out.join("2-tracks"));
    send(
        &mut h,
        [Message::Player(player::Msg::Panel(player::Panel::Settings))],
    );
    frame(&mut h);
    snapshot(&h, &out.join("3-settings"));
    send(
        &mut h,
        [Message::Player(player::Msg::Panel(player::Panel::Settings))],
    );
    let back = h.player.as_ref().unwrap().position() - 1.2;
    h.player.as_mut().unwrap().seek_to(back, true);
    wait(&mut h, "back", |g| {
        g.player.as_ref().unwrap().position() >= back + 0.3
    });
    send(&mut h, [Message::Player(player::Msg::TogglePause)]);
    wait(&mut h, "paused", |g| g.player.as_ref().unwrap().paused());
    frame(&mut h);
    snapshot(&h, &out.join("1-paused"));
    send(&mut h, [Message::Player(player::Msg::TogglePause)]);
    std::thread::sleep(Duration::from_millis(400));
    h.player.as_mut().unwrap().active = Instant::now() - Duration::from_secs(10);
    frame(&mut h);
    assert!(
        !h.player.as_ref().unwrap().controls_shown(),
        "frames don't count as activity"
    );
    snapshot(&h, &out.join("4-playing"));
}
