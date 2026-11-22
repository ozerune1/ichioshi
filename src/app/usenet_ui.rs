//! Usenet in the app: your providers, which releases are on Usenet, and the
//! cards for Usenet downloads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText, Ui, vec2};

use super::{
    Action, App, FilesPage, GREEN, Msg, Page, ReleaseKind, fmt_bytes, fmt_duration, fmt_speed,
};
use crate::usenet::indexers::{Indexers, NzbLink};
use crate::usenet::jobs::{Manager, Phase, View};
use crate::usenet::{Config, Provider};

pub(super) struct UsenetState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) manager: Arc<Manager>,
    pub(super) indexers: Indexers,
    /// Each release's NZBs, by info hash; `None` while being looked up.
    pub(super) lookups: HashMap<String, Option<Vec<NzbLink>>>,
    /// Lookups at once.
    limit: Arc<tokio::sync::Semaphore>,
    pub(super) dialog: Option<Dialog>,
}

/// The providers dialog.
#[derive(Default)]
pub(super) struct Dialog {
    /// A copy being edited, and whether it's a new one.
    editing: Option<(Provider, bool)>,
    /// Connection checks, by provider ID.
    checks: HashMap<u64, Check>,
    seq: u64,
}

enum Check {
    Running(u64),
    Done(Result<(), String>),
}

impl UsenetState {
    pub(super) fn load(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        ctx: egui::Context,
        http: reqwest::Client,
    ) -> Self {
        let file = state_dir.join("usenet.json");
        let config = Config::load(&file);
        let indexers = Indexers::new(http);
        let manager = Manager::load(state_dir, rt, &config, indexers.clone(), move || {
            ctx.request_repaint()
        });
        Self {
            config,
            file,
            manager,
            indexers,
            lookups: HashMap::new(),
            limit: Arc::new(tokio::sync::Semaphore::new(6)),
            dialog: None,
        }
    }

    fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
        self.manager.set_config(&self.config);
    }

    pub(super) fn active(&self) -> bool {
        self.config.active()
    }

    /// The release's NZBs, if it's been looked up (empty: not on Usenet).
    pub(super) fn links(&self, info_hash: &str) -> Option<&[NzbLink]> {
        self.lookups
            .get(&info_hash.to_ascii_lowercase())?
            .as_deref()
    }
}

impl App {
    /// Look up the releases on screen on the Usenet indexers (once each).
    pub(super) fn request_nzb_lookups(&mut self) {
        if !self.usenet.active() {
            return;
        }
        let mut wanted: Vec<(String, Option<u64>)> = Vec::new();
        match &self.page {
            Page::Anime(ap) => match (&ap.nyaa, &ap.anime.entry) {
                (Some(search), _) => {
                    for r in search.releases.iter().take(super::MAX_NYAA_ROWS) {
                        match &r.kind {
                            ReleaseKind::Torrent { item, .. } => {
                                wanted.push((item.info_hash.clone(), Some(item.id)))
                            }
                            ReleaseKind::Episodes(items) => {
                                wanted.extend(
                                    items
                                        .iter()
                                        .map(|(_, item)| (item.info_hash.clone(), Some(item.id))),
                                );
                            }
                        }
                    }
                }
                (None, Some(entry)) => {
                    for r in &entry.releases {
                        if let Some(hash) = r.info_hash_hex() {
                            wanted.push((hash.to_owned(), r.nyaa_id()));
                        }
                    }
                }
                (None, None) => {}
            },
            Page::Files(fp) => {
                if let super::Picker::Episodes { items, .. } = &fp.picker {
                    wanted.extend(
                        items
                            .iter()
                            .map(|(_, item)| (item.info_hash.clone(), Some(item.id))),
                    );
                }
            }
            _ => {}
        }
        for (hash, nyaa_id) in wanted {
            let hash = hash.to_ascii_lowercase();
            if hash.is_empty() || self.usenet.lookups.contains_key(&hash) {
                continue;
            }
            self.usenet.lookups.insert(hash.clone(), None);
            let (indexers, limit) = (self.usenet.indexers.clone(), self.usenet.limit.clone());
            self.spawn(async move {
                let _turn = limit.acquire_owned().await;
                let links = indexers.lookup(&hash, nyaa_id).await;
                Msg::NzbLookup { hash, links }
            });
        }
    }

