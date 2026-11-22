//! The player page: mpv's picture with controls made for anime (skip the
//! opening, next episode, audio and subtitle tracks), playing a series'
//! episodes in order and remembering where each was left.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CursorIcon, Key, Modifiers, Rect, RichText, Sense, Ui, pos2, vec2,
};

use super::{Action, App, Page};
use crate::library::{Series, Watch};
use crate::player::{self, Happened, Output, Part, Player, TrackKind, inhibit::Inhibitor, video};

/// The player page: the page to go back to, and what the controls are doing.
pub(super) struct PlayerPage {
    pub(super) back: Box<Page>,
    /// The picture's size on screen last frame, in pixels: mpv draws at it.
    want: [i32; 2],
    /// When the mouse last moved (the controls show for a while after).
    active: Instant,
    /// The seek bar being dragged, to where.
    dragging: Option<f64>,
}

impl PlayerPage {
    fn new(back: Page) -> Self {
        Self {
            back: Box::new(back),
            want: [1280, 720],
            active: Instant::now(),
            dragging: None,
        }
    }
}

/// The episode playing.
#[derive(Clone, Debug)]
pub(super) struct Playing {
    pub(super) series: u64,
    pub(super) episode: u32,
    pub(super) path: PathBuf,
    /// "Frieren - 05".
    pub(super) title: String,
    /// The episode's own title.
    pub(super) name: Option<String>,
}

pub(super) struct PlayerState {
    pub(super) player: Player,
    inhibit: Inhibitor,
    pub(super) playing: Option<Playing>,
    last_saved: Instant,
    was_paused: bool,
    /// Chapters skipped automatically in this episode.
    auto_skipped: HashSet<usize>,
    /// The app went fullscreen for the player (and leaves it after).
    made_fullscreen: bool,
}

impl PlayerState {
    pub(super) fn set_made_fullscreen(&mut self, on: bool) {
        self.made_fullscreen = on;
    }
}

