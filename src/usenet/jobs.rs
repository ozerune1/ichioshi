//! Usenet downloads, one release at a time.
//!
//! A job is one torrent's release, found on the indexers as one or more NZBs.
//! Only the files you picked are downloaded: their articles are fetched from
//! your providers, decoded, and written in place into a staging folder
//! (`<output>/.usenet-<id>`) under their posted names. Finished files move
//! to where a torrent would have put them. If articles are missing on every
//! provider, the release's PAR2 recovery files are fetched (only as many as
//! the damage needs) and the files repaired; if that can't work, the next
//! indexer's NZB (a separate upload of the same release) is tried.
//!
//! Jobs, with the articles already written, are saved in
//! `<state>/usenet/jobs.json`, so they resume after a restart.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};

use super::Config;
use super::indexers::{Indexers, NzbLink};
use super::nntp::{FetchError, Providers};
use super::nzb::{self, NzbFile};
use super::repair::{self, Check};
use crate::engine::{Existing, Listing, SeriesFiles};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobFile {
    /// The file's index in the torrent.
    pub index: usize,
    /// Its path inside the torrent (under the output folder).
    pub path: String,
    pub len: u64,
    pub wanted: bool,
}

impl JobFile {
    fn base_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub info_hash: String,
    /// The release's NZBs, in the order to try them.
    pub nzbs: Vec<NzbLink>,
    /// Which of `nzbs` is in use.
    pub nzb: usize,
    pub output_folder: PathBuf,
    pub files: Vec<JobFile>,
    pub paused: bool,
    /// Why it gave up, until retried.
    #[serde(default)]
    pub failed: Option<String>,
    /// Articles already written, by segment number, per posted file name.
    #[serde(default)]
    pub done: BTreeMap<String, BTreeSet<u32>>,
}

impl Job {
    fn wanted(&self) -> impl Iterator<Item = (usize, &JobFile)> {
        self.files.iter().enumerate().filter(|(_, f)| f.wanted)
    }

    pub fn wanted_bytes(&self) -> u64 {
        self.wanted().map(|(_, f)| f.len).sum()
    }

    fn dest(&self, file: &JobFile) -> PathBuf {
        self.output_folder.join(&file.path)
    }

    fn staging(&self) -> PathBuf {
        self.output_folder.join(format!(".usenet-{}", self.id))
    }

    fn complete(&self, file: &JobFile) -> bool {
        std::fs::metadata(self.dest(file)).is_ok_and(|m| m.len() == file.len)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Waiting for the release before it.
    Queued,
    /// Getting the NZB from the indexer.
    Fetching,
    Downloading,
    /// Fixing missing articles with PAR2; the text says how far along.
    Repairing(String),
    Done,
    Paused,
    Failed(String),
}

/// How a job is doing right now.
#[derive(Clone, Debug)]
pub struct Live {
    pub phase: Phase,
    /// Bytes downloaded for each file (same order as `Job::files`).
    pub have: Vec<u64>,
    /// Bytes per second.
    pub speed: f64,
    /// A passing problem being retried.
    pub note: Option<String>,
    /// Articles no provider has (so far).
    pub missing: u32,
}

impl Live {
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            Phase::Fetching | Phase::Downloading | Phase::Repairing(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub job: Job,
    pub live: Live,
}

impl View {
    pub fn have_bytes(&self) -> u64 {
        self.job.wanted().map(|(i, _)| self.live.have[i]).sum()
    }

    /// The indexer whose NZB is in use.
    pub fn source(&self) -> Option<&'static str> {
        self.job.nzbs.get(self.job.nzb).map(|l| l.indexer.name())
    }
}

struct Entry {
    job: Job,
    live: Arc<Mutex<Live>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
struct Timing {
    /// After a passing error; doubles each time.
    retry: Duration,
    /// How often progress is saved.
    save: Duration,
}

/// Why a run stopped.
enum Stop {
    /// Paused, removed or restarted: whoever did it set the state.
    Cancelled,
    /// Worth retrying later (network, provider trouble).
    Transient(String),
    /// This NZB can't make the files; another indexer's might.
    Nzb(String),
    /// Nothing will help without you.
    Fatal(String),
}

impl From<anyhow::Error> for Stop {
    fn from(e: anyhow::Error) -> Self {
        Stop::Transient(format!("{e:#}"))
    }
}

/// A file to fetch: articles go to `path`.
struct Target {
    path: PathBuf,
    nzb: NzbFile,
    /// The decoded size, when known (it is for the files from the torrent).
    len: Option<u64>,
    /// Segments already there.
    done: BTreeSet<u32>,
    /// The job file it is, if any (PAR2 files aren't).
    job_file: Option<usize>,
}

pub struct Manager {
    dir: PathBuf,
    rt: tokio::runtime::Handle,
    indexers: Indexers,
    providers: Mutex<Arc<Providers>>,
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    notify: Box<dyn Fn() + Send + Sync>,
    wake: Notify,
    timing: Timing,
}

impl Manager {
    /// Load saved jobs from `<state_dir>/usenet` and carry on with them.
    pub fn load(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        config: &Config,
        indexers: Indexers,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::load_with(
            state_dir,
            rt,
            config,
            indexers,
            notify,
            Timing {
                retry: Duration::from_secs(5),
                save: Duration::from_secs(3),
            },
        )
    }

