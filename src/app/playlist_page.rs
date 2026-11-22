//! The playlist page: order remuxed episodes and export them as a zip named
//! after the current date and time, the files numbered `1) <title>`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;
use eframe::egui::{self, RichText, Ui, vec2};

use super::{Action, App, GREEN, Msg, Page, fmt_bytes};
use crate::playlist::{self, Item};

pub(super) struct PlaylistPage {
    out_dir: String,
    export: Option<ExportRun>,
    adding: bool,
}

pub(super) struct ExportRun {
    seq: u64,
    cancel: Arc<AtomicBool>,
    progress: f32,
    pub(super) result: Option<Result<PathBuf, String>>,
}

/// Leaving the page (or closing the app) stops an export in progress.
impl Drop for ExportRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ExportRun {
    fn running(&self) -> bool {
        self.result.is_none()
    }
}

/// Titles for files added from disk: the title stored in the file, else its name.
fn items_for(mut paths: Vec<PathBuf>) -> (Vec<Item>, Vec<String>) {
    paths.sort();
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match crate::mkv::probe(&path) {
            Ok(info) => {
                let title = info.title.filter(|t| !t.trim().is_empty()).unwrap_or(stem);
                items.push(Item { path, title });
            }
            Err(e) => errors.push(format!("{stem}: {e:#}")),
        }
    }
    (items, errors)
}

impl App {
    pub(super) fn open_playlist(&mut self) {
        if matches!(self.page, Page::Playlist(_)) {
            return;
        }
        self.cancel_listing();
        self.page = Page::Playlist(PlaylistPage {
            out_dir: self.download_dir().display().to_string(),
            export: None,
            adding: false,
        });
    }

    pub(super) fn handle_playlist_msg(&mut self, msg: Msg) {
        match msg {
            Msg::PlaylistAdd { items, errors } => {
                if let Page::Playlist(pp) = &mut self.page {
                    pp.adding = false;
                }
                let offered = items.len();
                let added = self.playlist.add(items);
                if !errors.is_empty() {
                    self.set_error(format!("Couldn't add {}", errors.join("; ")));
                } else if offered > 0 {
                    let skipped = offered - added;
                    let mut text = format!(
                        "Added {added} episode{} to the playlist",
                        if added == 1 { "" } else { "s" }
                    );
                    if skipped > 0 {
                        text.push_str(&format!(" ({skipped} already in it)"));
                    }
                    self.set_info(text);
                }
            }
            Msg::PlaylistExportProgress { seq, fraction } => {
                if let Page::Playlist(PlaylistPage {
                    export: Some(run), ..
                }) = &mut self.page
                    && run.seq == seq
                {
                    run.progress = fraction;
                }
            }
            Msg::PlaylistExportDone { seq, result } => {
                let Page::Playlist(PlaylistPage {
                    export: Some(run), ..
                }) = &mut self.page
                else {
                    return;
                };
                if run.seq != seq {
                    return;
                }
                run.result = Some(result.clone());
                match result {
                    Ok(path) => {
                        self.set_info(format!("Exported the playlist to {}", path.display()));
                        if self.drive.account.upload_after_export && self.drive.ready_to_upload() {
                            self.start_upload(path);
                        }
                    }
                    Err(e) if e == "cancelled" => self.set_info("Export cancelled"),
                    Err(e) => self.set_error(format!("Export failed: {e}")),
                }
            }
            Msg::PlaylistFolderPicked(Some(dir)) => {
                if let Page::Playlist(pp) = &mut self.page {
                    pp.out_dir = dir.display().to_string();
                }
            }
            _ => {}
        }
    }

