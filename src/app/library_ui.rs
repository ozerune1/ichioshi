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

impl App {
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
        if !self.settings.migrated {
            self.settings.migrated = true;
            self.move_old_remuxes();
        }
        self.move_finished_torrents();
        self.move_finished_jobs();
        self.hand_over_failed();
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
    fn busy_files(&self) -> HashSet<PathBuf> {
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
                    Err(e) => tracing::warn!(
                        "couldn't set the default tracks of {}: {e:#}",
                        path.display()
                    ),
                }
                let Some((len, modified)) = stamp(&path) else {
                    continue;
                };
                let applied = Applied {
                    key: prefs.key(),
                    len,
                    modified,
                    error: result.err().map(|e| format!("{e:#}")),
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

use eframe::egui::{self, RichText, Sense, Ui, vec2};

use super::{Action, AnimePage, GREEN, Page, ReleaseKind, fmt_bytes};
use crate::engine::Listing;
use crate::library::Details;
use crate::nyaa::NyaaItem;
use crate::prefs::{LANGUAGES, SubType};
use crate::remux::language_name;
use crate::seadex::Anime;

/// What the library page shows about a series (refreshed every few seconds).
#[derive(Clone, Default)]
pub(super) struct SeriesStats {
    /// Episodes on disk: path within the folder, and size.
    pub(super) files: Vec<(String, u64)>,
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
}

impl Coverage<'_> {
    pub(super) fn has(&self, episode: u32) -> bool {
        match self {
            Coverage::Batch { files, .. } => files.contains_key(&episode),
            Coverage::PerEpisode(items) => items.contains_key(&episode),
            _ => false,
        }
    }

    fn last(&self) -> u32 {
        match self {
            Coverage::Batch { files, .. } => files.keys().max().copied().unwrap_or(0),
            Coverage::PerEpisode(items) => items.keys().max().copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Why an episode can't be downloaded.
    fn why_not(&self) -> String {
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
    last_stats: Option<Instant>,
    /// AniList details being fetched, and when fetches failed.
    details_loading: HashSet<u64>,
    details_failed: HashMap<u64, Instant>,
    /// Details of anime not in the library.
    pub(super) details: HashMap<u64, Details>,
    /// The library's languages, while being changed.
    pub(super) languages: Option<LangPrefs>,
    /// The files of releases on series pages, by Nyaa ID.
    pub(super) release_files: HashMap<u64, ReleaseFiles>,
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
        let mut files_progress: HashMap<String, Vec<(String, f32)>> = HashMap::new();
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
                    if f.len == 0 {
                        1.0
                    } else {
                        v.live.have[i] as f32 / f.len as f32
                    },
                ));
            }
        }
        let busy = self.busy_files();
        let mut stats = HashMap::new();
        for series in &self.library.series {
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
            let mut progress = BTreeMap::new();
            for hash in &series.downloads {
                for (path, done) in files_progress.get(hash).into_iter().flatten() {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    if let Some(ep) = crate::title::episode_of_file(name) {
                        progress.insert(ep, done.clamp(0.0, 1.0));
                    }
                }
            }
            let s = SeriesStats {
                bytes: files.iter().map(|f| f.1).sum(),
                files,
                pending: self.pending_defaults(series).len(),
                downloading: series.downloads.iter().any(|h| active.contains(h)),
                progress,
            };
            stats.insert(series.anilist_id, s);
        }
        self.pages.stats = stats;
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
                now - d.fetched < max_age
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
        };
        self.cancel_listing();
        self.page = Page::Anime(AnimePage {
            anime,
            chosen: None,
            nyaa: None,
            from_library: true,
            entry_loading: true,
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
                let anime = ap.anime.clone();
                self.show_anime(anime, true);
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
        if matches!(&self.page, Page::Anime(ap) if ap.anime.anilist_id == id && ap.from_library) {
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

    /// The front page: your library.
    pub(super) fn library_page(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading("Library");
            let n = self.library.series.len();
            ui.weak(if n == 1 {
                "1 series".to_owned()
            } else {
                format!("{n} series")
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Change…")
                    .on_hover_text("Which audio and subtitles play by default")
                    .clicked()
                {
                    actions.push(Action::OpenLanguages);
                }
                ui.label(RichText::new(self.settings.prefs.summary()).strong());
                ui.weak("Languages:");
            });
        });
        let note = format!(
            "Each series has its own folder in {}",
            self.library_dir().display()
        );
        ui.add(egui::Label::new(RichText::new(&note).weak()).truncate())
            .on_hover_text(&note);
        ui.separator();
        if self.library.series.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.25);
                ui.heading("Your library is empty");
                ui.add_space(8.0);
                ui.label("Search for an anime above, then add it or download episodes of it.");
                ui.label("Releases come from what SeaDex (releases.moe) rates best, or, for anime");
                ui.label("SeaDex doesn't cover, from the best-ranked release group on Nyaa.");
            });
            return;
        }
        let mut order: Vec<&Series> = self.library.series.iter().collect();
        order.sort_by(|a, b| b.added.cmp(&a.added).then(a.title().cmp(b.title())));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Cards in rows, tops lined up.
                ui.with_layout(
                    egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
                    |ui| {
                        ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                        for series in order {
                            self.series_card(ui, series, actions);
                        }
                    },
                );
            });
    }

    fn series_card(&self, ui: &mut Ui, series: &Series, actions: &mut Vec<Action>) {
        let stats = self
            .pages
            .stats
            .get(&series.anilist_id)
            .cloned()
            .unwrap_or_default();
        let details = series.details.as_ref();
        // The whole card is the button: its text isn't selectable.
        let text = |ui: &mut Ui, t: RichText| {
            ui.add(egui::Label::new(t).wrap().selectable(false));
        };
        let response = ui
            .allocate_ui_with_layout(
                vec2(164.0, 320.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(150.0);
                        ui.set_min_height(300.0);
                        let cover = series
                            .cover
                            .as_deref()
                            .or(details.and_then(|d| d.cover.as_deref()));
                        super::cover(ui, cover, vec2(150.0, 212.0));
                        text(ui, RichText::new(series.title()).strong());
                        let total = series.episodes.or(details.and_then(|d| d.episodes));
                        let count = stats.files.len();
                        let episodes = match (count, total) {
                            (0, _) => "No episodes yet".to_owned(),
                            (n, Some(t)) => {
                                format!("{n} of {t} episodes · {}", fmt_bytes(stats.bytes))
                            }
                            (1, None) => format!("1 episode · {}", fmt_bytes(stats.bytes)),
                            (n, None) => format!("{n} episodes · {}", fmt_bytes(stats.bytes)),
                        };
                        text(ui, RichText::new(episodes).weak());
                        if stats.downloading {
                            text(ui, RichText::new("⬇ Downloading").color(GREEN));
                        } else if let Some(line) = details.and_then(airing_line) {
                            text(ui, RichText::new(line).weak());
                        }
                    });
                },
            )
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(series.title());
        if response.clicked() {
            actions.push(Action::OpenSeries(series.anilist_id));
        }
    }

    /// AniList's details on an anime page.
    pub(super) fn details_section(&self, ui: &mut Ui, id: u64) {
        let Some(d) = self.details(id) else {
            if self.pages.details_loading.contains(&id) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Getting details from AniList…");
                });
            }
            return;
        };
        let mut facts = Vec::new();
        if let Some(score) = d.score {
            facts.push(format!("★ {score}%"));
        }
        if let (Some(season), Some(year)) = (&d.season, d.year) {
            facts.push(format!("{} {year}", title_case(season)));
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
        if !facts.is_empty() {
            ui.label(facts.join("  ·  "));
        }
        if !d.genres.is_empty() {
            ui.weak(d.genres.join(" · "));
        }
        if let Some(text) = airing_line(d) {
            ui.label(text);
        }
        if let Some(native) = &d.native {
            ui.weak(native);
        }
    }

    pub(super) fn description_section(&self, ui: &mut Ui, id: u64) {
        let Some(text) = self.details(id).and_then(|d| d.description.as_deref()) else {
            return;
        };
        egui::CollapsingHeader::new(RichText::new("Story").strong())
            .id_salt(("story", id))
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(("story_scroll", id))
                    .max_height(140.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(text).wrap());
                    });
            });
    }

    /// The library part of an anime page: in it or not, its folder,
    /// languages and episodes.
    pub(super) fn series_library_section(
        &self,
        ui: &mut Ui,
        anime: &Anime,
        actions: &mut Vec<Action>,
    ) {
        let Some(series) = self.library.find(anime.anilist_id) else {
            ui.horizontal(|ui| {
                if ui
                    .button("＋ Add to library")
                    .on_hover_text("Downloading episodes adds it too")
                    .clicked()
                {
                    actions.push(Action::AddToLibrary);
                }
                ui.weak(format!(
                    "It would go in {}",
                    self.library
                        .folder_for(&super::new_series(anime), &self.library_dir())
                        .display()
                ));
            });
            return;
        };
        let stats = self
            .pages
            .stats
            .get(&series.anilist_id)
            .cloned()
            .unwrap_or_default();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.colored_label(GREEN, "In your library");
                if ui.small_button("🗁 Folder").clicked() {
                    actions.push(Action::OpenFolder(series.folder.clone()));
                }
                ui.menu_button("Remove", |ui| {
                    if ui.button("Remove from library (keep files)").clicked() {
                        actions.push(Action::RemoveSeries { id: series.anilist_id, delete_files: false });
                    }
                    if ui.button("Remove and delete its folder…").clicked() {
                        actions.push(Action::AskRemoveSeries(series.anilist_id));
                    }
                });
            });
            let folder = series.folder.display().to_string();
            ui.add(egui::Label::new(RichText::new(&folder).weak()).truncate()).on_hover_text(&folder);
            ui.add_space(4.0);

            // Languages: the library's, or this series' own.
            ui.label(RichText::new("Languages").strong());
            let mut own = series.prefs.is_some();
            let library = self.settings.prefs.summary();
            if ui
                .checkbox(&mut own, "Use different languages for this series")
                .on_hover_text(format!("Otherwise the library's: {library}"))
                .changed()
            {
                let prefs = own.then(|| self.settings.prefs.clone());
                actions.push(Action::SetSeriesPrefs { id: series.anilist_id, prefs });
            }
            match &series.prefs {
                Some(prefs) => {
                    let mut edited = prefs.clone();
                    if lang_prefs_editor(ui, series.anilist_id, &mut edited) {
                        actions.push(Action::SetSeriesPrefs { id: series.anilist_id, prefs: Some(edited) });
                    }
                }
                None => {
                    ui.weak(format!("The library's: {library}"));
                }
            }
            let count = stats.files.len();
            if count > 0 {
                let done = count.saturating_sub(stats.pending);
                let note = if stats.pending > 0 {
                    format!("Default tracks set on {done} of {count} episodes; setting the rest…")
                } else {
                    format!("Default tracks set on all {count} episodes. Every track is still there to switch to.")
                };
                ui.weak(note);
            }
        });
    }

    /// The Nyaa torrent of the release chosen on a series page, if it's one
    /// torrent (per-episode releases don't need reading).
    fn chosen_torrent(ap: &AnimePage) -> Option<u64> {
        match &ap.nyaa {
            Some(search) => match &search.releases.get(search.chosen?)?.kind {
                ReleaseKind::Torrent { item, .. } => Some(item.id),
                ReleaseKind::Episodes(_) => None,
            },
            None => super::chosen_release(ap)?.nyaa_id(),
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

    pub(super) fn handle_release_files(&mut self, id: u64, result: Result<Listing, String>) {
        let files = match result {
            Ok(listing) => ReleaseFiles::Ready(listing),
            Err(e) => ReleaseFiles::Failed(e),
        };
        self.pages.release_files.insert(id, files);
    }

    /// What the release chosen on a series page has, episode by episode.
    pub(super) fn coverage<'a>(&'a self, ap: &'a AnimePage) -> Coverage<'a> {
        if let Some(search) = &ap.nyaa
            && let Some(r) = search.chosen.and_then(|i| search.releases.get(i))
            && let ReleaseKind::Episodes(items) = &r.kind
        {
            return Coverage::PerEpisode(items.iter().map(|(e, i)| (*e, i)).collect());
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
                for f in &listing.files {
                    let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                    if crate::remux::is_video(Path::new(name))
                        && let Some(ep) = crate::title::episode_of_file(name)
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
        }
        self.pages.last_stats = None;
    }

    /// Every episode of the series: in the library, downloading, or not
    /// (grayed out, with Download).
    pub(super) fn episodes_section(&self, ui: &mut Ui, ap: &AnimePage, actions: &mut Vec<Action>) {
        let id = ap.anime.anilist_id;
        let details = self.details(id);
        let series = self.library.find(id);
        let stats = self.pages.stats.get(&id).cloned().unwrap_or_default();
        let coverage = self.coverage(ap);
        let now = chrono::Utc::now().timestamp();

        // Files in the folder, by episode; the rest are extras.
        let mut on_disk: BTreeMap<u32, (String, u64)> = BTreeMap::new();
        let mut extras = Vec::new();
        for (rel, len) in &stats.files {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            match crate::title::episode_of_file(name) {
                Some(ep) if !on_disk.contains_key(&ep) => {
                    on_disk.insert(ep, (rel.clone(), *len));
                }
                _ => extras.push((rel.clone(), *len)),
            }
        }
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
        let airs = |n: u32| {
            let d = details?;
            d.episode(n).and_then(|e| e.airs).or(d
                .next_episode
                .filter(|(next, _)| *next == n)
                .map(|(_, t)| t))
        };
        // Out yet: by its air date, else by AniList's next episode.
        let out = |n: u32| match airs(n) {
            Some(t) => t <= now,
            None => details
                .and_then(|d| d.aired())
                .is_none_or(|aired| n <= aired),
        };
        let missing: Vec<u32> = (1..=last)
            .filter(|n| {
                !on_disk.contains_key(n)
                    && !stats.progress.contains_key(n)
                    && out(*n)
                    && coverage.has(*n)
            })
            .collect();

        ui.horizontal(|ui| {
            ui.heading("Episodes");
            ui.weak(format!("{} of {last} in your library", on_disk.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !on_disk.is_empty()
                    && let Some(s) = series
                    && ui
                        .button("Remux all…")
                        .on_hover_text("Make copies with only the audio and subtitles you want")
                        .clicked()
                {
                    let files = on_disk
                        .values()
                        .map(|(rel, _)| s.folder.join(rel))
                        .collect();
                    actions.push(Action::RemuxLibrary { id, files });
                }
                let label = match missing.len() {
                    0 => "⬇ Download series".to_owned(),
                    n => format!("⬇ Download series ({n} missing)"),
                };
                let button = egui::Button::new(RichText::new(label).strong());
                let response = ui.add_enabled(!missing.is_empty(), button);
                if response.clicked() {
                    actions.push(Action::DownloadEpisodes(missing.clone()));
                }
                if missing.is_empty() && last > 0 && on_disk.len() < last as usize {
                    response.on_disabled_hover_text(coverage.why_not());
                }
            });
        });
        if last == 0 {
            ui.weak("AniList doesn't list its episodes yet.");
            return;
        }
        let gray = ui.visuals().weak_text_color();
        let normal = ui.visuals().text_color();
        egui_extras::TableBuilder::new(ui)
            .id_salt(("episodes", id))
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::exact(34.0))
            .column(egui_extras::Column::remainder().clip(true))
            .column(egui_extras::Column::exact(110.0))
            .column(egui_extras::Column::exact(230.0))
            .body(|body| {
                body.rows(28.0, last as usize, |mut row| {
                    let n = row.index() as u32 + 1;
                    let info = details.and_then(|d| d.episode(n));
                    let have = on_disk.get(&n);
                    let color = if have.is_some() { normal } else { gray };
                    row.col(|ui| {
                        ui.label(RichText::new(format!("{n:02}")).color(color).monospace());
                    });
                    row.col(|ui| {
                        let title = info
                            .and_then(|e| e.title.clone())
                            .unwrap_or_else(|| format!("Episode {n}"));
                        let label =
                            ui.add(egui::Label::new(RichText::new(&title).color(color)).truncate());
                        if let Some((rel, _)) = have {
                            label.on_hover_text(rel);
                        }
                    });
                    row.col(|ui| {
                        if let Some(t) = airs(n)
                            && let Some(when) = chrono::DateTime::from_timestamp(t, 0)
                        {
                            ui.label(
                                RichText::new(
                                    when.with_timezone(&chrono::Local)
                                        .format("%-d %b %Y")
                                        .to_string(),
                                )
                                .color(gray),
                            );
                        }
                    });
                    row.col(|ui| {
                        if let Some(p) = stats.progress.get(&n) {
                            ui.add(
                                egui::ProgressBar::new(*p)
                                    .desired_width(200.0)
                                    .text(format!("{:.0}%", p * 100.0)),
                            );
                        } else if let Some((rel, len)) = have {
                            ui.label(fmt_bytes(*len));
                            if let Some(s) = series
                                && ui
                                    .small_button("Remux…")
                                    .on_hover_text("Make a copy with only the tracks you want")
                                    .clicked()
                            {
                                actions.push(Action::RemuxLibrary {
                                    id,
                                    files: vec![s.folder.join(rel)],
                                });
                            }
                        } else if !out(n) {
                            ui.label(RichText::new("Not out yet").color(gray));
                        } else {
                            let ok = coverage.has(n);
                            let button =
                                ui.add_enabled(ok, egui::Button::new("⬇ Download").small());
                            if button.clicked() {
                                actions.push(Action::DownloadEpisodes(vec![n]));
                            }
                            if !ok {
                                button.on_disabled_hover_text(coverage.why_not());
                            }
                        }
                    });
                });
            });
        if !extras.is_empty() {
            egui::CollapsingHeader::new(format!("Other files ({})", extras.len()))
                .id_salt(("extras", id))
                .show(ui, |ui| {
                    for (rel, len) in &extras {
                        ui.horizontal(|ui| {
                            ui.label(fmt_bytes(*len));
                            ui.add(egui::Label::new(rel.as_str()).truncate());
                        });
                    }
                });
        }
    }

    /// The library's languages, in a dialog.
    pub(super) fn languages_dialog(&mut self, ctx: &egui::Context) {
        let Some(prefs) = &mut self.pages.languages else {
            return;
        };
        let (mut save, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("languages")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Languages");
            ui.weak("Which audio and subtitles play by default, for every series without its own. Every track stays in the files.");
            ui.add_space(6.0);
            lang_prefs_editor(ui, 0, prefs);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                save = ui.button("Save").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            self.settings.prefs = prefs.clone();
            self.pages.languages = None;
            self.recheck_defaults();
            self.pages.last_stats = None;
        } else if cancel || modal.should_close() {
            self.pages.languages = None;
        }
    }
}

/// Audio language, subtitle language (or none) and subtitle kind. Returns
/// whether anything changed.
/// `salt` keeps editors apart (a series' ID; 0 for the library's).
pub(super) fn lang_prefs_editor(ui: &mut Ui, salt: u64, prefs: &mut LangPrefs) -> bool {
    let before = prefs.clone();
    egui::Grid::new(("lang_prefs", salt))
        .num_columns(2)
        .spacing(vec2(10.0, 6.0))
        .show(ui, |ui| {
            ui.label("Audio");
            egui::ComboBox::from_id_salt(("audio", salt))
                .selected_text(language_name(&prefs.audio))
                .show_ui(ui, |ui| {
                    for code in LANGUAGES {
                        ui.selectable_value(&mut prefs.audio, code.to_owned(), language_name(code));
                    }
                });
            ui.end_row();
            ui.label("Subtitles");
            let shown = prefs
                .subtitles
                .as_deref()
                .map_or("None".to_owned(), language_name);
            egui::ComboBox::from_id_salt(("subs", salt))
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut prefs.subtitles, None, "None");
                    for code in LANGUAGES {
                        ui.selectable_value(
                            &mut prefs.subtitles,
                            Some(code.to_owned()),
                            language_name(code),
                        );
                    }
                });
            ui.end_row();
            ui.label("Kind");
            ui.add_enabled_ui(prefs.subtitles.is_some(), |ui| {
                egui::ComboBox::from_id_salt(("kind", salt))
                    .selected_text(prefs.sub_type.name())
                    .show_ui(ui, |ui| {
                        for kind in SubType::ALL {
                            ui.selectable_value(&mut prefs.sub_type, kind, kind.name())
                                .on_hover_text(kind.help());
                        }
                    });
            });
            ui.end_row();
        });
    if prefs.subtitles.is_some() {
        ui.weak(prefs.sub_type.help());
    }
    *prefs != before
}

/// "Airing: episode 9 on Sat 29 Nov", "Finished", …
fn airing_line(d: &Details) -> Option<String> {
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
