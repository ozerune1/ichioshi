//! The library in the app: moving finished downloads into their series'
//! folders, and keeping each episode's default tracks in line with the
//! language preferences.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{App, EngineState, Msg};
use crate::library::{Applied, Original, Series, move_files};
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
    /// Episode files' releases are being read.
    describing: bool,
    last_describe: Option<Instant>,
}

/// A file's size and modification time (Unix seconds).
fn stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
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

/// Episode `n`, as the page shows it.
pub(super) struct EpisodeRow {
    pub(super) n: u32,
    pub(super) title: String,
    pub(super) airs: Option<i64>,
    pub(super) out: bool,
    /// Its file (within the folder) and size.
    pub(super) file: Option<(String, u64)>,
    pub(super) downloading: Option<f32>,
}

impl App {
    /// Every episode (in the library, downloading, out or not), and the
    /// files in the folder that aren't episodes.
    /// The series' episode numbers on disk (as the stats last counted them).
    pub(super) fn on_disk(&self, id: u64) -> std::collections::BTreeSet<u32> {
        self.pages
            .stats
            .get(&id)
            .map(|s| s.episodes.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Every episode of the series (in the library, downloading, out or
    /// not), and the files in its folder that aren't episodes.
    pub(super) fn episode_rows(&self, ap: &AnimePage) -> (Vec<EpisodeRow>, Vec<(String, u64)>) {
        let id = ap.anime.anilist_id;
        let details = self.details(id);
        let stats = self.pages.stats.get(&id).cloned().unwrap_or_default();
        let coverage = self.coverage(ap);
        let now = chrono::Utc::now().timestamp();
        let on_disk = stats.episodes.clone();
        let extras: Vec<(String, u64)> = stats
            .files
            .iter()
            .filter(|f| !on_disk.values().any(|e| e == *f))
            .cloned()
            .collect();
        let last = [
            details
                .and_then(|d| d.episodes)
                .or(ap.anime.episodes)
                .unwrap_or(0),
            details
                .and_then(|d| d.episode_list.iter().map(|e| e.number).max())
                .unwrap_or(0),
            on_disk.keys().max().copied().unwrap_or(0),
            stats.progress.keys().max().copied().unwrap_or(0),
            coverage.last(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let episodes = (1..=last)
            .map(|n| {
                let airs = details.and_then(|d| {
                    d.episode(n).and_then(|e| e.airs).or(d
                        .next_episode
                        .filter(|(next, _)| *next == n)
                        .map(|(_, t)| t))
                });
                let out = match airs {
                    Some(t) => t <= now,
                    None => details
                        .and_then(crate::library::Details::aired)
                        .is_none_or(|aired| n <= aired),
                };
                EpisodeRow {
                    n,
                    title: details
                        .and_then(|d| d.episode(n))
                        .and_then(|e| e.title.clone())
                        .unwrap_or_else(|| format!("Episode {n}")),
                    airs,
                    out,
                    file: on_disk.get(&n).cloned(),
                    downloading: stats.progress.get(&n).copied(),
                }
            })
            .collect();
        (episodes, extras)
    }

    /// Every few seconds: move finished downloads that belong to a series,
    /// but aren't in its folder, into it.
    pub(super) fn tidy_library(&mut self) {
        if self
            .lib
            .last_tidy
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.lib.last_tidy = Some(Instant::now());
        // Torrents must be loaded to know which folders are in use.
        if matches!(
            self.engine,
            EngineState::Starting | EngineState::WaitingForVpn(_)
        ) {
            return;
        }
        // The library's folders are on this computer only in its own
        // library (the servers' have none here, except 18+ series', which
        // are downloaded here and moved in like any).
        if !self.servers_mode() && !self.settings.migrated {
            self.settings.migrated = true;
            self.move_old_remuxes();
        }
        self.move_finished_torrents();
        self.move_finished_jobs();
        self.hand_over_failed();
    }

    /// Now and then: read the releases of episode files not read yet (or
    /// changed), a few at a time, off the UI thread.
    pub(super) fn describe_episodes(&mut self) {
        if self.lib.describing
            || self
                .lib
                .last_describe
                .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.lib.last_describe = Some(Instant::now());
        let mut todo = Vec::new();
        'all: for s in &self.library.series {
            // (A series on the servers has no files here.)
            if self.on_servers(s.anilist_id) {
                continue;
            }
            let Some(stats) = self.pages.stats.get(&s.anilist_id) else {
                continue;
            };
            for (rel, len) in stats.episodes.values() {
                if s.releases.get(rel).is_none_or(|d| d.len != *len) {
                    todo.push((s.anilist_id, s.folder.join(rel), rel.clone(), *len));
                    if todo.len() >= 16 {
                        break 'all;
                    }
                }
            }
        }
        if todo.is_empty() {
            return;
        }
        self.lib.describing = true;
        self.spawn(async move {
            let read = tokio::task::spawn_blocking(move || {
                todo.into_iter()
                    .map(|(id, path, rel, len)| {
                        let r = crate::library::describe_file(&path, &rel);
                        (id, rel, len, r)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
            Msg::Described(read)
        });
    }

    pub(super) fn handle_described(
        &mut self,
        read: Vec<(u64, String, u64, crate::library::EpisodeRelease)>,
    ) {
        self.lib.describing = false;
        for (id, rel, len, release) in read {
            if let Some(s) = self.library.find_mut(id) {
                s.releases
                    .insert(rel, crate::library::Described { len, release });
            }
        }
        // Files no longer there take theirs with them.
        for s in &mut self.library.series {
            if let Some(stats) = self.pages.stats.get(&s.anilist_id) {
                s.releases
                    .retain(|rel, _| stats.files.iter().any(|(f, _)| f == rel));
            }
        }
        self.library.save_or_warn();
    }

    /// An episode's release, as far as it's known.
    pub(super) fn episode_release(
        &self,
        id: u64,
        n: u32,
    ) -> Option<&crate::library::EpisodeRelease> {
        let s = self.library.find(id)?;
        if let Some(r) = &s.remote {
            return r.episodes.get(&n)?.release.as_ref();
        }
        let (rel, len) = self.pages.stats.get(&id)?.episodes.get(&n)?;
        s.releases
            .get(rel)
            .filter(|d| d.len == *len)
            .map(|d| &d.release)
    }

    /// The preferences that apply to a series.
    pub(super) fn prefs_for(&self, series: &Series) -> LangPrefs {
        series
            .prefs
            .clone()
            .unwrap_or_else(|| self.settings.prefs.clone())
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
                let rel = p
                    .strip_prefix(&series.folder)
                    .ok()?
                    .to_string_lossy()
                    .into_owned();
                let (len, modified) = stamp(&p)?;
                let done = series
                    .applied
                    .get(&rel)
                    .is_some_and(|a| a.key == key && a.len == len && a.modified == modified);
                (!done).then_some((rel, p))
            })
            .collect()
    }

    /// Files a torrent is still writing.
    pub(super) fn busy_files(&self) -> HashSet<PathBuf> {
        let mut busy = HashSet::new();
        let Some(engine) = self.engine() else {
            return busy;
        };
        for (_, handle) in engine.torrents() {
            if handle.stats().finished {
                continue;
            }
            // Only the files it's downloading: others of the same name may
            // be episodes already in the library.
            let only = handle.only_files();
            if let Ok(listing) = engine.listing_for(&handle) {
                busy.extend(
                    listing
                        .files
                        .iter()
                        .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.index)))
                        .map(|f| handle.output_folder().join(&f.path)),
                );
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
        if self.lib.applying
            || self
                .lib
                .last_defaults
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.lib.last_defaults = Some(Instant::now());
        let mut work: Vec<(u64, String, PathBuf, LangPrefs)> = Vec::new();
        for series in &self.library.series {
            if self.on_servers(series.anilist_id) {
                continue;
            }
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
                    crate::mkv::set_default_tracks_keeping(&path, &on)
                });
                match &result {
                    Ok((edit, _)) => {
                        tracing::info!("default tracks of {}: {edit:?}", path.display())
                    }
                    Err(e) => tracing::warn!(
                        "couldn't set the default tracks of {}: {e:#}",
                        path.display()
                    ),
                }
                let Some((len, modified)) = stamp(&path) else {
                    continue;
                };
                let (original, error) = match result {
                    Ok((_, was)) => (was.map(|(at, b)| Original::new(at, &b)), None),
                    Err(e) => (None, Some(format!("{e:#}"))),
                };
                let applied = Applied {
                    key: prefs.key(),
                    len,
                    modified,
                    error,
                    original,
                };
                let _ = tx.send(Msg::DefaultsApplied {
                    anilist_id,
                    rel,
                    applied,
                });
                ctx.request_repaint();
            }
            let _ = tx.send(Msg::DefaultsDone);
            ctx.request_repaint();
        });
    }

    pub(super) fn handle_defaults_applied(
        &mut self,
        anilist_id: u64,
        rel: String,
        applied: Applied,
    ) {
        if let Some(series) = self.library.find_mut(anilist_id) {
            // What the file had before its first edit stays: a later edit
            // (other languages) only replaced the first one's bytes. A file
            // of another size is another file.
            let mut applied = applied;
            if let Some(before) = series.applied.get(&rel)
                && before.len == applied.len
                && before.original.is_some()
            {
                applied.original = before.original.clone();
            }
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

    fn start_move(
        &mut self,
        m: Move,
        before: impl Future<Output = Result<(), String>> + Send + 'static,
    ) {
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
            Msg::Moved {
                key: m.key,
                name: m.name,
                result,
            }
        });
    }

    fn move_finished_torrents(&mut self) {
        let Some(engine) = self.engine() else { return };
        for (id, handle) in engine.torrents() {
            let stats = handle.stats();
            if !stats.finished
                || matches!(
                    stats.state,
                    librqbit::TorrentStatsState::Initializing { .. }
                )
            {
                continue;
            }
            let hash = handle.info_hash().as_string().to_ascii_lowercase();
            let Some(series) = self.library.series_for(&hash) else {
                continue;
            };
            let from = handle.output_folder().to_owned();
            if self.lib.moving.contains(&hash) {
                continue;
            }
            let Ok(listing) = engine.listing_for(&handle) else {
                continue;
            };
            let only = listing.existing.as_ref().and_then(|e| e.only_files.clone());
            // Already in the series' folder: just off the list, it's imported.
            let files = if from.starts_with(&series.folder) {
                Vec::new()
            } else {
                listing
                    .files
                    .iter()
                    .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.index)))
                    .map(|f| f.path.clone())
                    .collect()
            };
            let m = Move {
                key: hash,
                name: listing.name.clone(),
                from,
                to: series.folder.clone(),
                files,
            };
            // Out of the session first, so nothing writes to the files.
            let engine = engine.clone();
            self.start_move(m, async move {
                engine.remove(id, false).await.map_err(|e| format!("{e:#}"))
            });
        }
    }

    fn move_finished_jobs(&mut self) {
        let debrid = self.debrid.manager.clone();
        for v in debrid.views() {
            if v.live.phase != crate::debrid::jobs::Phase::Done {
                continue;
            }
            let Some(series) = self.library.series_for(&v.job.info_hash) else {
                continue;
            };
            // Already in the series' folder: just off the list, it's imported.
            let files = if v.job.output_folder.starts_with(&series.folder) {
                Vec::new()
            } else {
                v.job
                    .files
                    .iter()
                    .filter(|f| f.wanted)
                    .map(|f| f.path.clone())
                    .collect()
            };
            let m = Move {
                key: v.job.info_hash.clone(),
                name: v.job.name.clone(),
                from: v.job.output_folder.clone(),
                to: series.folder.clone(),
                files,
            };
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
            let Some(series) = self.library.series_for(&v.job.info_hash) else {
                continue;
            };
            let files = if v.job.output_folder.starts_with(&series.folder) {
                Vec::new()
            } else {
                v.job
                    .files
                    .iter()
                    .filter(|f| f.wanted)
                    .map(|f| f.path.clone())
                    .collect()
            };
            let m = Move {
                key: v.job.info_hash.clone(),
                name: v.job.name.clone(),
                from: v.job.output_folder.clone(),
                to: series.folder.clone(),
                files,
            };
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
        let mut busy: Vec<PathBuf> = self
            .debrid
            .manager
            .views()
            .into_iter()
            .map(|v| v.job.output_folder)
            .collect();
        busy.extend(
            self.usenet
                .manager
                .views()
                .into_iter()
                .map(|v| v.job.output_folder),
        );
        if let Some(engine) = self.engine() {
            busy.extend(
                engine
                    .torrents()
                    .into_iter()
                    .map(|(_, h)| h.output_folder().to_owned()),
            );
        }
        let mut moves = Vec::new();
        for series in &self.library.series {
            let names = [
                crate::remux::sanitize_filename(series.title()),
                crate::engine::sanitize_folder_name(series.title()),
            ];
            for name in names.iter().collect::<HashSet<_>>() {
                let dir = old_root.join(name);
                let in_use = busy
                    .iter()
                    .any(|b| b.starts_with(&dir) || dir.starts_with(b));
                if !dir.is_dir()
                    || in_use
                    || series.folder.starts_with(&dir)
                    || dir.starts_with(&series.folder)
                {
                    continue;
                }
                let files: Vec<String> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| {
                        e.file_type().is_ok_and(|t| t.is_file())
                            && crate::remux::is_video(&e.path())
                    })
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                if !files.is_empty() {
                    let key = format!("folder:{}", dir.display());
                    moves.push(Move {
                        key,
                        name: series.title().to_owned(),
                        from: dir,
                        to: series.folder.clone(),
                        files,
                    });
                }
            }
        }
        for m in moves {
            self.start_move(m, async { Ok(()) });
        }
    }

    pub(super) fn handle_moved(
        &mut self,
        key: String,
        name: String,
        result: Result<Vec<(PathBuf, PathBuf)>, String>,
    ) {
        self.lib.moving.remove(&key);
        match result {
            Ok(moved) => {
                for (from, to) in &moved {
                    self.playlist.relocate(from, to);
                }
                if moved.is_empty() {
                    self.set_info(format!("{name} is in your library"));
                } else {
                    let files = if moved.len() == 1 {
                        "1 file".to_owned()
                    } else {
                        format!("{} files", moved.len())
                    };
                    self.set_info(format!("Moved {name} into your library ({files})"));
                }
                self.pages.last_stats = None;
            }
            Err(e) => {
                self.lib.failed.insert(key);
                self.set_error(format!("Couldn't move {name} into your library: {e}"));
            }
        }
    }
}

