//! Debrid downloads: each torrent goes to the service, and once the service
//! has it, the files you picked come down over HTTPS into the same place a
//! torrent download would put them.
//!
//! Jobs are saved in `<state>/debrid/jobs.json` (with their .torrent files
//! alongside), so they carry on after a restart. A file downloads to
//! `<name>.part` and is renamed when complete; an interrupted download
//! resumes from the `.part` file where the server supports it.

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::{BadKey, Client, Gone, Provider, Remote, RemoteState, match_files};
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub provider: Provider,
    /// The service's ID for the torrent, once added.
    pub remote_id: Option<String>,
    pub name: String,
    pub info_hash: String,
    pub output_folder: PathBuf,
    pub files: Vec<JobFile>,
    pub paused: bool,
    /// The files the service was told to fetch (Real-Debrid only).
    #[serde(default)]
    pub remote_selection: Vec<usize>,
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Sending the torrent to the service.
    Adding,
    /// The service is fetching it.
    Remote {
        status: String,
        progress: Option<f32>,
        speed: Option<u64>,
        seeders: Option<u64>,
    },
    /// Downloading the files from the service.
    Downloading,
    Done,
    Paused,
    Failed(String),
}

/// How a job is doing right now.
#[derive(Clone, Debug)]
pub struct Live {
    pub phase: Phase,
    /// Bytes on disk for each file (same order as `Job::files`).
    pub have: Vec<u64>,
    /// Download speed from the service, bytes per second.
    pub speed: f64,
    /// A passing problem being retried.
    pub note: Option<String>,
}

impl Live {
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            Phase::Adding | Phase::Remote { .. } | Phase::Downloading
        )
    }
}

/// A job and how it's doing, for the UI.
#[derive(Clone, Debug)]
pub struct View {
    pub job: Job,
    pub live: Live,
}

impl View {
    pub fn have_bytes(&self) -> u64 {
        self.job.wanted().map(|(i, _)| self.live.have[i]).sum()
    }
}

struct Entry {
    job: Job,
    live: Arc<Mutex<Live>>,
    cancel: Arc<AtomicBool>,
}

/// Timings (shortened in tests).
#[derive(Clone, Copy)]
struct Timing {
    /// Between checks while the service fetches the torrent.
    poll: Duration,
    /// After a passing error; doubles each time.
    retry: Duration,
}

pub struct Manager {
    dir: PathBuf,
    http: reqwest::Client,
    rt: tokio::runtime::Handle,
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    keys: Mutex<BTreeMap<Provider, String>>,
    notify: Box<dyn Fn() + Send + Sync>,
    timing: Timing,
    #[cfg(test)]
    bases: Mutex<HashMap<Provider, String>>,
}

impl Manager {
    /// Load saved jobs from `<state_dir>/debrid` and carry on with the
    /// unfinished ones. `notify` is called when something changes.
    pub fn load(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        keys: BTreeMap<Provider, String>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build the HTTP client");
        Self::load_with(
            state_dir,
            rt,
            keys,
            notify,
            http,
            Timing {
                poll: Duration::from_secs(4),
                retry: Duration::from_secs(5),
            },
        )
    }

    fn load_with(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        keys: BTreeMap<Provider, String>,
        notify: impl Fn() + Send + Sync + 'static,
        http: reqwest::Client,
        timing: Timing,
    ) -> Arc<Self> {
        let dir = state_dir.join("debrid");
        let jobs: Vec<Job> = std::fs::read_to_string(dir.join("jobs.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let next_id = jobs.iter().map(|j| j.id + 1).max().unwrap_or(1);
        let entries = jobs
            .into_iter()
            .map(|job| {
                let have: Vec<u64> = job
                    .files
                    .iter()
                    .map(|f| on_disk(&job.dest(f), f.len))
                    .collect();
                let complete = job
                    .wanted()
                    .all(|(i, f)| have[i] == f.len && job.dest(f).exists());
                let phase = if complete {
                    Phase::Done
                } else if job.paused {
                    Phase::Paused
                } else {
                    Phase::Adding
                };
                Entry {
                    job,
                    live: Arc::new(Mutex::new(Live {
                        phase,
                        have,
                        speed: 0.0,
                        note: None,
                    })),
                    cancel: Arc::default(),
                }
            })
            .collect();
        let manager = Arc::new(Self {
            dir,
            http,
            rt,
            entries: Mutex::new(entries),
            next_id: AtomicU64::new(next_id),
            keys: Mutex::new(keys),
            notify: Box::new(notify),
            timing,
            #[cfg(test)]
            bases: Mutex::default(),
        });
        let unfinished: Vec<u64> = manager
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.live.lock().unwrap().phase == Phase::Adding)
            .map(|e| e.job.id)
            .collect();
        for id in unfinished {
            manager.start(id);
        }
        manager
    }

    /// Use these API keys from now on (jobs keep their own service).
    pub fn set_keys(&self, keys: BTreeMap<Provider, String>) {
        *self.keys.lock().unwrap() = keys;
    }

    /// A client for `provider` with its saved key.
    pub fn client(&self, provider: Provider) -> Result<Client> {
        let keys = self.keys.lock().unwrap();
        let key = keys
            .get(&provider)
            .map(|k| k.trim())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow!("no {} API key; add it in Settings", provider.name()))?;
        #[cfg(test)]
        if let Some(base) = self.bases.lock().unwrap().get(&provider) {
            return Ok(Client::with_base(self.http.clone(), provider, key, base));
        }
        Ok(Client::new(self.http.clone(), provider, key))
    }

