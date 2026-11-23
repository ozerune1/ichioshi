//! The GUI (iced): draws the app (`super::App`) and turns clicks into its
//! actions.
//!
//! Background work reports to the app through its channel and wakes the
//! GUI with a `Waker`; after every message, the app's regular duties run
//! (moving finished downloads into the library, setting default tracks,
//! reading lists).

mod calendar;
#[cfg(test)]
mod demo;
mod dialogs;
mod discover;
mod discussion;
mod downloads;
#[cfg(test)]
mod flows;
mod images;
mod library;
mod manage;
mod player;
mod player_view;
mod playlist;
mod remux;
mod search;
mod series;
mod servers;
mod settings;
mod style;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{button, container, row, stack, text};
use iced::{Alignment, Element, Fill, Subscription, Task, Theme};
use tokio::sync::Notify;

use super::watching::{PlayerAction, episode_files};
use super::{Action, App, Page, Waker};
use crate::library::Watch;
use style::*;

/// Run the GUI until its window closes.
pub fn run(rt: tokio::runtime::Handle, state_dir: PathBuf) -> iced::Result {
    iced::application(
        move || {
            let gui = Gui::new(rt.clone(), state_dir.clone());
            let scale = iced::window::latest()
                .and_then(iced::window::scale_factor)
                .map(Message::Scale);
            (gui, Task::batch([Task::done(Message::Tick), scale]))
        },
        Gui::update,
        Gui::view,
    )
    .title("SeaDex")
    .subscription(Gui::subscription)
    .theme(|_: &Gui| Theme::Dark)
    .default_font(INTER)
    .window(window_settings())
    .run()
}

/// The app's icon (assets/icon/seadex.svg, as made by scripts/make-icons.sh).
pub(super) const ICON_SVG: &[u8] = include_bytes!("../../../assets/icon/seadex.svg");
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/png/seadex-256.png");

/// The main window: its size and icon (and, on Linux, the app ID desktops
/// match it to its desktop entry by).
fn window_settings() -> iced::window::Settings {
    iced::window::Settings {
        size: iced::Size::new(1440.0, 900.0),
        icon: iced::window::icon::from_file_data(ICON_PNG, None).ok(),
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "seadex".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[derive(Clone)]
pub(super) enum Message {
    /// Something for the app to do.
    Act(Action),
    /// A change to what's being edited (fields, choices).
    Edit(Edit),
    Player(player::Msg),
    /// Background work finished; its results are in the app's channel.
    Wake,
    Tick,
    Image(String, Option<images::Picture>),
    /// What's in an episode's file on Jellyfin, to stream it.
    Stream {
        id: u64,
        episode: u32,
        result: Result<Box<crate::servers::jellyfin::Playable>, String>,
    },
    Key(Key, keyboard::Modifiers),
    /// The window's size (logical) and display scale.
    Resized(iced::Size),
    Scale(f32),
    DismissNote,
    /// The window is closing: stop downloads cleanly first.
    Close(iced::window::Id),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::Player(m) => write!(f, "Player({m:?})"),
            Message::Key(k, _) => write!(f, "Key({k:?})"),
            Message::Image(url, _) => write!(f, "Image({url})"),
            Message::Stream { id, episode, .. } => write!(f, "Stream({id}, {episode})"),
            other => f.write_str(match other {
                Message::Act(_) => "Act",
                Message::Edit(_) => "Edit",
                Message::Wake => "Wake",
                Message::Tick => "Tick",
                Message::Resized(_) => "Resized",
                Message::Scale(_) => "Scale",
                Message::DismissNote => "DismissNote",
                _ => "Close",
            }),
        }
    }
}

/// A change to the GUI's state, made when the message arrives.
#[derive(Clone)]
pub(super) struct Edit(Arc<dyn Fn(&mut Gui) + Send + Sync>);

/// A message that changes something (a field, a choice) when it arrives.
pub(super) fn edit(f: impl Fn(&mut Gui) + Send + Sync + 'static) -> Message {
    Message::Edit(Edit(Arc::new(f)))
}

pub(super) fn act(a: Action) -> Message {
    Message::Act(a)
}

/// Which of your series the library shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Filter {
    All,
    Only(crate::tracking::Status),
}

pub(super) struct Gui {
    app: App,
    wake: Arc<Notify>,
    images: images::Images,
    /// The episode playing, shown over everything.
    player: Option<player::Player>,
    fullscreen: bool,
    window: iced::Size,
    scale: f32,
    /// The library page's search and filter.
    lib_search: String,
    filter: Filter,
    /// The series whose release choice is open (to change it).
    release_open: Option<u64>,
    /// Downloads whose files are shown.
    expanded: std::collections::HashSet<String>,
    /// Downloads picked on the Downloads page (to remove together).
    picked: std::collections::HashSet<crate::app::Picked>,
    /// Text to put on the clipboard.
    copy: Option<String>,
    /// The note in the corner (the app's status), and since when.
    note_since: Option<(String, Instant)>,
}