    pub(super) fn handle_usenet_msg(&mut self, msg: Msg) {
        match msg {
            Msg::NzbLookup { hash, links } => {
                self.usenet.lookups.insert(hash, Some(links));
            }
            Msg::UsenetChecked { id, seq, result } => {
                if let Some(d) = &mut self.usenet.dialog
                    && matches!(d.checks.get(&id), Some(Check::Running(s)) if *s == seq)
                {
                    d.checks.insert(id, Check::Done(result));
                }
            }
            _ => {}
        }
    }

    pub(super) fn usenet_check(&mut self, id: u64) {
        let Some(provider) = self
            .usenet
            .config
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
        else {
            return;
        };
        let Some(d) = &mut self.usenet.dialog else {
            return;
        };
        d.seq += 1;
        let seq = d.seq;
        d.checks.insert(id, Check::Running(seq));
        self.spawn(async move {
            let result = crate::usenet::nntp::check(&provider)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::UsenetChecked { id, seq, result }
        });
    }

    /// The Usenet part of the settings menu.
    pub(super) fn usenet_settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.strong("Usenet");
        let providers: Vec<&Provider> = self
            .usenet
            .config
            .providers
            .iter()
            .filter(|p| p.usable())
            .collect();
        if providers.is_empty() {
            ui.weak(
                "Add a Usenet provider to download releases that are on Usenet from there instead.",
            );
        } else {
            for p in providers {
                let backup = if p.backup { " · backup" } else { "" };
                ui.label(format!(
                    "{} · {} connections{backup}",
                    p.label(),
                    p.connections
                ));
            }
            ui.weak("Releases on Usenet (AnimeTosho, TsukiHime) download from there; the rest as torrents.");
        }
        if ui.button("Usenet providers…").clicked() {
            actions.push(Action::UsenetOpenProviders);
            ui.close();
        }
    }

    /// The providers dialog: add, edit, check, order.
    pub(super) fn usenet_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(dialog) = &mut self.usenet.dialog else {
            return;
        };
        let mut close = false;
        let mut changed = false;
        let mut check = None;
        let config = &mut self.usenet.config;
        egui::Modal::new(egui::Id::unique("usenet_providers")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Usenet providers");
            ui.weak(
                "Articles come from the main providers, in this order. Backup (block) accounts are only asked \
                 for articles the main ones don't have.",
            );
            ui.add_space(6.0);
            if let Some((p, new)) = &mut dialog.editing {
                let (mut save, mut cancel) = (false, false);
                provider_form(ui, p, &mut save, &mut cancel);
                if save {
                    if *new {
                        config.add(p.clone());
                    } else if let Some(slot) = config.providers.iter_mut().find(|x| x.id == p.id) {
                        *slot = p.clone();
                        dialog.checks.remove(&p.id);
                    }
                    changed = true;
                }
                if save || cancel {
                    dialog.editing = None;
                }
                return;
            }
            if config.providers.is_empty() {
                ui.label("No providers yet.");
            }
            let count = config.providers.len();
            let mut moves = None;
            let mut remove = None;
            for (i, p) in config.providers.iter_mut().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let label = p.label().to_owned();
                        if ui.checkbox(&mut p.enabled, RichText::new(label).strong()).changed() {
                            changed = true;
                        }
                        let security = if p.tls { "TLS" } else { "no encryption" };
                        let role = if p.backup { " · backup" } else { "" };
                        ui.weak(format!("{}:{} · {security} · {} connections{role}", p.host, p.port, p.connections));
                    });
                    ui.horizontal(|ui| {
                        match dialog.checks.get(&p.id) {
                            Some(Check::Running(_)) => {
                                ui.spinner();
                                ui.label("Connecting…");
                            }
                            Some(Check::Done(Ok(()))) => {
                                ui.colored_label(GREEN, "✔ Connected and logged in");
                            }
                            Some(Check::Done(Err(e))) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            None => {
                                if ui.button("Check").on_hover_text("Connect and log in").clicked() {
                                    check = Some(p.id);
                                }
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Remove").clicked() {
                                remove = Some(i);
                            }
                            if ui.button("Edit").clicked() {
                                dialog.editing = Some((p.clone(), false));
                            }
                            if ui.add_enabled(i + 1 < count, egui::Button::new("Down")).clicked() {
                                moves = Some((i, i + 1));
                            }
                            if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                                moves = Some((i, i - 1));
                            }
                        });
                    });
                });
            }
            if let Some((a, b)) = moves {
                config.providers.swap(a, b);
                changed = true;
            }
            if let Some(i) = remove {
                config.providers.remove(i);
                changed = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Add provider").clicked() {
                    dialog.editing = Some((Provider::default(), true));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });
        if changed {
            self.usenet.save();
        }
        if close {
            self.usenet.dialog = None;
        }
        if let Some(id) = check {
            actions.push(Action::UsenetCheck(id));
        }
    }

    /// The Usenet part of the status bar.
    pub(super) fn usenet_status(&self, ui: &mut Ui) {
        let names: Vec<&str> = self
            .usenet
            .config
            .providers
            .iter()
            .filter(|p| p.usable())
            .map(Provider::label)
            .collect();
        let Some(first) = names.first() else { return };
        let more = if names.len() > 1 {
            format!(" +{}", names.len() - 1)
        } else {
            String::new()
        };
        super::status_dot(ui, GREEN);
        ui.label(RichText::new(format!("Usenet · {first}{more}")).color(GREEN));
    }

    pub(super) fn usenet_edit_files(&mut self, id: u64) {
        match self.usenet.manager.listing(id) {
            Ok(listing) => {
                self.cancel_listing();
                let mut page = FilesPage::new(
                    listing,
                    None,
                    "Change which files this download gets".to_owned(),
                );
                page.nzbs = self.usenet.manager.nzbs(id);
                self.page = Page::Files(page);
            }
            Err(e) => self.set_error(format!("Can't change the files: {e:#}")),
        }
    }

    pub(super) fn usenet_open_remux(&mut self, id: u64) {
        let Some(series) = self.usenet.manager.series_files(id, &self.library_dir()) else {
            return;
        };
        let known = series.info_hashes.iter().find_map(|h| self.library.get(h));
        self.open_remux_series(series, known);
    }

    pub(super) fn usenet_remove(&mut self, id: u64, delete_files: bool) {
        self.usenet.manager.remove(id, delete_files);
        self.set_info(if delete_files {
            "Removed and deleted its files"
        } else {
            "Removed (files kept on disk)"
        });
    }
}

