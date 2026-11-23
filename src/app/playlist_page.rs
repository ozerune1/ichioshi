//! The playlist page: order episodes and export them as a zip named
//! after the current date and time, the files numbered `1) <title>`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;

use super::{App, Msg, Page};
use crate::playlist::{self, Item};

pub(super) struct PlaylistPage {
    pub(super) out_dir: String,
    pub(super) export: Option<ExportRun>,
    pub(super) adding: bool,
}

pub(super) struct ExportRun {
    seq: u64,
    cancel: Arc<AtomicBool>,
    pub(super) progress: f32,
    pub(super) result: Option<Result<PathBuf, String>>,
}

/// Leaving the page (or closing the app) stops an export in progress.
impl Drop for ExportRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ExportRun {
    pub(super) fn running(&self) -> bool {
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
                .set_title("Add episodes")
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
