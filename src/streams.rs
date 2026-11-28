//! 18+ anime from streaming sites, after the trackers: hstream.moe (its
//! video and audio joined into one file) and ohentai.org (one MP4). A show's
//! episodes are found on them by its names, and each episode is a job that
//! downloads into the show's folder here, as "<Show> - 01.mp4".
//!
//! Only what the sites' own players read is used: nothing that gets past a
//! captcha or a bot check.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// A streaming site, in the order they're used (after the trackers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StreamSite {
    Hstream,
    Ohentai,
}

impl StreamSite {
    pub const ALL: [StreamSite; 2] = [StreamSite::Hstream, StreamSite::Ohentai];

    pub fn name(self) -> &'static str {
        match self {
            StreamSite::Hstream => "hstream.moe",
            StreamSite::Ohentai => "ohentai.org",
        }
    }

    /// Ties between releases of the same resolution go to the trackers,
    /// then hstream.moe, then ohentai.org (higher wins).
    pub fn preference(self) -> u8 {
        match self {
            StreamSite::Hstream => 2,
            StreamSite::Ohentai => 1,
        }
    }
}

/// An episode's page on a streaming site.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamEpisode {
    pub site: StreamSite,
    pub episode: u32,
    pub url: String,
    pub title: String,
}

/// A site's episodes of one show, and the best resolution it has.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub site: StreamSite,
    pub episodes: Vec<StreamEpisode>,
    pub resolution: Option<u32>,
}

/// The show's episodes on a site (`None`: it isn't there).
pub async fn find(
    client: &reqwest::Client,
    site: StreamSite,
    names: &[String],
) -> Result<Option<Found>> {
    let (episodes, mut resolution) = match site {
        StreamSite::Ohentai => {
            let eps = crate::ohentai::episodes(client, names).await?;
            let res = eps.iter().filter_map(|e| e.resolution).max();
            let eps: Vec<StreamEpisode> = eps
                .into_iter()
                .map(|e| StreamEpisode {
                    site,
                    episode: e.episode,
                    url: e.url,
                    title: e.title,
                })
                .collect();
            (eps, res)
        }
        StreamSite::Hstream => {
            let eps = crate::hstream::episodes(client, names).await?;
            let res = eps.iter().filter_map(|e| e.resolution).max();
            let eps: Vec<StreamEpisode> = eps
                .into_iter()
                .map(|e| StreamEpisode {
                    site,
                    episode: e.episode,
                    url: e.url,
                    title: e.title,
                })
                .collect();
            (eps, res)
        }
    };
    if episodes.is_empty() {
        return Ok(None);
    }
    // ohentai only says its resolution in the file: read the first one's.
    if resolution.is_none()
        && site == StreamSite::Ohentai
        && let Ok(v) = crate::ohentai::video(client, &episodes[0].url).await
    {
        resolution = v.resolution;
    }
    Ok(Some(Found {
        site,
        episodes,
        resolution,
    }))
}

/// One episode to download.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub episode: StreamEpisode,
    /// The show's title, for the file's name and the card.
    pub series: String,
    /// The show's folder in the library.
    pub folder: PathBuf,
    pub paused: bool,
    /// The finished file.
    #[serde(default)]
    pub file: Option<PathBuf>,
}

impl Job {
    /// "<Show> - 01", the file's name without its extension (fit for any
    /// system's file names: "Show: Part" is "Show - Part").
    pub fn stem(&self) -> String {
        let series: String = crate::remux::sanitize_filename(&self.series)
            .replace(": ", " - ")
            .chars()
            .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
            .collect();
        format!("{series} - {:02}", self.episode.episode)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Waiting for a free slot.
    Queued,
    /// Reading the episode's page.
    Starting,
    Downloading,
    Done,
    Paused,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Live {
    pub phase: Phase,
    pub done: u64,
    /// The whole size, when the site says (an estimate for hstream.moe).
    pub total: Option<u64>,
    /// Bytes a second.
    pub speed: f64,
}

impl Live {
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            Phase::Queued | Phase::Starting | Phase::Downloading
        )
    }