fn provider_form(ui: &mut Ui, p: &mut Provider, save: &mut bool, cancel: &mut bool) {
    egui::Grid::new("usenet_provider_form")
        .num_columns(2)
        .spacing(vec2(10.0, 6.0))
        .show(ui, |ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut p.name)
                    .hint_text("e.g. Eweka")
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("Server");
            ui.add(
                egui::TextEdit::singleline(&mut p.host)
                    .hint_text("news.example.com")
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("Port");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut p.port).range(1..=65535));
                let was = p.tls;
                ui.checkbox(&mut p.tls, "Encrypted (TLS)");
                if p.tls != was {
                    p.port = match (p.tls, p.port) {
                        (true, 119) => 563,
                        (false, 563) => 119,
                        (_, port) => port,
                    };
                }
            });
            ui.end_row();
            ui.label("Username");
            ui.add(egui::TextEdit::singleline(&mut p.username).desired_width(300.0));
            ui.end_row();
            ui.label("Password");
            ui.add(
                egui::TextEdit::singleline(&mut p.password)
                    .password(true)
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("Connections");
            ui.add(egui::DragValue::new(&mut p.connections).range(1..=100))
                .on_hover_text("How many your plan allows (often 20–60). More is faster.");
            ui.end_row();
            ui.label("");
            ui.checkbox(
                &mut p.backup,
                "Backup (block) account: only for articles the others lack",
            );
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!p.host.trim().is_empty(), egui::Button::new("Save"))
            .clicked()
        {
            p.host = p.host.trim().to_owned();
            *save = true;
        }
        if ui.button("Cancel").clicked() {
            *cancel = true;
        }
    });
}

/// "On Usenet" for a release row: the indexers that have it.
pub(super) fn nzb_badge(ui: &mut Ui, links: &[NzbLink]) {
    if links.is_empty() {
        ui.weak("—");
        return;
    }
    let names: Vec<&str> = links.iter().map(|l| l.indexer.name()).collect();
    ui.colored_label(GREEN, "NZB")
        .on_hover_text(format!("On Usenet: {}", names.join(", ")));
}

