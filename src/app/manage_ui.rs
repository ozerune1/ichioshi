//! Managing a series (or film) on the servers from its page, as in Sonarr
//! (or Radarr): watching for it, its settings, searching (automatic and
//! interactive), its files, renaming, its history, deleting it.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::discover_ui::Loaded;
use super::{App, Msg, Page};
use crate::library::ArrRef;
use crate::servers::arr::{Arr, Event, FileInfo, Kind, Release, Rename, Scope, Tag};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tab {
    Overview,
    Search,
    Files,
    History,
}

/// What interactive search looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Looking {
    /// The anime's season (in Sonarr).
    Season,
    /// One episode (AniList's numbering).
    Episode(u32),
    Film,
}

/// The series' settings, being changed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Edit {
    pub(super) profile: u64,
    /// standard, daily, anime (Sonarr).
    pub(super) series_type: String,
    pub(super) season_folder: bool,
    /// all or none: whether new seasons are watched for (Sonarr).
    pub(super) monitor_new: String,
    /// announced, inCinemas, released (Radarr).
    pub(super) availability: String,
    pub(super) root: String,
    pub(super) move_files: bool,
    pub(super) tags: Vec<u64>,
    /// A tag being typed, to make.
    pub(super) new_tag: String,
}

/// A series' (or film's) management panel.
pub(super) struct Manage {
    pub(super) anilist: u64,
    pub(super) kind: Kind,
    /// Sonarr's series or Radarr's film.
    pub(super) owner: u64,
    /// The anime's season, in Sonarr.
    pub(super) season: Option<u32>,
    /// It's there as a later part of this anime's series (no episodes of
    /// its own there yet).
    pub(super) follows: Option<u64>,
    pub(super) tab: Tab,
    /// The series or film as Sonarr or Radarr has it.
    pub(super) record: Loaded<Value>,
    pub(super) edit: Option<Edit>,
    pub(super) looking: Looking,
    pub(super) releases: Option<Loaded<Vec<Release>>>,
    pub(super) grabbing: HashSet<String>,
    pub(super) grabbed: HashSet<String>,
    pub(super) files: Option<Loaded<Vec<FileInfo>>>,
    pub(super) picked_files: HashSet<u64>,
    pub(super) history: Option<Loaded<Vec<Event>>>,
    pub(super) renames: Option<Loaded<Vec<Rename>>>,
    pub(super) busy: bool,
    /// Its server's tags.
    pub(super) tags: Option<Loaded<Vec<Tag>>>,
    /// Deleting it also keeps import lists from adding it back.
    pub(super) exclude: bool,
    /// The episode whose details are open (AniList's numbering), and
    /// Sonarr's record of it.
    pub(super) episode: Option<(u32, Loaded<Value>)>,
}

#[derive(Clone, Debug)]
pub(super) enum ManageAction {
    Tab(Tab),
    /// Watch for the series (or film), or stop.
    SetMonitored(bool),
    /// Watch for some of the anime's episodes (AniList's numbering), or
    /// stop; empty: all of them.
    SetEpisodesMonitored(Vec<u32>, bool),
    StartEdit,
    ChangeEdit(Edit),
    /// Make a tag (and give it the series), from the edit form.
    AddTag,
    /// Deleting it keeps import lists from adding it back, or not.
    SetExclude(bool),
    /// Open (or close) an episode's details.
    Episode(u32),
    CancelEdit,
    SaveEdit,
    /// Sonarr's (or Radarr's) own search: these episodes, or (empty) all
    /// of the anime's that are missing.
    AutoSearch(Vec<u32>),
    /// Its indexers' releases, to pick from.
    Look(Looking),
    Grab(usize),
    Refresh,
    PreviewRenames,
    Rename,
    CancelRenames,
    PickFile(u64, bool),
    AskDeleteFiles(Vec<u64>),
    DeleteFiles(Vec<u64>),
    AskDelete,
    Delete {
        delete_files: bool,
        exclude: bool,
    },
}

/// Episodes Sonarr was asked to watch for (or not), by its IDs: shown so as
/// soon as it says it has, not once the servers are read again (all of
/// them, which can take a while).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Monitored {
    pub(super) episodes: Vec<u64>,
    pub(super) on: bool,
    /// Its series' season, watched for too (Sonarr's series, the season).
    pub(super) season: Option<(u64, u32)>,
}