// ── Pages ──────────────────────────────────────────────────────────────

use std::collections::{BTreeMap, HashMap};

use super::{AnimePage, Origin, Page, ReleaseKind};
use crate::engine::Listing;
use crate::library::Details;
use crate::nyaa::{NyaaItem, TorrentId};
use crate::seadex::Anime;

/// What the library page shows about a series (refreshed every few seconds).
#[derive(Clone, Default)]
pub(super) struct SeriesStats {
    /// Videos on disk: path within the folder, and size.
    pub(super) files: Vec<(String, u64)>,
    /// Which of them is each episode.
    pub(super) episodes: BTreeMap<u32, (String, u64)>,
    pub(super) bytes: u64,
    /// Episodes whose default tracks aren't set yet.
    pub(super) pending: usize,
    /// Something is downloading for it.
    pub(super) downloading: bool,
    /// Episodes downloading, and how far along (0–1).
    pub(super) progress: BTreeMap<u32, f32>,
}

/// The chosen release's files, once its .torrent is read.
pub(super) enum ReleaseFiles {
    Loading,
    Ready(Listing),
    Failed(String),
}

/// What the chosen release has, episode by episode.
pub(super) enum Coverage<'a> {
    /// No release to download from; the text says why.
    Nothing(&'static str),
    Loading,
    Failed(&'a str),
    /// One torrent: each episode's file in it.
    Batch {
        listing: &'a Listing,
        files: BTreeMap<u32, usize>,
    },
    /// One torrent per episode.
    PerEpisode(BTreeMap<u32, &'a NyaaItem>),
    /// A streaming site's episodes (18+ anime).
    Stream(BTreeMap<u32, &'a crate::streams::StreamEpisode>),
}

impl Coverage<'_> {
    pub(super) fn has(&self, episode: u32) -> bool {
        match self {
            Coverage::Batch { files, .. } => files.contains_key(&episode),
            Coverage::PerEpisode(items) => items.contains_key(&episode),
            Coverage::Stream(eps) => eps.contains_key(&episode),
            _ => false,
        }
    }

    pub(super) fn last(&self) -> u32 {
        match self {
            Coverage::Batch { files, .. } => files.keys().max().copied().unwrap_or(0),
            Coverage::PerEpisode(items) => items.keys().max().copied().unwrap_or(0),
            Coverage::Stream(eps) => eps.keys().max().copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Why an episode can't be downloaded.
    pub(super) fn why_not(&self) -> String {
        match self {
            Coverage::Nothing(why) => (*why).to_owned(),
            Coverage::Loading => "Reading the release's files…".to_owned(),
            Coverage::Failed(e) => format!("Couldn't read the release: {e}"),
            _ => "The release doesn't have this episode; pick another one below".to_owned(),
        }
    }
}

#[derive(Default)]
pub(super) struct PagesState {
    pub(super) stats: HashMap<u64, SeriesStats>,
    pub(super) last_stats: Option<Instant>,
    /// AniList details being fetched, and when fetches failed.
    pub(super) details_loading: HashSet<u64>,
    details_failed: HashMap<u64, Instant>,
    /// Details of anime not in the library.
    pub(super) details: HashMap<u64, Details>,
    /// The library's languages, while being changed.
    pub(super) languages: Option<LangPrefs>,
    /// The files of releases on series pages, by Nyaa ID.
    pub(super) release_files: HashMap<TorrentId, ReleaseFiles>,
}

impl App {
    /// Recount each series' episodes now and then.
    pub(super) fn refresh_library_stats(&mut self) {
        let every = if self.pages.stats.values().any(|s| s.downloading) {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(2)
        };
        if self.pages.last_stats.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.pages.last_stats = Some(Instant::now());
        // On the servers, what they have; 18+ series are counted here, from
        // their folders, as in a library of your own.
        let servers = self.servers_mode();
        if servers {
            self.servers_stats();
            if !self.library.series.iter().any(|s| s.adult) {
                return;
            }
        }
        let mut active: HashSet<String> = HashSet::new();
        if let Some(engine) = self.engine() {
            for (_, h) in engine.torrents() {
                if !h.stats().finished {
                    active.insert(h.info_hash().as_string().to_ascii_lowercase());
                }
            }
        }
        active.extend(
            self.debrid
                .manager
                .views()
                .into_iter()
                .filter(|v| v.live.active())
                .map(|v| v.job.info_hash),
        );
        active.extend(
            self.usenet
                .manager
                .views()
                .into_iter()
                .filter(|v| v.live.active() || v.live.phase == crate::usenet::jobs::Phase::Queued)
                .map(|v| v.job.info_hash),
        );
        // How far along each downloading file is, by info hash.
        let mut files_progress: HashMap<String, Vec<(String, u64, f32)>> = HashMap::new();
        if let Some(engine) = self.engine() {
            for (_, h) in engine.torrents() {
                let stats = h.stats();
                if stats.finished {
                    continue;
                }
                let Ok(listing) = engine.listing_for(&h) else {
                    continue;
                };
                let only = h.only_files();
                let entry = files_progress
                    .entry(h.info_hash().as_string().to_ascii_lowercase())
                    .or_default();
                for f in listing
                    .files
                    .iter()
                    .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.index)))
                {
                    let have = stats.file_progress.get(f.index).copied().unwrap_or(0);
                    entry.push((
                        f.path.clone(),
                        f.len,
                        if f.len == 0 {
                            1.0
                        } else {
                            have as f32 / f.len as f32
                        },
                    ));
                }
            }
        }
        for v in self
            .debrid
            .manager
            .views()
            .into_iter()
            .filter(|v| v.live.active())
        {
            let entry = files_progress.entry(v.job.info_hash.clone()).or_default();
            for (i, f) in v.job.files.iter().enumerate().filter(|(_, f)| f.wanted) {
                entry.push((
                    f.path.clone(),
                    f.len,
                    if f.len == 0 {
                        1.0
                    } else {
                        v.live.have[i] as f32 / f.len as f32
                    },
                ));
            }
        }
        for v in self
            .usenet
            .manager
            .views()
            .into_iter()
            .filter(|v| v.live.active() || v.live.phase == crate::usenet::jobs::Phase::Queued)
        {
            let entry = files_progress.entry(v.job.info_hash.clone()).or_default();
            for (i, f) in v.job.files.iter().enumerate().filter(|(_, f)| f.wanted) {
                entry.push((
                    f.path.clone(),
                    f.len,
                    if f.len == 0 {
                        1.0
                    } else {
                        v.live.have[i] as f32 / f.len as f32
                    },
                ));
            }
        }
        let busy = self.busy_files();
        // Episodes downloading from streaming sites, into series' folders.
        let streaming: Vec<crate::streams::View> = self
            .streams
            .views()
            .into_iter()
            .filter(|v| v.live.active())
            .collect();
        let mut stats = HashMap::new();
        for series in &self.library.series {
            if servers && !series.adult {
                continue;
            }
            // Files a torrent is still writing aren't episodes yet.
            let files: Vec<(String, u64)> = series
                .video_files()
                .iter()
                .filter(|p| !busy.contains(*p))
                .filter_map(|p| {
                    let rel = p
                        .strip_prefix(&series.folder)
                        .ok()?
                        .to_string_lossy()
                        .into_owned();
                    Some((rel, std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)))
                })
                .collect();
            let one = series.one_episode();
            let mut episodes = BTreeMap::new();
            let named: Vec<(&str, u64)> = files.iter().map(|(r, l)| (r.as_str(), *l)).collect();
            for (f, ep) in files
                .iter()
                .zip(crate::title::episodes_of_files(&named, one))
            {
                if let Some(ep) = ep {
                    episodes.entry(ep).or_insert_with(|| f.clone());
                }
            }
            let mut progress = BTreeMap::new();
            for hash in &series.downloads {
                let list = files_progress.get(hash).map(Vec::as_slice).unwrap_or(&[]);
                let named: Vec<(&str, u64)> =
                    list.iter().map(|(p, len, _)| (p.as_str(), *len)).collect();
                for ((_, _, done), ep) in list
                    .iter()
                    .zip(crate::title::episodes_of_files(&named, one))
                {
                    if let Some(ep) = ep {
                        progress.insert(ep, done.clamp(0.0, 1.0));
                    }
                }
            }
            let mut streamed = false;
            for v in streaming.iter().filter(|v| v.job.folder == series.folder) {
                streamed = true;
                progress.insert(v.job.episode.episode, v.live.fraction().unwrap_or(0.0));
            }
            let s = SeriesStats {
                bytes: files.iter().map(|f| f.1).sum(),
                episodes,
                files,
                pending: self.pending_defaults(series).len(),
                downloading: streamed || series.downloads.iter().any(|h| active.contains(h)),
                progress,
            };
            stats.insert(series.anilist_id, s);
        }
        if servers {
            self.pages.stats.extend(stats);
        } else {
            self.pages.stats = stats;
        }
    }

    /// AniList details for what's on screen, a couple at a time.
    pub(super) fn request_details(&mut self) {
        let wanted: Vec<u64> = match &self.page {
            Page::Home => self.library.series.iter().map(|s| s.anilist_id).collect(),
            Page::Anime(ap) => vec![ap.anime.anilist_id],
            _ => Vec::new(),
        };
        let now = chrono::Utc::now().timestamp();
        for id in wanted {
            if self.pages.details_loading.len() >= 2 {
                break;
            }
            // Airing shows change (new episodes, titles); finished ones rarely do.
            let fresh = self.details(id).is_some_and(|d| {
                let max_age = if d.status.as_deref() == Some("RELEASING") {
                    6 * 3600
                } else {
                    3 * 86_400
                };
                now - d.fetched < max_age && d.recommended
            });
            let failed_lately = self
                .pages
                .details_failed
                .get(&id)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(300));
            if fresh || failed_lately || self.pages.details_loading.contains(&id) {
                continue;
            }
            self.pages.details_loading.insert(id);
            let client = self.http.clone();
            self.spawn(async move {
                let result = async {
                    let mut details = crate::seadex::anilist_details(&client, id).await?;
                    // Kitsu has episode titles for many shows AniList doesn't.
                    let titled = details
                        .episode_list
                        .iter()
                        .filter(|e| e.title.is_some())
                        .count();
                    if titled < details.episodes.or(details.aired()).unwrap_or(0) as usize {
                        match crate::seadex::kitsu_episodes(&client, id).await {
                            Ok(more) => {
                                crate::seadex::merge_episodes(&mut details.episode_list, more)
                            }
                            Err(e) => tracing::warn!("Kitsu episodes for {id}: {e:#}"),
                        }
                    }
                    anyhow::Ok(Box::new(details))
                }
                .await
                .map_err(|e| format!("{e:#}"));
                Msg::Details { id, result }
            });
        }
    }

    pub(super) fn details(&self, id: u64) -> Option<&Details> {
        self.library
            .find(id)
            .and_then(|s| s.details.as_ref())
            .or_else(|| self.pages.details.get(&id))
    }

    pub(super) fn handle_details(&mut self, id: u64, result: Result<Box<Details>, String>) {
        self.pages.details_loading.remove(&id);
        match result {
            Ok(details) => {
                if let Some(series) = self.library.find_mut(id) {
                    series.episodes = series.episodes.or(details.episodes);
                    series.cover = series.cover.take().or(details.cover.clone());
                    series.details = Some(*details);
                    self.library.save_or_warn();
                } else {
                    self.pages.details.insert(id, *details);
                }
            }
            Err(e) => {
                tracing::warn!("AniList details for {id}: {e}");
                self.pages.details_failed.insert(id, Instant::now());
            }
        }
    }

    /// Open a series from the library: its page, with its SeaDex releases
    /// looked up.
    pub(super) fn open_series(&mut self, id: u64) {
        let Some(series) = self.library.find(id) else {
            return;
        };
        let d = series.details.as_ref();
        let romaji = series.romaji.clone();
        let english = series.english.clone();
        let title = series.title().to_owned();
        let alt_title = romaji.clone().filter(|r| Some(r) != english.as_ref());
        let anime = Anime {
            anilist_id: id,
            title,
            alt_title,
            romaji,
            english,
            format: series.format.clone().or(d.and_then(|d| d.format.clone())),
            year: series.year.or(d.and_then(|d| d.year)),
            episodes: series.episodes.or(d.and_then(|d| d.episodes)),
            cover_small: series.cover.clone(),
            cover_large: series.cover.clone().or(d.and_then(|d| d.cover.clone())),
            entry: None,
            adult: series.adult,
        };
        self.look_up_and_show(anime);
        if let Page::Anime(ap) = &mut self.page {
            ap.origin = Origin::Library;
        }
    }

    /// An anime's page, while its SeaDex entry is looked up (then its best
    /// release is picked).
    pub(super) fn look_up_and_show(&mut self, anime: Anime) {
        let id = anime.anilist_id;
        self.cancel_listing();
        self.page = Page::Anime(AnimePage {
            anime,
            chosen: None,
            nyaa: None,
            origin: Origin::Results,
            entry_loading: true,
            back: None,
        });
        let client = self.http.clone();
        self.spawn(async move {
            let result = crate::seadex::fetch_entries(&client, &[id])
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::SeriesEntry {
                id,
                result: result.map(|mut m| m.remove(&id)),
            }
        });
    }

    pub(super) fn handle_series_entry(
        &mut self,
        id: u64,
        result: Result<Option<crate::seadex::Entry>, String>,
    ) {
        let Page::Anime(ap) = &mut self.page else {
            return;
        };
        if ap.anime.anilist_id != id || !ap.entry_loading {
            return;
        }
        ap.entry_loading = false;
        match result {
            Ok(entry) => {
                ap.anime.entry = entry;
                let (anime, origin, back) = (ap.anime.clone(), ap.origin, ap.back.take());
                self.show_anime(anime, origin);
                // Where Back goes stays as it was.
                if let Page::Anime(ap) = &mut self.page {
                    ap.back = back;
                }
            }
            Err(e) => self.set_error(format!("Couldn't look it up on SeaDex: {e}")),
        }
    }

    pub(super) fn add_to_library(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let new = super::new_series(&ap.anime);
        let root = self.library_dir();
        let title = ap.anime.title.clone();
        let series = self.library.add(&new, &root);
        if let Some(details) = self.pages.details.remove(&series.anilist_id) {
            series.details = Some(details);
            self.library.save_or_warn();
        }
        self.pages.last_stats = None;
        self.set_info(format!("Added {title} to your library"));
    }

    pub(super) fn remove_series(&mut self, id: u64, delete_files: bool) {
        let Some(series) = self.library.remove(id) else {
            return;
        };
        if delete_files {
            match std::fs::remove_dir_all(&series.folder) {
                Ok(()) => {
                    self.set_info(format!("Removed {} and deleted its folder", series.title()))
                }
                Err(e) => self.set_error(format!(
                    "Removed {}, but couldn't delete {}: {e}",
                    series.title(),
                    series.folder.display()
                )),
            }
        } else {
            self.set_info(format!(
                "Removed {} from your library (its files are still in {})",
                series.title(),
                series.folder.display()
            ));
        }
        if let Some(d) = series.details {
            self.pages.details.insert(id, d);
        }
        if matches!(&self.page, Page::Anime(ap) if ap.anime.anilist_id == id && ap.opened_from_library())
        {
            self.page = Page::Home;
        }
    }

    /// Remux episodes from the library; Back comes back to the series.
    pub(super) fn remux_library(&mut self, id: u64, files: Vec<PathBuf>) {
        let Some(series) = self.library.find(id) else {
            return;
        };
        let name = series.title().to_owned();
        let known = series.anime_ref();
        let info_hashes = series.downloads.iter().cloned().collect();
        let back = match std::mem::replace(&mut self.page, Page::Home) {
            Page::Anime(ap) => Some(ap),
            other => {
                self.page = other;
                None
            }
        };
        self.open_remux_series(
            crate::engine::SeriesFiles {
                name,
                files,
                not_ready: 0,
                info_hashes,
            },
            Some(known),
        );
        if let Page::Remux(rp) = &mut self.page {
            rp.back = back;
        }
    }

    pub(super) fn set_series_prefs(&mut self, id: u64, prefs: Option<LangPrefs>) {
        if let Some(series) = self.library.find_mut(id) {
            series.prefs = prefs;
            self.library.save_or_warn();
        }
        self.recheck_defaults();
        self.pages.last_stats = None;
    }

    /// The Nyaa torrent of the release chosen on a series page, if it's one
    /// torrent (per-episode releases don't need reading).
    fn chosen_torrent(ap: &AnimePage) -> Option<TorrentId> {
        match &ap.nyaa {
            Some(search) => match &search.releases.get(search.chosen?)?.kind {
                ReleaseKind::Torrent { item, .. } => Some(item.torrent()),
                ReleaseKind::Episodes(_) | ReleaseKind::Stream { .. } => None,
            },
            None => super::chosen_release(ap)?.nyaa_id().map(TorrentId::nyaa),
        }
    }

    /// Read the chosen release's file list (once), to know its episodes.
    pub(super) fn request_release_files(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let Some(id) = Self::chosen_torrent(ap) else {
            return;
        };
        if self.pages.release_files.contains_key(&id) {
            return;
        }
        self.pages.release_files.insert(id, ReleaseFiles::Loading);
        let client = self.http.clone();
        self.spawn(async move {
            let result = async {
                crate::engine::listing_from_bytes(crate::nyaa::fetch_torrent(&client, id).await?)
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::ReleaseFiles { id, result }
        });
    }

    pub(super) fn handle_release_files(&mut self, id: TorrentId, result: Result<Listing, String>) {
        let files = match result {
            Ok(listing) => ReleaseFiles::Ready(listing),
            Err(e) => ReleaseFiles::Failed(e),
        };
        self.pages.release_files.insert(id, files);
    }

    /// Whether the anime on a page is one episode (a film).
    pub(super) fn one_episode(&self, ap: &AnimePage) -> bool {
        let id = ap.anime.anilist_id;
        if let Some(s) = self.library.find(id) {
            return s.one_episode();
        }
        let d = self.details(id);
        ap.anime.episodes.or(d.and_then(|d| d.episodes)) == Some(1)
            || ap
                .anime
                .format
                .as_deref()
                .or(d.and_then(|d| d.format.as_deref()))
                == Some("MOVIE")
    }

    /// What the release chosen on a series page has, episode by episode.
    pub(super) fn coverage<'a>(&'a self, ap: &'a AnimePage) -> Coverage<'a> {
        if let Some(search) = &ap.nyaa
            && let Some(r) = search.chosen.and_then(|i| search.releases.get(i))
        {
            match &r.kind {
                ReleaseKind::Episodes(items) => {
                    return Coverage::PerEpisode(items.iter().map(|(e, i)| (*e, i)).collect());
                }
                ReleaseKind::Stream { episodes, .. } => {
                    return Coverage::Stream(episodes.iter().map(|e| (e.episode, e)).collect());
                }
                ReleaseKind::Torrent { .. } => {}
            }
        }
        let Some(id) = Self::chosen_torrent(ap) else {
            let searching = ap.entry_loading || ap.nyaa.as_ref().is_some_and(|s| s.loading);
            return Coverage::Nothing(if searching {
                "Looking for releases…"
            } else {
                "No release to download from"
            });
        };
        match self.pages.release_files.get(&id) {
            None | Some(ReleaseFiles::Loading) => Coverage::Loading,
            Some(ReleaseFiles::Failed(e)) => Coverage::Failed(e),
            Some(ReleaseFiles::Ready(listing)) => {
                let mut files = BTreeMap::new();
                let named: Vec<(&str, u64)> = listing
                    .files
                    .iter()
                    .map(|f| (f.path.as_str(), f.len))
                    .collect();
                let eps = crate::title::episodes_of_files(&named, self.one_episode(ap));
                for (f, ep) in listing.files.iter().zip(eps) {
                    if crate::remux::is_video(Path::new(&f.path))
                        && let Some(ep) = ep
                    {
                        files.entry(ep).or_insert(f.index);
                    }
                }
                Coverage::Batch { listing, files }
            }
        }
    }

    /// Download some of a series' episodes from the chosen release, into
    /// its folder (adding it to the library).
    pub(super) fn download_episodes(&mut self, episodes: Vec<u32>) {
        let Page::Anime(ap) = &self.page else { return };
        let new = super::new_series(&ap.anime);
        let root = self.library_dir();
        let folder = self.library.folder_for(&new, &root);
        let title = ap.anime.title.clone();
        enum Plan {
            Files(Listing, Vec<usize>),
            Items(Vec<(u32, NyaaItem)>),
            Stream(Vec<crate::streams::StreamEpisode>),
        }
        let plan = match self.coverage(ap) {
            Coverage::Batch { listing, files } => Plan::Files(
                listing.clone(),
                episodes
                    .iter()
                    .filter_map(|e| files.get(e))
                    .copied()
                    .collect(),
            ),
            Coverage::PerEpisode(items) => Plan::Items(
                episodes
                    .iter()
                    .filter_map(|e| items.get(e).map(|i| (*e, (*i).clone())))
                    .collect(),
            ),
            Coverage::Stream(eps) => Plan::Stream(
                episodes
                    .iter()
                    .filter_map(|e| eps.get(e).map(|ep| (*ep).clone()))
                    .collect(),
            ),
            other => {
                let why = other.why_not();
                self.set_error(why);
                return;
            }
        };
        match plan {
            Plan::Files(listing, mut files) => {
                let hash = crate::engine::info_hash_of(&listing.torrent_bytes).unwrap_or_default();
                if self.usenet.active() && self.usenet.links(&hash).is_none() {
                    self.set_info("Still checking whether it's on Usenet; try again in a moment");
                    return;
                }
                // Keep what's already downloading from this torrent.
                let existing = self
                    .usenet
                    .manager
                    .existing(&hash)
                    .or_else(|| self.debrid.manager.existing(&hash))
                    .or_else(|| self.engine().and_then(|e| e.existing(&hash)));
                if let Some(e) = existing {
                    match e.only_files {
                        Some(only) => files.extend(only),
                        None => files = listing.files.iter().map(|f| f.index).collect(),
                    }
                }
                files.sort_unstable();
                files.dedup();
                let nzbs = self
                    .usenet
                    .links(&hash)
                    .map(<[_]>::to_vec)
                    .unwrap_or_default();
                self.library.record([hash], &new, &root);
                self.download_files(&listing, files, nzbs, folder);
            }
            Plan::Items(items) => {
                self.library
                    .record(items.iter().map(|(_, i)| i.info_hash.clone()), &new, &root);
                self.download_episode_torrents(items, title, folder);
            }
            // Straight into its folder, one job per episode.
            Plan::Stream(eps) => {
                let folder = self.library.add(&new, &root).folder.clone();
                let n = eps.len();
                let site = eps.first().map(|e| e.site.name()).unwrap_or_default();
                for ep in eps {
                    self.streams.add(ep, &title, folder.clone());
                }
                self.set_info(format!(
                    "Downloading {n} episode{} of {title} from {site}",
                    if n == 1 { "" } else { "s" }
                ));
            }
        }
        self.pages.last_stats = None;
    }

    /// Add library episodes to the playlist, titled like remuxes:
    /// "<series> - 05" (numbered alike across the series' episodes).
    pub(super) fn add_episodes_to_playlist(&mut self, id: u64, files: Vec<PathBuf>) {
        let Some(series) = self.library.find(id) else {
            return;
        };
        let title = match &self.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.title.clone(),
            _ => series.title().to_owned(),
        };
        let all: Vec<PathBuf> = series.video_files();
        let titles = crate::remux::episode_titles(&title, &all);
        let items: Vec<crate::playlist::Item> = files
            .into_iter()
            .map(|path| {
                let title = all
                    .iter()
                    .position(|p| *p == path)
                    .map(|i| titles[i].clone())
                    .unwrap_or_else(|| {
                        crate::remux::episode_titles(&title, std::slice::from_ref(&path)).remove(0)
                    });
                crate::playlist::Item { path, title }
            })
            .collect();
        self.handle_playlist_msg(Msg::PlaylistAdd {
            items,
            errors: Vec::new(),
        });
    }
}