    /// Send `provider`'s requests to a mock server (tests).
    #[cfg(test)]
    pub fn use_base_for_test(&self, provider: Provider, base: &str) {
        self.bases.lock().unwrap().insert(provider, base.to_owned());
    }

    fn torrent_path(&self, info_hash: &str) -> PathBuf {
        self.dir
            .join("torrents")
            .join(format!("{info_hash}.torrent"))
    }

    fn save(&self, entries: &[Entry]) {
        let jobs: Vec<&Job> = entries.iter().map(|e| &e.job).collect();
        let result = serde_json::to_vec_pretty(&jobs)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::fsutil::write_private(&self.dir.join("jobs.json"), &bytes));
        if let Err(e) = result {
            tracing::warn!("could not save the debrid downloads: {e:#}");
        }
    }

    /// Download `wanted` (torrent file indices) of a torrent through
    /// `provider` into `output_folder`. If the torrent is already a job,
    /// its selection changes instead.
    pub fn add(
        self: &Arc<Self>,
        provider: Provider,
        listing: &Listing,
        wanted: &[usize],
        output_folder: PathBuf,
    ) -> Result<()> {
        if wanted.is_empty() {
            bail!("no files selected");
        }
        let info_hash =
            crate::engine::info_hash_of(&listing.torrent_bytes).context("invalid .torrent file")?;
        if let Some(id) = self.job_for(&info_hash) {
            return self.reselect(id, wanted);
        }
        self.client(provider)?;
        let path = self.torrent_path(&info_hash);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, &listing.torrent_bytes)
            .with_context(|| format!("could not save {}", path.display()))?;
        let files: Vec<JobFile> = listing
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
            provider,
            remote_id: None,
            name: listing.name.clone(),
            info_hash,
            output_folder,
            files,
            paused: false,
            remote_selection: Vec::new(),
        };
        let have: Vec<u64> = job
            .files
            .iter()
            .map(|f| on_disk(&job.dest(f), f.len))
            .collect();
        {
            let mut entries = self.entries.lock().unwrap();
            entries.push(Entry {
                job,
                live: Arc::new(Mutex::new(Live {
                    phase: Phase::Adding,
                    have,
                    speed: 0.0,
                    note: None,
                })),
                cancel: Arc::default(),
            });
            self.save(&entries);
        }
        self.start(id);
        Ok(())
    }

    /// Change which files a job downloads. Files already downloaded stay;
    /// partial downloads of files no longer wanted are deleted.
    pub fn reselect(self: &Arc<Self>, id: u64, wanted: &[usize]) -> Result<()> {
        {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .iter_mut()
                .find(|e| e.job.id == id)
                .context("no such download")?;
            for f in &mut entry.job.files {
                let was = f.wanted;
                f.wanted = wanted.contains(&f.index);
                if was && !f.wanted {
                    let _ = std::fs::remove_file(part_path(&entry.job.output_folder.join(&f.path)));
                }
            }
            entry.job.paused = false;
            entry.cancel.store(true, Ordering::Relaxed);
            entry.cancel = Arc::default();
            self.save(&entries);
        }
        self.start(id);
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
        let info_hash = info_hash.to_ascii_lowercase();
        let entries = self.entries.lock().unwrap();
        let job = &entries.iter().find(|e| e.job.info_hash == info_hash)?.job;
        Some(Existing {
            only_files: Some(
                job.files
                    .iter()
                    .filter(|f| f.wanted)
                    .map(|f| f.index)
                    .collect(),
            ),
            output_folder: job.output_folder.clone(),
        })
    }

    /// The torrent's contents with the job's selection, to change it.
    pub fn listing(&self, id: u64) -> Result<Listing> {
        let info_hash = self.view(id).context("no such download")?.job.info_hash;
        let bytes = std::fs::read(self.torrent_path(&info_hash))
            .context("the download's .torrent file is missing")?;
        let mut listing = crate::engine::listing_from_bytes(bytes)?;
        listing.existing = self.existing(&info_hash);
        Ok(listing)
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
    }

    /// Resume a paused job, or retry a failed one.
    pub fn resume(self: &Arc<Self>, id: u64) {
        {
            let mut entries = self.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.job.id == id) else {
                return;
            };
            e.job.paused = false;
            e.cancel.store(true, Ordering::Relaxed);
            e.cancel = Arc::default();
            self.save(&entries);
        }
        self.start(id);
    }

    /// Remove a job, deleting it from the service too, and optionally its files.
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
        for f in &job.files {
            let dest = job.dest(f);
            let _ = std::fs::remove_file(part_path(&dest));
            if delete_files && f.wanted {
                let _ = std::fs::remove_file(&dest);
            }
        }
        if delete_files && job.output_folder.exists() {
            remove_empty_dirs(&job.output_folder);
        }
        let _ = std::fs::remove_file(self.torrent_path(&job.info_hash));
        if let (Some(remote), Ok(client)) = (job.remote_id, self.client(job.provider)) {
            self.rt.spawn(async move {
                if let Err(e) = client.delete(&remote).await {
                    tracing::warn!(
                        "could not delete the torrent from {}: {e:#}",
                        client.provider.name()
                    );
                }
            });
        }
        (self.notify)();
    }

    /// Stop all downloads (they resume from their partial files next time).
    pub fn stop(&self) {
        for e in self.entries.lock().unwrap().iter() {
            e.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// A series' finished video files: this job's, plus those of other jobs
    /// in the same folder (one torrent per episode), unless that's the
    /// download folder itself.
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
            for (i, f) in v.job.wanted() {
                let dest = v.job.dest(f);
                if !crate::remux::is_video(&dest) {
                    continue;
                }
                if v.live.have[i] == f.len && dest.exists() {
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

    /// Run the job in the background.
    fn start(self: &Arc<Self>, id: u64) {
        let Some((live, cancel)) = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.job.id == id)
            .map(|e| (e.live.clone(), e.cancel.clone()))
        else {
            return;
        };
        {
            let mut l = live.lock().unwrap();
            l.phase = Phase::Adding;
            l.note = None;
        }
        (self.notify)();
        let me = self.clone();
        self.rt.spawn(async move {
            let result = me.run(id, &live, &cancel).await;
            if cancel.load(Ordering::Relaxed) {
                return; // paused, removed or restarted: whoever did it set the state
            }
            {
                let mut l = live.lock().unwrap();
                l.speed = 0.0;
                l.note = None;
                l.phase = match result {
                    Ok(()) => Phase::Done,
                    Err(e) => Phase::Failed(format!("{e:#}")),
                };
            }
            (me.notify)();
        });
    }

    fn job(&self, id: u64) -> Result<Job> {
        let entries = self.entries.lock().unwrap();
        Ok(entries
            .iter()
            .find(|e| e.job.id == id)
            .context("the download was removed")?
            .job
            .clone())
    }

    fn update_job(&self, id: u64, f: impl FnOnce(&mut Job)) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.job.id == id) {
            f(&mut e.job);
        }
        self.save(&entries);
    }

    fn set_phase(&self, live: &Mutex<Live>, phase: Phase) {
        let changed = {
            let mut l = live.lock().unwrap();
            let changed = l.phase != phase;
            l.phase = phase;
            changed
        };
        if changed {
            (self.notify)();
        }
    }

    /// Take the job through the service and download its files, retrying
    /// passing errors.
    async fn run(&self, id: u64, live: &Mutex<Live>, cancel: &AtomicBool) -> Result<()> {
        let provider = self.job(id)?.provider;
        let client = self.client(provider)?;
        let info_hash = self.job(id)?.info_hash;
        let torrent = std::fs::read(self.torrent_path(&info_hash))
            .context("the download's .torrent file is missing")?;
        let mut retries = 0u32;
        let mut re_adds = 0u32;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            match self.pass(id, &client, &torrent, live, cancel).await {
                Ok(()) => return Ok(()),
                Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
                Err(e)
                    if e.downcast_ref::<BadKey>().is_some()
                        || e.downcast_ref::<Permanent>().is_some() =>
                {
                    return Err(e);
                }
                Err(e) if e.downcast_ref::<Gone>().is_some() => {
                    // Deleted from the account (or it needs other files picked): add it again.
                    re_adds += 1;
                    if re_adds > 3 {
                        return Err(
                            e.context(format!("{} keeps losing the torrent", provider.name()))
                        );
                    }
                    self.update_job(id, |j| j.remote_id = None);
                }
                Err(e) => {
                    retries += 1;
                    if retries > 8 {
                        return Err(e);
                    }
                    let wait = self.timing.retry * 2u32.pow(retries.min(6) - 1);
                    live.lock().unwrap().note =
                        Some(format!("{e:#}; retrying in {}s", wait.as_secs().max(1)));
                    (self.notify)();
                    sleep_unless(wait, cancel).await;
                }
            }
        }
    }

    /// One attempt: make sure the service has the torrent, wait for it, and
    /// download whatever is still missing.
    async fn pass(
        &self,
        id: u64,
        client: &Client,
        torrent: &[u8],
        live: &Mutex<Live>,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let job = self.job(id)?;
        let wanted: Vec<(usize, JobFile)> = job.wanted().map(|(i, f)| (i, f.clone())).collect();
        let missing: Vec<&(usize, JobFile)> = wanted
            .iter()
            .filter(|(_, f)| !complete(&job.dest(f), f.len))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        let remote_id = match &job.remote_id {
            Some(r) => r.clone(),
            None => {
                self.set_phase(live, Phase::Adding);
                let r = client
                    .add(torrent, &job.name)
                    .await
                    .map_err(permanent_unless_key)?;
                self.update_job(id, |j| {
                    j.remote_id = Some(r.clone());
                    j.remote_selection.clear();
                });
                r
            }
        };

        // Wait for the service.
        let mut polls = 0u32;
        let remote: Remote = loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let remote = client
                .status(&remote_id)
                .await
                .context("checking on the download")?;
            match &remote.state {
                RemoteState::Ready => break remote,
                RemoteState::Failed(message) => {
                    return Err(anyhow::Error::new(Permanent(message.clone())));
                }
                RemoteState::NeedsSelection => {
                    let wanted_paths: Vec<(&str, u64)> = wanted
                        .iter()
                        .map(|(_, f)| (f.path.as_str(), f.len))
                        .collect();
                    let matched = match_files(&wanted_paths, &remote.files);
                    if let Some(pos) = matched.iter().position(Option::is_none) {
                        return Err(Permanent::error(format!(
                            "{} doesn't list “{}”",
                            client.provider.name(),
                            wanted[pos].1.path
                        )));
                    }
                    let ids: Vec<String> = matched
                        .iter()
                        .flatten()
                        .map(|&r| remote.files[r].id.clone())
                        .collect();
                    client.select(&remote_id, &ids).await?;
                    self.update_job(id, |j| {
                        j.remote_selection = wanted.iter().map(|(_, f)| f.index).collect()
                    });
                }
                RemoteState::Working {
                    status,
                    progress,
                    speed,
                    seeders,
                } => {
                    self.set_phase(
                        live,
                        Phase::Remote {
                            status: status.clone(),
                            progress: *progress,
                            speed: *speed,
                            seeders: *seeders,
                        },
                    );
                    let wait = if polls < 15 {
                        self.timing.poll
                    } else {
                        self.timing.poll * 3
                    };
                    polls += 1;
                    sleep_unless(wait, cancel).await;
                }
            }
        };

        // Which of the service's files is which.
        let wanted_paths: Vec<(&str, u64)> = missing
            .iter()
            .map(|(_, f)| (f.path.as_str(), f.len))
            .collect();
        let matched = match_files(&wanted_paths, &remote.files);
        for (k, m) in matched.iter().enumerate() {
            let selected = m.is_some_and(|r| remote.files[r].selected);
            if !selected {
                if client.selects_files() && m.is_some() {
                    // Real-Debrid fetched other files: start over with the new selection.
                    let _ = client.delete(&remote_id).await;
                    return Err(anyhow::Error::new(Gone));
                }
                return Err(Permanent::error(format!(
                    "{} doesn't list “{}”",
                    client.provider.name(),
                    missing[k].1.path
                )));
            }
        }

        self.set_phase(live, Phase::Downloading);
        live.lock().unwrap().note = None;
        for ((i, file), r) in missing.iter().zip(matched) {
            let remote_file = &remote.files[r.expect("checked above")];
            let url = client
                .link(&remote_id, remote_file)
                .await
                .context("asking for the download link")?;
            let dest = job.dest(file);
            fetch(&self.http, &url, &dest, file.len, cancel, |have, speed| {
                let mut l = live.lock().unwrap();
                l.have[*i] = have;
                l.speed = speed;
                drop(l);
                (self.notify)();
            })
            .await
            .with_context(|| format!("downloading {}", file.path))?;
            live.lock().unwrap().have[*i] = file.len;
        }
        Ok(())
    }
}

