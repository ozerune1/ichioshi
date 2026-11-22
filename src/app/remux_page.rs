//! The remux page: for a downloaded series, pick the audio and subtitle
//! language (and track, when a language has several) and write copies that
//! keep only those, named after the anime's English AniList title.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, RichText, Ui, vec2};

use super::{Action, App, GREEN, Msg, Page};
use crate::engine::{SeriesFiles, TorrentHandle};
use crate::library::AnimeRef;
use crate::prefs::{LangPrefs, SubType, sub_type_of};
use crate::remux::{
    self, Analysis, Choice, Kind, PlannedFile, SubtitleSample, TrackSig, language_name,
};
use crate::{seadex, title};

pub(super) struct RemuxPage {
    pub(super) series: SeriesFiles,
    pub(super) title: String,
    /// Where the title came from.
    pub(super) title_note: String,
    pub(super) out_dir: String,
    /// The folder was typed or picked, so it no longer follows the title.
    pub(super) out_dir_edited: bool,
    pub(super) seq: u64,
    pub(super) analysis: Option<Analysis>,
    /// What each subtitle choice contains, once read.
    pub(super) samples: HashMap<TrackSig, SubtitleSample>,
    pub(super) error: Option<String>,
    pub(super) audio_lang: Option<String>,
    pub(super) audio: Option<TrackSig>,
    /// `None` for no subtitles.
    pub(super) sub_lang: Option<String>,
    pub(super) subs: Option<TrackSig>,
    pub(super) run: Option<RemuxRun>,
    /// The series, when known: its language preferences pick the tracks.
    anilist_id: Option<u64>,
    /// The series page it was opened from, to go back to.
    pub(super) back: Option<super::AnimePage>,
}

pub(super) struct RemuxRun {
    pub(super) states: Vec<FileState>,
    /// What's being remuxed, parallel to `states`.
    plan: Vec<PlannedFile>,
    cancel: Arc<AtomicBool>,
    pub(super) finished: bool,
}

/// Leaving the page (or closing the app) stops a remux in progress.
impl Drop for RemuxRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub(super) enum FileState {
    Waiting,
    Running(f32),
    Done,
    Failed(String),
    Skipped(String),
    Cancelled,
}

impl RemuxPage {
    pub(super) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.finished)
    }

    pub(super) fn plan(&self) -> Vec<PlannedFile> {
        let (Some(analysis), Some(audio)) = (&self.analysis, &self.audio) else {
            return Vec::new();
        };
        let title = self.title.trim();
        if title.is_empty() {
            return Vec::new();
        }
        let choice = Choice {
            audio: audio.clone(),
            subtitles: self.subs.clone(),
        };
        remux::plan(analysis, &choice, title, Path::new(self.out_dir.trim()))
    }

    /// Pick the suggested languages and tracks once the files are read.
    /// Start from the language preferences, where the files have those
    /// languages; otherwise from what the files suggest.
    fn apply_defaults(&mut self, prefs: &LangPrefs) {
        let Some(a) = &self.analysis else { return };
        let has = |kind, code: &str| a.languages(kind).iter().any(|l| l.code == code);
        self.audio_lang = if has(Kind::Audio, &prefs.audio) {
            Some(prefs.audio.clone())
        } else {
            a.default_audio_language()
        };
        self.audio = self
            .audio_lang
            .as_deref()
            .and_then(|l| a.default_candidate(Kind::Audio, l, None));
        self.sub_lang = match &prefs.subtitles {
            None => None,
            Some(l) if has(Kind::Subtitles, l) => Some(l.clone()),
            Some(_) => a.default_subtitle_language(),
        };
        let audio_lang = self.audio_lang.clone();
        self.subs = self.sub_lang.as_deref().and_then(|l| match prefs.sub_type {
            SubType::Auto => a.default_candidate(Kind::Subtitles, l, audio_lang.as_deref()),
            wanted => {
                let candidates = a.candidates(Kind::Subtitles, l);
                wanted.fallbacks().iter().find_map(|kind| {
                    candidates
                        .iter()
                        .find(|c| {
                            sub_type_of(&c.sig.name, c.forced, c.hearing_impaired, c.commentary)
                                == *kind
                        })
                        .map(|c| c.sig.clone())
                })
            }
        });
    }

    pub(super) fn refresh_audio_track(&mut self) {
        let Some(a) = &self.analysis else { return };
        self.audio = self
            .audio_lang
            .as_deref()
            .and_then(|l| a.default_candidate(Kind::Audio, l, None));
        // The best subtitles depend on the audio (full vs signs only).
        self.refresh_subtitle_track();
    }

    pub(super) fn refresh_subtitle_track(&mut self) {
        let Some(a) = &self.analysis else { return };
        self.subs = self
            .sub_lang
            .as_deref()
            .and_then(|l| a.default_candidate(Kind::Subtitles, l, self.audio_lang.as_deref()));
    }
}