    pub fn fraction(&self) -> Option<f32> {
        match self.phase {
            Phase::Done => Some(1.0),
            _ => self
                .total
                .filter(|t| *t > 0)
                .map(|t| (self.done as f64 / t as f64).clamp(0.0, 1.0) as f32),
        }
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub job: Job,
    pub live: Live,
}

struct Entry {
    job: Job,
    live: Arc<Mutex<Live>>,
    cancel: Arc<AtomicBool>,
}

/// How many episodes download at once.
const AT_ONCE: usize = 2;

pub struct Manager {
    file: PathBuf,
    http: reqwest::Client,
    rt: tokio::runtime::Handle,
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    slots: Arc<tokio::sync::Semaphore>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Manager {
    /// Load the jobs saved in `<state_dir>/streams.json` and carry on with
    /// the unfinished ones.
    pub fn load(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build the HTTP client");
        Self::load_with(state_dir, rt, notify, http)
    }

    fn load_with(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        notify: impl Fn() + Send + Sync + 'static,
        http: reqwest::Client,
    ) -> Arc<Self> {
        let file = state_dir.join("streams.json");
        let jobs: Vec<Job> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let next_id = jobs.iter().map(|j| j.id + 1).max().unwrap_or(1);
        let entries = jobs
            .into_iter()
            .map(|job| {
                let phase = if job.file.as_ref().is_some_and(|f| f.exists()) {
                    Phase::Done
                } else if job.paused {
                    Phase::Paused
                } else {
                    Phase::Queued
                };
                Entry {
                    job,
                    live: Arc::new(Mutex::new(Live {
                        phase,
                        done: 0,
                        total: None,
                        speed: 0.0,
                    })),
                    cancel: Arc::default(),
                }
            })
            .collect();
        let m = Arc::new(Self {
            file,
            http,
            rt,
            entries: Mutex::new(entries),
            next_id: AtomicU64::new(next_id),
            slots: Arc::new(tokio::sync::Semaphore::new(AT_ONCE)),
            notify: Box::new(notify),
        });
        let queued: Vec<u64> = m
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.live.lock().unwrap().phase == Phase::Queued)
            .map(|e| e.job.id)
            .collect();
        for id in queued {
            m.start(id);
        }
        m
    }

    fn save(&self, entries: &[Entry]) {
        let jobs: Vec<&Job> = entries.iter().map(|e| &e.job).collect();
        match serde_json::to_vec_pretty(&jobs) {
            Ok(json) => {
                if let Some(dir) = self.file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = crate::fsutil::write_private(&self.file, &json) {
                    tracing::warn!("could not save {}: {e:#}", self.file.display());
                }
            }
            Err(e) => tracing::warn!("could not save the stream downloads: {e}"),
        }
    }

    /// Download an episode into `folder` (the show's), as
    /// "<series> - 01". The same episode isn't added twice.
    pub fn add(self: &Arc<Self>, episode: StreamEpisode, series: &str, folder: PathBuf) -> u64 {
        let id = {
            let mut entries = self.entries.lock().unwrap();
            if let Some(e) = entries
                .iter()
                .find(|e| e.job.episode.url == episode.url && e.job.folder == folder)
            {
                let id = e.job.id;
                let failed = matches!(e.live.lock().unwrap().phase, Phase::Failed(_));
                drop(entries);
                if failed {
                    self.resume(id);
                }
                return id;
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            entries.push(Entry {
                job: Job {
                    id,
                    episode,
                    series: series.to_owned(),
                    folder,
                    paused: false,
                    file: None,
                },
                live: Arc::new(Mutex::new(Live {
                    phase: Phase::Queued,
                    done: 0,
                    total: None,
                    speed: 0.0,
                })),
                cancel: Arc::default(),
            });
            self.save(&entries);
            id
        };
        self.start(id);
        id
    }

    pub fn views(&self) -> Vec<View> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| View {
                job: e.job.clone(),
                live: e.live.lock().unwrap().clone(),
            })
            .collect()
    }

