//! The iced GUI ("Studio"): the library, series pages, downloads, and the
//! player (GStreamer), on the app's view model. Being ported from the egui
//! GUI piece by piece; `--egui` runs that one.

mod lists;
mod pages;
mod player;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{button, container, row, stack, text};
use iced::{Alignment, Color, Element, Fill, Subscription, Task, Theme};

use crate::library::{Library, Watch};
use crate::tracking::{Change, Status};
use crate::view::{self, Model};
use pages::INTER;

#[derive(Debug, Clone)]
pub enum Message {
    Go(Page),
    Search(String),
    Filter(Filter),
    Status(u64, Status),
    Score(u64, ScoreChoice),
    /// Play an episode from the library.
    Play(u64, u32),
    Player(player::Msg),
    Lists(lists::Msg),
    /// Something to say in the corner (an error when true).
    Note(String, bool),
    DismissNote,
    Tick,
    Key(Key, keyboard::Modifiers),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Series(u64),
    Downloads,
    Player,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Only(Status),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreChoice(pub u8);

impl std::fmt::Display for ScoreChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == 0 {
            f.write_str("No score")
        } else {
            write!(f, "★ {}", self.0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusChoice(pub Status);

impl std::fmt::Display for StatusChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.name())
    }
}

pub struct App {
    state_dir: PathBuf,
    library: Library,
    lists: lists::Lists,
    pub model: Model,
    model_at: Instant,
    pub page: Page,
    /// Where the player goes back to.
    back: Page,
    pub search: String,
    pub filter: Filter,
    player: Option<player::Player>,
    player_settings: crate::player::Settings,
    fullscreen: bool,
    note: Option<(String, bool, Instant)>,
    /// A demo library: lists and downloads from its files, not the sites.
    demo: bool,
}

pub fn run(state_dir: PathBuf) -> iced::Result {
    iced::application(
        move || {
            let app = App::new(&state_dir);
            (app, Task::done(Message::Tick))
        },
        App::update,
        App::view,
    )
    .title("SeaDex")
    .subscription(App::subscription)
    .theme(|_: &App| Theme::Dark)
    .default_font(INTER)
    .window_size((1440.0, 900.0))
    .run()
}

fn key_message(event: keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed { key, modifiers, .. } => Some(Message::Key(key, modifiers)),
        _ => None,
    }
}

impl App {
    pub fn new(state_dir: &Path) -> Self {
        let library = Library::load(state_dir.join("library.json"), state_dir);
        let http = reqwest::Client::builder()
            .user_agent("seadex-downloader")
            .build()
            .unwrap_or_default();
        let demo = state_dir.join("lists-cache.json").exists();
        let mut app = Self {
            state_dir: state_dir.to_owned(),
            lists: lists::Lists::load(state_dir, http),
            library,
            model: Model {
                cards: Vec::new(),
                series: Vec::new(),
                downloads: Vec::new(),
                accounts: Vec::new(),
            },
            model_at: Instant::now(),
            page: Page::Library,
            back: Page::Library,
            search: String::new(),
            filter: Filter::All,
            player: None,
            player_settings: crate::player::Settings::default(),
            fullscreen: false,
            note: None,
            demo,
        };
        app.rebuild();
        app
    }

    /// The view model, from the library and the lists as they are now.
    fn rebuild(&mut self) {
        self.model_at = Instant::now();
        if self.demo {
            self.model = view::load(&self.state_dir);
            return;
        }
        let lists = self.lists.shown(&self.library);
        let accounts = view::accounts(&self.lists.config);
        self.model = view::build(
            &self.library,
            &self.state_dir.join("images"),
            &lists,
            Vec::new(),
            accounts,
        );
    }

    fn episodes(&self, id: u64) -> Option<u32> {
        let s = self.library.find(id)?;
        s.details.as_ref().and_then(|d| d.episodes).or(s.episodes)
    }