    pub(super) fn add_playlist_files(&mut self) {
        if let Page::Playlist(pp) = &mut self.page {
            pp.adding = true;
        }
        let start = self.download_dir();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Add remuxed episodes")
                .add_filter("Matroska video", &["mkv"])
                .set_directory(&start)
                .pick_files()
                .await;
            let paths: Vec<PathBuf> = picked
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path().to_owned())
                .collect();
            let (items, errors) = tokio::task::spawn_blocking(move || items_for(paths))
                .await
                .unwrap_or_default();
            Msg::PlaylistAdd { items, errors }
        });
    }

    pub(super) fn pick_export_folder(&mut self) {
        let Page::Playlist(pp) = &self.page else {
            return;
        };
        let start = PathBuf::from(pp.out_dir.trim());
        let start = start
            .ancestors()
            .find(|p| p.is_dir())
            .map(|p| p.to_owned())
            .unwrap_or_else(|| self.download_dir());
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Export the playlist to")
                .set_directory(&start)
                .pick_folder()
                .await;
            Msg::PlaylistFolderPicked(picked.map(|f| f.path().to_owned()))
        });
    }

    pub(super) fn start_export(&mut self) {
        self.playlist_seq += 1;
        let seq = self.playlist_seq;
        let items = self.playlist.items.clone();
        let Page::Playlist(pp) = &mut self.page else {
            return;
        };
        if pp.export.as_ref().is_some_and(ExportRun::running) || items.is_empty() {
            return;
        }
        let dest = PathBuf::from(pp.out_dir.trim());
        let cancel = Arc::new(AtomicBool::new(false));
        pp.export = Some(ExportRun {
            seq,
            cancel: cancel.clone(),
            progress: 0.0,
            result: None,
        });
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn_blocking(move || {
            let send = |msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };
            let mut shown = -1.0f32;
            let result = playlist::export(&items, &dest, Local::now(), &cancel, |fraction| {
                if fraction - shown >= 0.005 || fraction >= 1.0 {
                    shown = fraction;
                    send(Msg::PlaylistExportProgress { seq, fraction });
                }
            });
            send(Msg::PlaylistExportDone {
                seq,
                result: result.map_err(|e| format!("{e:#}")),
            });
        });
    }

    pub(super) fn cancel_export(&mut self) {
        if let Page::Playlist(PlaylistPage {
            export: Some(run), ..
        }) = &self.page
        {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn playlist_page(
        &self,
        ui: &mut Ui,
        pp: &mut PlaylistPage,
        actions: &mut Vec<Action>,
    ) {
        let exporting = pp.export.as_ref().is_some_and(ExportRun::running);
        let items = &self.playlist.items;
        egui::Panel::top("playlist_header")
            .resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!exporting, egui::Button::new("⏴ Back")).clicked() {
                        actions.push(Action::Back);
                    }
                    ui.heading("Playlist");
                });
                ui.weak(
                    "Episodes in the order you want them. Exporting makes a zip named after the current date and time, \
                     holding a folder of the same name with the files numbered “1) title”.",
                );
                ui.add_space(8.0);

                ui.add_enabled_ui(!exporting, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!pp.adding, egui::Button::new("Add files…")).clicked() {
                            actions.push(Action::PlaylistAddFiles);
                        }
                        if pp.adding {
                            ui.spinner();
                        }
                        if !items.is_empty() {
                            ui.menu_button("Clear…", |ui| {
                                if ui.button("Remove everything from the playlist").clicked() {
                                    actions.push(Action::PlaylistClear);
                                }
                                ui.weak("The files themselves are kept.");
                            });
                        }
                    });
                });
            });

        // Below the header, the export and Drive controls may take up to two
        // thirds of the page, scrolling if they need more (e.g. with the
        // setup steps open); the episode list keeps the rest.
        let footer_max = (ui.available_height() * 0.66).max(160.0);
        let names = playlist::export_names(items);
        let sizes: Vec<Option<u64>> = items
            .iter()
            .map(|i| std::fs::metadata(&i.path).ok().map(|m| m.len()))
            .collect();
        let missing = sizes.iter().filter(|s| s.is_none()).count();
        let total: u64 = sizes.iter().flatten().sum();

        egui::Panel::bottom("playlist_footer")
            .resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
            .show(ui, |ui| {
                // The panel lays out in last frame's height; the minimum lets
                // the footer grow to fit its content (up to the cap).
                egui::ScrollArea::vertical()
                    .id_salt("playlist_footer")
                    .max_height(footer_max)
                    .min_scrolled_height(footer_max)
                    .show(ui, |ui| {
                    if !items.is_empty() {
                        ui.add_enabled_ui(!exporting, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Export to");
                                ui.add(egui::TextEdit::singleline(&mut pp.out_dir).desired_width(420.0));
                                if ui.button("Browse…").clicked() {
                                    actions.push(Action::PlaylistPickFolder);
                                }
                            });
                        });
                        let name = playlist::export_name(&Local::now());
                        ui.weak(format!(
                            "Creates “{name}.zip” ({}) with a “{name}” folder inside; the time is taken when you export.",
                            fmt_bytes(total)
                        ));
                        ui.add_space(4.0);
                        ui.horizontal(|ui| match &pp.export {
                            Some(run) if run.running() => {
                                ui.add(egui::ProgressBar::new(run.progress).desired_width(260.0).show_percentage());
                                if ui.button("Cancel").clicked() {
                                    actions.push(Action::CancelExport);
                                }
                            }
                            _ => {
                                let label = format!("Export {} episode{}", items.len(), if items.len() == 1 { "" } else { "s" });
                                let button = egui::Button::new(RichText::new(label).strong()).min_size(vec2(170.0, 32.0));
                                let can = missing == 0 && !pp.out_dir.trim().is_empty();
                                if ui.add_enabled(can, button).clicked() {
                                    actions.push(Action::StartExport);
                                }
                                if missing > 0 {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        format!("{missing} file{} missing: remove or re-add {}", if missing == 1 { " is" } else { "s are" }, if missing == 1 { "it" } else { "them" }),
                                    );
                                }
                                if let Some(Some(Ok(path))) = pp.export.as_ref().map(|r| &r.result) {
                                    ui.colored_label(GREEN, format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                                    if ui.button("🗁 Open folder").clicked() {
                                        actions.push(Action::OpenFolder(path.parent().map(|p| p.to_owned()).unwrap_or_default()));
                                    }
                                }
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                    }
                    let exported = pp.export.as_ref().and_then(|r| r.result.as_ref()).and_then(|r| r.as_ref().ok());
                    self.drive_section(ui, exported, actions);
                });
            });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if items.is_empty() {
                    ui.label("Nothing here yet. Remux a series and click “Add to playlist”, or add remuxed files above.");
                    return;
                }
                ui.add_enabled_ui(!exporting, |ui| {
                    egui::Grid::new("playlist").striped(true).num_columns(3).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
                        for (i, (item, name)) in items.iter().zip(&names).enumerate() {
                            let shown = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
                            ui.label(shown).on_hover_text(item.path.display().to_string());
                            match sizes[i] {
                                Some(size) => ui.weak(fmt_bytes(size)),
                                None => ui.colored_label(ui.visuals().error_fg_color, "File missing"),
                            };
                            ui.horizontal(|ui| {
                                if ui.add_enabled(i > 0, egui::Button::new("⏶").small()).on_hover_text("Move up").clicked() {
                                    actions.push(Action::PlaylistMove { from: i, to: i - 1 });
                                }
                                if ui
                                    .add_enabled(i + 1 < items.len(), egui::Button::new("⏷").small())
                                    .on_hover_text("Move down")
                                    .clicked()
                                {
                                    actions.push(Action::PlaylistMove { from: i, to: i + 1 });
                                }
                                if ui.small_button("✖").on_hover_text("Remove from the playlist").clicked() {
                                    actions.push(Action::PlaylistRemove(i));
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
            });
        });
    }
}

#[cfg(test)]
impl PlaylistPage {
    pub(super) fn exported_for_test(&mut self, path: PathBuf) {
        self.export = Some(ExportRun {
            seq: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: 1.0,
            result: Some(Ok(path)),
        });
    }
}