impl Gui {
    fn new(rt: tokio::runtime::Handle, state_dir: PathBuf) -> Self {
        let wake = Arc::new(Notify::new());
        let waker = {
            let wake = wake.clone();
            Waker::with_browser(move || wake.notify_one())
        };
        let app = App::create(rt, state_dir, waker);
        Self::with(app, wake)
    }

    fn with(app: App, wake: Arc<Notify>) -> Self {
        let images = images::Images::new(app.state_dir.join("images"));
        Self {
            app,
            wake,
            images,
            player: None,
            fullscreen: false,
            window: iced::Size::new(1440.0, 900.0),
            scale: 1.0,
            lib_search: String::new(),
            filter: Filter::All,
            release_open: None,
            expanded: Default::default(),
            picked: Default::default(),
            copy: None,
            note_since: None,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let task = match message {
            Message::Act(action) => self.act(action),
            Message::Edit(Edit(f)) => {
                f(self);
                match self.copy.take() {
                    Some(text) => iced::clipboard::write(text),
                    None => Task::none(),
                }
            }
            Message::Player(msg) => self.player_message(msg),
            Message::Wake => Task::none(),
            Message::Tick => self.tick(),
            Message::Image(url, handle) => {
                self.images.loaded(url, handle);
                Task::none()
            }
            Message::Stream {
                id,
                episode,
                result,
            } => match result {
                Ok(p) => self.open_stream(id, episode, *p),
                Err(e) => {
                    self.app
                        .set_error(format!("Couldn't play it from Jellyfin: {e}"));
                    Task::none()
                }
            },
            Message::Key(key, modifiers) => self.key(key, modifiers),
            Message::Resized(size) => {
                self.window = size;
                self.size_player();
                Task::none()
            }
            Message::Scale(scale) => {
                self.scale = scale;
                self.size_player();
                Task::none()
            }
            Message::DismissNote => {
                self.app.status = None;
                Task::none()
            }
            Message::Close(id) => {
                self.close();
                return iced::window::close(id);
            }
        };
        self.duties();
        Task::batch([task, self.images.fetch(&self.app.http)])
    }

    /// After every message: take in background results, and keep the
    /// library, downloads and lists going.
    fn duties(&mut self) {
        let a = &mut self.app;
        a.handle_messages();
        a.tracking_tick();
        a.sync_engine();
        a.request_nzb_lookups();
        a.tidy_library();
        a.apply_defaults();
        a.describe_episodes();
        a.refresh_library_stats();
        a.request_details();
        a.request_release_files();
        a.request_browse();
        a.request_episode_scores();
        a.servers_tick();
        a.manage_tick();
        a.save_settings();
    }

    /// Stop what's running before the window goes.
    fn close(&mut self) {
        self.save_progress();
        self.player = None;
        self.app.save_settings();
        self.app.shut_down();
    }

    fn act(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::Player(PlayerAction::Play { id, episode }) => self.play(id, episode),
            Action::OpenDiscussion { id, episode } => {
                // Reading over the player: it waits.
                if let Some(p) = &mut self.player
                    && !p.paused()
                {
                    p.update(player::Msg::TogglePause);
                    self.save_progress();
                }
                self.app.perform(Action::OpenDiscussion { id, episode });
                Task::none()
            }
            Action::RemoveMany { ref items, .. } => {
                for p in items {
                    self.picked.remove(p);
                }
                self.app.perform(action);
                Task::none()
            }
            other => {
                self.app.perform(other);
                Task::none()
            }
        }
    }