pub(super) enum ManageMsg {
    Record(u64, Result<Value, String>),
    Releases(u64, Looking, Result<Vec<Release>, String>),
    Files(u64, Result<Vec<FileInfo>, String>),
    History(u64, Result<Vec<Event>, String>),
    Renames(u64, Result<Vec<Rename>, String>),
    Grabbed(u64, String, Result<(), String>),
    Tags(u64, Result<Vec<Tag>, String>),
    Episode(u64, u32, Result<Value, String>),
    TagAdded(u64, Result<Tag, String>),
    /// Something changed there: what to say (and, when it worked, the
    /// episodes it now watches for, or doesn't); read the servers again.
    Done(u64, Result<String, String>, Option<Monitored>),
    Deleted(u64, Result<String, String>),
}

/// `marks` on the library's series: the episodes (whichever anime they're
/// part of: a split season's, say) and their seasons. Whether that changed
/// anything.
fn mark(series: &mut [crate::library::Series], marks: &Monitored) -> bool {
    let mut changed = false;
    for s in series {
        let Some(r) = &mut s.remote else { continue };
        let ArrRef::Sonarr { id, season, .. } = r.arr else {
            continue;
        };
        for e in r.episodes.values_mut() {
            if e.arr_id.is_some_and(|a| marks.episodes.contains(&a)) && e.monitored != marks.on {
                e.monitored = marks.on;
                changed = true;
            }
        }
        if marks.season == Some((id, season)) && !r.monitored {
            r.monitored = true;
            changed = true;
        }
    }
    changed
}