    fn load_with(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        config: &Config,
        indexers: Indexers,
        notify: impl Fn() + Send + Sync + 'static,
        timing: Timing,
    ) -> Arc<Self> {
        let dir = state_dir.join("usenet");
        let jobs: Vec<Job> = std::fs::read_to_string(dir.join("jobs.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let next_id = jobs.iter().map(|j| j.id + 1).max().unwrap_or(1);
        let entries = jobs
            .into_iter()
            .map(|job| {
                let have = initial_have(&job);
                let phase = if job.wanted().all(|(_, f)| job.complete(f)) {
                    Phase::Done
                } else if job.paused {
                    Phase::Paused
                } else if let Some(why) = &job.failed {
                    Phase::Failed(why.clone())
                } else {
                    Phase::Queued
                };
                Entry {
                    job,
                    live: Arc::new(Mutex::new(Live {
                        phase,
                        have,
                        speed: 0.0,
                        note: None,
                        missing: 0,
                    })),
                    cancel: Arc::default(),
                }
            })
            .collect();
        let manager = Arc::new(Self {
            dir,
            rt: rt.clone(),
            indexers,
            providers: Mutex::new(Arc::new(Providers::new(config))),
            entries: Mutex::new(entries),
            next_id: AtomicU64::new(next_id),
            notify: Box::new(notify),
            wake: Notify::new(),
            timing,
        });
        rt.spawn(manager.clone().queue());
        manager
    }

    /// Use these providers from now on. A running download restarts with them.
    pub fn set_config(&self, config: &Config) {
        *self.providers.lock().unwrap() = Arc::new(Providers::new(config));
        let mut entries = self.entries.lock().unwrap();
        for e in entries.iter_mut() {
            let live = e.live.lock().unwrap().clone();
            if live.active() {
                e.cancel.store(true, Ordering::Relaxed);
                e.cancel = Arc::default();
                e.live.lock().unwrap().phase = Phase::Queued;
            }
        }
        drop(entries);
        self.wake.notify_one();
    }

    fn torrent_path(&self, info_hash: &str) -> PathBuf {
        self.dir
            .join("torrents")
            .join(format!("{info_hash}.torrent"))
    }

    fn nzb_path(&self, job: &Job) -> PathBuf {
        self.dir
            .join("nzbs")
            .join(format!("{}-{}.nzb", job.id, job.nzb))
    }

    fn save(&self, entries: &[Entry]) {
        let jobs: Vec<&Job> = entries.iter().map(|e| &e.job).collect();
        let result = serde_json::to_vec(&jobs)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::fsutil::write_private(&self.dir.join("jobs.json"), &bytes));
        if let Err(e) = result {
            tracing::warn!("could not save the Usenet downloads: {e:#}");
        }
    }

    fn with_job<R>(&self, id: u64, f: impl FnOnce(&mut Job) -> R) -> Option<R> {
        let mut entries = self.entries.lock().unwrap();
        let r = entries
            .iter_mut()
            .find(|e| e.job.id == id)
            .map(|e| f(&mut e.job));
        if r.is_some() {
            self.save(&entries);
        }
        r
    }

    fn job(&self, id: u64) -> Option<Job> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.job.id == id)
            .map(|e| e.job.clone())
    }

    /// Download `wanted` (torrent file indices) of a release from Usenet into
    /// `output_folder`, using `nzbs` in order. If the release is already a
    /// job, its selection changes instead.
    pub fn add(
        self: &Arc<Self>,
        listing: &Listing,
        wanted: &[usize],
        output_folder: PathBuf,
        nzbs: Vec<NzbLink>,
    ) -> Result<()> {
        if wanted.is_empty() {
            bail!("no files selected");
        }
        if nzbs.is_empty() {
            bail!("the release isn't on Usenet");
        }
        let info_hash =
            crate::engine::info_hash_of(&listing.torrent_bytes).context("invalid .torrent file")?;
        if let Some(id) = self.job_for(&info_hash) {
            return self.reselect(id, wanted);
        }
        let path = self.torrent_path(&info_hash);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, &listing.torrent_bytes)
            .with_context(|| format!("could not save {}", path.display()))?;
        let files = listing
            .files
            .iter()
            .map(|f| JobFile {
                index: f.index,
                path: f.path.clone(),
                len: f.len,
                wanted: wanted.contains(&f.index),
            })
            .collect();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let job = Job {
            id,
            name: listing.name.clone(),
            info_hash,
            nzbs,
            nzb: 0,
            output_folder,
            files,
            paused: false,
            failed: None,
            done: BTreeMap::new(),
        };
        let have = initial_have(&job);
        {
            let mut entries = self.entries.lock().unwrap();
            entries.push(Entry {
                job,
                live: Arc::new(Mutex::new(Live {
                    phase: Phase::Queued,
                    have,
                    speed: 0.0,
                    note: None,
                    missing: 0,
                })),
                cancel: Arc::default(),
            });
            self.save(&entries);
        }
        (self.notify)();
        self.wake.notify_one();
        Ok(())
    }

