//! Updating Ichioshi from its GitHub releases (`crate::update`): checked
//! when the app starts (unless turned off), or by hand; downloaded and
//! checked on the side; installed when you say (the desktop app restarts;
//! the phone's installer takes over).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use super::{App, Msg};
use crate::update::{self, Release};

/// Where updating is at.
#[derive(Default)]
pub(crate) enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        have: Arc<AtomicU64>,
        cancel: Arc<AtomicBool>,
    },
    /// Downloaded and checked: the APK (phone), or what's unpacked from the
    /// archive (desktop).
    Ready {
        release: Release,
        path: PathBuf,
    },
    Failed {
        release: Option<Release>,
        error: String,
    },
}

#[derive(Default)]
pub(crate) struct UpdateState {
    pub(crate) status: Status,
    /// When it last checked.
    pub(crate) checked: Option<Instant>,
    /// Desktop: the update's in place, so the app is to close (and the
    /// new version start).
    pub(crate) restart: bool,
    seq: u64,
}

impl UpdateState {
    /// The newer release found, whatever's being done with it.
    pub(crate) fn release(&self) -> Option<&Release> {
        match &self.status {
            Status::Available(r) | Status::Downloading { release: r, .. } => Some(r),
            Status::Ready { release, .. } => Some(release),
            Status::Failed { release, .. } => release.as_ref(),
            _ => None,
        }
    }

    /// How much of the download is here (0 to 1), while downloading.
    pub(crate) fn progress(&self) -> Option<f32> {
        match &self.status {
            Status::Downloading { release, have, .. } => {
                let size = release.asset.as_ref().map_or(0, |a| a.size);
                Some(if size == 0 {
                    0.0
                } else {
                    (have.load(Ordering::Relaxed) as f64 / size as f64).clamp(0.0, 1.0) as f32
                })
            }
            _ => None,
        }
    }

    pub(crate) fn downloading(&self) -> bool {
        matches!(self.status, Status::Downloading { .. })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum UpdateAction {
    Check,
    Download,
    Cancel,
    /// Put it in place: the desktop app restarts as the new version (the
    /// phone app hands the APK to the system's installer instead).
    Install,
}

pub(super) enum UpdateMsg {
    Checked {
        seq: u64,
        quiet: bool,
        result: Result<Release, String>,
    },
    Downloaded {
        seq: u64,
        result: Result<PathBuf, String>,
    },
}

impl App {
    /// Where updates are downloaded.
    pub(super) fn updates_dir(&self) -> PathBuf {
        self.state_dir.join("update")
    }

    /// Check for an update, if the settings say to, as the app starts.
    pub(super) fn update_at_start(&mut self) {
        let dir = self.updates_dir();
        let failed = dir.join(update::FAILED);
        if let Ok(why) = std::fs::read_to_string(&failed) {
            let _ = std::fs::remove_file(&failed);
            self.set_error(format!("The update couldn't be installed: {}", why.trim()));
        }
        self.rt.spawn_blocking(move || update::tidy(&dir));
        if self.settings.check_updates && !cfg!(debug_assertions) && !cfg!(test) {
            self.update_check(true);
        }
    }

    /// Ask GitHub for the latest release (`quiet`: only say if there's a
    /// newer one).
    fn update_check(&mut self, quiet: bool) {
        if self.update.downloading() {
            return;
        }
        self.update.seq += 1;
        let seq = self.update.seq;
        if !quiet {
            self.update.status = Status::Checking;
        }
        let http = self.http.clone();
        let api = self.update_api.clone();
        self.spawn(async move {
            let result = update::latest(&http, &api)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::Update(UpdateMsg::Checked { seq, quiet, result })
        });
    }

    pub(crate) fn update_action(&mut self, action: UpdateAction) {
        match action {
            UpdateAction::Check => self.update_check(false),
            UpdateAction::Download => self.update_download(),
            UpdateAction::Cancel => {
                if let Status::Downloading { cancel, .. } = &self.update.status {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            UpdateAction::Install => self.update_install(),
        }
    }

    fn update_download(&mut self) {
        let release = match std::mem::take(&mut self.update.status) {
            Status::Available(r)
            | Status::Failed {
                release: Some(r), ..
            } => r,
            other => {
                self.update.status = other;
                return;
            }
        };
        let Some(asset) = release.asset.clone() else {
            self.update.status = Status::Failed {
                error: format!("{} has no download for this system", release.name),
                release: Some(release),
            };
            return;
        };
        self.update.seq += 1;
        let seq = self.update.seq;
        let (have, cancel) = (Arc::<AtomicU64>::default(), Arc::<AtomicBool>::default());
        let dir = update::version_dir(&self.updates_dir(), release.version);
        self.update.status = Status::Downloading {
            release,
            have: have.clone(),
            cancel: cancel.clone(),
        };
        let http = self.http.clone();
        let ctx = self.ctx.clone();
        self.spawn(async move {
            let result = async {
                let progress = |got: u64, _speed: f64| {
                    have.store(got, Ordering::Relaxed);
                    ctx.request_repaint();
                };
                let file = update::download(&http, &asset, &dir, &cancel, progress).await?;
                if cfg!(target_os = "android") {
                    return Ok(file);
                }
                // The desktop's archive, unpacked to put in place.
                let unpacked = dir.join("unpacked");
                let into = unpacked.clone();
                tokio::task::spawn_blocking(move || update::unpack(&file, &into)).await??;
                Ok(unpacked)
            }
            .await
            .map_err(|e: anyhow::Error| format!("{e:#}"));
            Msg::Update(UpdateMsg::Downloaded { seq, result })
        });
    }

    /// Desktop: the update in place of this program, to start once the
    /// app has closed (the GUI closes it when `restart` is set).
    fn update_install(&mut self) {
        let Status::Ready { release, path } = &self.update.status else {
            return;
        };
        let installed = std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(|exe| update::install(path, &exe));
        match installed {
            Ok(start) => {
                *update::AFTER_EXIT.lock().unwrap() = Some(start);
                self.update.restart = true;
            }
            Err(e) => {
                self.update.status = Status::Failed {
                    release: Some(release.clone()),
                    error: format!("{e:#}"),
                };
            }
        }
    }

    pub(super) fn handle_update_msg(&mut self, msg: UpdateMsg) {
        match msg {
            UpdateMsg::Checked { seq, quiet, result } => {
                if seq != self.update.seq {
                    return;
                }
                self.update.checked = Some(Instant::now());
                match result {
                    Ok(r) if r.is_newer() => {
                        if quiet {
                            self.set_info(format!(
                                "{} is out: {} › Updates",
                                r.name,
                                if cfg!(target_os = "android") {
                                    "More"
                                } else {
                                    "Settings"
                                }
                            ));
                        }
                        self.update.status = Status::Available(r);
                    }
                    Ok(_) => {
                        if !quiet {
                            self.update.status = Status::UpToDate;
                        }
                    }
                    Err(e) if quiet => tracing::warn!("couldn't check for updates: {e}"),
                    Err(error) => {
                        self.update.status = Status::Failed {
                            release: None,
                            error,
                        };
                    }
                }
            }
            UpdateMsg::Downloaded { seq, result } => {
                if seq != self.update.seq {
                    return;
                }
                let Some(release) = self.update.release().cloned() else {
                    return;
                };
                self.update.status = match result {
                    Ok(path) => Status::Ready { release, path },
                    Err(e) if e.contains("cancelled") => Status::Available(release),
                    Err(error) => Status::Failed {
                        release: Some(release),
                        error,
                    },
                };
            }
        }
    }
}