/// Open a file with the system's default app.
fn open_with_system(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opener = "xdg-open";
    let mut child = std::process::Command::new(opener).arg(path).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// What the player controls ask the app to do.
#[derive(Debug, Clone)]
pub(super) enum PlayerAction {
    Play { id: u64, episode: u32 },
    Settings(player::Settings),
}

/// "1:23", "1:02:03".
pub(super) fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A series' episodes on disk, by number (files still downloading left out).
pub(super) fn episode_files(series: &Series, busy: &HashSet<PathBuf>) -> BTreeMap<u32, PathBuf> {
    let mut out = BTreeMap::new();
    for path in series.video_files() {
        if busy.contains(&path) {
            continue;
        }
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

/// The episode to watch next (of those on disk, in order): the first after
/// the last one seen, or the first of all once everything's been seen.
pub(super) fn up_next(
    series: &Series,
    mut on_disk: impl Iterator<Item = u32> + Clone,
) -> Option<u32> {
    let last_done = series
        .watched
        .iter()
        .filter(|(_, w)| w.done)
        .map(|(e, _)| *e)
        .max();
    let unseen = |e: &u32| {
        last_done.is_none_or(|d| *e > d) && !series.watched.get(e).is_some_and(|w| w.done)
    };
    on_disk.clone().find(unseen).or_else(|| on_disk.next())
}

/// `area`, narrowed to the picture's shape.
fn fit(area: Rect, size: Option<(i64, i64)>) -> Rect {
    let Some((w, h)) = size else { return area };
    let aspect = w as f32 / h as f32;
    let mut s = area.size();
    if s.x / s.y > aspect {
        s.x = s.y * aspect;
    } else {
        s.y = s.x / aspect;
    }
    Rect::from_center_size(area.center(), s)
}

impl App {
    /// Start mpv the first time something plays.
    fn ensure_player(&mut self) -> Result<(), String> {
        if self.player.is_some() {
            return Ok(());
        }
        let output = if self.gl.is_some() {
            Output::Window
        } else {
            Output::Headless
        };
        #[cfg(not(test))]
        if output == Output::Headless {
            return Err("The player needs OpenGL, which isn't available here".into());
        }
        let ctx = self.ctx.clone();
        let mut player = Player::new(output, &self.settings.player, move || ctx.request_repaint())
            .map_err(|e| format!("{e:#}"))?;
        if let Some((gl, get_proc)) = &self.gl {
            let ctx = self.ctx.clone();
            let video = video::Video::new(player.mpv(), gl.clone(), get_proc.clone(), move || {
                ctx.request_repaint()
            })
            .map_err(|e| format!("{e:#}"))?;
            player.video = Some(video);
        }
        self.player = Some(PlayerState {
            player,
            inhibit: Inhibitor::new(),
            playing: None,
            last_saved: Instant::now(),
            was_paused: false,
            auto_skipped: HashSet::new(),
            made_fullscreen: false,
        });
        Ok(())
    }

    /// Play an episode from the library, where it was left.
    pub(super) fn play_episode(&mut self, id: u64, episode: u32) {
        let busy = self.busy_files();
        let Some(series) = self.library.find(id) else {
            return;
        };
        let Some(path) = episode_files(series, &busy).remove(&episode) else {
            self.set_error(format!("Episode {episode} isn't in the library"));
            return;
        };
        let start = series.watched.get(&episode).and_then(Watch::resume_at);
        let title = match &self.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.title.clone(),
            _ => series.title().to_owned(),
        };
        let name = self
            .details(id)
            .and_then(|d| d.episode(episode))
            .and_then(|e| e.title.clone());
        if crate::player::ffi::api().is_err() {
            // No libmpv: the system's video player, then.
            match open_with_system(&path) {
                Ok(()) => {
                    self.set_info("Opened it in your video player (install mpv to watch here)")
                }
                Err(e) => self.set_error(format!("Couldn't open {}: {e}", path.display())),
            }
            return;
        }
        if let Err(e) = self.ensure_player() {
            self.set_error(format!("The player couldn't start: {e}"));
            return;
        }
        self.save_progress();
        let ps = self.player.as_mut().expect("just made");
        if let Err(e) = ps.player.load(&path, start) {
            self.set_error(format!("Couldn't play it: {e:#}"));
            return;
        }
        ps.auto_skipped.clear();
        ps.was_paused = false;
        ps.playing = Some(Playing {
            series: id,
            episode,
            path,
            title: format!("{title} - {episode:02}"),
            name,
        });
        ps.last_saved = Instant::now();
        if let Some(at) = start {
            self.set_info(format!("Resuming at {}", clock(at)));
        }
        if !matches!(self.page, Page::Player(_)) {
            let back = std::mem::replace(&mut self.page, Page::Home);
            self.page = Page::Player(PlayerPage::new(back));
        }
    }

    pub(super) fn player_action(&mut self, action: PlayerAction) {
        match action {
            PlayerAction::Play { id, episode } => self.play_episode(id, episode),
            PlayerAction::Settings(s) => self.settings.player = s,
        }
    }

    /// Remember how far the episode playing got.
    pub(super) fn save_progress(&mut self) {
        let Some(ps) = &mut self.player else { return };
        let Some(playing) = &ps.playing else { return };
        let now = &ps.player.now;
        if !now.loaded || now.duration <= 0.0 {
            return;
        }
        let done = ps.player.watched_enough();
        ps.last_saved = Instant::now();
        let Some(series) = self.library.find_mut(playing.series) else {
            return;
        };
        let before = series.watched.get(&playing.episode).is_some_and(|w| w.done);
        series.watched.insert(
            playing.episode,
            Watch {
                position: now.pos,
                duration: now.duration,
                done: done || before,
                at: chrono::Utc::now().timestamp(),
            },
        );
        self.library.save_or_warn();
    }

    /// The next (or previous) episode on disk.
    fn neighbour(&self, forward: bool) -> Option<u32> {
        let playing = self.player.as_ref()?.playing.as_ref()?;
        let series = self.library.find(playing.series)?;
        let eps = episode_files(series, &self.busy_files());
        if forward {
            eps.range(playing.episode + 1..).next().map(|(e, _)| *e)
        } else {
            eps.range(..playing.episode).next_back().map(|(e, _)| *e)
        }
    }

    /// Every frame: read mpv's news, draw its picture, and keep the
    /// library's progress up to date.
    pub(super) fn player_tick(&mut self, frame: &mut eframe::Frame) {
        let Some(ps) = &mut self.player else { return };
        if let Some(v) = &mut ps.player.video
            && v.id.is_none()
            && frame.gl().is_some()
        {
            v.id = Some(frame.register_native_glow_texture(v.texture));
        }
        let happened = ps.player.poll();
        let on_page = matches!(self.page, Page::Player(_));

        // Left the player: stop, and leave fullscreen.
        if !on_page && ps.playing.is_some() {
            self.save_progress();
            let ps = self.player.as_mut().expect("checked");
            ps.player.stop();
            ps.playing = None;
            ps.inhibit.set(false);
            if std::mem::take(&mut ps.made_fullscreen) {
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            return;
        }

        for h in happened {
            match h {
                Happened::Loaded => {}
                Happened::Ended => {
                    self.save_progress();
                    if self.settings.player.autoplay
                        && let Some(next) = self.neighbour(true)
                        && let Some(id) = self
                            .player
                            .as_ref()
                            .and_then(|p| p.playing.as_ref())
                            .map(|p| p.series)
                    {
                        self.play_episode(id, next);
                    }
                }
                Happened::Failed(e) => {
                    let what = self
                        .player
                        .as_ref()
                        .and_then(|p| p.playing.as_ref())
                        .map(|p| p.title.clone())
                        .unwrap_or_default();
                    self.set_error(format!("Couldn't play {what}: {e}"));
                }
            }
        }

        let skip_op = self.settings.player.skip_openings;
        let skip_ed = self.settings.player.skip_endings;
        let Some(ps) = &mut self.player else { return };
        // Skip openings and endings, if that's the setting (once each).
        if let (Some(i), Some((chapter, _))) = (ps.player.now.chapter, ps.player.current_part())
            && ((chapter.part == Part::Opening && skip_op)
                || (chapter.part == Part::Ending && skip_ed))
            && ps.auto_skipped.insert(i)
        {
            let what = if chapter.part == Part::Opening {
                "opening"
            } else {
                "ending"
            };
            ps.player.skip_part();
            self.set_info(format!("Skipped the {what}"));
        }

        let Some(ps) = &mut self.player else { return };
        if let (Page::Player(pp), Some(v)) = (&self.page, &mut ps.player.video) {
            v.shown();
            v.render(pp.want);
        }
        let playing = ps.player.now.loaded && !ps.player.now.paused && !ps.player.now.eof;
        if ps.player.output == Output::Window {
            ps.inhibit.set(playing);
        }
        self.settings.player.volume = ps.player.now.volume;
        let paused_now = ps.player.now.paused;
        let due = ps.last_saved.elapsed() > Duration::from_secs(15);
        if (playing && due) || paused_now != ps.was_paused {
            ps.was_paused = paused_now;
            self.save_progress();
        }
        if playing {
            // Keep the clock moving while the picture doesn't change.
            self.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Save progress and let mpv go (with the GL context current).
    pub(super) fn close_player(&mut self) {
        self.save_progress();
        self.player = None;
    }

    pub(super) fn player_page(&self, ui: &mut Ui, pp: &mut PlayerPage, actions: &mut Vec<Action>) {
        let Some(ps) = &self.player else {
            ui.label("Nothing is playing.");
            return;
        };
        let p = &ps.player;
        let ctx = ui.ctx().clone();
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0.0, Color32::BLACK);
        let mut toggle_fullscreen = false;

        // Keys (taken before any widget sees them).
        if !ctx.egui_wants_keyboard_input() {
            let pressed = |key: Key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, key));
            if pressed(Key::Space) || pressed(Key::K) {
                p.toggle_pause();
            }
            if pressed(Key::ArrowRight) {
                p.seek_by(5.0);
            }
            if pressed(Key::ArrowLeft) {
                p.seek_by(-5.0);
            }
            if pressed(Key::L) {
                p.seek_by(85.0);
            }
            if pressed(Key::J) {
                p.seek_by(-85.0);
            }
            if pressed(Key::ArrowUp) {
                p.set_volume(p.now.volume + 5.0);
            }
            if pressed(Key::ArrowDown) {
                p.set_volume(p.now.volume - 5.0);
            }
            if pressed(Key::M) {
                p.set_muted(!p.now.muted);
            }
            if pressed(Key::F) {
                toggle_fullscreen = true;
            }
            if pressed(Key::S)
                && p.current_part()
                    .is_some_and(|(c, _)| matches!(c.part, Part::Opening | Part::Ending))
            {
                p.skip_part();
            }
            if pressed(Key::N)
                && let Some(next) = self.neighbour(true)
                && let Some(playing) = &ps.playing
            {
                actions.push(Action::Player(PlayerAction::Play {
                    id: playing.series,
                    episode: next,
                }));
            }
            if pressed(Key::P)
                && let Some(prev) = self.neighbour(false)
                && let Some(playing) = &ps.playing
            {
                actions.push(Action::Player(PlayerAction::Play {
                    id: playing.series,
                    episode: prev,
                }));
            }
            if pressed(Key::Escape) {
                if fullscreen {
                    toggle_fullscreen = true;
                } else {
                    actions.push(Action::Back);
                }
            }
            if ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO || !i.keys_down.is_empty()) {
                pp.active = Instant::now();
            }
        }

        // The picture, pixel-aligned at the size mpv draws it.
        let ppp = ctx.pixels_per_point();
        let fitted = fit(area, p.now.size);
        pp.want = [
            (fitted.width() * ppp).round() as i32,
            (fitted.height() * ppp).round() as i32,
        ];
        let min = (fitted.min.to_vec2() * ppp).round() / ppp;
        let picture = Rect::from_min_size(
            min.to_pos2(),
            vec2(pp.want[0] as f32, pp.want[1] as f32) / ppp,
        );
        let surface = ui.interact(area, egui::Id::unique("player-picture"), Sense::click());
        match p.video.as_ref().and_then(|v| v.id) {
            Some(id) if p.now.loaded && p.now.size.is_some() => {
                ui.painter().image(
                    id,
                    picture,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            _ if !p.now.loaded && ps.playing.is_some() => {
                ui.painter().text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    "Loading…",
                    egui::FontId::proportional(18.0),
                    Color32::GRAY,
                );
            }
            _ => {}
        }
        if surface.double_clicked() {
            toggle_fullscreen = true;
        } else if surface.clicked() {
            p.toggle_pause();
        }

        let show = !p.now.loaded
            || p.now.paused
            || p.now.eof
            || pp.dragging.is_some()
            || pp.active.elapsed() < Duration::from_millis(2500);
        if show {
            ctx.request_repaint_after(
                Duration::from_millis(2600).saturating_sub(pp.active.elapsed()),
            );
        } else if surface.hovered() {
            ctx.set_cursor_icon(CursorIcon::None);
        }

        // Skip the opening (or ending), shown whenever it's on.
        let next = self.neighbour(true);
        if let Some((chapter, end)) = p.current_part() {
            let last = (end - p.now.duration).abs() < 0.5;
            let label = match chapter.part {
                Part::Opening => Some("Skip opening ⏵"),
                Part::Ending if last && next.is_some() => Some("Next episode ⏵"),
                Part::Ending => Some("Skip ending ⏵"),
                _ => None,
            };
            if let Some(label) = label {
                let at = pos2(
                    area.right() - 24.0,
                    area.bottom() - if show { 110.0 } else { 40.0 },
                );
                egui::Area::new(egui::Id::unique("player-skip"))
                    .order(egui::Order::Foreground)
                    .pivot(egui::Align2::RIGHT_BOTTOM)
                    .fixed_pos(at)
                    .show(&ctx, |ui| {
                        let button = egui::Button::new(RichText::new(label).size(16.0).strong())
                            .min_size(vec2(150.0, 36.0));
                        if ui.add(button).clicked() {
                            match (label, next, &ps.playing) {
                                ("Next episode ⏵", Some(next), Some(playing)) => {
                                    actions.push(Action::Player(PlayerAction::Play {
                                        id: playing.series,
                                        episode: next,
                                    }));
                                }
                                _ => p.skip_part(),
                            }
                        }
                    });
            }
        }

        // At the end: what's next.
        if p.now.eof
            && let (Some(next), Some(playing)) = (next, &ps.playing)
        {
            let name = self
                .details(playing.series)
                .and_then(|d| d.episode(next))
                .and_then(|e| e.title.clone());
            let label = match name {
                Some(n) => format!("▶ Episode {next} · {n}"),
                None => format!("▶ Episode {next}"),
            };
            egui::Area::new(egui::Id::unique("player-next"))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::CENTER_CENTER)
                .fixed_pos(area.center())
                .show(&ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Up next").color(Color32::GRAY));
                        let button = egui::Button::new(RichText::new(label).size(18.0).strong())
                            .min_size(vec2(220.0, 44.0));
                        if ui.add(button).clicked() {
                            actions.push(Action::Player(PlayerAction::Play {
                                id: playing.series,
                                episode: next,
                            }));
                        }
                    });
                });
        }

        if show {
            // Top: back, and what's playing.
            let top = Rect::from_min_size(area.min, vec2(area.width(), 44.0));
            ui.painter()
                .rect_filled(top, 0.0, Color32::from_black_alpha(150));
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(top.shrink2(vec2(12.0, 8.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("⏴ Back").clicked() {
                            actions.push(Action::Back);
                        }
                        if let Some(playing) = &ps.playing {
                            ui.label(RichText::new(&playing.title).strong().color(Color32::WHITE))
                                .on_hover_text(playing.path.display().to_string());
                            if let Some(name) = &playing.name {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(name).color(Color32::LIGHT_GRAY),
                                    )
                                    .truncate(),
                                );
                            }
                        }
                    });
                },
            );

            // Bottom: seek bar and buttons.
            let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - 78.0), area.max);
            ui.painter()
                .rect_filled(bar, 0.0, Color32::from_black_alpha(170));
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(bar.shrink2(vec2(16.0, 8.0))),
                |ui| {
                    ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                    seek_bar(ui, p, pp);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        self.buttons(ui, ps, next, actions, &mut toggle_fullscreen, fullscreen);
                    });
                },
            );
        }

        if toggle_fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            actions.push(Action::PlayerFullscreen(!fullscreen));
        }
    }

    fn buttons(
        &self,
        ui: &mut Ui,
        ps: &PlayerState,
        next: Option<u32>,
        actions: &mut Vec<Action>,
        toggle_fullscreen: &mut bool,
        fullscreen: bool,
    ) {
        let p = &ps.player;
        let prev = self.neighbour(false);
        let series = ps.playing.as_ref().map(|p| p.series);
        let big = |s: &str| RichText::new(s).size(18.0);
        if let Some(id) = series {
            let b = ui
                .add_enabled(prev.is_some(), egui::Button::new(big("⏮")).frame(false))
                .on_hover_text("Previous episode (P)");
            if b.clicked()
                && let Some(e) = prev
            {
                actions.push(Action::Player(PlayerAction::Play { id, episode: e }));
            }
        }
        let play = if p.now.paused || p.now.eof {
            "▶"
        } else {
            "⏸"
        };
        if ui
            .add(egui::Button::new(big(play)).frame(false))
            .on_hover_text("Play or pause (Space)")
            .clicked()
        {
            p.toggle_pause();
        }
        if let Some(id) = series {
            let b = ui
                .add_enabled(next.is_some(), egui::Button::new(big("⏭")).frame(false))
                .on_hover_text("Next episode (N)");
            if b.clicked()
                && let Some(e) = next
            {
                actions.push(Action::Player(PlayerAction::Play { id, episode: e }));
            }
        }
        ui.label(format!("{} / {}", clock(p.now.pos), clock(p.now.duration)));
        ui.add_space(8.0);
        let speaker = if p.now.muted || p.now.volume <= 0.0 {
            "🔇"
        } else {
            "🔊"
        };
        if ui
            .add(egui::Button::new(speaker).frame(false))
            .on_hover_text("Mute (M)")
            .clicked()
        {
            p.set_muted(!p.now.muted);
        }
        let mut volume = p.now.volume;
        let slider = ui.add(
            egui::Slider::new(&mut volume, 0.0..=150.0)
                .show_value(false)
                .trailing_fill(true),
        );
        if slider.changed() {
            p.set_volume(volume);
            if p.now.muted {
                p.set_muted(false);
            }
        }
        slider.on_hover_text(format!("Volume {volume:.0}%"));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if fullscreen {
                "Exit fullscreen"
            } else {
                "Fullscreen"
            };
            if ui
                .button(label)
                .on_hover_text("F, or double-click the picture")
                .clicked()
            {
                *toggle_fullscreen = true;
            }
            let mut s = self.settings.player.clone();
            ui.menu_button("⚙", |ui| {
                let mut changed = false;
                if ui
                    .checkbox(&mut s.deband, "Deband")
                    .on_hover_text("Smooth out color banding in flat colors and gradients")
                    .changed()
                {
                    p.set_deband(s.deband);
                    changed = true;
                }
                changed |= ui.checkbox(&mut s.skip_openings, "Skip openings").changed();
                changed |= ui.checkbox(&mut s.skip_endings, "Skip endings").changed();
                changed |= ui
                    .checkbox(&mut s.autoplay, "Play the next episode automatically")
                    .changed();
                if changed {
                    actions.push(Action::Player(PlayerAction::Settings(s.clone())));
                }
            });

            let subs = p
                .selected(TrackKind::Sub)
                .map_or("No subtitles".to_owned(), |t| t.label());
            egui::ComboBox::from_id_salt("player-subs")
                .selected_text(subs)
                .width(200.0)
                .show_ui(ui, |ui| {
                    let none = p.selected(TrackKind::Sub).is_none();
                    if ui.selectable_label(none, "No subtitles").clicked() {
                        p.set_subtitles(None);
                    }
                    for t in p.tracks(TrackKind::Sub) {
                        if ui.selectable_label(t.selected, t.label()).clicked() {
                            p.set_subtitles(Some(t.id));
                        }
                    }
                });
            let audio = p
                .selected(TrackKind::Audio)
                .map_or("No audio".to_owned(), |t| t.label());
            egui::ComboBox::from_id_salt("player-audio")
                .selected_text(audio)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for t in p.tracks(TrackKind::Audio) {
                        if ui.selectable_label(t.selected, t.label()).clicked() {
                            p.set_audio(t.id);
                        }
                    }
                });
        });
    }
}