    /// Every quarter second: the player's progress and the note's time.
    fn tick(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
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
            let s = &self.app.settings.player;
            if let Some((i, chapter, _)) = p.current_part()
                && ((chapter.part == crate::player::Part::Opening && s.skip_openings)
                    || (chapter.part == crate::player::Part::Ending && s.skip_endings))
                && p.skipped.insert(i)
            {
                p.skip_part();
            }
            // Paused, no frames arrive: subtitles for seeks while paused.
            if p.paused() {
                p.draw_subs(None);
            }
            if !p.paused() && p.last_saved.elapsed() > Duration::from_secs(15) {
                self.save_progress();
            }
        }
        // Notes go away after a while (errors stay longer).
        match &self.app.status {
            None => self.note_since = None,
            Some(s) => match &self.note_since {
                Some((text, since)) if *text == s.text => {
                    let keep = Duration::from_secs(if s.error { 12 } else { 6 });
                    if since.elapsed() > keep {
                        self.app.status = None;
                        self.note_since = None;
                    }
                }
                _ => self.note_since = Some((s.text.clone(), Instant::now())),
            },
        }
        Task::batch(tasks)
    }

    // ── The player ────────────────────────────────────────────────────

    /// Tell the player its size in pixels (subtitles are drawn at it).
    fn size_player(&mut self) {
        let px = (
            (self.window.width * self.scale).round() as u32,
            (self.window.height * self.scale).round() as u32,
        );
        if let Some(p) = &mut self.player {
            p.view_px = px;
            p.scale = self.scale;
            p.draw_subs(None);
        }
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
        let Some(series) = self.app.library.find_mut(id) else {
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
        self.app.library.save_or_warn();
        if self.app.servers_mode() {
            self.app.report_watched(id, episode, pos, done || before);
        }
        if done && !before {
            self.app.sync_watched(id, episode);
        }
    }

    /// The next (or previous) episode of the one playing, on disk (or in
    /// Jellyfin).
    fn neighbour(&self, forward: bool) -> Option<u32> {
        let p = self.player.as_ref()?;
        let series = self.app.library.find(p.series)?;
        let files: std::collections::BTreeMap<u32, ()> = match &series.remote {
            Some(r) => r
                .episodes
                .iter()
                .filter(|(_, e)| e.jellyfin.is_some())
                .map(|(n, _)| (*n, ()))
                .collect(),
            None => episode_files(series, &self.app.busy_files())
                .into_keys()
                .map(|n| (n, ()))
                .collect(),
        };
        if forward {
            files.range(p.episode + 1..).next().map(|(e, _)| *e)
        } else {
            files.range(..p.episode).next_back().map(|(e, _)| *e)
        }
    }

    fn play(&mut self, id: u64, episode: u32) -> Task<Message> {
        if self.app.servers_mode() {
            // Its offline copy, if there's one; else Jellyfin's stream.
            return match self.app.offline_copy(id, episode).cloned() {
                Some(path) => {
                    self.save_progress();
                    self.play_file(id, episode, path)
                }
                None => self.play_from_jellyfin(id, episode),
            };
        }
        self.save_progress();
        let busy = self.app.busy_files();
        let Some(series) = self.app.library.find(id) else {
            return Task::none();
        };
        let Some(path) = episode_files(series, &busy).remove(&episode) else {
            self.app
                .set_error(format!("Episode {episode} isn't in the library"));
            return Task::none();
        };
        self.play_file(id, episode, path)
    }

    /// Play an episode's file on this computer.
    fn play_file(&mut self, id: u64, episode: u32, path: PathBuf) -> Task<Message> {
        let Some(series) = self.app.library.find(id) else {
            return Task::none();
        };
        // Your languages choose the audio and subtitles (the series' own,
        // else the library's), as they do for remuxes.
        let prefs = self.app.prefs_for(series);
        let described = crate::gst_video::describe(&path, Some(&prefs));
        let uri = match crate::gst_video::file_uri(&path) {
            Ok(u) => u,
            Err(e) => {
                self.app.set_error(format!("Couldn't play it: {e:#}"));
                return Task::none();
            }
        };
        // The subtitles and fonts, read off the UI thread.
        let read = path.clone();
        let subs = async move {
            tokio::task::spawn_blocking(move || crate::mkv::text_subtitles(&read))
                .await
                .ok()
                .and_then(Result::ok)
        };
        self.start_player(id, episode, &uri, path, described, subs)
    }

    /// Ask Jellyfin what's in the episode's file; it plays once that's in.
    fn play_from_jellyfin(&mut self, id: u64, episode: u32) -> Task<Message> {
        self.save_progress();
        let Some(j) = self.app.jellyfin() else {
            self.app
                .set_error("Sign in to Jellyfin in Settings to watch from your servers");
            return Task::none();
        };
        let item = self
            .app
            .library
            .find(id)
            .and_then(|s| s.remote.as_ref()?.episodes.get(&episode)?.jellyfin.clone());
        let Some(item) = item else {
            self.app.set_error(format!(
                "Jellyfin doesn't have episode {episode} (yet: it shows up once Jellyfin has \
                 scanned it)"
            ));
            return Task::none();
        };
        self.app.set_info("Opening it from Jellyfin…");
        Task::perform(
            async move {
                j.playable(&item)
                    .await
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}"))
            },
            move |result| Message::Stream {
                id,
                episode,
                result,
            },
        )
    }

    /// Jellyfin said what's in the file: stream it.
    fn open_stream(
        &mut self,
        id: u64,
        episode: u32,
        p: crate::servers::jellyfin::Playable,
    ) -> Task<Message> {
        let (Some(j), Some(series)) = (self.app.jellyfin(), self.app.library.find(id)) else {
            return Task::none();
        };
        let prefs = self.app.prefs_for(series);
        let described = crate::gst_video::describe_info(
            &crate::servers::jellyfin::track_info(&p),
            Some(&prefs),
        );
        let uri = j.stream_url(&p);
        let key = PathBuf::from(format!("jellyfin/{}", p.item));
        let subs = async move { Some(j.text_subtitles(&p).await) };
        self.start_player(id, episode, &uri, key, described, subs)
    }

    /// Play `uri` (named `key`): episode `episode` of series `id`, from
    /// where it was left; its text subtitles and fonts arrive from `subs`.
    fn start_player(
        &mut self,
        id: u64,
        episode: u32,
        uri: &str,
        key: PathBuf,
        described: crate::gst_video::Described,
        subs: impl std::future::Future<Output = Option<crate::mkv::TextSubtitles>> + Send + 'static,
    ) -> Task<Message> {
        let Some(series) = self.app.library.find(id) else {
            return Task::none();
        };
        let start = series.watched.get(&episode).and_then(Watch::resume_at);
        let title = match &self.app.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.title.clone(),
            _ => series.title().to_owned(),
        };
        let name = self
            .app
            .details(id)
            .and_then(|d| d.episode(episode))
            .and_then(|e| e.title.clone())
            .filter(|t| !t.starts_with("Episode "));
        let volume = self
            .player
            .as_ref()
            .map_or(self.app.settings.player.volume, |p| p.volume);
        // The old one stops before the new one starts.
        self.player = None;
        let opened = player::Player::open(
            uri,
            &key,
            described,
            start,
            (id, episode),
            title,
            name,
            volume,
        );
        match opened {
            Ok(p) => {
                self.player = Some(p);
                self.size_player();
                if let Some(at) = start {
                    self.app
                        .set_info(format!("Resuming at {}", player::clock(at)));
                }
                Task::perform(
                    async move { (key, subs.await.map(Arc::new)) },
                    |(key, subs)| Message::Player(player::Msg::SubsLoaded(key, subs)),
                )
            }
            Err(e) => {
                self.app.set_error(format!("Couldn't play it: {e:#}"));
                Task::none()
            }
        }
    }

    fn player_message(&mut self, msg: player::Msg) -> Task<Message> {
        use player::Msg;
        match msg {
            Msg::Back => {
                self.save_progress();
                if let Some(p) = &self.player {
                    self.app.settings.player.volume = p.volume;
                }
                self.player = None;
                if std::mem::take(&mut self.fullscreen) {
                    return set_fullscreen(false);
                }
                Task::none()
            }
            Msg::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                set_fullscreen(self.fullscreen)
            }
            Msg::Languages(prefs) => {
                // The series' own, if it has them; else everyone's (the
                // library's default tracks follow, as remuxes do).
                let id = self.player.as_ref().map(|p| p.series);
                let own = id
                    .and_then(|id| self.app.library.find(id))
                    .is_some_and(|s| s.prefs.is_some());
                match id {
                    Some(id) if own => self.app.perform(Action::SetSeriesPrefs {
                        id,
                        prefs: Some(prefs.clone()),
                    }),
                    _ => {
                        self.app.settings.prefs = prefs.clone();
                        self.app.recheck_defaults();
                        self.app.pages.last_stats = None;
                    }
                }
                if let Some(p) = &mut self.player {
                    p.use_languages(&prefs);
                }
                Task::none()
            }
            Msg::Setting(setting, on) => {
                let s = &mut self.app.settings.player;
                match setting {
                    player::Setting::SkipOpenings => s.skip_openings = on,
                    player::Setting::SkipEndings => s.skip_endings = on,
                    player::Setting::Autoplay => s.autoplay = on,
                }
                Task::none()
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
                if self.app.settings.player.autoplay
                    && let (Some(ep), Some(id)) =
                        (self.neighbour(true), self.player.as_ref().map(|p| p.series))
                {
                    return self.play(id, ep);
                }
                Task::none()
            }
            Msg::SubDelay(d) => {
                if let Some(p) = &mut self.player {
                    p.update(Msg::SubDelay(d));
                    let ms = p.sub_delay_ms;
                    self.app.set_info(format!(
                        "Subtitle delay {}{:.1} s",
                        if ms > 0 { "+" } else { "" },
                        ms as f64 / 1000.0
                    ));
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
        if modifiers.control() || modifiers.alt() {
            return Task::none();
        }
        if self.player.is_none() || self.app.discussion.is_some() {
            if key == Key::Named(Named::Escape) {
                self.app.close_dialogs();
            }
            return Task::none();
        }
        use player::Msg;
        let volume = self.player.as_ref().map_or(100.0, |p| p.volume);
        let msg = match key.as_ref() {
            Key::Named(Named::Space) | Key::Character("k") => Msg::TogglePause,
            Key::Named(Named::ArrowRight) => Msg::SeekBy(5.0),
            Key::Named(Named::ArrowLeft) => Msg::SeekBy(-5.0),
            Key::Character("l") => Msg::SeekBy(player::SKIP),
            Key::Character("j") => Msg::SeekBy(-player::SKIP),
            Key::Named(Named::ArrowUp) => Msg::Volume(volume + 5.0),
            Key::Named(Named::ArrowDown) => Msg::Volume(volume - 5.0),
            Key::Character("m") => Msg::Mute,
            Key::Character("f") => Msg::Fullscreen,
            Key::Character("s") => Msg::SkipPart,
            Key::Character("n") => Msg::Next,
            Key::Character("p") => Msg::Prev,
            Key::Character("z") => Msg::SubDelay(-100),
            Key::Character("x") => Msg::SubDelay(100),
            Key::Named(Named::Escape) if self.fullscreen => Msg::Fullscreen,
            Key::Named(Named::Escape) => Msg::Back,
            _ => return Task::none(),
        };
        self.player_message(msg)
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(Wakeups(self.wake.clone()), wakeups),
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::window::close_requests().map(Message::Close),
            keyboard::listen().filter_map(key_message),
        ])
    }

    // ── Drawing ───────────────────────────────────────────────────────

    fn view(&self) -> Element<'_, Message> {
        if let Some(p) = &self.player {
            let own = self
                .app
                .library
                .find(p.series)
                .and_then(|s| s.prefs.as_ref());
            let view = p.view(
                self.neighbour(true),
                self.neighbour(false),
                self.fullscreen,
                &self.app.settings.player,
                (own.unwrap_or(&self.app.settings.prefs), own.is_some()),
            );
            return self.with_note(match &self.app.discussion {
                Some(d) => self.discussion(view, d),
                None => view,
            });
        }
        let page: Element<'_, Message> = match &self.app.page {
            Page::Anime(ap) => self.series_page(ap),
            Page::Home => self.library_page(),
            Page::Results => self.search_page(),
            Page::Files(fp) => self.files_page(fp),
            Page::Downloads => self.downloads_page(),
            Page::Settings => self.settings_page(),
            Page::Remux(rp) => self.remux_page(rp),
            Page::Playlist(pp) => self.playlist_page(pp),
            Page::Discover => self.discover_page(),
            Page::Calendar => self.calendar_page(),
        };
        let body = row![
            self.sidebar(),
            container(page).width(Fill).height(Fill).style(|_| fill(BG))
        ];
        let page = self.dialogs(body.into());
        self.with_note(page)
    }

    /// The page, with the app's note in the corner if there is one.
    fn with_note<'a>(&'a self, page: Element<'a, Message>) -> Element<'a, Message> {
        let Some(status) = &self.app.status else {
            return page;
        };
        let color = if status.error { RED } else { GREEN };
        let toast = button(
            row![dot(color), text(status.text.clone()).size(13).color(TEXT)]
                .spacing(10)
                .align_y(Alignment::Center),
        )
        .padding([10, 14])
        .on_press(Message::DismissNote)
        .style(|_, _| button::Style {
            background: Some(fill_color(SURFACE2)),
            text_color: TEXT,
            border: iced::Border {
                color: LINE,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..button::Style::default()
        });
        stack![
            page,
            container(container(toast).max_width(560))
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::End)
                .padding(20)
        ]
        .into()
    }
}

/// Background work's wake-ups, as messages.
struct Wakeups(Arc<Notify>);

impl std::hash::Hash for Wakeups {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "wakeups".hash(state);
    }
}

fn wakeups(w: &Wakeups) -> impl iced::futures::Stream<Item = Message> + use<> {
    iced::futures::stream::unfold(w.0.clone(), |notify| async move {
        notify.notified().await;
        Some((Message::Wake, notify))
    })
}

fn key_message(event: keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed { key, modifiers, .. } => Some(Message::Key(key, modifiers)),
        _ => None,
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