/// An error retrying won't fix (the service said no).
#[derive(Debug)]
struct Permanent(String);

impl Permanent {
    fn error(message: String) -> anyhow::Error {
        anyhow::Error::new(Self(message))
    }
}

impl std::fmt::Display for Permanent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Permanent {}

/// Adding a torrent fails for reasons like limits or a bad torrent, which
/// retrying won't fix; a bad key stays a bad key.
fn permanent_unless_key(e: anyhow::Error) -> anyhow::Error {
    if e.downcast_ref::<BadKey>().is_some() || e.to_string().contains("could not reach") {
        e
    } else {
        Permanent::error(format!("{e:#}"))
    }
}

/// `<file>.part`
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    dest.with_file_name(name)
}

fn complete(dest: &Path, len: u64) -> bool {
    std::fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == len)
}

/// Bytes of a file on disk: all of it if complete, else its `.part`.
fn on_disk(dest: &Path, len: u64) -> u64 {
    if complete(dest, len) {
        return len;
    }
    std::fs::metadata(part_path(dest))
        .map(|m| m.len().min(len))
        .unwrap_or(0)
}

async fn sleep_unless(duration: Duration, cancel: &AtomicBool) {
    let until = Instant::now() + duration;
    while Instant::now() < until && !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep((until - Instant::now()).min(Duration::from_millis(200))).await;
    }
}