    pub fn pause(&self, id: u64) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.job.id == id) {
            if e.live.lock().unwrap().phase == Phase::Done {
                return;
            }
            e.job.paused = true;
            e.cancel.store(true, Ordering::Relaxed);
            e.live.lock().unwrap().phase = Phase::Paused;
        }
        self.save(&entries);
        drop(entries);
        (self.notify)();
    }

    /// Carry on with a paused (or failed) job.
    pub fn resume(self: &Arc<Self>, id: u64) {
        {
            let mut entries = self.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.job.id == id) else {
                return;
            };
            if e.live.lock().unwrap().active() {
                return;
            }
            e.job.paused = false;
            e.cancel = Arc::default();
            e.live.lock().unwrap().phase = Phase::Queued;
            self.save(&entries);
        }
        self.start(id);
    }

    /// Stop and forget a job. What of it was downloaded goes; its
    /// finished file stays, unless `delete_files`.
    pub fn remove(&self, id: u64, delete_files: bool) {
        let mut entries = self.entries.lock().unwrap();
        let Some(i) = entries.iter().position(|e| e.job.id == id) else {
            return;
        };
        let e = entries.remove(i);
        e.cancel.store(true, Ordering::Relaxed);
        for f in self.files_of(&e.job) {
            if delete_files || f.extension().is_some_and(|x| x == "part") {
                let _ = std::fs::remove_file(f);
            }
        }
        self.save(&entries);
        drop(entries);
        (self.notify)();
    }

    /// Stop everything (the app is closing); unfinished jobs carry on next time.
    pub fn stop(&self) {
        for e in self.entries.lock().unwrap().iter() {
            e.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// A job's files on disk: the finished one, or partial ones.
    fn files_of(&self, job: &Job) -> Vec<PathBuf> {
        let stem = job.stem();
        let mut out: Vec<PathBuf> = job.file.iter().cloned().collect();
        if let Ok(dir) = std::fs::read_dir(&job.folder) {
            for f in dir.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if name.starts_with(&stem) && name.ends_with(".part") {
                    out.push(f.path());
                }
            }
        }
        out
    }

    fn entry(&self, id: u64) -> Option<(Job, Arc<Mutex<Live>>, Arc<AtomicBool>)> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.job.id == id)
            .map(|e| (e.job.clone(), e.live.clone(), e.cancel.clone()))
    }

    fn start(self: &Arc<Self>, id: u64) {
        let Some((_, live, cancel)) = self.entry(id) else {
            return;
        };
        let me = self.clone();
        self.rt.spawn(async move {
            let Ok(_slot) = me.slots.clone().acquire_owned().await else {
                return;
            };
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let mut tries = 0u32;
            let result = loop {
                match me.run(id, &live, &cancel).await {
                    Ok(file) => break Ok(file),
                    Err(e) if cancel.load(Ordering::Relaxed) => break Err(e),
                    Err(e) => {
                        tries += 1;
                        if tries >= 3 {
                            break Err(e);
                        }
                        tracing::warn!("stream download {id}: {e:#}; trying again");
                        tokio::time::sleep(Duration::from_secs(5 * u64::from(tries))).await;
                    }
                }
            };
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(file) => {
                    let mut entries = me.entries.lock().unwrap();
                    if let Some(e) = entries.iter_mut().find(|e| e.job.id == id) {
                        e.job.file = Some(file);
                    }
                    me.save(&entries);
                    drop(entries);
                    live.lock().unwrap().phase = Phase::Done;
                }
                Err(e) => live.lock().unwrap().phase = Phase::Failed(format!("{e:#}")),
            }
            (me.notify)();
        });
    }

    async fn run(&self, id: u64, live: &Mutex<Live>, cancel: &AtomicBool) -> Result<PathBuf> {
        let (job, _, _) = self.entry(id).ok_or_else(|| anyhow!("removed"))?;
        let set = |phase: Phase| {
            live.lock().unwrap().phase = phase;
            (self.notify)();
        };
        set(Phase::Starting);
        tokio::fs::create_dir_all(&job.folder)
            .await
            .with_context(|| format!("could not make {}", job.folder.display()))?;
        let stem = job.stem();
        let progress = |done: u64, total: Option<u64>, speed: f64| {
            let mut l = live.lock().unwrap();
            l.phase = Phase::Downloading;
            l.done = done;
            l.total = total.or(l.total);
            l.speed = speed;
            drop(l);
            (self.notify)();
        };
        match job.episode.site {
            StreamSite::Ohentai => {
                let v = crate::ohentai::video(&self.http, &job.episode.url).await?;
                let len = v
                    .len
                    .ok_or_else(|| anyhow!("ohentai.org didn't say how big the video is"))?;
                let dest = job.folder.join(format!("{stem}.mp4"));
                let http = reqwest::Client::builder()
                    .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
                    .default_headers(reqwest::header::HeaderMap::from_iter([(
                        reqwest::header::REFERER,
                        reqwest::header::HeaderValue::from_str(&v.referer).unwrap_or(
                            reqwest::header::HeaderValue::from_static(crate::ohentai::REFERER),
                        ),
                    )]))
                    .connect_timeout(Duration::from_secs(30))
                    .read_timeout(Duration::from_secs(90))
                    .build()?;
                crate::debrid::jobs::fetch(&http, &v.url, &dest, len, cancel, |have, speed| {
                    progress(have, Some(len), speed)
                })
                .await?;
                if let Some(subs) = &v.subtitles {
                    let ext = if subs.ends_with(".vtt") { "vtt" } else { "srt" };
                    match self.http.get(subs).send().await {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(bytes) = r.bytes().await {
                                let _ = tokio::fs::write(
                                    job.folder.join(format!("{stem}.en.{ext}")),
                                    bytes,
                                )
                                .await;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(dest)
            }
            StreamSite::Hstream => {
                let dest = job
                    .folder
                    .join(format!("{stem}.{}", crate::hstream::EXTENSION));
                let started = std::time::Instant::now();
                let file = crate::hstream::download(
                    &self.http,
                    &job.episode.url,
                    &dest,
                    cancel,
                    |done, total| {
                        let secs = started.elapsed().as_secs_f64().max(0.5);
                        progress(done, total, done as f64 / secs)
                    },
                )
                .await?;
                Ok(file)
            }
        }
        .map_err(|e: anyhow::Error| {
            if cancel.load(Ordering::Relaxed) {
                anyhow!("cancelled")
            } else {
                e
            }
        })
        .and_then(|file: PathBuf| {
            if file.exists() {
                Ok(file)
            } else {
                bail!("{} wasn't written", file.display())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_files_like_remuxes_and_ranks_sites() {
        let job = Job {
            id: 1,
            episode: StreamEpisode {
                site: StreamSite::Ohentai,
                episode: 3,
                url: "https://ohentai.org/detail.php?vid=NDYx".into(),
                title: "Kuroinu - Episode 3".into(),
            },
            series: "Kuroinu: Kedakaki Seijo".into(),
            folder: PathBuf::from("/lib/Kuroinu"),
            paused: false,
            file: None,
        };
        let stem = job.stem();
        assert!(stem.ends_with(" - 03"), "{stem}");
        assert!(!stem.contains(':'));
        assert_eq!(
            crate::title::episode_of_file(&format!("{stem}.mp4")),
            Some(3)
        );
        assert!(StreamSite::Hstream.preference() > StreamSite::Ohentai.preference());
    }
}