/// "Airing: episode 9 on Sat 29 Nov", "Finished", …
pub(super) fn airing_line(d: &Details) -> Option<String> {
    if let Some((episode, at)) = d.next_episode {
        let when = chrono::DateTime::from_timestamp(at, 0)?.with_timezone(&chrono::Local);
        return Some(format!(
            "Airing: episode {episode} on {}",
            when.format("%a %-d %b, %H:%M")
        ));
    }
    Some(match d.status.as_deref()? {
        "FINISHED" => "Finished airing".to_owned(),
        "RELEASING" => "Airing".to_owned(),
        "NOT_YET_RELEASED" => d
            .start_date
            .as_ref()
            .map_or("Not out yet".to_owned(), |s| format!("Starts {s}")),
        "CANCELLED" => "Cancelled".to_owned(),
        "HIATUS" => "On hiatus".to_owned(),
        other => title_case(other),
    })
}

fn title_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// What AniList says of it, in a line: score, season, format, studio...
pub(super) fn facts(d: &crate::library::Details) -> String {
    let mut facts = Vec::new();
    if let Some(score) = d.score {
        facts.push(format!("★ {score}%"));
    }
    if let (Some(season), Some(year)) = (&d.season, d.year) {
        facts.push(format!("{} {year}", title_case(season)));
    } else if let Some(year) = d.year {
        facts.push(year.to_string());
    }
    if let Some(format) = &d.format {
        facts.push(format.replace('_', " "));
    }
    if !d.studios.is_empty() {
        facts.push(d.studios.join(", "));
    }
    if let Some(source) = &d.source {
        facts.push(format!("from {}", title_case(&source.replace('_', " "))));
    }
    if let Some(minutes) = d.duration {
        facts.push(format!("{minutes} min episodes"));
    }
    if let Some(line) = airing_line(d) {
        facts.push(line);
    }
    facts.join("   ·   ")
}