/// Draw one Usenet download. Returns whether it's busy.
pub(super) fn usenet_card(ui: &mut Ui, v: &View, actions: &mut Vec<Action>) -> bool {
    let job = &v.job;
    let total = job.wanted_bytes();
    let have = v.have_bytes().min(total);
    let source = v.source().unwrap_or("Usenet");
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(&job.name).strong()).truncate())
            .on_hover_text(&job.name);
        let fraction = if total == 0 {
            0.0
        } else {
            have as f32 / total as f32
        };
        let bar = egui::ProgressBar::new(fraction).text(format!(
            "{:.1}%  ·  {} of {}",
            fraction * 100.0,
            fmt_bytes(have),
            fmt_bytes(total)
        ));
        ui.add(bar.animate(matches!(
            v.live.phase,
            Phase::Fetching | Phase::Repairing(_)
        )));
        let detail = match &v.live.phase {
            Phase::Queued => "Waiting for the download before it".to_owned(),
            Phase::Fetching => format!("Getting the NZB from {source}"),
            Phase::Downloading => {
                let mut d = format!("Downloading from Usenet ({source})");
                if v.live.speed > 0.0 {
                    d.push_str(&format!("  ·  ⬇ {}", fmt_speed(v.live.speed / 1_048_576.0)));
                    d.push_str(&format!(
                        "  ·  {} left",
                        fmt_duration(total.saturating_sub(have) as f64 / v.live.speed)
                    ));
                }
                d
            }
            Phase::Repairing(what) => format!("Repairing with PAR2: {what}"),
            Phase::Done => format!("Done · from Usenet ({source})"),
            Phase::Paused => "Paused".to_owned(),
            Phase::Failed(_) => String::new(),
        };
        if !detail.is_empty() {
            ui.label(RichText::new(detail).weak());
        }
        if v.live.missing > 0 && !matches!(v.live.phase, Phase::Done) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} articles aren't on any of your providers; they'll be repaired with PAR2",
                    v.live.missing
                ),
            );
        }
        if let Phase::Failed(e) = &v.live.phase {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if let Some(note) = &v.live.note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }
        ui.horizontal_wrapped(|ui| {
            match &v.live.phase {
                Phase::Queued | Phase::Fetching | Phase::Downloading | Phase::Repairing(_) => {
                    if ui.button("⏸ Pause").clicked() {
                        actions.push(Action::UsenetPause(job.id));
                    }
                }
                Phase::Paused => {
                    if ui.button("▶ Resume").clicked() {
                        actions.push(Action::UsenetResume(job.id));
                    }
                }
                Phase::Failed(_) => {
                    if ui.button("⟳ Retry").clicked() {
                        actions.push(Action::UsenetResume(job.id));
                    }
                }
                Phase::Done => {}
            }
            if ui
                .button("Episodes…")
                .on_hover_text("Change which files this download gets")
                .clicked()
            {
                actions.push(Action::UsenetEditFiles(job.id));
            }
            if ui
                .button("Remux…")
                .on_hover_text("Make copies with only the audio and subtitle languages you want")
                .clicked()
            {
                actions.push(Action::UsenetOpenRemux(job.id));
            }
            if ui.button("🗁 Folder").clicked() {
                actions.push(Action::OpenFolder(job.output_folder.clone()));
            }
            ui.menu_button("Remove", |ui| {
                if ui.button("Remove from list (keep files)").clicked() {
                    actions.push(Action::UsenetRemove {
                        id: job.id,
                        delete_files: false,
                    });
                }
                if ui.button("Remove and delete files…").clicked() {
                    actions.push(Action::UsenetAskDelete {
                        id: job.id,
                        name: job.name.clone(),
                    });
                }
            });
        });
        egui::CollapsingHeader::new("Files")
            .id_salt(("usenet_files", job.id))
            .show(ui, |ui| {
                for (i, f) in job.files.iter().enumerate().filter(|(_, f)| f.wanted) {
                    let fraction = if f.len == 0 {
                        1.0
                    } else {
                        (v.live.have[i] as f32 / f.len as f32).min(1.0)
                    };
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(64.0)
                                .text(format!("{:.0}%", fraction * 100.0)),
                        );
                        ui.add(egui::Label::new(RichText::new(&f.path).weak()).truncate())
                            .on_hover_text(&f.path);
                    });
                }
            });
    });
    v.live.active() || v.live.phase == Phase::Queued
}

#[cfg(test)]
impl UsenetState {
    /// Look releases up on these indexers (mock servers) instead of the real ones.
    pub(super) fn use_indexers_for_test(&mut self, indexers: Indexers) {
        self.indexers = indexers;
    }
}