/// Remuxed copies go in `<library>/Remuxed/<title>`, apart from the series'
/// own folder (which keeps the downloads, all tracks intact).
pub(super) fn default_out_dir(library_dir: &Path, title: &str) -> String {
    let title = title.trim();
    let name = if title.is_empty() {
        "Remuxes".to_owned()
    } else {
        remux::sanitize_filename(title)
    };
    library_dir.join("Remuxed").join(name).display().to_string()
}

impl App {
    pub(super) fn open_remux(&mut self, handle: TorrentHandle) {
        let Some(engine) = self.engine() else { return };
        match engine.series_files(&handle, &self.library_dir()) {
            Ok(series) => {
                let known = series.info_hashes.iter().find_map(|h| self.library.get(h));
                self.open_remux_series(series, known);
            }
            Err(e) => self.set_error(format!("Can't remux this yet: {e:#}")),
        }
    }

    /// Show the remux page for `series`, reading its tracks and, if the
    /// anime isn't known, looking its title up on AniList.
    pub(super) fn open_remux_series(&mut self, series: SeriesFiles, known: Option<AnimeRef>) {
        self.remux_seq += 1;
        let seq = self.remux_seq;
        let (title, title_note) = match &known {
            Some(a) if a.english.is_some() => (
                a.title().unwrap_or_default().to_owned(),
                "English title from AniList".to_owned(),
            ),
            Some(a) => (
                a.title().unwrap_or_default().to_owned(),
                "AniList has no English title for this, so it's the romaji one".to_owned(),
            ),
            None => (String::new(), "Looking up the title on AniList…".to_owned()),
        };
        let files = series.files.clone();
        let query = title::anilist_query(&series.name);
        self.cancel_listing();
        self.page = Page::Remux(RemuxPage {
            out_dir: default_out_dir(&self.library_dir(), &title),
            series,
            title,
            title_note,
            out_dir_edited: false,
            seq,
            analysis: None,
            samples: HashMap::new(),
            error: None,
            audio_lang: None,
            audio: None,
            sub_lang: None,
            subs: None,
            run: None,
            anilist_id: known.as_ref().map(|a| a.anilist_id),
            back: None,
        });
        if !files.is_empty() {
            self.spawn(async move {
                let result =
                    match tokio::task::spawn_blocking(move || remux::probe_all(files)).await {
                        Ok(r) => r.map_err(|e| format!("{e:#}")),
                        Err(e) => Err(e.to_string()),
                    };
                Msg::RemuxProbed { seq, result }
            });
        }
        if known.is_none() {
            let client = self.http.clone();
            self.spawn(async move {
                let result = seadex::anilist_search(&client, &query)
                    .await
                    .ok()
                    .and_then(|r| r.into_iter().next());
                Msg::RemuxTitle {
                    seq,
                    found: result.map(|a| AnimeRef {
                        anilist_id: a.anilist_id,
                        english: a.english,
                        romaji: a.romaji,
                    }),
                }
            });
        }
    }

