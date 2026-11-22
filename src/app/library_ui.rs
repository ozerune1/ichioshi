//! The library in the app: moving finished downloads into their series'
//! folders, and keeping each episode's default tracks in line with the
//! language preferences.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{App, EngineState, Msg};
use crate::library::{Applied, Series, move_files};
use crate::prefs::LangPrefs;

#[derive(Default)]
pub(super) struct LibraryState {
    last_tidy: Option<Instant>,
    /// Downloads being moved into the library (by info hash, or folder).
    moving: HashSet<String>,
    /// Moves that failed: not tried again until the app restarts.
    failed: HashSet<String>,
    last_defaults: Option<Instant>,
    /// Default tracks are being set on some files.
    applying: bool,
}

/// A file's size and modification time (Unix seconds).
fn stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((meta.len(), modified))
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

    /// The preferences that apply to a series.
    pub(super) fn prefs_for(&self, series: &Series) -> LangPrefs {
        series.prefs.clone().unwrap_or_else(|| self.settings.prefs.clone())
    }

    /// Episodes whose default tracks don't match the preferences yet, or
    /// changed since: `(series, path within its folder, path)`.
    pub(super) fn pending_defaults(&self, series: &Series) -> Vec<(String, PathBuf)> {
        let key = self.prefs_for(series).key();
        let busy = self.busy_files();
        series
            .video_files()
            .into_iter()
            .filter(|p| !busy.contains(p))
            .filter_map(|p| {
                let rel = p.strip_prefix(&series.folder).ok()?.to_string_lossy().into_owned();
                let (len, modified) = stamp(&p)?;
                let done = series.applied.get(&rel).is_some_and(|a| a.key == key && a.len == len && a.modified == modified);
                (!done).then_some((rel, p))
            })
            .collect()
    }

    /// Files a torrent is still writing.
    fn busy_files(&self) -> HashSet<PathBuf> {
        let mut busy = HashSet::new();
        let Some(engine) = self.engine() else { return busy };
        for (_, handle) in engine.torrents() {
            if handle.stats().finished {
                continue;
            }
            if let Ok(listing) = engine.listing_for(&handle) {
                busy.extend(listing.files.iter().map(|f| handle.output_folder().join(&f.path)));
            }
        }
        busy
    }

    /// Look again for episodes to set default tracks on, now.
    pub(super) fn recheck_defaults(&mut self) {
        self.lib.last_defaults = None;
    }

    /// Every few seconds: set default tracks on episodes that need it, one
    /// at a time in the background. Nothing is removed from the files.
    pub(super) fn apply_defaults(&mut self) {
        if self.lib.applying || self.lib.last_defaults.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.lib.last_defaults = Some(Instant::now());
        let mut work: Vec<(u64, String, PathBuf, LangPrefs)> = Vec::new();
        for series in &self.library.series {
            let prefs = self.prefs_for(series);
            for (rel, path) in self.pending_defaults(series) {
                work.push((series.anilist_id, rel, path, prefs.clone()));
            }
        }
        if work.is_empty() {
            return;
        }
        self.lib.applying = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn_blocking(move || {
            for (anilist_id, rel, path, prefs) in work {
                let result = crate::mkv::probe(&path).and_then(|info| {
                    let on = crate::prefs::pick_defaults(&info.tracks, &prefs);
                    crate::mkv::set_default_tracks(&path, &on)
                });
                match &result {
                    Ok(edit) => tracing::info!("default tracks of {}: {edit:?}", path.display()),
                    Err(e) => tracing::warn!("couldn't set the default tracks of {}: {e:#}", path.display()),
                }
                let Some((len, modified)) = stamp(&path) else { continue };
                let applied = Applied { key: prefs.key(), len, modified, error: result.err().map(|e| format!("{e:#}")) };
                let _ = tx.send(Msg::DefaultsApplied { anilist_id, rel, applied });
                ctx.request_repaint();
            }
            let _ = tx.send(Msg::DefaultsDone);
            ctx.request_repaint();
        });
    }

    pub(super) fn handle_defaults_applied(&mut self, anilist_id: u64, rel: String, applied: Applied) {
        if let Some(series) = self.library.find_mut(anilist_id) {
            series.applied.insert(rel, applied);
            self.library.save_or_warn();
        }
    }

    pub(super) fn defaults_done(&mut self) {
        self.lib.applying = false;
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
