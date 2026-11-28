//! Usenet in the app: your providers, which releases are on Usenet, and the
//! cards for Usenet downloads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{App, FilesPage, Msg, Page, ReleaseKind};
use crate::usenet::indexers::{Indexers, NzbLink};
use crate::usenet::jobs::Manager;
use crate::usenet::{Config, Provider};

pub(super) struct UsenetState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) manager: Arc<Manager>,
    pub(super) indexers: Indexers,
    /// Each release's NZBs, by info hash; `None` while being looked up.
    pub(super) lookups: HashMap<String, Option<Vec<NzbLink>>>,
    /// Lookups at once.
    limit: Arc<tokio::sync::Semaphore>,
    pub(super) dialog: Option<Dialog>,
}

/// The providers dialog.
#[derive(Default)]
pub(super) struct Dialog {
    /// A copy being edited, and whether it's a new one.
    pub(super) editing: Option<(Provider, bool)>,
    /// Connection checks, by provider ID.
    pub(super) checks: HashMap<u64, Check>,
    seq: u64,
}

pub(super) enum Check {
    Running(u64),
    Done(Result<(), String>),
}

impl UsenetState {
    pub(super) fn load(
        state_dir: &Path,
        rt: tokio::runtime::Handle,
        ctx: super::Waker,
        http: reqwest::Client,
    ) -> Self {
        let file = state_dir.join("usenet.json");
        let config = Config::load(&file);
        let indexers = Indexers::new(http);
        let manager = Manager::load(state_dir, rt, &config, indexers.clone(), move || {
            ctx.request_repaint()
        });
        Self {
            config,
            file,
            manager,
            indexers,
            lookups: HashMap::new(),
            limit: Arc::new(tokio::sync::Semaphore::new(6)),
            dialog: None,
        }
    }

    pub(super) fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
        self.manager.set_config(&self.config);
    }

    pub(super) fn active(&self) -> bool {
        self.config.active()
    }

    /// The release's NZBs, if it's been looked up (empty: not on Usenet).
    pub(super) fn links(&self, info_hash: &str) -> Option<&[NzbLink]> {
        self.lookups
            .get(&info_hash.to_ascii_lowercase())?
            .as_deref()
    }
}

impl App {
    /// Look up the releases on screen on the Usenet indexers (once each).
    pub(super) fn request_nzb_lookups(&mut self) {
        if !self.usenet.active() {
            return;
        }
        let mut wanted: Vec<(String, Option<u64>)> = Vec::new();
        match &self.page {
            Page::Anime(ap) => match (&ap.nyaa, &ap.anime.entry) {
                (Some(search), _) => {
                    for r in search.releases.iter().take(super::MAX_NYAA_ROWS) {
                        match &r.kind {
                            ReleaseKind::Torrent { item, .. } => {
                                wanted.push((item.info_hash.clone(), item.nyaa_id()))
                            }
                            ReleaseKind::Episodes(items) => {
                                wanted.extend(
                                    items
                                        .iter()
                                        .map(|(_, item)| (item.info_hash.clone(), item.nyaa_id())),
                                );
                            }
                            ReleaseKind::Stream { .. } => {}
                        }
                    }
                }
                (None, Some(entry)) => {
                    for r in &entry.releases {
                        if let Some(hash) = r.info_hash_hex() {
                            wanted.push((hash.to_owned(), r.nyaa_id()));
                        }
                    }
                }
                (None, None) => {}
            },
            Page::Files(fp) => {
                if let super::Picker::Episodes { items, .. } = &fp.picker {
                    wanted.extend(
                        items
                            .iter()
                            .map(|(_, item)| (item.info_hash.clone(), item.nyaa_id())),
                    );
                }
            }
            _ => {}
        }
        for (hash, nyaa_id) in wanted {
            let hash = hash.to_ascii_lowercase();
            if hash.is_empty() || self.usenet.lookups.contains_key(&hash) {
                continue;
            }
            self.usenet.lookups.insert(hash.clone(), None);
            let (indexers, limit) = (self.usenet.indexers.clone(), self.usenet.limit.clone());
            self.spawn(async move {
                let _turn = limit.acquire_owned().await;
                let links = indexers.lookup(&hash, nyaa_id).await;
                Msg::NzbLookup { hash, links }
            });
        }
    }

    pub(super) fn handle_usenet_msg(&mut self, msg: Msg) {
        match msg {
            Msg::NzbLookup { hash, links } => {
                self.usenet.lookups.insert(hash, Some(links));
            }
            Msg::UsenetChecked { id, seq, result } => {
                if let Some(d) = &mut self.usenet.dialog
                    && matches!(d.checks.get(&id), Some(Check::Running(s)) if *s == seq)
                {
                    d.checks.insert(id, Check::Done(result));
                }
            }
            _ => {}
        }
    }

    pub(super) fn usenet_check(&mut self, id: u64) {
        let Some(provider) = self
            .usenet
            .config
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
        else {
            return;
        };
        let Some(d) = &mut self.usenet.dialog else {
            return;
        };
        d.seq += 1;
        let seq = d.seq;
        d.checks.insert(id, Check::Running(seq));
        self.spawn(async move {
            let result = crate::usenet::nntp::check(&provider)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::UsenetChecked { id, seq, result }
        });
    }

    pub(super) fn usenet_edit_files(&mut self, id: u64) {
        match self.usenet.manager.listing(id) {
            Ok(listing) => {
                self.cancel_listing();
                let mut page = FilesPage::new(
                    listing,
                    None,
                    "Change which files this download gets".to_owned(),
                );
                page.nzbs = self.usenet.manager.nzbs(id);
                self.page = Page::Files(page);
            }
            Err(e) => self.set_error(format!("Can't change the files: {e:#}")),
        }
    }

    pub(super) fn usenet_open_remux(&mut self, id: u64) {
        let Some(series) = self.usenet.manager.series_files(id, &self.library_dir()) else {
            return;
        };
        let known = series.info_hashes.iter().find_map(|h| self.library.get(h));
        self.open_remux_series(series, known);
    }

    pub(super) fn usenet_remove(&mut self, id: u64, delete_files: bool) {
        self.usenet.manager.remove(id, delete_files);
        self.set_info(if delete_files {
            "Removed and deleted its files"
        } else {
            "Removed (files kept on disk)"
        });
    }
}

#[cfg(test)]
impl UsenetState {
    /// Look releases up on these indexers (mock servers) instead of the real ones.
    pub(super) fn use_indexers_for_test(&mut self, indexers: Indexers) {
        self.indexers = indexers;
    }
}