    pub(super) fn handle_remux_msg(&mut self, msg: Msg) {
        let download_dir = self.library_dir();
        let prefs = match &self.page {
            Page::Remux(rp) => rp
                .anilist_id
                .and_then(|id| self.library.find(id))
                .map_or_else(|| self.settings.prefs.clone(), |s| self.prefs_for(s)),
            _ => return,
        };
        let Page::Remux(rp) = &mut self.page else {
            return;
        };
        let mut sample = None;
        match msg {
            Msg::RemuxProbed { seq, result } if seq == rp.seq => match result {
                Ok(analysis) => {
                    sample = Some((seq, analysis.clone()));
                    rp.analysis = Some(analysis);
                    rp.apply_defaults(&prefs);
                }
                Err(e) => rp.error = Some(e),
            },
            Msg::RemuxSamples { seq, samples } if seq == rp.seq => {
                rp.samples = samples.into_iter().collect()
            }
            Msg::RemuxTitle { seq, found } if seq == rp.seq && rp.title.trim().is_empty() => {
                match found {
                    Some(anime) => {
                        rp.title = anime.title().unwrap_or_default().to_owned();
                        let matched = anime
                            .romaji
                            .as_deref()
                            .or(anime.english.as_deref())
                            .unwrap_or_default();
                        rp.title_note = format!(
                            "Matched on AniList: {matched}. Edit it if that's the wrong anime."
                        );
                        if anime.english.is_none() {
                            rp.title_note.push_str(" (No English title on AniList.)");
                        }
                        if !rp.out_dir_edited {
                            rp.out_dir = default_out_dir(&download_dir, &rp.title);
                        }
                    }
                    None => {
                        rp.title_note =
                            "Couldn't find this on AniList; type the English title.".to_owned()
                    }
                }
            }
            Msg::RemuxProgress {
                seq,
                index,
                fraction,
            } if seq == rp.seq => {
                if let Some(state) = rp.run.as_mut().and_then(|r| r.states.get_mut(index)) {
                    *state = FileState::Running(fraction);
                }
            }
            Msg::RemuxFileDone { seq, index, result } if seq == rp.seq => {
                if let Some(state) = rp.run.as_mut().and_then(|r| r.states.get_mut(index)) {
                    *state = match result {
                        Ok(()) => FileState::Done,
                        Err(e) if e == "cancelled" => FileState::Cancelled,
                        Err(e) => FileState::Failed(e),
                    };
                }
            }
            Msg::RemuxFinished { seq } if seq == rp.seq => {
                let Some(run) = rp.run.as_mut() else { return };
                run.finished = true;
                for state in &mut run.states {
                    if matches!(state, FileState::Waiting | FileState::Running(_)) {
                        *state = FileState::Cancelled;
                    }
                }
                let done = run
                    .states
                    .iter()
                    .filter(|s| matches!(s, FileState::Done))
                    .count();
                let failed = run
                    .states
                    .iter()
                    .filter(|s| matches!(s, FileState::Failed(_)))
                    .count();
                let text = format!(
                    "Remuxed {done} episode{} into {}",
                    plural(done),
                    rp.out_dir.trim()
                );
                if failed > 0 {
                    self.set_error(format!("{text}; {failed} failed"));
                } else {
                    self.set_info(text);
                }
            }
            Msg::RemuxFolderPicked(Some(dir)) => {
                rp.out_dir = dir.display().to_string();
                rp.out_dir_edited = true;
            }
            _ => {}
        }
        // Then read what the subtitle tracks contain, in the background.
        if let Some((seq, analysis)) = sample {
            self.spawn(async move {
                let samples =
                    tokio::task::spawn_blocking(move || remux::sample_subtitles(&analysis))
                        .await
                        .unwrap_or_default();
                Msg::RemuxSamples { seq, samples }
            });
        }
    }

