//! Debrid services in the app: the "download with" setting, API keys, and
//! the cards for debrid downloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{App, EngineState, FilesPage, Msg, Page};
use crate::debrid::jobs::Manager;
use crate::debrid::{Account, Config, Provider};

pub(super) struct DebridState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) manager: Arc<Manager>,
    /// The last account check for the chosen service.
    pub(super) account: Option<(Provider, Result<Account, String>)>,
    pub(super) checking: bool,
    seq: u64,
}

impl DebridState {
    pub(super) fn load(state_dir: &Path, rt: tokio::runtime::Handle, ctx: super::Waker) -> Self {
        let file = state_dir.join("debrid.json");
        let config = Config::load(&file);
        let manager = Manager::load(state_dir, rt, config.keys.clone(), move || {
            ctx.request_repaint()
        });
        Self {
            config,
            file,
            manager,
            account: None,
            checking: false,
            seq: 0,
        }
    }

    pub(super) fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
        self.manager.set_keys(self.config.keys.clone());
    }

    /// The service's name, when downloads go through one.
    pub(super) fn service(&self) -> Option<&'static str> {
        self.config.enabled.then(|| self.config.provider.name())
    }
}

impl App {
    pub(super) fn handle_debrid_msg(&mut self, msg: Msg) {
        if let Msg::DebridAccount {
            seq,
            provider,
            result,
        } = msg
        {
            if seq != self.debrid.seq {
                return;
            }
            self.debrid.checking = false;
            self.debrid.account = Some((provider, result));
        }
    }

    /// Check the API key with the service (shows who it belongs to).
    pub(super) fn debrid_check(&mut self) {
        let provider = self.debrid.config.provider;
        let client = match self.debrid.manager.client(provider) {
            Ok(client) => client,
            Err(_) => {
                self.debrid.account = None;
                return;
            }
        };
        self.debrid.seq += 1;
        let seq = self.debrid.seq;
        self.debrid.checking = true;
        self.spawn(async move {
            let result = client.account().await.map_err(|e| format!("{e:#}"));
            Msg::DebridAccount {
                seq,
                provider,
                result,
            }
        });
    }

    pub(super) fn set_debrid_enabled(&mut self, on: bool) {
        self.debrid.config.enabled = on;
        self.debrid.save();
        if on {
            self.debrid_check();
        } else if matches!(self.engine, EngineState::Off) {
            // Back to BitTorrent: it's needed now.
            self.start_engine();
        }
    }

    /// Change a debrid download's files.
    pub(super) fn debrid_edit_files(&mut self, id: u64) {
        match self.debrid.manager.listing(id) {
            Ok(listing) => {
                self.cancel_listing();
                let subtitle = "Change which files this download gets".to_owned();
                self.page = Page::Files(FilesPage::new(listing, None, subtitle));
            }
            Err(e) => self.set_error(format!("Can't change the files: {e:#}")),
        }
    }

    pub(super) fn debrid_open_remux(&mut self, id: u64) {
        let Some(series) = self.debrid.manager.series_files(id, &self.library_dir()) else {
            return;
        };
        let known = series.info_hashes.iter().find_map(|h| self.library.get(h));
        self.open_remux_series(series, known);
    }

    pub(super) fn debrid_remove(&mut self, id: u64, delete_files: bool) {
        let service = self
            .debrid
            .manager
            .view(id)
            .map(|v| v.job.provider.name())
            .unwrap_or("the service");
        self.debrid.manager.remove(id, delete_files);
        self.set_info(if delete_files {
            format!("Removed, deleted its files, and removed it from {service}")
        } else {
            format!("Removed (files kept on disk) and removed it from {service}")
        });
    }
}