    fn note(&mut self, text: impl Into<String>, error: bool) {
        self.note = Some((text.into(), error, Instant::now()));
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Go(page) => {
                self.page = page;
                Task::none()
            }
            Message::Search(s) => {
                self.search = s;
                Task::none()
            }
            Message::Filter(f) => {
                self.filter = f;
                Task::none()
            }
            Message::Status(id, status) => self.change_list(id, Change::Status(status)),
            Message::Score(id, score) => self.change_list(id, Change::Score(f32::from(score.0))),
            Message::Play(id, episode) => self.play(id, episode),
            Message::Player(msg) => self.player_message(msg),
            Message::Lists(msg) => {
                if let Some(error) = self.lists.handle(msg) {
                    self.note(error, true);
                }
                self.rebuild();
                Task::none()
            }
            Message::Note(text, error) => {
                self.note(text, error);
                Task::none()
            }
            Message::DismissNote => {
                self.note = None;
                Task::none()
            }
            Message::Tick => self.tick(),
            Message::Key(key, modifiers) => self.key(key, modifiers),
        }
    }

    fn change_list(&mut self, id: u64, change: Change) -> Task<Message> {
        if self.demo {
            return Task::none();
        }
        let episodes = self.episodes(id);
        let (task, _) = self.lists.change(id, change, episodes);
        self.rebuild();
        task
    }

    /// Every quarter second: the player's progress, the lists, and the
    /// library's files now and then.
    fn tick(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if !self.demo {
            let library = &self.library;
            let episodes = |id: u64| {
                library
                    .find(id)
                    .and_then(|s| s.details.as_ref().and_then(|d| d.episodes).or(s.episodes))
            };
            tasks.push(self.lists.tick(library, episodes));
        }
        // The end (the widget says so too, while the window draws).
        if self
            .player
            .as_ref()
            .is_some_and(player::Player::reached_end)
        {
            tasks.push(self.player_message(player::Msg::Ended));
        }
        if let Some(p) = &mut self.player {
            // Skip openings and endings, if that's the setting (once each).
            let (skip_op, skip_ed) = (
                self.player_settings.skip_openings,
                self.player_settings.skip_endings,
            );
            if let Some((i, chapter, _)) = p.current_part()
                && ((chapter.part == crate::player::Part::Opening && skip_op)
                    || (chapter.part == crate::player::Part::Ending && skip_ed))
                && p.skipped.insert(i)
            {
                p.skip_part();
            }
            if !p.paused() && p.last_saved.elapsed() > Duration::from_secs(15) {
                self.save_progress();
            }
        }
        if self.note.as_ref().is_some_and(|(_, error, at)| {
            at.elapsed() > Duration::from_secs(if *error { 12 } else { 5 })
        }) {
            self.note = None;
        }
        if self.model_at.elapsed() > Duration::from_secs(5) && self.player.is_none() {
            self.rebuild();
        }
        Task::batch(tasks)
    }

    /// Remember how far the episode playing got; a newly finished one goes
    /// to the lists.
    fn save_progress(&mut self) {
        let Some(p) = &mut self.player else { return };
        let (pos, dur) = (p.position(), p.duration());
        if dur <= 0.0 {
            return;
        }
        p.last_saved = Instant::now();
        let done = p.watched_enough();
        let (id, episode) = (p.series, p.episode);
        let Some(series) = self.library.find_mut(id) else {
            return;
        };
        let before = series.watched.get(&episode).is_some_and(|w| w.done);
        series.watched.insert(
            episode,
            Watch {
                position: pos,
                duration: dur,
                done: done || before,
                at: chrono::Utc::now().timestamp(),
            },
        );
        self.library.save_or_warn();
        if done && !before {
            let total = self.episodes(id);
            self.lists.watched(id, episode, total);
        }
    }

    /// The next (or previous) episode of the one playing, on disk.
    fn neighbour(&self, forward: bool) -> Option<u32> {
        let p = self.player.as_ref()?;
        let files = view::episode_files(self.library.find(p.series)?);
        if forward {
            files.range(p.episode + 1..).next().map(|(e, _)| *e)
        } else {
            files.range(..p.episode).next_back().map(|(e, _)| *e)
        }
    }

    fn play(&mut self, id: u64, episode: u32) -> Task<Message> {
        self.save_progress();
        let Some(series) = self.library.find(id) else {
            return Task::none();
        };
        let Some(path) = view::episode_files(series).remove(&episode) else {
            self.note(format!("Episode {episode} isn't in the library"), true);
            return Task::none();
        };
        let start = series.watched.get(&episode).and_then(Watch::resume_at);
        let title = format!("{} - {episode:02}", series.title());
        let name = series
            .details
            .as_ref()
            .and_then(|d| d.episode(episode))
            .and_then(|e| e.title.clone())
            .filter(|t| !t.starts_with("Episode "));
        let volume = self
            .player
            .as_ref()
            .map_or(self.player_settings.volume, |p| p.volume);
        // The old one stops before the new one starts.
        self.player = None;
        match player::Player::open(&path, start, (id, episode), title, name, volume) {
            Ok(p) => {
                self.player = Some(p);
                if self.page != Page::Player {
                    self.back = self.page;
                    self.page = Page::Player;
                }
                if let Some(at) = start {
                    self.note(format!("Resuming at {}", player::clock(at)), false);
                }
            }
            Err(e) => self.note(format!("Couldn't play it: {e:#}"), true),
        }
        Task::none()
    }

    fn player_message(&mut self, msg: player::Msg) -> Task<Message> {
        use player::Msg;
        match msg {
            Msg::Back => {
                self.save_progress();
                if let Some(p) = &self.player {
                    self.player_settings.volume = p.volume;
                }
                self.player = None;
                self.page = self.back;
                self.rebuild();
                if std::mem::take(&mut self.fullscreen) {
                    return set_fullscreen(false);
                }
                Task::none()
            }
            Msg::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                set_fullscreen(self.fullscreen)
            }
            Msg::Next | Msg::Prev => {
                let forward = matches!(msg, Msg::Next);
                match (
                    self.neighbour(forward),
                    self.player.as_ref().map(|p| p.series),
                ) {
                    (Some(ep), Some(id)) => self.play(id, ep),
                    _ => Task::none(),
                }
            }
            Msg::Ended => {
                // Once (the widget and the tick may both see it).
                match &mut self.player {
                    Some(p) if !p.ended => p.update(Msg::Ended),
                    _ => return Task::none(),
                }
                self.save_progress();
                if self.player_settings.autoplay
                    && let (Some(ep), Some(id)) =
                        (self.neighbour(true), self.player.as_ref().map(|p| p.series))
                {
                    return self.play(id, ep);
                }
                Task::none()
            }
            other => {
                let pause_change = matches!(other, Msg::TogglePause);
                if let Some(p) = &mut self.player {
                    p.update(other);
                }
                if pause_change {
                    self.save_progress();
                }
                Task::none()
            }
        }
    }

    fn key(&mut self, key: Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        if self.page != Page::Player
            || self.player.is_none()
            || modifiers.control()
            || modifiers.alt()
        {
            return Task::none();
        }
        use player::Msg;
        let msg = match key.as_ref() {
            Key::Named(Named::Space) | Key::Character("k") => Msg::TogglePause,
            Key::Named(Named::ArrowRight) => Msg::SeekBy(5.0),
            Key::Named(Named::ArrowLeft) => Msg::SeekBy(-5.0),
            Key::Character("l") => Msg::SeekBy(85.0),
            Key::Character("j") => Msg::SeekBy(-85.0),
            Key::Named(Named::ArrowUp) => {
                Msg::Volume(self.player.as_ref().map_or(100.0, |p| p.volume) + 5.0)
            }
            Key::Named(Named::ArrowDown) => {
                Msg::Volume(self.player.as_ref().map_or(100.0, |p| p.volume) - 5.0)
            }
            Key::Character("m") => Msg::Mute,
            Key::Character("f") => Msg::Fullscreen,
            Key::Character("s") => Msg::SkipPart,
            Key::Character("n") => Msg::Next,
            Key::Character("p") => Msg::Prev,
            Key::Named(Named::Escape) if self.fullscreen => Msg::Fullscreen,
            Key::Named(Named::Escape) => Msg::Back,
            _ => return Task::none(),
        };
        self.player_message(msg)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            keyboard::listen().filter_map(key_message),
        ])
    }

    pub fn view(&self) -> Element<'_, Message> {
        let page: Element<'_, Message> = match (self.page, &self.player) {
            (Page::Player, Some(p)) => {
                return self.with_note(p.view(
                    self.neighbour(true),
                    self.neighbour(false),
                    self.fullscreen,
                ));
            }
            (Page::Library | Page::Player, _) => self.library(),
            (Page::Series(id), _) => self.series(id),
            (Page::Downloads, _) => self.downloads(),
        };
        let body = row![
            self.sidebar(),
            container(page)
                .width(Fill)
                .height(Fill)
                .style(|_| pages::fill(pages::BG))
        ];
        self.with_note(body.into())
    }

    /// The page, with a note in the corner if there is one.
    fn with_note<'a>(&'a self, page: Element<'a, Message>) -> Element<'a, Message> {
        let Some((text_, error, _)) = &self.note else {
            return page;
        };
        let color = if *error {
            Color::from_rgb8(0xf8, 0x71, 0x71)
        } else {
            pages::GREEN
        };
        let toast = button(
            row![
                pages::dot(color),
                text(text_.clone()).size(13).color(pages::TEXT)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([10, 14])
        .on_press(Message::DismissNote)
        .style(|_, _| button::Style {
            background: Some(pages::fill_color(pages::SURFACE2)),
            text_color: pages::TEXT,
            border: iced::Border {
                color: pages::LINE,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..button::Style::default()
        });
        stack![
            page,
            container(toast)
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::End)
                .padding(20)
        ]
        .into()
    }
}

fn set_fullscreen(on: bool) -> Task<Message> {
    let mode = if on {
        iced::window::Mode::Fullscreen
    } else {
        iced::window::Mode::Windowed
    };
    iced::window::latest().and_then(move |id| iced::window::set_mode(id, mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Click the widget with `label` in the app as drawn now, and run the
    /// messages it sends.
    fn click(app: &mut App, label: &str) {
        let settings = iced::Settings {
            default_font: INTER,
            ..Default::default()
        };
        let mut ui = iced_test::Simulator::with_size(settings, (1440.0, 900.0), app.view());
        ui.click(label)
            .unwrap_or_else(|e| panic!("no {label:?}: {e:?}"));
        let messages: Vec<Message> = ui.into_messages().collect();
        for m in messages {
            let _ = app.update(m);
        }
    }

    fn wait(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(app) {
            if Instant::now() > deadline {
                let state = app.player.as_ref().map(|p| {
                    use gstreamer::prelude::*;
                    (
                        p.episode,
                        p.position(),
                        p.duration(),
                        p.paused(),
                        p.ended,
                        p.video.eos(),
                        p.video.pipeline().state(gstreamer::ClockTime::ZERO),
                    )
                });
                panic!("timed out waiting for {what}: {state:?}");
            }
            let _ = app.update(Message::Tick);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn snapshot(app: &App, path: &Path) {
        let settings = iced::Settings {
            default_font: INTER,
            ..Default::default()
        };
        let mut ui = iced_test::Simulator::with_size(settings, (1440.0, 900.0), app.view());
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

    /// The player (GStreamer): plays an episode from the library with its
    /// subtitles, skips the opening, saves how far it got, and goes on to
    /// the next episode at the end.
    #[test]
    #[ignore = "renders screenshots; needs GStreamer and ffmpeg"]
    fn plays_episodes() {
        if !crate::gst_video::available() {
            eprintln!("skipping: no GStreamer");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let mut library = Library::load(state.join("library.json"), dir.path());
        let new = crate::library::NewSeries {
            anilist_id: 9,
            english: Some("Show".into()),
            ..Default::default()
        };
        let folder = library.add(&new, dir.path()).folder.clone();
        let Some(sample) = crate::gst_video::tests::typeset_sample(&dir.path().join("sample"))
        else {
            return;
        };
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::copy(&sample, folder.join("Show - 01.mkv")).unwrap();
        std::fs::copy(&sample, folder.join("Show - 02.mkv")).unwrap();
        library.save_or_warn();
        let out = std::env::var_os("LIVE_TEST_DIR").map_or(dir.path().join("screens"), |d| {
            PathBuf::from(d).join("iced-player")
        });
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        let mut app = App::new(&state);
        let _ = app.update(Message::Go(Page::Series(9)));
        click(&mut app, "▶  Play episode 1");
        assert_eq!(app.page, Page::Player);
        let p = app.player.as_ref().unwrap();
        let labels: Vec<String> = p.tracks.iter().map(crate::player::Track::label).collect();
        assert_eq!(labels, ["Japanese · FLAC 1.0", "English · Full Subtitles"]);
        assert_eq!(
            (p.audio, p.subs),
            (Some(0), Some(0)),
            "the file's default tracks"
        );

        // In the opening: skip it from the button.
        wait(&mut app, "playing", |a| {
            a.player.as_ref().unwrap().position() > 0.5
        });
        snapshot(&app, &out.join("1-opening"));
        click(&mut app, "Skip opening  ⏵");
        wait(&mut app, "past the opening", |a| {
            a.player.as_ref().unwrap().position() >= 2.0
        });

        // Pause (space): progress saved.
        let _ = app.update(Message::Key(
            Key::Named(Named::Space),
            keyboard::Modifiers::empty(),
        ));
        wait(&mut app, "paused", |a| a.player.as_ref().unwrap().paused());
        snapshot(&app, &out.join("2-paused"));
        let w = app.library.find(9).unwrap().watched[&1].clone();
        assert!(w.position >= 2.0 && !w.done, "{w:?}");

        // On to the end: episode 1 watched, episode 2 playing.
        let _ = app.update(Message::Key(
            Key::Named(Named::Space),
            keyboard::Modifiers::empty(),
        ));
        wait(&mut app, "playing again", |a| {
            !a.player.as_ref().unwrap().paused()
        });
        app.player.as_mut().unwrap().seek_to(4.6, true);
        wait(&mut app, "episode 2", |a| {
            a.player.as_ref().is_some_and(|p| p.episode == 2)
        });
        assert!(app.library.find(9).unwrap().watched[&1].done);

        // Back to the series page.
        let _ = app.update(Message::Key(
            Key::Named(Named::Escape),
            keyboard::Modifiers::empty(),
        ));
        assert_eq!(app.page, Page::Series(9));
        assert!(app.player.is_none());
        snapshot(&app, &out.join("3-series"));
    }

    /// `DEMO_DIR=<demo library>`: screenshots of each page, in
    /// `$DEMO_DIR/screens-iced/`.
    #[test]
    #[ignore = "needs DEMO_DIR"]
    fn screenshots() {
        let Some(dir) = std::env::var_os("DEMO_DIR").map(PathBuf::from) else {
            return;
        };
        let mut app = App::new(&dir.join("state"));
        let out = dir.join("screens-iced");
        let _ = std::fs::remove_dir_all(&out);
        let series = app
            .model
            .cards
            .iter()
            .find(|c| c.title.contains("Apothecary"))
            .map_or(app.model.cards[0].id, |c| c.id);
        for (name, page) in [
            ("1-library", Page::Library),
            ("2-series", Page::Series(series)),
            ("3-downloads", Page::Downloads),
        ] {
            app.page = page;
            let settings = iced::Settings {
                default_font: INTER,
                ..Default::default()
            };
            let mut ui = iced_test::Simulator::with_size(settings, (1440.0, 900.0), app.view());
            // Images are decoded in the background: draw until they're in.
            for _ in 0..8 {
                let _ = ui.snapshot(&Theme::Dark).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            let snapshot = ui.snapshot(&Theme::Dark).unwrap();
            assert!(snapshot.matches_image(out.join(name)).unwrap());
        }
    }
}