    pub(super) fn start_remux(&mut self) {
        let Page::Remux(rp) = &mut self.page else {
            return;
        };
        if rp.running() {
            return;
        }
        let plan = rp.plan();
        if plan.iter().all(|p| p.problem().is_some()) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        rp.run = Some(RemuxRun {
            states: plan
                .iter()
                .map(|p| p.problem().map_or(FileState::Waiting, FileState::Skipped))
                .collect(),
            plan: plan.clone(),
            cancel: cancel.clone(),
            finished: false,
        });
        let seq = rp.seq;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        // Remuxing is file I/O: one blocking thread works through the episodes.
        self.rt.spawn_blocking(move || {
            let send = |msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };
            for (index, item) in plan.iter().enumerate() {
                if item.problem().is_some() {
                    continue;
                }
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let mut shown = -1.0f32;
                let result = remux::remux(item, &cancel, |fraction| {
                    // Progress comes per cluster; update the UI every half percent.
                    if fraction - shown >= 0.005 || fraction >= 1.0 {
                        shown = fraction;
                        send(Msg::RemuxProgress {
                            seq,
                            index,
                            fraction,
                        });
                    }
                });
                send(Msg::RemuxFileDone {
                    seq,
                    index,
                    result: result.map_err(|e| format!("{e:#}")),
                });
            }
            send(Msg::RemuxFinished { seq });
        });
    }

    /// Add the episodes this run remuxed, in order, to the playlist.
    pub(super) fn add_remuxes_to_playlist(&mut self) {
        let Page::Remux(RemuxPage { run: Some(run), .. }) = &self.page else {
            return;
        };
        let items: Vec<crate::playlist::Item> = run
            .plan
            .iter()
            .zip(&run.states)
            .filter(|(_, s)| matches!(s, FileState::Done))
            .map(|(p, _)| crate::playlist::Item {
                path: p.output.clone(),
                title: p.title.clone(),
            })
            .collect();
        self.handle_playlist_msg(Msg::PlaylistAdd {
            items,
            errors: Vec::new(),
        });
    }

    pub(super) fn cancel_remux(&mut self) {
        if let Page::Remux(RemuxPage { run: Some(run), .. }) = &self.page {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn pick_remux_folder(&mut self) {
        let Page::Remux(rp) = &self.page else { return };
        let start = PathBuf::from(rp.out_dir.trim());
        let start = start
            .ancestors()
            .find(|p| p.is_dir())
            .map(Path::to_owned)
            .unwrap_or_else(|| self.download_dir());
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Save remuxes in")
                .set_directory(&start)
                .pick_folder()
                .await;
            Msg::RemuxFolderPicked(picked.map(|f| f.path().to_owned()))
        });
    }

    pub(super) fn remux_page(&self, ui: &mut Ui, rp: &mut RemuxPage, actions: &mut Vec<Action>) {
        let locked = rp.running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!locked, egui::Button::new("⏴ Back"))
                .clicked()
            {
                actions.push(Action::Back);
            }
            ui.heading("Remux");
        });
        ui.add(egui::Label::new(RichText::new(&rp.series.name).strong()).truncate());
        let ready = rp.series.files.len();
        let mut summary = format!("{ready} finished episode{}", plural(ready));
        if rp.series.not_ready > 0 {
            summary.push_str(&format!(
                " · {} still downloading (not included)",
                rp.series.not_ready
            ));
        }
        ui.weak(summary);
        if ready == 0 {
            ui.add_space(8.0);
            ui.label("Nothing to remux yet: wait for an episode to finish downloading.");
            return;
        }
        ui.add_space(8.0);

        let download_dir = self.library_dir();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_enabled_ui(!locked, |ui| {
                egui::Grid::new("remux_settings").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                    ui.label("Title");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut rp.title)
                            .hint_text("English title")
                            .desired_width(460.0),
                    );
                    if edit.changed() && !rp.out_dir_edited {
                        rp.out_dir = default_out_dir(&download_dir, &rp.title);
                    }
                    ui.end_row();
                    ui.label("");
                    let note = rp.title_note.trim_end_matches('.');
                    ui.weak(format!("{note}. Files are named “<title> - 01.mkv” and titled the same."));
                    ui.end_row();
                    ui.label("Save to");
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut rp.out_dir).desired_width(460.0)).changed() {
                            rp.out_dir_edited = true;
                        }
                        if ui.button("Browse…").clicked() {
                            actions.push(Action::PickRemuxFolder);
                        }
                    });
                    ui.end_row();
                });
            });
            ui.add_space(10.0);

            if let Some(e) = &rp.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't read the episodes' tracks: {e}"));
                return;
            }
            let Some(analysis) = rp.analysis.clone() else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading tracks…");
                });
                return;
            };

            if !analysis.unreadable.is_empty() {
                let names: Vec<String> = analysis
                    .unreadable
                    .iter()
                    .map(|(p, _)| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                    .collect();
                let why = analysis.unreadable.iter().map(|(_, e)| e.as_str()).collect::<Vec<_>>().join("\n");
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Can't remux {} (only MKV files are supported): {}", names.len(), names.join(", ")),
                )
                .on_hover_text(why);
                ui.add_space(6.0);
            }
            ui.add_enabled_ui(!locked, |ui| {
                ui.heading("Audio");
                let before = rp.audio_lang.clone();
                language_combo(ui, "remux_audio_lang", &analysis, Kind::Audio, &mut rp.audio_lang, false);
                if rp.audio_lang != before {
                    rp.refresh_audio_track();
                }
                if let Some(lang) = rp.audio_lang.clone() {
                    track_choice(ui, &analysis, Kind::Audio, &lang, &mut rp.audio, &rp.samples);
                }
                ui.add_space(8.0);
                ui.heading("Subtitles");
                let before = rp.sub_lang.clone();
                language_combo(ui, "remux_sub_lang", &analysis, Kind::Subtitles, &mut rp.sub_lang, true);
                if rp.sub_lang != before {
                    rp.refresh_subtitle_track();
                }
                if let Some(lang) = rp.sub_lang.clone() {
                    track_choice(ui, &analysis, Kind::Subtitles, &lang, &mut rp.subs, &rp.samples);
                }
                ui.weak("Every other audio and subtitle track is left out. Video and chapters are kept.");
            });
            ui.add_space(10.0);

            let plan = rp.plan();
            remux_controls(ui, rp, &plan, actions);
            ui.add_space(8.0);
            ui.heading("Episodes");
            episode_table(ui, rp, &plan);
        });
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn language_combo(
    ui: &mut Ui,
    id: &str,
    analysis: &Analysis,
    kind: Kind,
    selected: &mut Option<String>,
    allow_none: bool,
) {
    let total = analysis.files.len();
    let langs = analysis.languages(kind);
    let label = |code: &str| {
        let files = langs.iter().find(|l| l.code == code).map_or(0, |l| l.files);
        if files == total {
            language_name(code)
        } else {
            format!("{} ({files} of {total} episodes)", language_name(code))
        }
    };
    let current = selected.as_deref().map_or_else(|| "None".to_owned(), label);
    ui.horizontal(|ui| {
        ui.label("Language");
        egui::ComboBox::from_id_salt(id)
            .width(280.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                if allow_none {
                    ui.selectable_value(selected, None, "None (no subtitles)");
                }
                for l in &langs {
                    ui.selectable_value(selected, Some(l.code.clone()), label(&l.code));
                }
            });
    });
}

