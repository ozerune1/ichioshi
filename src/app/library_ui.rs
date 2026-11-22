//! The library in the app: moving finished downloads into their series'
//! folders, and keeping each episode's default tracks in line with the
//! language preferences.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{App, EngineState, Msg};
use crate::library::move_files;

#[derive(Default)]
pub(super) struct LibraryState {
    last_tidy: Option<Instant>,
    /// Downloads being moved into the library (by info hash, or folder).
    moving: HashSet<String>,
    /// Moves that failed: not tried again until the app restarts.
    failed: HashSet<String>,
}

/// A move into the library, ready to run.
struct Move {
    key: String,
    name: String,
    from: PathBuf,
    to: PathBuf,
    files: Vec<String>,
}

impl App {
    /// Every few seconds: move finished downloads that belong to a series,
    /// but aren't in its folder, into it.
    pub(super) fn tidy_library(&mut self) {
        if self.lib.last_tidy.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return;
        }
        self.lib.last_tidy = Some(Instant::now());
        // Torrents must be loaded to know which folders are in use.
        if matches!(self.engine, EngineState::Starting | EngineState::WaitingForVpn(_)) {
            return;
        }
        if !self.settings.migrated {
            self.settings.migrated = true;
            self.move_old_remuxes();
        }
        self.move_finished_torrents();
        self.move_finished_jobs();
    }

    /// Folders never removed, even when a move empties them.
    fn protected_dirs(&self) -> Vec<PathBuf> {
        vec![self.download_dir(), self.library_dir()]
    }

    fn start_move(&mut self, m: Move, before: impl Future<Output = Result<(), String>> + Send + 'static) {
        if self.lib.moving.contains(&m.key) || self.lib.failed.contains(&m.key) {
            return;
        }
        self.lib.moving.insert(m.key.clone());
        let protected = self.protected_dirs();
        self.spawn(async move {
            let result = match before.await {
                Ok(()) => tokio::task::spawn_blocking(move || {
                    move_files(&m.from, &m.to, &m.files, &protected).map_err(|e| format!("{e:#}"))
                })
                .await
                .unwrap_or_else(|e| Err(format!("the move crashed: {e}"))),
                Err(e) => Err(e),
            };
            Msg::Moved { key: m.key, name: m.name, result }
        });
    }

    fn move_finished_torrents(&mut self) {
        let Some(engine) = self.engine() else { return };
        for (id, handle) in engine.torrents() {
            let stats = handle.stats();
            if !stats.finished || matches!(stats.state, librqbit::TorrentStatsState::Initializing { .. }) {
                continue;
            }
            let hash = handle.info_hash().as_string().to_ascii_lowercase();
            let Some(series) = self.library.series_for(&hash) else { continue };
            let from = handle.output_folder().to_owned();
            if from.starts_with(&series.folder) || self.lib.moving.contains(&hash) {
                continue;
            }
            let Ok(listing) = engine.listing_for(&handle) else { continue };
            let only = listing.existing.as_ref().and_then(|e| e.only_files.clone());
            let files = listing.files.iter().filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.index))).map(|f| f.path.clone()).collect();
            let m = Move { key: hash, name: listing.name.clone(), from, to: series.folder.clone(), files };
            // Out of the session first, so nothing writes to the files.
            let engine = engine.clone();
            self.start_move(m, async move { engine.remove(id, false).await.map_err(|e| format!("{e:#}")) });
        }
    }

    fn move_finished_jobs(&mut self) {
        let debrid = self.debrid.manager.clone();
        for v in debrid.views() {
            if v.live.phase != crate::debrid::jobs::Phase::Done {
                continue;
            }
            let Some(series) = self.library.series_for(&v.job.info_hash) else { continue };
            if v.job.output_folder.starts_with(&series.folder) {
                continue;
            }
            let files = v.job.files.iter().filter(|f| f.wanted).map(|f| f.path.clone()).collect();
            let m = Move { key: v.job.info_hash.clone(), name: v.job.name.clone(), from: v.job.output_folder.clone(), to: series.folder.clone(), files };
            let (manager, id) = (debrid.clone(), v.job.id);
            // Off the list first (its files stay), so nothing writes to them.
            self.start_move(m, async move {
                manager.remove(id, false);
                Ok(())
            });
        }
        let usenet = self.usenet.manager.clone();
        for v in usenet.views() {
            if v.live.phase != crate::usenet::jobs::Phase::Done {
                continue;
            }
            let Some(series) = self.library.series_for(&v.job.info_hash) else { continue };
            if v.job.output_folder.starts_with(&series.folder) {
                continue;
            }
            let files = v.job.files.iter().filter(|f| f.wanted).map(|f| f.path.clone()).collect();
            let m = Move { key: v.job.info_hash.clone(), name: v.job.name.clone(), from: v.job.output_folder.clone(), to: series.folder.clone(), files };
            let (manager, id) = (usenet.clone(), v.job.id);
            self.start_move(m, async move {
                manager.remove(id, false);
                Ok(())
            });
        }
    }

    /// Once: remuxes used to go to `<Downloads>/<series title>`; move them
    /// into the series' folder. Folders a download is using are left alone.
    fn move_old_remuxes(&mut self) {
        let old_root = self.download_dir();
        let mut busy: Vec<PathBuf> = self.debrid.manager.views().into_iter().map(|v| v.job.output_folder).collect();
        busy.extend(self.usenet.manager.views().into_iter().map(|v| v.job.output_folder));
        if let Some(engine) = self.engine() {
            busy.extend(engine.torrents().into_iter().map(|(_, h)| h.output_folder().to_owned()));
        }
        let mut moves = Vec::new();
        for series in &self.library.series {
            let names = [crate::remux::sanitize_filename(series.title()), crate::engine::sanitize_folder_name(series.title())];
            for name in names.iter().collect::<HashSet<_>>() {
                let dir = old_root.join(name);
                let in_use = busy.iter().any(|b| b.starts_with(&dir) || dir.starts_with(b));
                if !dir.is_dir() || in_use || series.folder.starts_with(&dir) || dir.starts_with(&series.folder) {
                    continue;
                }
                let files: Vec<String> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && crate::remux::is_video(&e.path()))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                if !files.is_empty() {
                    let key = format!("folder:{}", dir.display());
                    moves.push(Move { key, name: series.title().to_owned(), from: dir, to: series.folder.clone(), files });
                }
            }
        }
        for m in moves {
            self.start_move(m, async { Ok(()) });
        }
    }

    pub(super) fn handle_moved(&mut self, key: String, name: String, result: Result<Vec<(PathBuf, PathBuf)>, String>) {
        self.lib.moving.remove(&key);
        match result {
            Ok(moved) => {
                for (from, to) in &moved {
                    self.playlist.relocate(from, to);
                }
                if !moved.is_empty() {
                    let files = if moved.len() == 1 { "1 file".to_owned() } else { format!("{} files", moved.len()) };
                    self.set_info(format!("Moved {name} into your library ({files})"));
                }
            }
            Err(e) => {
                self.lib.failed.insert(key);
                self.set_error(format!("Couldn't move {name} into your library: {e}"));
            }
        }
    }
}