/// The seek bar: openings and endings shaded, chapters marked, the time
/// under the pointer shown.
fn seek_bar(ui: &mut Ui, p: &Player, pp: &mut PlayerPage) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click_and_drag());
    let duration = p.now.duration.max(0.001);
    let x_of = |t: f64| rect.left() + (t / duration).clamp(0.0, 1.0) as f32 * rect.width();
    let t_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
    let thick = if response.hovered() || pp.dragging.is_some() {
        8.0
    } else {
        5.0
    };
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), thick));
    let painter = ui.painter();
    painter.rect_filled(track, 2.0, Color32::from_white_alpha(50));
    let pos = pp.dragging.unwrap_or(p.now.pos);
    let accent = ui.visuals().selection.bg_fill;
    painter.rect_filled(
        Rect::from_x_y_ranges(track.left()..=x_of(pos), track.y_range()),
        2.0,
        accent,
    );
    for (i, ch) in p.chapters.iter().enumerate() {
        let end = p.chapters.get(i + 1).map_or(duration, |n| n.start);
        let tint = match ch.part {
            Part::Opening | Part::Ending => Color32::from_rgba_unmultiplied(255, 200, 80, 70),
            _ => continue,
        };
        painter.rect_filled(
            Rect::from_x_y_ranges(x_of(ch.start)..=x_of(end), track.y_range()),
            0.0,
            tint,
        );
    }
    for ch in p.chapters.iter().skip(1) {
        let x = x_of(ch.start);
        painter.line_segment(
            [pos2(x, track.top()), pos2(x, track.bottom())],
            (2.0, Color32::from_black_alpha(200)),
        );
    }
    painter.circle_filled(pos2(x_of(pos), track.center().y), thick, Color32::WHITE);

    if let Some(at) = response
        .interact_pointer_pos()
        .filter(|_| response.dragged())
    {
        let t = t_of(at.x);
        pp.dragging = Some(t);
        let _ = p
            .mpv()
            .command(&["seek", &format!("{t:.3}"), "absolute+keyframes"]);
    }
    if response.drag_stopped() {
        if let Some(t) = pp.dragging.take() {
            p.seek_to(t);
        }
    } else if response.clicked()
        && let Some(at) = response.interact_pointer_pos()
    {
        p.seek_to(t_of(at.x));
    }
    if let Some(hover) = response.hover_pos() {
        let t = t_of(hover.x);
        let chapter = p
            .chapters
            .iter()
            .rev()
            .find(|c| c.start <= t)
            .map(|c| c.title.clone())
            .filter(|c| !c.is_empty());
        let text = match chapter {
            Some(c) => format!("{} · {c}", clock(t)),
            None => clock(t),
        };
        response.on_hover_text_at_pointer(text);
    }
}
