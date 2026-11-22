//! Debrid services in the app: the "download with" setting, API keys, and
//! the cards for debrid downloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText, Ui};

use super::{Action, App, EngineState, FilesPage, GREEN, Msg, Page, fmt_bytes, fmt_duration, fmt_speed};
use crate::debrid::jobs::{Manager, Phase, View};
use crate::debrid::{Account, Config, Provider};

pub(super) struct DebridState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) manager: Arc<Manager>,
    /// The last account check for the chosen service.
    pub(super) account: Option<(Provider, Result<Account, String>)>,
    checking: bool,
    seq: u64,
}

impl DebridState {
    pub(super) fn load(state_dir: &Path, rt: tokio::runtime::Handle, ctx: egui::Context) -> Self {
        let file = state_dir.join("debrid.json");
        let config = Config::load(&file);
        let manager = Manager::load(state_dir, rt, config.keys.clone(), move || ctx.request_repaint());
        Self { config, file, manager, account: None, checking: false, seq: 0 }
    }

    fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
        self.manager.set_keys(self.config.keys.clone());
    }

    /// The service's name, when downloads go through one.
    pub(super) fn service(&self) -> Option<&'static str> {
        self.config.enabled.then(|| self.config.provider.name())
    }
}

impl App {
    pub(super) fn handle_debrid_msg(&mut self, msg: Msg) {
        if let Msg::DebridAccount { seq, provider, result } = msg {
            if seq != self.debrid.seq {
                return;
            }
            self.debrid.checking = false;
            self.debrid.account = Some((provider, result));
        }
    }

    /// Check the API key with the service (shows who it belongs to).
    pub(super) fn debrid_check(&mut self) {
        let provider = self.debrid.config.provider;
        let client = match self.debrid.manager.client(provider) {
            Ok(client) => client,
            Err(_) => {
                self.debrid.account = None;
                return;
            }
        };
        self.debrid.seq += 1;
        let seq = self.debrid.seq;
        self.debrid.checking = true;
        self.spawn(async move {
            let result = client.account().await.map_err(|e| format!("{e:#}"));
            Msg::DebridAccount { seq, provider, result }
        });
    }

    pub(super) fn set_debrid_enabled(&mut self, on: bool) {
        self.debrid.config.enabled = on;
        self.debrid.save();
        if on {
            self.debrid_check();
        } else if matches!(self.engine, EngineState::Off) {
            // Back to BitTorrent: it's needed now.
            self.start_engine();
        }
    }

