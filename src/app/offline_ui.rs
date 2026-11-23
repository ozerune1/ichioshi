//! Copies of episodes on Jellyfin, kept on this computer to watch offline:
//! saved one at a time into `<library>/Offline/<series>/`, resuming where
//! a download stopped; played instead of the stream when they're there.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{App, Msg};

/// Where an offline copy is getting to.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Saving {
    Waiting,
    Getting,
    Failed(String),
}

/// An episode being saved offline.
pub(super) struct OfflineJob {
    pub(super) id: u64,
    pub(super) anilist: u64,
    pub(super) episode: u32,
    pub(super) title: String,
    pub(super) state: Saving,
    /// Bytes so far, and of the whole file (once Jellyfin's said).
    pub(super) have: Arc<AtomicU64>,
    pub(super) len: Arc<AtomicU64>,
    /// Bytes a second.
    pub(super) speed: Arc<Mutex<f64>>,
    cancel: Arc<AtomicBool>,
}

impl OfflineJob {
    pub(super) fn fraction(&self) -> f32 {
        let len = self.len.load(Ordering::Relaxed);
        if len == 0 {
            0.0
        } else {
            (self.have.load(Ordering::Relaxed) as f64 / len as f64).clamp(0.0, 1.0) as f32
        }
    }
}

#[derive(Default)]
pub(super) struct OfflineState {
    pub(super) jobs: Vec<OfflineJob>,
    next: u64,
}

#[derive(Clone, Debug)]
pub(super) enum OfflineAction {
    /// Save some of a series' episodes (empty: all of them that are on
    /// Jellyfin and not saved yet).
    Save {
        id: u64,
        episodes: Vec<u32>,
    },
    /// Stop saving one (its partial file stays, to resume).
    Cancel(u64),
    Retry(u64),
    /// Delete an episode's offline copy (the server keeps its own).
    Remove {
        id: u64,
        episode: u32,
    },
}

pub(super) enum OfflineMsg {
    Saved {
        job: u64,
        result: Result<PathBuf, String>,
    },
}

impl App {
    /// An episode's offline copy, if it's there.
    pub(super) fn offline_copy(&self, id: u64, episode: u32) -> Option<&PathBuf> {
        self.library
            .find(id)?
            .offline
            .get(&episode)
            .filter(|p| p.exists())
    }

    /// The episode's save, if one is queued or under way (or failed).
    pub(super) fn offline_job(&self, id: u64, episode: u32) -> Option<&OfflineJob> {
        self.offline
            .jobs
            .iter()
            .find(|j| j.anilist == id && j.episode == episode)
    }