fn loaded<T>(r: Result<T, String>) -> Loaded<T> {
    match r {
        Ok(v) => Loaded::Ready(v),
        Err(e) => Loaded::Failed(e),
    }
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

impl App {
    /// The panel for the anime on the page, if it's on the servers (made
    /// when the page shows).
    pub(super) fn manage_tick(&mut self) {
        let Page::Anime(ap) = &self.page else {
            self.manage = None;
            return;
        };
        let id = ap.anime.anilist_id;
        if self.manage.as_ref().is_some_and(|m| m.anilist == id) {
            return;
        }
        self.manage = None;
        if !self.servers_mode() {
            return;
        }
        let Some((kind, owner, season, follows)) = self.server_owner(id) else {
            return;
        };
        self.manage = Some(Manage {
            anilist: id,
            kind,
            owner,
            season,
            follows,
            tab: Tab::Overview,
            record: Loaded::Loading,
            edit: None,
            looking: if kind == Kind::Radarr {
                Looking::Film
            } else {
                Looking::Season
            },
            releases: None,
            grabbing: HashSet::new(),
            grabbed: HashSet::new(),
            files: None,
            picked_files: HashSet::new(),
            history: None,
            renames: None,
            busy: false,
            tags: None,
            exclude: false,
            episode: None,
        });
        self.load_record();
        self.load_tags();
    }

    fn manage_arr(&self) -> Option<(Arr, u64, &Manage)> {
        let m = self.manage.as_ref()?;
        let account = match m.kind {
            Kind::Sonarr => &self.servers.config.sonarr,
            Kind::Radarr => &self.servers.config.radarr,
        };
        account
            .is_set()
            .then(|| (Arr::new(m.kind, account, &self.http), m.anilist, m))
    }

    fn load_record(&mut self) {
        let Some((arr, id, m)) = self.manage_arr() else {
            return;
        };
        let owner = m.owner;
        self.spawn(async move {
            Msg::Manage(ManageMsg::Record(id, arr.owner(owner).await.map_err(err)))
        });
    }

    /// The anime's episodes in Sonarr (by AniList's numbering).
    fn arr_episodes(&self, numbers: &[u32]) -> Vec<u64> {
        let Some(m) = &self.manage else {
            return Vec::new();
        };
        let Some(r) = self.library.find(m.anilist).and_then(|s| s.remote.as_ref()) else {
            return Vec::new();
        };
        r.episodes
            .iter()
            .filter(|(n, _)| numbers.is_empty() || numbers.contains(n))
            .filter_map(|(_, e)| e.arr_id)
            .collect()
    }

    /// Run `work` against the server; say what it says, then read the
    /// servers (and the series) again.
    fn manage_do<F, Fut>(&mut self, work: F)
    where
        F: FnOnce(Arr, u64) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.manage_do_marking(None, work);
    }

    /// `manage_do`, and once it's worked, `marks` shown at once.
    fn manage_do_marking<F, Fut>(&mut self, marks: Option<Monitored>, work: F)
    where
        F: FnOnce(Arr, u64) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        let Some((arr, id, m)) = self.manage_arr() else {
            return;
        };
        let fut = work(arr, m.owner);
        if let Some(m) = &mut self.manage {
            m.busy = true;
        }
        self.spawn(async move { Msg::Manage(ManageMsg::Done(id, fut.await.map_err(err), marks)) });
    }

    /// Sonarr watches for these episodes now, or doesn't (it said so): the
    /// library shows it before the servers are read again.
    pub(super) fn mark_monitored(&mut self, marks: &Monitored) {
        if mark(&mut self.library.series, marks) {
            self.library.save_or_warn();
        }
    }

    pub(super) fn manage_action(&mut self, action: ManageAction) {
        let Some(m) = &mut self.manage else { return };
        match action {
            ManageAction::Tab(tab) => {
                m.tab = tab;
                match tab {
                    Tab::Files if m.files.is_none() => self.load_files(),
                    Tab::History if m.history.is_none() => self.load_history(),
                    Tab::Search if m.releases.is_none() => {
                        let looking = m.looking;
                        self.look(looking);
                    }
                    _ => {}
                }
            }
            ManageAction::SetMonitored(on) => {
                let Loaded::Ready(record) = &m.record else {
                    return;
                };
                let mut record = record.clone();
                record["monitored"] = json!(on);
                let kind = m.kind;
                self.manage_do(move |arr, _| async move {
                    arr.save_owner(&record, false).await?;
                    Ok(format!(
                        "{} {} watching for it",
                        kind.name(),
                        if on { "is" } else { "stopped" }
                    ))
                });
            }
            ManageAction::SetEpisodesMonitored(numbers, on) => {
                let ids = self.arr_episodes(&numbers);
                if ids.is_empty() {
                    return;
                }
                let n = ids.len();
                // Watching for episodes needs the season watched for too
                // (else Sonarr ignores them).
                let (season, owner) = match self.manage.as_ref() {
                    Some(m) => (
                        match (&m.record, m.season) {
                            (Loaded::Ready(r), Some(s)) if on => Some((r.clone(), s)),
                            _ => None,
                        },
                        m.owner,
                    ),
                    None => (None, 0),
                };
                let marks = Monitored {
                    episodes: ids.clone(),
                    on,
                    season: season.as_ref().map(|(_, s)| (owner, *s)),
                };
                self.manage_do_marking(Some(marks), move |arr, _| async move {
                    arr.monitor(&ids, on).await?;
                    if let Some((mut record, number)) = season {
                        let mut changed = false;
                        for s in record["seasons"].as_array_mut().into_iter().flatten() {
                            if s["seasonNumber"].as_u64() == Some(u64::from(number))
                                && s["monitored"] != true
                            {
                                s["monitored"] = json!(true);
                                changed = true;
                            }
                        }
                        if record["monitored"] != true {
                            record["monitored"] = json!(true);
                            changed = true;
                        }
                        if changed {
                            arr.save_owner(&record, false).await?;
                        }
                    }
                    Ok(match (n, on) {
                        (1, true) => "Watching for the episode".to_owned(),
                        (1, false) => "Not watching for the episode".to_owned(),
                        (n, true) => format!("Watching for {n} episodes"),
                        (n, false) => format!("Not watching for {n} episodes"),
                    })
                });
            }
            ManageAction::StartEdit => {
                let Loaded::Ready(r) = &m.record else { return };
                let path = r["path"].as_str().unwrap_or_default();
                let root = r["rootFolderPath"]
                    .as_str()
                    .map(|s| s.trim_end_matches('/').to_owned())
                    .unwrap_or_else(|| {
                        path.rsplit_once('/')
                            .map(|(p, _)| p.to_owned())
                            .unwrap_or_default()
                    });
                m.edit = Some(Edit {
                    profile: r["qualityProfileId"].as_u64().unwrap_or(0),
                    series_type: r["seriesType"].as_str().unwrap_or("anime").to_owned(),
                    season_folder: r["seasonFolder"] != false,
                    monitor_new: r["monitorNewItems"].as_str().unwrap_or("all").to_owned(),
                    availability: r["minimumAvailability"]
                        .as_str()
                        .unwrap_or("released")
                        .to_owned(),
                    root,
                    move_files: true,
                    tags: r["tags"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_u64)
                        .collect(),
                    new_tag: String::new(),
                });
                let (no_tags, kind) = (m.tags.is_none(), m.kind);
                if no_tags {
                    self.load_tags();
                }
                // What it can be changed to.
                if !matches!(
                    self.servers.about.get(&kind),
                    Some(Loaded::Ready(_) | Loaded::Loading)
                ) {
                    self.servers_action(super::servers_ui::ServersAction::Check(kind));
                }
            }
            ManageAction::ChangeEdit(e) => m.edit = Some(e),
            ManageAction::SetExclude(on) => m.exclude = on,
            ManageAction::Episode(n) => {
                if m.episode.as_ref().is_some_and(|(e, _)| *e == n) {
                    m.episode = None;
                    return;
                }
                m.episode = Some((n, Loaded::Loading));
                let (need_files, need_history) = (m.files.is_none(), m.history.is_none());
                let Some(arr_id) = self.arr_episodes(&[n]).first().copied() else {
                    if let Some(m) = &mut self.manage {
                        m.episode = Some((n, Loaded::Failed("Sonarr doesn't list it".into())));
                    }
                    return;
                };
                let Some((arr, id, _)) = self.manage_arr() else {
                    return;
                };
                self.spawn(async move {
                    Msg::Manage(ManageMsg::Episode(
                        id,
                        n,
                        arr.episode(arr_id).await.map_err(err),
                    ))
                });
                if need_files {
                    self.load_files();
                }
                if need_history {
                    self.load_history();
                }
            }
            ManageAction::AddTag => {
                let Some(e) = &mut m.edit else { return };
                let label = e.new_tag.trim().to_owned();
                if label.is_empty() {
                    return;
                }
                e.new_tag.clear();
                let Some((arr, id, _)) = self.manage_arr() else {
                    return;
                };
                self.spawn(async move {
                    Msg::Manage(ManageMsg::TagAdded(
                        id,
                        arr.add_tag(&label).await.map_err(err),
                    ))
                });
            }
            ManageAction::CancelEdit => m.edit = None,
            ManageAction::SaveEdit => {
                let (Some(e), Loaded::Ready(r)) = (m.edit.take(), &m.record) else {
                    return;
                };
                let mut r = r.clone();
                let folder = r["path"]
                    .as_str()
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let root = e.root.trim_end_matches('/');
                let new_path = format!("{root}/{folder}");
                let moving = r["path"].as_str() != Some(new_path.as_str());
                r["qualityProfileId"] = json!(e.profile);
                match m.kind {
                    Kind::Sonarr => {
                        r["seriesType"] = json!(e.series_type);
                        r["seasonFolder"] = json!(e.season_folder);
                        r["monitorNewItems"] = json!(e.monitor_new);
                    }
                    Kind::Radarr => r["minimumAvailability"] = json!(e.availability),
                }
                if moving {
                    r["path"] = json!(new_path);
                    r["rootFolderPath"] = json!(root);
                }
                r["tags"] = json!(e.tags);
                let move_files = moving && e.move_files;
                self.manage_do(move |arr, _| async move {
                    arr.save_owner(&r, move_files).await?;
                    Ok(if move_files {
                        "Saved; its files are moving".to_owned()
                    } else {
                        "Saved".to_owned()
                    })
                });
            }
            ManageAction::AutoSearch(numbers) => {
                let kind = m.kind;
                if kind == Kind::Radarr {
                    self.manage_do(|arr, owner| async move {
                        arr.search(owner, &[]).await?;
                        Ok("Radarr is searching for it".to_owned())
                    });
                    return;
                }
                let anilist = m.anilist;
                // The anime's missing episodes (or those asked for).
                let ids: Vec<u64> = match self.library.find(anilist).and_then(|s| s.remote.as_ref())
                {
                    Some(r) => r
                        .episodes
                        .iter()
                        .filter(|(n, e)| {
                            if numbers.is_empty() {
                                e.file.is_none()
                            } else {
                                numbers.contains(n)
                            }
                        })
                        .filter_map(|(_, e)| e.arr_id)
                        .collect(),
                    None => Vec::new(),
                };
                if ids.is_empty() {
                    self.set_info("Nothing missing to search for");
                    return;
                }
                let marks = Monitored {
                    episodes: ids.clone(),
                    on: true,
                    season: None,
                };
                self.manage_do_marking(Some(marks), move |arr, owner| async move {
                    arr.monitor(&ids, true).await?;
                    arr.search(owner, &ids).await?;
                    Ok(match ids.len() {
                        1 => "Sonarr is searching for the episode".to_owned(),
                        n => format!("Sonarr is searching for {n} episodes"),
                    })
                });
            }
            ManageAction::Look(looking) => {
                m.tab = Tab::Search;
                self.look(looking);
            }
            ManageAction::Grab(i) => {
                let Some(Loaded::Ready(list)) = &m.releases else {
                    return;
                };
                let Some(r) = list.get(i).cloned() else {
                    return;
                };
                m.grabbing.insert(r.guid.clone());
                let Some((arr, id, _)) = self.manage_arr() else {
                    return;
                };
                self.spawn(async move {
                    let result = arr.grab(&r).await.map_err(err);
                    Msg::Manage(ManageMsg::Grabbed(id, r.guid, result))
                });
            }
            ManageAction::Refresh => {
                let kind = m.kind;
                self.manage_do(move |arr, owner| async move {
                    arr.command(match kind {
                        Kind::Sonarr => json!({"name": "RefreshSeries", "seriesId": owner}),
                        Kind::Radarr => json!({"name": "RefreshMovie", "movieIds": [owner]}),
                    })
                    .await?;
                    Ok(format!(
                        "{} is refreshing it and scanning its files",
                        kind.name()
                    ))
                });
            }
            ManageAction::PreviewRenames => {
                m.renames = Some(Loaded::Loading);
                let season = m.season;
                let Some((arr, id, m)) = self.manage_arr() else {
                    return;
                };
                let owner = m.owner;
                self.spawn(async move {
                    Msg::Manage(ManageMsg::Renames(
                        id,
                        arr.rename_preview(owner, season).await.map_err(err),
                    ))
                });
            }
            ManageAction::CancelRenames => m.renames = None,
            ManageAction::Rename => {
                let Some(Loaded::Ready(list)) = m.renames.take() else {
                    return;
                };
                let files: Vec<u64> = list.iter().map(|r| r.file).collect();
                self.manage_do(move |arr, owner| async move {
                    arr.rename(owner, &files).await?;
                    Ok(format!(
                        "Renaming {} file{}",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" }
                    ))
                });
            }
            ManageAction::PickFile(id, on) => {
                if on {
                    m.picked_files.insert(id);
                } else {
                    m.picked_files.remove(&id);
                }
            }
            ManageAction::AskDeleteFiles(ids) => {
                let n = ids.len();
                self.confirm_delete = Some((
                    super::DeleteTarget::ArrFiles(ids),
                    format!("{n} file{}", if n == 1 { "" } else { "s" }),
                ));
            }
            ManageAction::DeleteFiles(ids) => {
                self.confirm_delete = None;
                m.picked_files.retain(|f| !ids.contains(f));
                m.files = None;
                self.manage_do(move |arr, _| async move {
                    arr.delete_files(&ids).await?;
                    Ok(format!(
                        "Deleted {} file{}",
                        ids.len(),
                        if ids.len() == 1 { "" } else { "s" }
                    ))
                });
            }
            ManageAction::AskDelete => {
                let title = match &m.record {
                    Loaded::Ready(r) => r["title"].as_str().unwrap_or("it").to_owned(),
                    _ => "it".to_owned(),
                };
                self.confirm_delete = Some((super::DeleteTarget::ArrOwner, title));
            }
            ManageAction::Delete {
                delete_files,
                exclude,
            } => {
                self.confirm_delete = None;
                let Some((arr, id, m)) = self.manage_arr() else {
                    return;
                };
                let (owner, kind) = (m.owner, m.kind);
                self.spawn(async move {
                    let result = arr
                        .delete_owner(owner, delete_files, exclude)
                        .await
                        .map(|()| {
                            format!(
                                "Deleted from {}{}",
                                kind.name(),
                                if delete_files { ", with its files" } else { "" }
                            )
                        })
                        .map_err(err);
                    Msg::Manage(ManageMsg::Deleted(id, result))
                });
            }
        }
    }

    fn look(&mut self, looking: Looking) {
        let Some(m) = &mut self.manage else { return };
        m.looking = looking;
        m.releases = Some(Loaded::Loading);
        let scope = match (looking, m.season) {
            (Looking::Film, _) => Scope::Movie(m.owner),
            (Looking::Episode(n), _) => match self.arr_episodes(&[n]).first() {
                Some(id) => Scope::Episode(*id),
                None => return,
            },
            (Looking::Season, Some(season)) => Scope::Season {
                series: m.owner,
                season,
            },
            (Looking::Season, None) => return,
        };
        let Some((arr, id, _)) = self.manage_arr() else {
            return;
        };
        self.spawn(async move {
            Msg::Manage(ManageMsg::Releases(
                id,
                looking,
                arr.releases(scope).await.map_err(err),
            ))
        });
    }

    fn load_tags(&mut self) {
        let Some((arr, id, _)) = self.manage_arr() else {
            return;
        };
        if let Some(m) = &mut self.manage {
            m.tags = Some(Loaded::Loading);
        }
        self.spawn(async move { Msg::Manage(ManageMsg::Tags(id, arr.tags().await.map_err(err))) });
    }

    fn load_files(&mut self) {
        let Some((arr, id, m)) = self.manage_arr() else {
            return;
        };
        let owner = m.owner;
        if let Some(m) = &mut self.manage {
            m.files = Some(Loaded::Loading);
        }
        self.spawn(async move {
            Msg::Manage(ManageMsg::Files(id, arr.files(owner).await.map_err(err)))
        });
    }

    fn load_history(&mut self) {
        let Some((arr, id, m)) = self.manage_arr() else {
            return;
        };
        let (owner, season) = (m.owner, m.season);
        if let Some(m) = &mut self.manage {
            m.history = Some(Loaded::Loading);
        }
        self.spawn(async move {
            Msg::Manage(ManageMsg::History(
                id,
                arr.history(owner, season).await.map_err(err),
            ))
        });
    }

    pub(super) fn handle_manage_msg(&mut self, msg: ManageMsg) {
        let for_page = |m: &Option<Manage>, id: u64| m.as_ref().is_some_and(|m| m.anilist == id);
        match msg {
            ManageMsg::Record(id, r) if for_page(&self.manage, id) => {
                self.manage.as_mut().unwrap().record = loaded(r);
            }
            ManageMsg::Releases(id, looking, r) if for_page(&self.manage, id) => {
                let m = self.manage.as_mut().unwrap();
                if m.looking == looking {
                    m.releases = Some(loaded(r));
                }
            }
            ManageMsg::Files(id, r) if for_page(&self.manage, id) => {
                self.manage.as_mut().unwrap().files = Some(loaded(r));
            }
            ManageMsg::History(id, r) if for_page(&self.manage, id) => {
                self.manage.as_mut().unwrap().history = Some(loaded(r));
            }
            ManageMsg::Episode(id, n, r) if for_page(&self.manage, id) => {
                let m = self.manage.as_mut().unwrap();
                if m.episode.as_ref().is_some_and(|(e, _)| *e == n) {
                    m.episode = Some((n, loaded(r)));
                }
            }
            ManageMsg::Tags(id, r) if for_page(&self.manage, id) => {
                self.manage.as_mut().unwrap().tags = Some(loaded(r));
            }
            ManageMsg::TagAdded(id, r) if for_page(&self.manage, id) => match r {
                Ok(tag) => {
                    let m = self.manage.as_mut().unwrap();
                    if let Some(e) = &mut m.edit {
                        e.tags.push(tag.id);
                    }
                    if let Some(Loaded::Ready(list)) = &mut m.tags {
                        list.push(tag);
                    }
                }
                Err(e) => self.set_error(format!("Couldn't make the tag: {e}")),
            },
            ManageMsg::Renames(id, r) if for_page(&self.manage, id) => {
                self.manage.as_mut().unwrap().renames = Some(loaded(r));
            }
            ManageMsg::Grabbed(id, guid, r) => {
                if let Some(m) = self.manage.as_mut().filter(|m| m.anilist == id) {
                    m.grabbing.remove(&guid);
                    if r.is_ok() {
                        m.grabbed.insert(guid);
                    }
                }
                match r {
                    Ok(()) => self.set_info("Sent to the download client"),
                    Err(e) => self.set_error(format!("Couldn't grab it: {e}")),
                }
                self.servers.queue_read = None;
                self.refresh_servers();
            }
            ManageMsg::Done(id, r, marks) => {
                match r {
                    Ok(said) => {
                        if let Some(marks) = marks {
                            self.mark_monitored(&marks);
                        }
                        self.set_info(said);
                    }
                    Err(e) => self.set_error(e),
                }
                if self.manage.as_ref().is_some_and(|m| m.anilist == id) {
                    self.load_record();
                    let m = self.manage.as_mut().unwrap();
                    m.busy = false;
                    if m.files.is_some() {
                        self.load_files();
                    }
                }
                self.refresh_servers();
            }
            ManageMsg::Deleted(id, r) => {
                match r {
                    Ok(said) => {
                        self.set_info(said);
                        if self.manage.as_ref().is_some_and(|m| m.anilist == id) {
                            self.manage = None;
                        }
                    }
                    Err(e) => self.set_error(format!("Couldn't delete it: {e}")),
                }
                self.refresh_servers();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Remote, RemoteEpisode, Series};

    /// An anime in Sonarr: `show`'s `season`, with these episodes (its
    /// number, Sonarr's ID, watched for).
    fn in_sonarr(anilist: u64, show: u64, season: u32, eps: &[(u32, u64, bool)]) -> Series {
        let mut s: Series =
            serde_json::from_value(json!({"anilist_id": anilist, "folder": "/x"})).unwrap();
        s.remote = Some(Remote {
            arr: ArrRef::Sonarr {
                id: show,
                tvdb: 1,
                season,
                offset: 0,
            },
            path: String::new(),
            slug: String::new(),
            monitored: false,
            jellyfin: None,
            episodes: eps
                .iter()
                .map(|&(n, id, on)| {
                    let e = RemoteEpisode {
                        arr_id: Some(id),
                        monitored: on,
                        ..Default::default()
                    };
                    (n, e)
                })
                .collect(),
        });
        s
    }

    fn watched_for(series: &[Series], anilist: u64) -> Vec<bool> {
        let s = series.iter().find(|s| s.anilist_id == anilist).unwrap();
        let r = s.remote.as_ref().unwrap();
        r.episodes.values().map(|e| e.monitored).collect()
    }

    /// What Sonarr was told shows on the episodes' boxes at once: not only
    /// once the servers are read again.
    #[test]
    fn episodes_show_what_sonarr_was_told() {
        let mut series = vec![
            in_sonarr(
                10,
                1,
                1,
                &[(1, 101, true), (2, 102, false), (3, 103, false)],
            ),
            // The same season's second cour, as another anime.
            in_sonarr(11, 1, 1, &[(1, 104, false)]),
            in_sonarr(12, 2, 1, &[(1, 201, false)]),
        ];
        let stop = Monitored {
            episodes: vec![101],
            on: false,
            season: None,
        };
        assert!(mark(&mut series, &stop));
        assert_eq!(watched_for(&series, 10), [false, false, false]);
        assert!(!mark(&mut series, &stop), "nothing left to change");

        // Watching for some (the season too, in both its anime).
        let watch = Monitored {
            episodes: vec![102, 104],
            on: true,
            season: Some((1, 1)),
        };
        assert!(mark(&mut series, &watch));
        assert_eq!(watched_for(&series, 10), [false, true, false]);
        assert_eq!(watched_for(&series, 11), [true]);
        assert_eq!(watched_for(&series, 12), [false], "another series");
        let season = |id: u64| {
            series
                .iter()
                .find(|s| s.anilist_id == id)
                .and_then(|s| s.remote.as_ref())
                .unwrap()
                .monitored
        };
        assert!(season(10) && season(11) && !season(12));
    }
}