    /// The "Download with" part of the settings menu.
    pub(super) fn debrid_settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.strong("Download with");
        let mut enabled = self.debrid.config.enabled;
        let local = ui.radio_value(&mut enabled, false, "BitTorrent, on this computer");
        let service = ui.radio_value(&mut enabled, true, "A debrid service");
        if local.changed() || service.changed() {
            actions.push(Action::DebridSetEnabled(enabled));
        }
        if !self.debrid.config.enabled {
            return;
        }
        ui.indent("debrid", |ui| {
            ui.horizontal_wrapped(|ui| {
                for p in Provider::ALL {
                    if ui.radio_value(&mut self.debrid.config.provider, p, p.name()).changed() {
                        self.debrid.save();
                        self.debrid.account = None;
                        actions.push(Action::DebridCheck);
                    }
                }
            });
            let provider = self.debrid.config.provider;
            ui.horizontal(|ui| {
                ui.label("API key");
                let key = self.debrid.config.keys.entry(provider).or_default();
                let edit = ui.add(egui::TextEdit::singleline(key).password(true).desired_width(230.0).hint_text("paste it here"));
                if edit.changed() {
                    self.debrid.save();
                    self.debrid.account = None;
                }
                if edit.lost_focus() && self.debrid.config.key(provider).is_some() {
                    actions.push(Action::DebridCheck);
                }
                if ui.add_enabled(self.debrid.config.key(provider).is_some(), egui::Button::new("Check")).clicked() {
                    actions.push(Action::DebridCheck);
                }
            });
            ui.hyperlink_to(format!("Get your {} API key ↗", provider.name()), provider.key_page());
            match &self.debrid.account {
                _ if self.debrid.checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking…");
                    });
                }
                Some((p, Ok(a))) if *p == provider => {
                    let until = a.until.as_deref().map(|u| format!(" until {u}")).unwrap_or_default();
                    if a.premium {
                        ui.colored_label(GREEN, format!("✔ {} · premium{until}", a.name));
                    } else {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("{} · not premium: downloads may not work", a.name));
                    }
                }
                Some((p, Err(e))) if *p == provider => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                _ if self.debrid.config.key(provider).is_none() => {
                    ui.colored_label(ui.visuals().warn_fg_color, "Add your API key to download.");
                }
                _ => {}
            }
            ui.weak(format!(
                "{} fetches each torrent on its servers; this computer then downloads just the files you pick, \
                 over HTTPS. No BitTorrent traffic comes from here.",
                provider.name()
            ));
        });
    }

    /// The service's part of the status bar.
    pub(super) fn debrid_status(&self, ui: &mut Ui) {
        let Some(name) = self.debrid.service() else { return };
        let provider = self.debrid.config.provider;
        let (color, text, hover) = match &self.debrid.account {
            _ if self.debrid.config.key(provider).is_none() => {
                (ui.visuals().warn_fg_color, format!("{name}: add your API key in Settings"), None)
            }
            Some((p, Ok(a))) if *p == provider => (GREEN, format!("{name} · {}", a.name), None),
            Some((p, Err(e))) if *p == provider => (ui.visuals().error_fg_color, format!("{name}: key not accepted"), Some(e.clone())),
            _ => (ui.visuals().text_color(), name.to_owned(), None),
        };
        super::status_dot(ui, color);
        let label = ui.label(RichText::new(text).color(color));
        if let Some(h) = hover {
            label.on_hover_text(h);
        }
    }

    /// Change a debrid download's files.
    pub(super) fn debrid_edit_files(&mut self, id: u64) {
        match self.debrid.manager.listing(id) {
            Ok(listing) => {
                self.cancel_listing();
                let subtitle = "Change which files this download gets".to_owned();
                self.page = Page::Files(FilesPage::new(listing, None, subtitle));
            }
            Err(e) => self.set_error(format!("Can't change the files: {e:#}")),
        }
    }

    pub(super) fn debrid_open_remux(&mut self, id: u64) {
        let Some(series) = self.debrid.manager.series_files(id, &self.download_dir()) else { return };
        let known = series.info_hashes.iter().find_map(|h| self.library.get(h)).cloned();
        self.open_remux_series(series, known);
    }

    pub(super) fn debrid_remove(&mut self, id: u64, delete_files: bool) {
        let service = self.debrid.manager.view(id).map(|v| v.job.provider.name()).unwrap_or("the service");
        self.debrid.manager.remove(id, delete_files);
        self.set_info(if delete_files {
            format!("Removed, deleted its files, and removed it from {service}")
        } else {
            format!("Removed (files kept on disk) and removed it from {service}")
        });
    }
}