    /// Change which files a job downloads. Finished files stay; partly
    /// downloaded ones no longer wanted are deleted.
    pub fn reselect(&self, id: u64, wanted: &[usize]) -> Result<()> {
        {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .iter_mut()
                .find(|e| e.job.id == id)
                .context("no such download")?;
            let staging = entry.job.staging();
            let mut dropped = Vec::new();
            for f in &mut entry.job.files {
                let was = f.wanted;
                f.wanted = wanted.contains(&f.index);
                if was && !f.wanted {
                    dropped.push(f.base_name().to_owned());
                }
            }
            for name in dropped {
                let _ = std::fs::remove_file(staging.join(&name));
                entry.job.done.retain(|k, _| !k.eq_ignore_ascii_case(&name));
            }
            entry.job.paused = false;
            entry.job.failed = None;
            entry.cancel.store(true, Ordering::Relaxed);
            entry.cancel = Arc::default();
            let mut live = entry.live.lock().unwrap();
            live.have = initial_have(&entry.job);
            live.phase = Phase::Queued;
            drop(live);
            self.save(&entries);
        }
        (self.notify)();
        self.wake.notify_one();
        Ok(())
    }

    pub fn job_for(&self, info_hash: &str) -> Option<u64> {
        let info_hash = info_hash.to_ascii_lowercase();
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.job.info_hash == info_hash)
            .map(|e| e.job.id)
    }

    /// For the file picker: the job's selection and folder, if `info_hash` is a job.
    pub fn existing(&self, info_hash: &str) -> Option<Existing> {
        let id = self.job_for(info_hash)?;
        let job = self.job(id)?;
        Some(Existing {
            only_files: Some(
                job.files
                    .iter()
                    .filter(|f| f.wanted)
                    .map(|f| f.index)
                    .collect(),
            ),
            output_folder: job.output_folder,
        })
    }

    /// The torrent's contents with the job's selection, to change it.
    pub fn listing(&self, id: u64) -> Result<Listing> {
        let job = self.job(id).context("no such download")?;
        let bytes = std::fs::read(self.torrent_path(&job.info_hash))
            .context("the download's .torrent file is missing")?;
        let mut listing = crate::engine::listing_from_bytes(bytes)?;
        listing.existing = self.existing(&job.info_hash);
        Ok(listing)
    }

    /// The release's NZBs (to show where it comes from).
    pub fn nzbs(&self, id: u64) -> Vec<NzbLink> {
        self.job(id).map(|j| j.nzbs).unwrap_or_default()
    }

    /// All jobs, newest first.
    pub fn views(&self) -> Vec<View> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .rev()
            .map(|e| View {
                job: e.job.clone(),
                live: e.live.lock().unwrap().clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn view(&self, id: u64) -> Option<View> {
        self.views().into_iter().find(|v| v.job.id == id)
    }

    pub fn pause(&self, id: u64) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.job.id == id) {
            e.job.paused = true;
            e.cancel.store(true, Ordering::Relaxed);
            let mut live = e.live.lock().unwrap();
            if live.phase != Phase::Done {
                live.phase = Phase::Paused;
            }
            live.speed = 0.0;
        }
        self.save(&entries);
        drop(entries);
        (self.notify)();
    }

    /// Resume a paused job, or retry a failed one.
    pub fn resume(&self, id: u64) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.job.id == id) {
            e.job.paused = false;
            e.job.failed = None;
            e.cancel.store(true, Ordering::Relaxed);
            e.cancel = Arc::default();
            let mut live = e.live.lock().unwrap();
            if live.phase != Phase::Done {
                live.phase = Phase::Queued;
                live.note = None;
            }
        }
        self.save(&entries);
        drop(entries);
        (self.notify)();
        self.wake.notify_one();
    }

    /// Remove a job, and optionally its downloaded files.
    pub fn remove(&self, id: u64, delete_files: bool) {
        let entry = {
            let mut entries = self.entries.lock().unwrap();
            let Some(pos) = entries.iter().position(|e| e.job.id == id) else {
                return;
            };
            let entry = entries.remove(pos);
            self.save(&entries);
            entry
        };
        entry.cancel.store(true, Ordering::Relaxed);
        let job = entry.job;
        let _ = std::fs::remove_dir_all(job.staging());
        if delete_files {
            for (_, f) in job.wanted() {
                let _ = std::fs::remove_file(job.dest(f));
            }
            crate::debrid::jobs::remove_empty_dirs(&job.output_folder);
        }
        let _ = std::fs::remove_file(self.torrent_path(&job.info_hash));
        for n in 0..job.nzbs.len() {
            let _ = std::fs::remove_file(self.dir.join("nzbs").join(format!("{}-{n}.nzb", job.id)));
        }
        (self.notify)();
    }

    /// Stop downloading (it resumes from where it got to next time).
    pub fn stop(&self) {
        for e in self.entries.lock().unwrap().iter() {
            e.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// A series' finished video files: this job's, plus those of other jobs
    /// in the same folder, unless that's the download folder itself.
    pub fn series_files(&self, id: u64, download_dir: &Path) -> Option<SeriesFiles> {
        let views = self.views();
        let me = views.iter().find(|v| v.job.id == id)?;
        let folder = me.job.output_folder.clone();
        let shared = folder != download_dir;
        let group: Vec<&View> = if shared {
            views
                .iter()
                .filter(|v| v.job.output_folder == folder)
                .collect()
        } else {
            vec![me]
        };
        let mut files = Vec::new();
        let mut not_ready = 0;
        for v in &group {
            for (_, f) in v.job.wanted() {
                let dest = v.job.dest(f);
                if !crate::remux::is_video(&dest) {
                    continue;
                }
                if v.job.complete(f) {
                    files.push(dest);
                } else {
                    not_ready += 1;
                }
            }
        }
        files.sort();
        files.dedup();
        let name = if shared && group.len() > 1 {
            folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| me.job.name.clone())
        } else {
            me.job.name.clone()
        };
        Some(SeriesFiles {
            name,
            files,
            not_ready,
            info_hashes: group.iter().map(|v| v.job.info_hash.clone()).collect(),
        })
    }

    // ── Running ────────────────────────────────────────────────────────

    /// Download queued jobs, oldest first, one at a time.
    async fn queue(self: Arc<Self>) {
        loop {
            let next = {
                let entries = self.entries.lock().unwrap();
                entries
                    .iter()
                    .find(|e| e.live.lock().unwrap().phase == Phase::Queued)
                    .map(|e| (e.job.id, e.live.clone(), e.cancel.clone()))
            };
            match next {
                Some((id, live, cancel)) => self.run(id, &live, &cancel).await,
                None => self.wake.notified().await,
            }
        }
    }

    /// Run a job until it's done, gives up, or is cancelled. Passing
    /// problems are retried with growing pauses.
    async fn run(&self, id: u64, live: &Mutex<Live>, cancel: &AtomicBool) {
        let mut wait = self.timing.retry;
        for attempt in 1..=8 {
            let result = self.attempt(id, live, cancel).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let failed = match result {
                Ok(()) => {
                    let mut l = live.lock().unwrap();
                    l.phase = Phase::Done;
                    l.speed = 0.0;
                    l.note = None;
                    drop(l);
                    (self.notify)();
                    return;
                }
                Err(Stop::Cancelled) => return,
                Err(Stop::Fatal(why)) => why,
                Err(Stop::Nzb(why)) => {
                    // Another upload of the same release may be complete.
                    let next = self.with_job(id, |job| {
                        if job.nzb + 1 < job.nzbs.len() {
                            let _ = std::fs::remove_dir_all(job.staging());
                            job.nzb += 1;
                            job.done.clear();
                            Some(job.nzbs[job.nzb].indexer.name())
                        } else {
                            None
                        }
                    });
                    match next.flatten() {
                        Some(indexer) => {
                            let mut l = live.lock().unwrap();
                            l.note = Some(format!("{why}; trying {indexer}'s upload"));
                            l.have.iter_mut().for_each(|h| *h = 0);
                            l.missing = 0;
                            continue;
                        }
                        None => why,
                    }
                }
                Err(Stop::Transient(why)) if attempt < 8 => {
                    tracing::warn!("Usenet download {id}: {why} (retrying)");
                    {
                        let mut l = live.lock().unwrap();
                        l.note = Some(why);
                        l.speed = 0.0;
                    }
                    (self.notify)();
                    let until = Instant::now() + wait;
                    while Instant::now() < until {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(100).min(wait)).await;
                    }
                    wait *= 2;
                    continue;
                }
                Err(Stop::Transient(why)) => why,
            };
            tracing::warn!("Usenet download {id} failed: {failed}");
            self.with_job(id, |job| job.failed = Some(failed.clone()));
            let mut l = live.lock().unwrap();
            l.phase = Phase::Failed(failed);
            l.speed = 0.0;
            l.note = None;
            drop(l);
            (self.notify)();
            return;
        }
    }

    fn set_phase(&self, live: &Mutex<Live>, phase: Phase) {
        live.lock().unwrap().phase = phase;
        (self.notify)();
    }

    async fn attempt(&self, id: u64, live: &Mutex<Live>, cancel: &AtomicBool) -> Result<(), Stop> {
        let providers = self.providers.lock().unwrap().clone();
        if providers.is_empty() {
            return Err(Stop::Fatal(
                "No Usenet provider set up; add one in Settings".to_owned(),
            ));
        }
        let job = self.job(id).ok_or(Stop::Cancelled)?;
        if job.wanted().all(|(_, f)| job.complete(f)) {
            return Ok(());
        }
        self.set_phase(live, Phase::Fetching);
        let posted = self.nzb_files(&job).await?;
        self.not_a_repeat(&job, &posted)?;

        // The picked files, matched to the posted ones by name.
        let staging = job.staging();
        std::fs::create_dir_all(&staging)
            .map_err(|e| Stop::Fatal(format!("could not create {}: {e}", staging.display())))?;
        let mut targets = Vec::new();
        for (i, f) in job.wanted() {
            if job.complete(f) {
                live.lock().unwrap().have[i] = f.len;
                continue;
            }
            let posted_file = posted
                .iter()
                .find(|p| p.name == f.base_name())
                .or_else(|| {
                    posted
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(f.base_name()))
                })
                .ok_or_else(|| Stop::Nzb(format!("the NZB doesn't have {}", f.base_name())))?;
            let done = job.done.get(&posted_file.name).cloned().unwrap_or_default();
            targets.push(Target {
                path: staging.join(&posted_file.name),
                nzb: posted_file.clone(),
                len: Some(f.len),
                done,
                job_file: Some(i),
            });
        }

        self.set_phase(live, Phase::Downloading);
        let missing = match self.fetch(id, &providers, &mut targets, live, cancel).await {
            // Retrying won't fix a password.
            Err(Stop::Transient(why)) => {
                return Err(providers
                    .all_logins_failed()
                    .map_or(Stop::Transient(why), Stop::Fatal));
            }
            other => other?,
        };
        let damaged: Vec<String> = targets
            .iter()
            .zip(&missing)
            .filter(|(_, m)| **m > 0)
            .map(|(t, _)| t.nzb.name.clone())
            .collect();
        if !damaged.is_empty() {
            self.repair(id, &providers, &posted, &damaged, &staging, live, cancel)
                .await?;
        }
        self.finish(id, &staging)?;
        Ok(())
    }

    /// Another indexer's NZB that's the same upload as one that already
    /// failed would fail the same way: skip it.
    fn not_a_repeat(&self, job: &Job, posted: &[NzbFile]) -> Result<(), Stop> {
        let ids: std::collections::HashSet<&str> = posted
            .iter()
            .flat_map(|f| f.segments.iter().map(|s| s.message_id.as_str()))
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        for earlier in 0..job.nzb {
            let Ok(bytes) = std::fs::read(
                self.dir
                    .join("nzbs")
                    .join(format!("{}-{earlier}.nzb", job.id)),
            ) else {
                continue;
            };
            let Ok(before) = nzb::parse(&bytes) else {
                continue;
            };
            let shared = before
                .iter()
                .flat_map(|f| &f.segments)
                .filter(|s| ids.contains(s.message_id.as_str()))
                .count();
            if shared * 10 >= ids.len() * 9 {
                let (now, then) = (
                    job.nzbs[job.nzb].indexer.name(),
                    job.nzbs[earlier].indexer.name(),
                );
                return Err(Stop::Nzb(format!(
                    "{now}'s NZB is the same upload as {then}'s"
                )));
            }
        }
        Ok(())
    }

    /// The job's NZB, from disk or the indexer.
    async fn nzb_files(&self, job: &Job) -> Result<Vec<NzbFile>, Stop> {
        let path = self.nzb_path(job);
        let link = job
            .nzbs
            .get(job.nzb)
            .ok_or_else(|| Stop::Fatal("the release has no NZB".to_owned()))?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                let b =
                    self.indexers.fetch_nzb(link).await.map_err(|e| match e
                        .downcast_ref::<super::indexers::Unusable>(
                    ) {
                        Some(u) => Stop::Nzb(u.to_string()),
                        None => Stop::Transient(format!("{e:#}")),
                    })?;
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, &b);
                b
            }
        };
        nzb::parse(&bytes).map_err(|e| Stop::Nzb(format!("{}'s NZB: {e:#}", link.indexer.name())))
    }

    /// Fetch the targets' missing articles, writing each in place. Returns
    /// how many articles of each target no provider has.
    async fn fetch(
        &self,
        id: u64,
        providers: &Arc<Providers>,
        targets: &mut [Target],
        live: &Mutex<Live>,
        cancel: &AtomicBool,
    ) -> Result<Vec<u32>, Stop> {
        struct Task {
            target: usize,
            number: u32,
            message_id: String,
        }
        enum Done {
            Written {
                target: usize,
                number: u32,
                bytes: usize,
            },
            Missing {
                target: usize,
            },
            Unreachable(String),
            Mismatch(String),
        }

        let mut files = Vec::new();
        let mut queue = VecDeque::new();
        for (t, target) in targets.iter().enumerate() {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&target.path)
                .map_err(|e| {
                    Stop::Fatal(format!("could not write {}: {e}", target.path.display()))
                })?;
            if let Some(len) = target.len
                && file.metadata().map(|m| m.len()).unwrap_or(0) != len
            {
                file.set_len(len).map_err(|e| {
                    Stop::Fatal(format!("could not write {}: {e}", target.path.display()))
                })?;
            }
            files.push(Arc::new(file));
            for s in &target.nzb.segments {
                if !target.done.contains(&s.number) {
                    queue.push_back(Task {
                        target: t,
                        number: s.number,
                        message_id: s.message_id.clone(),
                    });
                }
            }
            if let Some(i) = target.job_file {
                live.lock().unwrap().have[i] = progress_bytes(target);
            }
        }
        if queue.is_empty() {
            return Ok(vec![0; targets.len()]);
        }

        let queue = Arc::new(Mutex::new(queue));
        let stop = Arc::new(AtomicBool::new(false));
        let lens: Vec<Option<u64>> = targets.iter().map(|t| t.len).collect();
        let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
        let workers = providers.workers().min(queue.lock().unwrap().len());
        let mut handles = Vec::new();
        for _ in 0..workers {
            let (queue, stop, tx, providers, files, lens) = (
                queue.clone(),
                stop.clone(),
                tx.clone(),
                providers.clone(),
                files.clone(),
                lens.clone(),
            );
            let retry = self.timing.retry;
            handles.push(self.rt.spawn(async move {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let Some(task) = queue.lock().unwrap().pop_front() else { return };
                    let mut skip = Vec::new();
                    let mut tries = 0;
                    let done = loop {
                        match providers.body(&task.message_id, &skip).await {
                            Ok((raw, slot)) => match yenc_simd::decode_yenc(&raw) {
                                Ok(part) => {
                                    let offset = part.part_begin.unwrap_or(0);
                                    let fits = lens[task.target].is_none_or(|len| {
                                        part.file_size.is_none_or(|s| s == len) && offset + part.data.len() as u64 <= len
                                    });
                                    if !fits {
                                        break Done::Mismatch(format!(
                                            "the posted file's size doesn't match the torrent's ({:?} vs {:?})",
                                            part.file_size, lens[task.target]
                                        ));
                                    }
                                    if let Err(e) = write_at(&files[task.target], offset, &part.data) {
                                        break Done::Unreachable(format!("could not write: {e}"));
                                    }
                                    break Done::Written { target: task.target, number: task.number, bytes: part.data.len() };
                                }
                                // A damaged copy: another provider may have a good one.
                                Err(e) => {
                                    tracing::debug!("article {} from {} is damaged: {e}", task.message_id, providers.name(slot));
                                    skip.push(slot);
                                }
                            },
                            Err(FetchError::Missing) => break Done::Missing { target: task.target },
                            Err(FetchError::Unreachable(why)) => {
                                tries += 1;
                                if tries > 3 {
                                    break Done::Unreachable(why);
                                }
                                tokio::time::sleep(retry * tries).await;
                            }
                        }
                    };
                    let _ = tx.send(done);
                }
            }));
        }
        drop(tx);

        let mut missing = vec![0u32; targets.len()];
        let mut outcome = Ok(());
        let mut last_save = Instant::now();
        let mut window = (Instant::now(), 0usize);
        let mut notified = Instant::now();
        // Until the workers finish: once stopping, articles already on their
        // way are still recorded, so nothing written is fetched again.
        while let Some(done) = rx.recv().await {
            let stopping = stop.load(Ordering::Relaxed);
            if cancel.load(Ordering::Relaxed) && !stopping {
                stop.store(true, Ordering::Relaxed);
                outcome = Err(Stop::Cancelled);
            }
            match done {
                Done::Written {
                    target,
                    number,
                    bytes,
                } => {
                    let t = &mut targets[target];
                    t.done.insert(number);
                    window.1 += bytes;
                    if let Some(i) = t.job_file {
                        let have = progress_bytes(t);
                        let mut l = live.lock().unwrap();
                        l.have[i] = have;
                        l.note = None;
                    }
                }
                Done::Missing { target } => {
                    missing[target] += 1;
                    live.lock().unwrap().missing += 1;
                    tracing::info!(
                        "an article of {} is on none of the providers",
                        targets[target].nzb.name
                    );
                }
                Done::Unreachable(why) if outcome.is_ok() => {
                    stop.store(true, Ordering::Relaxed);
                    outcome = Err(Stop::Transient(why));
                }
                Done::Mismatch(why) if outcome.is_ok() => {
                    stop.store(true, Ordering::Relaxed);
                    outcome = Err(Stop::Nzb(why));
                }
                Done::Unreachable(_) | Done::Mismatch(_) => {}
            }
            let elapsed = window.0.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let rate = window.1 as f64 / elapsed.as_secs_f64();
                let mut l = live.lock().unwrap();
                l.speed = if l.speed == 0.0 {
                    rate
                } else {
                    l.speed * 0.6 + rate * 0.4
                };
                window = (Instant::now(), 0);
            }
            if last_save.elapsed() >= self.timing.save {
                self.save_done(id, targets);
                last_save = Instant::now();
            }
            if notified.elapsed() >= Duration::from_millis(250) {
                (self.notify)();
                notified = Instant::now();
            }
        }
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.await;
        }
        self.save_done(id, targets);
        live.lock().unwrap().speed = 0.0;
        (self.notify)();
        outcome.map(|()| missing)
    }

    fn save_done(&self, id: u64, targets: &[Target]) {
        self.with_job(id, |job| {
            for t in targets.iter().filter(|t| t.job_file.is_some()) {
                job.done.insert(t.nzb.name.clone(), t.done.clone());
            }
        });
    }

    /// Repair the damaged files, each with its PAR2 set: the one named after
    /// it (releases often have one per episode), or the release's only one.
    #[allow(clippy::too_many_arguments)]
    async fn repair(
        &self,
        id: u64,
        providers: &Arc<Providers>,
        posted: &[NzbFile],
        damaged: &[String],
        staging: &Path,
        live: &Mutex<Live>,
        cancel: &AtomicBool,
    ) -> Result<(), Stop> {
        let missing = live.lock().unwrap().missing;
        let lost = |what: &str| lost(missing, what);
        let mut sets: BTreeMap<&str, Vec<&NzbFile>> = BTreeMap::new();
        for f in posted.iter().filter(|f| f.is_par2()) {
            sets.entry(f.par2_set().unwrap_or_default())
                .or_default()
                .push(f);
        }
        if sets.is_empty() {
            return Err(lost("the release has no PAR2 recovery data"));
        }
        let mut by_set: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for name in damaged {
            let own = sets
                .keys()
                .filter(|s| name.starts_with(&format!("{s}.")))
                .max_by_key(|s| s.len())
                .copied();
            let set = own.or_else(|| (sets.len() == 1).then(|| *sets.keys().next().unwrap()));
            let Some(set) = set else {
                return Err(lost(&format!("there's no PAR2 recovery data for {name}")));
            };
            by_set.entry(set).or_default().push(name);
        }
        for set in by_set.keys() {
            self.repair_set(id, providers, &sets[set], staging, live, cancel, missing)
                .await?;
        }
        Ok(())
    }

    /// Fetch a PAR2 set's index, then recovery volumes, smallest first,
    /// until there are enough blocks to repair; then repair.
    #[allow(clippy::too_many_arguments)]
    async fn repair_set(
        &self,
        id: u64,
        providers: &Arc<Providers>,
        set: &[&NzbFile],
        staging: &Path,
        live: &Mutex<Live>,
        cancel: &AtomicBool,
        missing: u32,
    ) -> Result<(), Stop> {
        let lost = |what: &str| lost(missing, what);
        // The index, or failing that the smallest volume (volumes carry the
        // same description of the files).
        let mut volumes: Vec<&NzbFile> = set.to_vec();
        volumes.sort_by_key(|f| {
            (
                f.recovery_blocks().is_some(),
                f.recovery_blocks(),
                f.encoded_bytes(),
            )
        });
        let index = volumes.remove(0);

        self.set_phase(
            live,
            Phase::Repairing("getting the recovery index".to_owned()),
        );
        let index_path = staging.join(&index.name);
        self.fetch_whole(id, providers, index, &index_path, live, cancel)
            .await?;
        let mut have_volumes: Vec<PathBuf> = Vec::new();
        let mut volumes = volumes.into_iter();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Stop::Cancelled);
            }
            let (dir, index, vols) = (
                staging.to_path_buf(),
                index_path.clone(),
                have_volumes.clone(),
            );
            let checked =
                tokio::task::spawn_blocking(move || repair::check(&dir, &index, &vols, false))
                    .await
                    .map_err(|e| Stop::Fatal(format!("PAR2 check crashed: {e}")))?
                    .map_err(|e| Stop::Nzb(format!("{e:#}")))?;
            match checked {
                Check::Intact => return Ok(()),
                Check::Repaired => break,
                Check::Short { blocks } => {
                    let Some(next) = volumes.next() else {
                        return Err(lost(&format!(
                            "the recovery data is {blocks} blocks short (repairing part of a batch needs the whole batch)"
                        )));
                    };
                    self.set_phase(
                        live,
                        Phase::Repairing(format!("getting recovery data ({blocks} blocks to go)")),
                    );
                    let path = staging.join(&next.name);
                    self.fetch_whole(id, providers, next, &path, live, cancel)
                        .await?;
                    have_volumes.push(path);
                }
            }
        }
        self.set_phase(live, Phase::Repairing("repairing".to_owned()));
        let (dir, index, vols) = (staging.to_path_buf(), index_path, have_volumes);
        let repaired =
            tokio::task::spawn_blocking(move || repair::check(&dir, &index, &vols, true))
                .await
                .map_err(|e| Stop::Fatal(format!("PAR2 repair crashed: {e}")))?
                .map_err(|e| Stop::Nzb(format!("{e:#}")))?;
        match repaired {
            Check::Intact | Check::Repaired => Ok(()),
            Check::Short { blocks } => Err(lost(&format!("repair came up {blocks} blocks short"))),
        }
    }

    /// Fetch a whole PAR2 file (articles that are missing are left out; PAR2
    /// copes with damaged packets).
    async fn fetch_whole(
        &self,
        id: u64,
        providers: &Arc<Providers>,
        file: &NzbFile,
        path: &Path,
        live: &Mutex<Live>,
        cancel: &AtomicBool,
    ) -> Result<(), Stop> {
        let missing_before = live.lock().unwrap().missing;
        let mut target = [Target {
            path: path.to_path_buf(),
            nzb: file.clone(),
            len: None,
            done: BTreeSet::new(),
            job_file: None,
        }];
        self.fetch(id, providers, &mut target, live, cancel).await?;
        // Missing recovery articles aren't missing data.
        live.lock().unwrap().missing = missing_before;
        Ok(())
    }

    /// Move the finished files into place and tidy up.
    fn finish(&self, id: u64, staging: &Path) -> Result<(), Stop> {
        let job = self.job(id).ok_or(Stop::Cancelled)?;
        for (_, f) in job.wanted() {
            if job.complete(f) {
                continue;
            }
            let from = staging.join(f.base_name());
            let from = if from.exists() {
                from
            } else {
                // Posted under a different case.
                std::fs::read_dir(staging)
                    .ok()
                    .and_then(|d| {
                        d.flatten().map(|e| e.path()).find(|p| {
                            p.file_name().is_some_and(|n| {
                                n.to_string_lossy().eq_ignore_ascii_case(f.base_name())
                            })
                        })
                    })
                    .unwrap_or(from)
            };
            let dest = job.dest(f);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Stop::Fatal(format!("could not create {}: {e}", parent.display()))
                })?;
            }
            std::fs::rename(&from, &dest)
                .map_err(|e| Stop::Fatal(format!("could not move {} into place: {e}", f.path)))?;
        }
        let _ = std::fs::remove_dir_all(staging);
        self.with_job(id, |job| job.done.clear());
        Ok(())
    }
}

