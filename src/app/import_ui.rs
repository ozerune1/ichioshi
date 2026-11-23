//! Importing files by hand into Sonarr or Radarr (its Manual Import): a
//! finished download it didn't import itself ("Waiting to import"), or a
//! folder on the server; each file as the episodes it's taken for (or ones
//! picked), moved or copied.

use std::collections::{HashMap, HashSet};

use super::discover_ui::Loaded;
use super::{App, Msg};
use crate::servers::arr::{Arr, Candidate, ImportFrom, Kind};

/// The import dialog.
pub(super) struct Importing {
    pub(super) kind: Kind,
    /// Sonarr's series or Radarr's film.
    pub(super) owner: u64,
    /// The anime, if it's in the library (to name episodes as AniList does).
    pub(super) anilist: Option<u64>,
    /// A download's files, or (`None`) a folder's.
    pub(super) download: Option<String>,
    /// The folder on the server, while one is being chosen.
    pub(super) folder: String,
    pub(super) candidates: Option<Loaded<Vec<Candidate>>>,
    pub(super) picked: HashSet<usize>,
    /// Episodes picked for files Sonarr couldn't place (Sonarr's IDs).
    pub(super) assigned: HashMap<usize, u64>,
    pub(super) copy: bool,
    pub(super) busy: bool,
}

#[derive(Clone, Debug)]
pub(super) enum ImportAction {
    Open {
        kind: Kind,
        owner: u64,
        anilist: Option<u64>,
        download: Option<String>,
    },
    Folder(String),
    Scan,
    Pick(usize, bool),
    /// Which episode a file is (Sonarr's ID).
    Assign(usize, u64),
    Copy(bool),
    Import,
    Close,
}

pub(super) enum ImportMsg {
    Candidates(Result<Vec<Candidate>, String>),
    Imported(Result<String, String>),
}

impl App {
    fn import_arr(&self, kind: Kind) -> Option<Arr> {
        let account = match kind {
            Kind::Sonarr => &self.servers.config.sonarr,
            Kind::Radarr => &self.servers.config.radarr,
        };
        account
            .is_set()
            .then(|| Arr::new(kind, account, &self.http))
    }

    pub(super) fn import_action(&mut self, action: ImportAction) {
        match action {
            ImportAction::Open {
                kind,
                owner,
                anilist,
                download,
            } => {
                let scan = download.is_some();
                self.import = Some(Importing {
                    kind,
                    owner,
                    anilist,
                    download,
                    folder: String::new(),
                    candidates: None,
                    picked: HashSet::new(),
                    assigned: HashMap::new(),
                    copy: false,
                    busy: false,
                });
                if scan {
                    self.import_action(ImportAction::Scan);
                }
            }
            ImportAction::Folder(f) => {
                if let Some(i) = &mut self.import {
                    i.folder = f;
                }
            }
            ImportAction::Scan => {
                let Some(i) = &mut self.import else { return };
                let from = match &i.download {
                    Some(d) => ImportFrom::Download(d.clone()),
                    None if i.folder.trim().is_empty() => return,
                    None => ImportFrom::Folder(i.folder.trim().to_owned()),
                };
                i.candidates = Some(Loaded::Loading);
                i.picked.clear();
                i.assigned.clear();
                let (kind, owner) = (i.kind, i.owner);
                let Some(arr) = self.import_arr(kind) else {
                    return;
                };
                self.spawn(async move {
                    let result = arr
                        .import_candidates(&from, owner)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    Msg::Import(ImportMsg::Candidates(result))
                });
            }
            ImportAction::Pick(n, on) => {
                if let Some(i) = &mut self.import {
                    if on {
                        i.picked.insert(n);
                    } else {
                        i.picked.remove(&n);
                    }
                }
            }
            ImportAction::Assign(n, episode) => {
                if let Some(i) = &mut self.import {
                    i.assigned.insert(n, episode);
                    i.picked.insert(n);
                }
            }
            ImportAction::Copy(on) => {
                if let Some(i) = &mut self.import {
                    i.copy = on;
                }
            }
            ImportAction::Import => {
                let Some(i) = &mut self.import else { return };
                let Some(Loaded::Ready(list)) = &i.candidates else {
                    return;
                };
                let mut files = Vec::new();
                for n in i.picked.iter().copied() {
                    let Some(c) = list.get(n) else { continue };
                    let episodes = match i.assigned.get(&n) {
                        Some(e) => vec![*e],
                        None => c.episodes.clone(),
                    };
                    if i.kind == Kind::Sonarr && episodes.is_empty() {
                        continue;
                    }
                    files.push((c.clone(), episodes));
                }
                if files.is_empty() {
                    self.set_error("Pick the files to import (and their episodes)");
                    return;
                }
                i.busy = true;
                let (kind, owner, copy) = (i.kind, i.owner, i.copy);
                let Some(arr) = self.import_arr(kind) else {
                    return;
                };
                let n = files.len();
                self.spawn(async move {
                    let result = arr
                        .import(owner, &files, copy)
                        .await
                        .map(|()| {
                            format!(
                                "{} is importing {n} file{}",
                                kind.name(),
                                if n == 1 { "" } else { "s" }
                            )
                        })
                        .map_err(|e| format!("Couldn't import: {e:#}"));
                    Msg::Import(ImportMsg::Imported(result))
                });
            }
            ImportAction::Close => self.import = None,
        }
    }

    pub(super) fn handle_import_msg(&mut self, msg: ImportMsg) {
        match msg {
            ImportMsg::Candidates(result) => {
                let Some(i) = &mut self.import else { return };
                match result {
                    Ok(list) => {
                        // What Sonarr is happy with, picked to begin with.
                        i.picked = list
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| {
                                c.rejections.is_empty()
                                    && (i.kind == Kind::Radarr || !c.episodes.is_empty())
                            })
                            .map(|(n, _)| n)
                            .collect();
                        i.candidates = Some(Loaded::Ready(list));
                    }
                    Err(e) => i.candidates = Some(Loaded::Failed(e)),
                }
            }
            ImportMsg::Imported(result) => {
                match result {
                    Ok(said) => {
                        self.set_info(said);
                        self.import = None;
                    }
                    Err(e) => {
                        if let Some(i) = &mut self.import {
                            i.busy = false;
                        }
                        self.set_error(e);
                    }
                }
                self.servers.queue_read = None;
                self.refresh_servers();
            }
        }
    }

    /// The episodes a file can be imported as: the anime's in Sonarr, named
    /// as AniList numbers them (and as Sonarr does).
    pub(super) fn import_choices(&self) -> Vec<(u64, String)> {
        let Some(i) = &self.import else {
            return Vec::new();
        };
        let Some(s) = i.anilist.and_then(|id| self.library.find(id)) else {
            return Vec::new();
        };
        let Some(r) = &s.remote else {
            return Vec::new();
        };
        let (season, offset) = match r.arr {
            crate::library::ArrRef::Sonarr { season, offset, .. } => (season, offset),
            crate::library::ArrRef::Radarr { .. } => return Vec::new(),
        };
        r.episodes
            .iter()
            .filter_map(|(n, e)| {
                Some((
                    e.arr_id?,
                    format!("Episode {n} (S{season:02}E{:02})", n + offset),
                ))
            })
            .collect()
    }
}
