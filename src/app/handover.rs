//! A download that can't finish one way is handed to the next: Usenet to
//! the debrid service (if one is set up) or else BitTorrent here; the debrid
//! service to BitTorrent here. It keeps its files and folder.

use std::collections::HashSet;

use super::{App, EngineState, Msg};
use crate::engine::Listing;

#[derive(Default)]
pub(super) struct Handover {
    /// Downloads already handed on ("usenet:3", "debrid:5").
    handed: HashSet<String>,
}

impl App {
    /// Hand failed Usenet and debrid downloads on.
    pub(super) fn hand_over_failed(&mut self) {
        for v in self.usenet.manager.views() {
            let crate::usenet::jobs::Phase::Failed(why) = &v.live.phase else {
                continue;
            };
            let key = format!("usenet:{}", v.job.id);
            if self.handover.handed.contains(&key) {
                continue;
            }
            let Ok(listing) = self.usenet.manager.listing(v.job.id) else {
                continue;
            };
            let wanted: Vec<usize> = v
                .job
                .files
                .iter()
                .filter(|f| f.wanted)
                .map(|f| f.index)
                .collect();
            let output = v.job.output_folder.clone();
            let provider = self.debrid.config.provider;
            if self.debrid.config.enabled && self.debrid.config.key(provider).is_some() {
                self.handover.handed.insert(key);
                if self.adult_download(&v.job.info_hash) {
                    self.debrid.manager.allow_links(&v.job.info_hash);
                }
                match self.debrid.manager.add(provider, &listing, &wanted, output) {
                    Ok(()) => {
                        self.usenet.manager.remove(v.job.id, false);
                        self.set_info(format!(
                            "{}: Usenet didn't work ({why}), so {} is getting it",
                            v.job.name,
                            provider.name()
                        ));
                    }
                    Err(e) => self.set_error(format!(
                        "{}: Usenet didn't work, and {} couldn't take it: {e:#}",
                        v.job.name,
                        provider.name()
                    )),
                }
            } else {
                let note = format!(
                    "{}: Usenet didn't work ({why}), so it's downloading as a torrent",
                    v.job.name
                );
                if self.start_here(key, &listing, wanted, output, note) {
                    self.usenet.manager.remove(v.job.id, false);
                }
            }
        }
        for v in self.debrid.manager.views() {
            let crate::debrid::jobs::Phase::Failed(why) = &v.live.phase else {
                continue;
            };
            let key = format!("debrid:{}", v.job.id);
            if self.handover.handed.contains(&key) {
                continue;
            }
            let Ok(listing) = self.debrid.manager.listing(v.job.id) else {
                continue;
            };
            let wanted: Vec<usize> = v
                .job
                .files
                .iter()
                .filter(|f| f.wanted)
                .map(|f| f.index)
                .collect();
            let note = format!(
                "{}: {} didn't work ({why}), so it's downloading as a torrent here",
                v.job.name,
                v.job.provider.name()
            );
            if self.start_here(key, &listing, wanted, v.job.output_folder.clone(), note) {
                self.debrid.manager.remove(v.job.id, false);
            }
        }
    }

    /// Download a torrent with BitTorrent here. False while the engine isn't
    /// up (it's started if it was off; this is tried again shortly).
    fn start_here(
        &mut self,
        key: String,
        listing: &Listing,
        wanted: Vec<usize>,
        output: std::path::PathBuf,
        note: String,
    ) -> bool {
        match self.engine {
            EngineState::Off => {
                self.start_engine();
                return false;
            }
            EngineState::Ready(_) => {}
            _ => return false,
        }
        let Some(engine) = self.engine() else {
            return false;
        };
        self.handover.handed.insert(key);
        let torrent = listing.torrent_bytes.clone();
        self.spawn(async move {
            let result = engine
                .download(torrent, Some(wanted), output)
                .await
                .map(|()| note)
                .map_err(|e| format!("{e:#}"));
            Msg::HandedOver(result)
        });
        true
    }

    pub(super) fn handled_over(&mut self, result: Result<String, String>) {
        match result {
            Ok(note) => self.set_info(note),
            Err(e) => self.set_error(format!("Couldn't start the torrent instead: {e}")),
        }
    }
}