/// Articles are missing everywhere, and they can't be made up for.
fn lost(missing: u32, what: &str) -> Stop {
    Stop::Nzb(format!(
        "{missing} articles are on none of your providers, and {what}"
    ))
}

/// Bytes a target has, judged by its articles (encoded sizes, scaled to the file).
fn progress_bytes(t: &Target) -> u64 {
    let total = t.nzb.encoded_bytes().max(1);
    let done: u64 = t
        .nzb
        .segments
        .iter()
        .filter(|s| t.done.contains(&s.number))
        .map(|s| u64::from(s.bytes))
        .sum();
    let len = t.len.unwrap_or(total);
    (u128::from(done) * u128::from(len) / u128::from(total)) as u64
}

/// Progress for each file of a saved job, before its NZB is read.
fn initial_have(job: &Job) -> Vec<u64> {
    job.files
        .iter()
        .map(|f| {
            if job.complete(f) {
                f.len
            } else if job
                .done
                .keys()
                .any(|k| k.eq_ignore_ascii_case(f.base_name()))
            {
                // Shown properly once the NZB is read.
                f.len / 100
            } else {
                0
            }
        })
        .collect()
}

fn write_at(file: &std::fs::File, offset: u64, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(data, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut offset, mut data) = (offset, data);
        while !data.is_empty() {
            let n = file.seek_write(data, offset)?;
            data = &data[n..];
            offset += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