async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Download `url` to `dest` (via `dest.part`), resuming a partial file
/// where the server allows it. `progress` gets bytes so far and the speed.
pub(crate) async fn fetch(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    len: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, f64),
) -> Result<()> {
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let part = part_path(dest);
    let mut have = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if have > len {
        have = 0;
    }
    if have < len || len == 0 {
        let mut req = http.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req
            .send()
            .await
            .context("could not reach the download server")?;
        let status = resp.status();
        let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT
            && resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|r| r.starts_with(&format!("bytes {have}-")));
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // Our partial file is no good to the server: start over.
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the server refused to resume");
        }
        if !status.is_success() {
            bail!("the download server returned HTTP {status}");
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .await?;
        if resumed {
            file.seek(std::io::SeekFrom::Start(have)).await?;
        } else {
            // A full copy (the server ignored the range, or there was none).
            have = 0;
            file.set_len(0).await?;
        }
        let mut file = tokio::io::BufWriter::with_capacity(1 << 20, file);
        let started = Instant::now();
        let start_have = have;
        let mut last = Instant::now();
        let mut speed = 0.0f64;
        loop {
            let chunk = tokio::select! {
                chunk = resp.chunk() => chunk.context("the download was interrupted")?,
                () = cancelled(cancel) => {
                    file.flush().await?;
                    bail!("cancelled");
                }
            };
            let Some(chunk) = chunk else { break };
            if have + chunk.len() as u64 > len {
                bail!("the server sent more than the file's {len} bytes");
            }
            file.write_all(&chunk).await?;
            have += chunk.len() as u64;
            if last.elapsed() >= Duration::from_millis(250) {
                let elapsed = started.elapsed().as_secs_f64().max(0.001);
                let average = (have - start_have) as f64 / elapsed;
                speed = if speed == 0.0 {
                    average
                } else {
                    speed * 0.7 + average * 0.3
                };
                progress(have, speed);
                last = Instant::now();
            }
        }
        file.flush().await?;
        file.into_inner().sync_all().await?;
        if have != len {
            bail!("the download stopped at {have} of {len} bytes");
        }
    }
    tokio::fs::rename(&part, dest)
        .await
        .with_context(|| format!("could not finish {}", dest.display()))?;
    progress(len, 0.0);
    Ok(())
}