/// Draw one debrid download. Returns whether it's busy.
pub(super) fn debrid_card(ui: &mut Ui, v: &View, actions: &mut Vec<Action>) -> bool {
    let job = &v.job;
    let service = job.provider.name();
    let total = job.wanted_bytes();
    let have = v.have_bytes();
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(&job.name).strong()).truncate()).on_hover_text(&job.name);

        let local = |ui: &mut Ui| {
            let fraction = if total == 0 { 0.0 } else { have as f32 / total as f32 };
            ui.add(egui::ProgressBar::new(fraction).text(format!(
                "{:.1}%  ·  {} of {}",
                fraction * 100.0,
                fmt_bytes(have),
                fmt_bytes(total)
            )));
        };
        let mut detail = String::new();
        match &v.live.phase {
            Phase::Adding => {
                ui.add(egui::ProgressBar::new(0.0).animate(true).text(format!("Sending to {service}…")));
                detail = format!("Adding the torrent to {service}");
            }
            Phase::Remote { status, progress, speed, seeders } => {
                let p = progress.unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(p).animate(progress.is_none()).text(format!("{service}: {:.0}%", p * 100.0)));
                detail = format!("{service} is getting it");
                if !status.is_empty() {
                    detail.push_str(&format!(" · {status}"));
                }
                if let Some(s) = speed.filter(|s| *s > 0) {
                    detail.push_str(&format!("  ·  ⬇ {}", fmt_speed(s as f64 / 1_048_576.0)));
                }
                if let Some(n) = seeders {
                    detail.push_str(&format!("  ·  {n} seeders"));
                }
            }
            Phase::Downloading => {
                local(ui);
                detail = format!("Downloading from {service}");
                if v.live.speed > 0.0 {
                    detail.push_str(&format!("  ·  ⬇ {}", fmt_speed(v.live.speed / 1_048_576.0)));
                    let left = (total.saturating_sub(have)) as f64 / v.live.speed;
                    detail.push_str(&format!("  ·  {} left", fmt_duration(left)));
                }
            }
            Phase::Done => {
                local(ui);
                detail = format!("Done · via {service}");
            }
            Phase::Paused => {
                local(ui);
                detail = "Paused".to_owned();
            }
            Phase::Failed(_) => {
                local(ui);
            }
        }
        if !detail.is_empty() {
            ui.label(RichText::new(detail).weak());
        }
        if let Phase::Failed(e) = &v.live.phase {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if let Some(note) = &v.live.note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }

        ui.horizontal_wrapped(|ui| {
            match &v.live.phase {
                Phase::Adding | Phase::Remote { .. } | Phase::Downloading => {
                    if ui.button("⏸ Pause").clicked() {
                        actions.push(Action::DebridPause(job.id));
                    }
                }
                Phase::Paused => {
                    if ui.button("▶ Resume").clicked() {
                        actions.push(Action::DebridResume(job.id));
                    }
                }
                Phase::Failed(_) => {
                    if ui.button("⟳ Retry").clicked() {
                        actions.push(Action::DebridResume(job.id));
                    }
                }
                Phase::Done => {}
            }
            if ui.button("Episodes…").on_hover_text("Change which files this download gets").clicked() {
                actions.push(Action::DebridEditFiles(job.id));
            }
            if ui
                .button("Remux…")
                .on_hover_text("Make copies with only the audio and subtitle languages you want")
                .clicked()
            {
                actions.push(Action::DebridOpenRemux(job.id));
            }
            if ui.button("🗁 Folder").clicked() {
                actions.push(Action::OpenFolder(job.output_folder.clone()));
            }
            ui.menu_button("Remove", |ui| {
                ui.weak(format!("Both also delete it from your {service} account."));
                if ui.button("Remove from list (keep files)").clicked() {
                    actions.push(Action::DebridRemove { id: job.id, delete_files: false });
                }
                if ui.button("Remove and delete files…").clicked() {
                    actions.push(Action::DebridAskDelete { id: job.id, name: job.name.clone() });
                }
            });
        });

        // Like a torrent's file list: a small progress bar, then the path.
        egui::CollapsingHeader::new("Files").id_salt(("debrid_files", job.id)).show(ui, |ui| {
            for (i, f) in job.files.iter().enumerate().filter(|(_, f)| f.wanted) {
                let fraction = if f.len == 0 { 1.0 } else { v.live.have[i] as f32 / f.len as f32 };
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(fraction).desired_width(64.0).text(format!("{:.0}%", fraction * 100.0)));
                    ui.add(egui::Label::new(RichText::new(&f.path).weak()).truncate()).on_hover_text(&f.path);
                });
            }
        });
    });
    v.live.active()
}