/// Radio buttons when a language has more than one track. Subtitle choices
/// show how many lines they have and how they start, which tells "signs
/// only" from "full dialogue" from "dub transcript" better than names do.
fn track_choice(
    ui: &mut Ui,
    analysis: &Analysis,
    kind: Kind,
    lang: &str,
    chosen: &mut Option<TrackSig>,
    samples: &HashMap<TrackSig, SubtitleSample>,
) {
    let candidates = analysis.candidates(kind, lang);
    if candidates.len() < 2 {
        return;
    }
    let total = analysis.files.len();
    ui.label(format!(
        "{} has {} tracks; keep:",
        language_name(lang),
        candidates.len()
    ));
    ui.indent(("tracks", lang, format!("{kind:?}")), |ui| {
        for c in candidates {
            let mut text = c.label();
            let sample = samples.get(&c.sig);
            if let Some(s) = sample {
                text.push_str(&format!(
                    " · {} line{}",
                    s.events,
                    if s.events == 1 { "" } else { "s" }
                ));
            }
            if c.files < total {
                text.push_str(&format!(" · in {} of {total} episodes", c.files));
            }
            ui.radio_value(chosen, Some(c.sig.clone()), text);
            if let Some(s) = sample.filter(|s| !s.lines.is_empty()) {
                let preview = s
                    .lines
                    .iter()
                    .map(|l| format!("“{l}”"))
                    .collect::<Vec<_>>()
                    .join("  ");
                ui.indent(("preview", &c.sig.name, &c.sig.codec), |ui| {
                    ui.add(egui::Label::new(RichText::new(preview).weak().italics()).truncate());
                });
            }
        }
        if kind == Kind::Subtitles && samples.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Reading what each track contains…");
            });
        }
    });
}