/// Remove `dir` and any empty folders inside it, if they're empty.
pub(crate) fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::AtomicUsize;

    use serde_json::json;

    use super::*;
    use crate::mock_http::{Resp, mock, reply};

    /// A minimal multi-file .torrent (piece hashes are zeros; nothing checks them here).
    pub fn make_torrent(name: &str, files: &[(&str, u64)]) -> Vec<u8> {
        fn s(out: &mut Vec<u8>, v: &[u8]) {
            out.extend_from_slice(format!("{}:", v.len()).as_bytes());
            out.extend_from_slice(v);
        }
        let piece = 16384u64;
        let total: u64 = files.iter().map(|f| f.1).sum();
        let mut out = b"d8:announce".to_vec();
        s(&mut out, b"http://tracker.invalid/announce");
        out.extend_from_slice(b"4:infod5:filesl");
        for (path, len) in files {
            out.extend_from_slice(format!("d6:lengthi{len}e4:pathl").as_bytes());
            for part in path.split('/') {
                s(&mut out, part.as_bytes());
            }
            out.extend_from_slice(b"ee");
        }
        out.extend_from_slice(b"e4:name");
        s(&mut out, name.as_bytes());
        out.extend_from_slice(format!("12:piece lengthi{piece}e6:pieces").as_bytes());
        s(&mut out, &vec![0u8; (total.div_ceil(piece) * 20) as usize]);
        out.extend_from_slice(b"ee");
        out
    }

    pub fn content(seed: u8, len: u64) -> String {
        (0..len)
            .map(|i| (b'a' + ((i as u8).wrapping_add(seed)) % 26) as char)
            .collect()
    }

    /// Serve `body` honouring a `Range: bytes=N-` header.
    pub fn serve(req: &crate::mock_http::Req, body: &str) -> Resp {
        match req
            .headers
            .get("range")
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.strip_suffix('-'))
        {
            Some(start) => {
                let start: usize = start.parse().unwrap();
                Resp {
                    status: 206,
                    headers: vec![(
                        "Content-Range".into(),
                        format!("bytes {start}-{}/{}", body.len() - 1, body.len()),
                    )],
                    body: body.as_bytes()[start..].to_vec(),
                }
            }
            None => Resp {
                status: 200,
                headers: vec![],
                body: body.as_bytes().to_vec(),
            },
        }
    }

    async fn wait_for(m: &Manager, id: u64, what: &str, done: impl Fn(&View) -> bool) -> View {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let v = m.view(id).expect("job exists");
            if done(&v) {
                return v;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                v.live
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn manager(state: &Path, provider: Provider, base: &str) -> Arc<Manager> {
        let keys = BTreeMap::from([(provider, "key".to_owned())]);
        let timing = Timing {
            poll: Duration::from_millis(20),
            retry: Duration::from_millis(20),
        };
        let m = Manager::load_with(
            state,
            tokio::runtime::Handle::current(),
            keys,
            || {},
            reqwest::Client::new(),
            timing,
        );
        m.bases.lock().unwrap().insert(provider, base.to_owned());
        m
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ichioshi-debrid-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A Real-Debrid account that holds one torrent at a time.
    #[derive(Default)]
    struct Rd {
        adds: usize,
        selected: Vec<String>,
        polls: usize,
        deleted: Vec<String>,
        gets: HashMap<String, usize>,
        ranges: Vec<String>,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_debrid_job_end_to_end() {
        let files = [
            ("Show - 01.mkv", 300_000u64),
            ("Show - 02.mkv", 200_000),
            ("Extras/NCOP.mkv", 100_000),
        ];
        let bodies: Vec<String> = files
            .iter()
            .enumerate()
            .map(|(i, f)| content(i as u8, f.1))
            .collect();
        let torrent = make_torrent("Show S01", &files);
        let listing = crate::engine::listing_from_bytes(torrent.clone()).unwrap();

        let rd = Arc::new(Mutex::new(Rd::default()));
        let base_cell = Arc::new(Mutex::new(String::new()));
        let (state_rd, base_for, served) = (rd.clone(), base_cell.clone(), bodies.clone());
        let base = mock(move |req| {
            let mut s = state_rd.lock().unwrap();
            let path = req.target.split('?').next().unwrap().to_owned();
            let id = format!("T{}", s.adds);
            match (req.method.as_str(), path.as_str()) {
                ("PUT", "/torrents/addTorrent") => {
                    s.adds += 1;
                    s.selected.clear();
                    s.polls = 0;
                    reply(201, json!({"id": format!("T{}", s.adds)}))
                }
                ("GET", p) if p == format!("/torrents/info/{id}") => {
                    let picked: Vec<String> = s.selected.last().map(|l| l.split(',').map(str::to_owned).collect()).unwrap_or_default();
                    let fs: Vec<serde_json::Value> = files
                        .iter()
                        .enumerate()
                        .map(|(i, (p, len))| json!({"id": i + 1, "path": format!("/{p}"), "bytes": len, "selected": i32::from(picked.contains(&(i + 1).to_string()))}))
                        .collect();
                    if picked.is_empty() {
                        return reply(200, json!({"status": "waiting_files_selection", "files": fs, "links": []}));
                    }
                    s.polls += 1;
                    if s.polls == 1 {
                        return reply(200, json!({"status": "downloading", "progress": 50, "speed": 1000, "seeders": 5, "files": fs, "links": []}));
                    }
                    let links: Vec<String> = picked.iter().map(|n| format!("https://real-debrid.com/d/{n}")).collect();
                    reply(200, json!({"status": "downloaded", "progress": 100, "files": fs, "links": links}))
                }
                ("POST", p) if p.starts_with("/torrents/selectFiles/") => {
                    let files = req.form()["files"].clone();
                    s.selected.push(files);
                    Resp { status: 204, headers: vec![], body: Vec::new() }
                }
                ("POST", "/unrestrict/link") => {
                    let n = req.form()["link"].rsplit('/').next().unwrap().to_owned();
                    reply(200, json!({"download": format!("{}/files/{n}", base_for.lock().unwrap())}))
                }
                ("GET", p) if p.starts_with("/files/") => {
                    let n: usize = p.rsplit('/').next().unwrap().parse().unwrap();
                    let count = {
                        let c = s.gets.entry(p.to_owned()).or_default();
                        *c += 1;
                        *c
                    };
                    if let Some(r) = req.headers.get("range") {
                        s.ranges.push(format!("{n}:{r}"));
                    }
                    // The first try at the first episode hits a server hiccup.
                    if n == 1 && count == 1 {
                        return reply(503, json!({}));
                    }
                    serve(req, &served[n - 1])
                }
                ("DELETE", p) if p.starts_with("/torrents/delete/") => {
                    s.deleted.push(p.rsplit('/').next().unwrap().to_owned());
                    Resp { status: 204, headers: vec![], body: Vec::new() }
                }
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        *base_cell.lock().unwrap() = base.clone();

        let dir = scratch("rd");
        let state = dir.join("state");
        let out = dir.join("Show S01");
        // Part of episode 1 is already here from an earlier, interrupted run.
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(part_path(&out.join("Show - 01.mkv")), &bodies[0][..1000]).unwrap();

        let m = manager(&state, Provider::RealDebrid, &base);
        m.add(Provider::RealDebrid, &listing, &[0, 2], out.clone())
            .unwrap();
        let id = m.job_for(&listing_hash(&torrent)).unwrap();
        let v = wait_for(&m, id, "episodes 1 and NCOP", |v| {
            v.live.phase == Phase::Done
        })
        .await;
        assert_eq!(v.have_bytes(), 400_000);
        assert_eq!(
            std::fs::read_to_string(out.join("Show - 01.mkv")).unwrap(),
            bodies[0]
        );
        assert_eq!(
            std::fs::read_to_string(out.join("Extras/NCOP.mkv")).unwrap(),
            bodies[2]
        );
        assert!(!out.join("Show - 02.mkv").exists(), "not selected");
        assert!(!part_path(&out.join("Show - 01.mkv")).exists());
        {
            let s = rd.lock().unwrap();
            assert_eq!(
                s.selected,
                ["1,3"],
                "only the picked files are fetched by Real-Debrid"
            );
            assert_eq!(
                s.ranges,
                ["1:bytes=1000-", "1:bytes=1000-"],
                "resumed from the partial file, also after the 503"
            );
        }

        // Pick episode 2 too: Real-Debrid didn't fetch it, so the torrent is re-added.
        m.add(Provider::RealDebrid, &listing, &[0, 1, 2], out.clone())
            .unwrap();
        let v = wait_for(&m, id, "episode 2", |v| {
            v.live.phase == Phase::Done && v.have_bytes() == 600_000
        })
        .await;
        assert_eq!(v.job.remote_id.as_deref(), Some("T2"));
        assert_eq!(
            std::fs::read_to_string(out.join("Show - 02.mkv")).unwrap(),
            bodies[1]
        );
        {
            let s = rd.lock().unwrap();
            assert_eq!(s.adds, 2);
            assert_eq!(s.deleted, ["T1"]);
            assert_eq!(s.selected.last().unwrap(), "1,2,3");
            assert_eq!(
                s.gets.get("/files/3"),
                Some(&1),
                "finished files aren't downloaded again"
            );
        }

        // Series files for remuxing, and the file picker's view of the job.
        let series = m.series_files(id, &dir).unwrap();
        assert_eq!(series.files.len(), 3);
        assert_eq!(series.not_ready, 0);
        assert_eq!(
            m.listing(id).unwrap().existing.unwrap().only_files,
            Some(vec![0, 1, 2])
        );

        // A restart finds it done.
        m.stop();
        drop(m);
        let m = manager(&state, Provider::RealDebrid, &base);
        let v = m.view(id).unwrap();
        assert_eq!(
            (v.live.phase.clone(), v.have_bytes()),
            (Phase::Done, 600_000)
        );

        // Removing it deletes the files and the torrent on Real-Debrid.
        m.remove(id, true);
        assert!(m.views().is_empty());
        assert!(!out.exists(), "files and the emptied folder are gone");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rd.lock().unwrap().deleted.len() < 2 {
            assert!(Instant::now() < deadline, "remote delete");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(rd.lock().unwrap().deleted, ["T1", "T2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn listing_hash(torrent: &[u8]) -> String {
        crate::engine::info_hash_of(torrent).unwrap()
    }

    /// TorBox's file server ignoring the range: the partial file is replaced
    /// by a full copy. Pausing stops the job and resuming finishes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn ignores_unhonoured_ranges_and_pauses() {
        let body = content(7, 50_000);
        let torrent = make_torrent("Movie", &[("Movie.mkv", 50_000)]);
        let listing = crate::engine::listing_from_bytes(torrent.clone()).unwrap();
        let hold = Arc::new(AtomicBool::new(true));
        let polls = Arc::new(AtomicUsize::new(0));
        let base_cell = Arc::new(Mutex::new(String::new()));
        let (hold_in, polls_in, base_for, body_in) =
            (hold.clone(), polls.clone(), base_cell.clone(), body.clone());
        let base = mock(move |req| {
            let ok = |data: serde_json::Value| reply(200, json!({"success": true, "data": data}));
            match req.target.split('?').next().unwrap() {
                "/torrents/createtorrent" => ok(json!({"torrent_id": 3, "hash": "h"})),
                "/torrents/mylist" => {
                    polls_in.fetch_add(1, Ordering::SeqCst);
                    let ready = !hold_in.load(Ordering::SeqCst);
                    ok(json!({"id": 3, "download_state": if ready { "cached" } else { "downloading" }, "progress": 0.3,
                        "download_finished": ready, "download_present": ready,
                        "files": [{"id": 0, "name": "Movie/Movie.mkv", "size": 50_000}]}))
                }
                "/torrents/requestdl" => ok(json!(format!("{}/cdn/movie", base_for.lock().unwrap()))),
                "/cdn/movie" => Resp { status: 200, headers: vec![], body: body_in.clone().into_bytes() },
                other => panic!("unexpected {other}"),
            }
        })
        .await;
        *base_cell.lock().unwrap() = base.clone();
        let dir = scratch("tb");
        let out = dir.join("Movie");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            part_path(&out.join("Movie.mkv")),
            "garbage that isn't the start of the file",
        )
        .unwrap();

        let m = manager(&dir.join("state"), Provider::TorBox, &base);
        m.add(Provider::TorBox, &listing, &[0], out.clone())
            .unwrap();
        let id = m.job_for(&listing_hash(&torrent)).unwrap();
        let v = wait_for(&m, id, "TorBox fetching", |v| {
            matches!(v.live.phase, Phase::Remote { .. })
        })
        .await;
        assert!(
            matches!(v.live.phase, Phase::Remote { progress: Some(p), .. } if (p - 0.3).abs() < 1e-6)
        );

        m.pause(id);
        assert_eq!(m.view(id).unwrap().live.phase, Phase::Paused);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after_pause = polls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            polls.load(Ordering::SeqCst),
            after_pause,
            "a paused job stops polling"
        );

        hold.store(false, Ordering::SeqCst);
        m.resume(id);
        wait_for(&m, id, "done", |v| v.live.phase == Phase::Done).await;
        assert_eq!(
            std::fs::read_to_string(out.join("Movie.mkv")).unwrap(),
            body
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A torrent the service can't get fails with the service's reason; a
    /// rejected key fails right away.
    #[tokio::test(flavor = "multi_thread")]
    async fn reports_service_failures() {
        let torrent = make_torrent("Dead", &[("Dead.mkv", 10)]);
        let listing = crate::engine::listing_from_bytes(torrent.clone()).unwrap();
        let base = mock(|req| match req.target.split('?').next().unwrap() {
            "/v4/magnet/upload/file" => reply(200, json!({"status": "success", "data": {"files": [{"id": 1, "ready": false}]}})),
            "/v4.1/magnet/status" => reply(200, json!({"status": "success", "data": {"magnets": {"id": 1, "status": "File not available - no peer", "statusCode": 15}}})),
            other => panic!("unexpected {other}"),
        })
        .await;
        let dir = scratch("ad");
        let m = manager(&dir.join("state"), Provider::AllDebrid, &base);
        m.add(Provider::AllDebrid, &listing, &[0], dir.join("Dead"))
            .unwrap();
        let id = m.job_for(&listing_hash(&torrent)).unwrap();
        let v = wait_for(&m, id, "failure", |v| {
            matches!(v.live.phase, Phase::Failed(_))
        })
        .await;
        assert_eq!(
            v.live.phase,
            Phase::Failed("AllDebrid: File not available - no peer".into())
        );

        let bad = mock(|_| reply(401, json!({"error": "bad_token", "error_code": 8}))).await;
        let m = manager(&dir.join("state2"), Provider::RealDebrid, &bad);
        m.add(Provider::RealDebrid, &listing, &[0], dir.join("Dead"))
            .unwrap();
        let v = wait_for(&m, id, "bad key", |v| {
            matches!(v.live.phase, Phase::Failed(_))
        })
        .await;
        assert_eq!(
            v.live.phase,
            Phase::Failed("Real-Debrid doesn't accept the API key; check it in Settings".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