    pub(super) fn offline_action(&mut self, action: OfflineAction) {
        match action {
            OfflineAction::Save { id, episodes } => {
                let Some(series) = self.library.find(id) else {
                    return;
                };
                let Some(remote) = &series.remote else { return };
                let wanted: Vec<u32> = remote
                    .episodes
                    .iter()
                    .filter(|(n, e)| {
                        e.jellyfin.is_some()
                            && (episodes.is_empty() || episodes.contains(n))
                            && self.offline_copy(id, **n).is_none()
                            && self.offline_job(id, **n).is_none()
                    })
                    .map(|(n, _)| *n)
                    .collect();
                let title = series.title().to_owned();
                for n in &wanted {
                    self.offline.next += 1;
                    self.offline.jobs.push(OfflineJob {
                        id: self.offline.next,
                        anilist: id,
                        episode: *n,
                        title: title.clone(),
                        state: Saving::Waiting,
                        have: Arc::default(),
                        len: Arc::default(),
                        speed: Arc::default(),
                        cancel: Arc::default(),
                    });
                }
                match wanted.len() {
                    0 => self.set_info("Nothing to save: it's all here, or not on Jellyfin"),
                    1 => self.set_info(format!("Saving episode {} offline", wanted[0])),
                    n => self.set_info(format!("Saving {n} episodes offline")),
                }
                self.offline_tick();
            }
            OfflineAction::Cancel(job) => {
                if let Some(i) = self.offline.jobs.iter().position(|j| j.id == job) {
                    self.offline.jobs[i].cancel.store(true, Ordering::Relaxed);
                    self.offline.jobs.remove(i);
                }
                self.offline_tick();
            }
            OfflineAction::Retry(job) => {
                if let Some(j) = self.offline.jobs.iter_mut().find(|j| j.id == job) {
                    j.state = Saving::Waiting;
                    j.cancel = Arc::default();
                }
                self.offline_tick();
            }
            OfflineAction::Remove { id, episode } => {
                let Some(series) = self.library.find_mut(id) else {
                    return;
                };
                if let Some(path) = series.offline.remove(&episode) {
                    let _ = std::fs::remove_file(&path);
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::remove_dir(dir);
                    }
                    self.library.save_or_warn();
                    self.set_info(format!(
                        "Removed episode {episode}'s offline copy (Jellyfin still has it)"
                    ));
                }
            }
        }
    }

    /// Start the next save, if none is under way.
    pub(super) fn offline_tick(&mut self) {
        if self.offline.jobs.iter().any(|j| j.state == Saving::Getting) {
            return;
        }
        let Some(i) = self
            .offline
            .jobs
            .iter()
            .position(|j| j.state == Saving::Waiting)
        else {
            return;
        };
        let (anilist, episode, job_id) = {
            let j = &self.offline.jobs[i];
            (j.anilist, j.episode, j.id)
        };
        let Some(j) = self.jellyfin() else {
            self.offline.jobs[i].state =
                Saving::Failed("sign in to Jellyfin in Settings first".into());
            return;
        };
        let Some(series) = self.library.find(anilist) else {
            self.offline.jobs.remove(i);
            return;
        };
        let Some(item) = series
            .remote
            .as_ref()
            .and_then(|r| r.episodes.get(&episode)?.jellyfin.clone())
        else {
            self.offline.jobs[i].state = Saving::Failed("Jellyfin doesn't have it".into());
            return;
        };
        let folder = self
            .library_dir()
            .join("Offline")
            .join(crate::engine::sanitize_folder_name(series.title()));
        let job = &mut self.offline.jobs[i];
        job.state = Saving::Getting;
        let (have, len, speed, cancel) = (
            job.have.clone(),
            job.len.clone(),
            job.speed.clone(),
            job.cancel.clone(),
        );
        let http = self.http.clone();
        let ctx = self.ctx.clone();
        self.spawn(async move {
            let result = async {
                let p = j.playable(&item).await?;
                if p.size == 0 {
                    anyhow::bail!("Jellyfin doesn't say how big the file is");
                }
                len.store(p.size, Ordering::Relaxed);
                let name = p
                    .file_name
                    .clone()
                    .unwrap_or_else(|| format!("Episode {episode:02}.mkv"));
                let dest = folder.join(crate::remux::sanitize_filename(&name));
                // Its download, else (not allowed to download) its stream.
                let mut last = None;
                for url in j.download_urls(&p) {
                    let progress = |got: u64, bps: f64| {
                        have.store(got, Ordering::Relaxed);
                        *speed.lock().unwrap() = bps;
                        ctx.request_repaint();
                    };
                    match crate::debrid::jobs::fetch(&http, &url, &dest, p.size, &cancel, progress)
                        .await
                    {
                        Ok(()) => return Ok(dest),
                        Err(e) if e.to_string().contains("HTTP 40") => last = Some(e),
                        Err(e) => return Err(e),
                    }
                }
                Err(last.unwrap_or_else(|| anyhow::anyhow!("Jellyfin wouldn't send it")))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::Offline(OfflineMsg::Saved {
                job: job_id,
                result,
            })
        });
    }

    pub(super) fn handle_offline_msg(&mut self, msg: OfflineMsg) {
        match msg {
            OfflineMsg::Saved { job, result } => {
                let Some(i) = self.offline.jobs.iter().position(|j| j.id == job) else {
                    return; // cancelled
                };
                match result {
                    Ok(path) => {
                        let j = self.offline.jobs.remove(i);
                        if let Some(s) = self.library.find_mut(j.anilist) {
                            s.offline.insert(j.episode, path);
                            self.library.save_or_warn();
                        }
                    }
                    Err(e) if e.contains("cancelled") => {}
                    Err(e) => self.offline.jobs[i].state = Saving::Failed(e),
                }
                self.offline_tick();
            }
        }
    }
}