fn remux_controls(ui: &mut Ui, rp: &RemuxPage, plan: &[PlannedFile], actions: &mut Vec<Action>) {
    let runnable = plan.iter().filter(|p| p.problem().is_none()).count();
    ui.horizontal(|ui| match &rp.run {
        Some(run) if !run.finished => {
            let progress: f32 = run
                .states
                .iter()
                .map(|s| match s {
                    FileState::Done => 1.0,
                    FileState::Running(f) => *f,
                    _ => 0.0,
                })
                .sum::<f32>()
                / runnable.max(1) as f32;
            ui.add(egui::ProgressBar::new(progress).desired_width(260.0).show_percentage());
            if ui.button("Cancel").clicked() {
                actions.push(Action::CancelRemux);
            }
        }
        _ => {
            let button = egui::Button::new(RichText::new(format!("Remux {runnable} episode{}", plural(runnable))).strong())
                .min_size(vec2(170.0, 32.0));
            if ui
                .add_enabled(runnable > 0, button)
                .on_hover_text("Writes new files; the downloads are left as they are. Earlier remuxes with the same names are replaced.")
                .clicked()
            {
                actions.push(Action::StartRemux);
            }
            if let Some(run) = rp.run.as_ref().filter(|r| r.finished) {
                if ui.button("🗁 Open folder").clicked() {
                    actions.push(Action::OpenFolder(PathBuf::from(rp.out_dir.trim())));
                }
                let done = run.states.iter().filter(|s| matches!(s, FileState::Done)).count();
                if done > 0
                    && ui
                        .button(format!("Add {done} to playlist"))
                        .on_hover_text("Add these remuxes, in order, to the playlist")
                        .clicked()
                {
                    actions.push(Action::AddRemuxesToPlaylist);
                }
            }
            if rp.title.trim().is_empty() {
                ui.weak("Enter a title first.");
            }
        }
    });
}

fn episode_table(ui: &mut Ui, rp: &RemuxPage, plan: &[PlannedFile]) {
    let warn = ui.visuals().warn_fg_color;
    let error = ui.visuals().error_fg_color;
    egui::Grid::new("remux_episodes")
        .striped(true)
        .num_columns(4)
        .spacing(vec2(14.0, 6.0))
        .show(ui, |ui| {
            for header in ["New file", "Audio", "Subtitles", ""] {
                ui.strong(header);
            }
            ui.end_row();
            for (i, item) in plan.iter().enumerate() {
                let name = item
                    .output
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                ui.label(&name)
                    .on_hover_text(format!("From {}", item.input.display()));
                ui.label(
                    item.audio
                        .as_ref()
                        .map_or("—".to_owned(), |t| t.short_label()),
                );
                ui.label(
                    item.subtitles
                        .as_ref()
                        .map_or("None".to_owned(), |t| t.short_label()),
                );
                match rp.run.as_ref().and_then(|r| r.states.get(i)) {
                    Some(FileState::Waiting) => {
                        ui.weak("Waiting");
                    }
                    Some(FileState::Running(f)) => {
                        ui.add(
                            egui::ProgressBar::new(*f)
                                .desired_width(120.0)
                                .show_percentage(),
                        );
                    }
                    Some(FileState::Done) => {
                        ui.colored_label(GREEN, "Done");
                    }
                    Some(FileState::Failed(e)) => {
                        ui.colored_label(error, "Failed").on_hover_text(e);
                    }
                    Some(FileState::Skipped(why)) => {
                        ui.colored_label(warn, format!("Skipped: {why}"));
                    }
                    Some(FileState::Cancelled) => {
                        ui.weak("Cancelled");
                    }
                    None => match item.problem() {
                        Some(problem) => {
                            ui.colored_label(warn, format!("Will skip: {problem}"));
                        }
                        None if !item.notes.is_empty() => {
                            ui.colored_label(warn, item.notes.join("; "));
                        }
                        None => {
                            ui.label("");
                        }
                    },
                }
                ui.end_row();
            }
        });
}
