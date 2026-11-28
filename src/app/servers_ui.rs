//! The library on your servers: switching to it (and back), reading it from
//! Sonarr, Radarr and Jellyfin now and then, adding anime to Sonarr or
//! Radarr, and handing them a release (SeaDex's, or Nyaa's) or a search.
//! The library on this computer stays as it was meanwhile.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::discover_ui::Loaded;
use super::library_ui::SeriesStats;
use super::{App, Msg, Page};
use crate::fallback::ReleaseKind;
use crate::library::{ArrRef, Library};
use crate::servers::arr::{About, Arr, Kind, Queued};
use crate::servers::build::{self, Built};
use crate::servers::jellyfin::Jellyfin;
use crate::servers::mapping::{self, Mapping};
use crate::servers::{Config, LibraryMode};

pub(super) struct ServersState {
    /// What AniList said about anime's lengths and sequels (which episodes
    /// a season's sequel gets), and when (Unix time); kept on disk.
    parts: HashMap<u64, (crate::browse::Part, i64)>,
    pub(super) config: Config,
    /// As last saved.
    saved: Config,
    mapping: Option<Arc<Mapping>>,
    /// Sonarr's and Radarr's versions, quality profiles and root folders.
    pub(super) about: HashMap<Kind, Loaded<About>>,
    pub(super) refreshing: bool,
    pub(super) refreshed: Option<Instant>,
    /// A read the connection failed (part of): when to read again.
    retry_at: Option<Instant>,
    /// What couldn't be read last time.
    pub(super) problems: Vec<String>,
    /// Episodes downloading on the servers, how far along, by AniList ID.
    pub(super) progress: HashMap<u64, BTreeMap<u32, f32>>,
    /// The Jellyfin password being typed (never saved).
    pub(super) password: String,
    pub(super) signing_in: bool,
    pub(super) jellyfin_error: Option<String>,
    /// Anime being added, or handed a release or a search.
    pub(super) busy: HashSet<u64>,
    seq: u64,
    /// Sonarr's and Radarr's queues, as last read, and when.
    pub(super) queue: Vec<(Kind, Queued)>,
    pub(super) queue_read: Option<Instant>,
    queue_loading: bool,
}

#[derive(Clone, Debug)]
pub(super) enum ServersAction {
    SetMode(LibraryMode),
    /// Check Sonarr or Radarr, and read what anime can be added with.
    Check(Kind),
    SignIn,
    SignOut,
    Refresh,
    /// Add the anime on the page to Sonarr or Radarr.
    Add,
    /// Hand the release chosen on the page to Sonarr or Radarr (adding the
    /// anime first if it isn't there).
    Push,
    /// Have Sonarr or Radarr search for some of a series' episodes (all
    /// it's missing, if none are given).
    Search {
        id: u64,
        episodes: Vec<u32>,
    },
    /// Ask before taking something out of a queue.
    AskRemoveQueued {
        kind: Kind,
        id: u64,
        title: String,
    },
    /// Take it out (and out of the download client), maybe blocklisting
    /// its release.
    RemoveQueued {
        kind: Kind,
        id: u64,
        blocklist: bool,
    },
    /// Several at once.
    RemoveQueuedMany {
        kind: Kind,
        ids: Vec<u64>,
        blocklist: bool,
    },
}

pub(super) enum ServersMsg {
    Checked(Kind, Result<About, String>),
    SignedIn(Result<(String, String, String), String>),
    Built {
        seq: u64,
        result: Result<(Arc<Mapping>, Built), String>,
    },
    /// Something handed over: what to say (and, when it worked, episodes
    /// Sonarr now watches for), then read everything again.
    Done {
        id: u64,
        result: Result<String, String>,
        marks: Option<super::manage_ui::Monitored>,
    },
    /// The queues, read again (and what couldn't be read).
    Queue(Vec<(Kind, Queued)>, Vec<String>),
    /// Something taken out of a queue.
    Removed(Result<String, String>),
}

impl ServersState {
    pub(super) fn load(state_dir: &Path) -> Self {
        let config = Config::load(state_dir);
        let parts = std::fs::read(parts_file(state_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            parts,
            saved: config.clone(),
            config,
            mapping: None,
            about: HashMap::new(),
            refreshing: false,
            refreshed: None,
            retry_at: None,
            problems: Vec::new(),
            progress: HashMap::new(),
            password: String::new(),
            signing_in: false,
            jellyfin_error: None,
            busy: HashSet::new(),
            seq: 0,
            queue: Vec::new(),
            queue_read: None,
            queue_loading: false,
        }
    }
}

fn parts_file(state_dir: &Path) -> PathBuf {
    state_dir.join("anilist-parts.json")
}

/// How long what AniList said about an anime's length is trusted.
const PARTS_FOR: i64 = 12 * 3600;

/// Where each library is kept.
pub(super) fn library_file(state_dir: &Path, mode: LibraryMode) -> PathBuf {
    match mode {
        LibraryMode::Local => state_dir.join("library.json"),
        LibraryMode::Servers => state_dir.join("servers-library.json"),
    }
}

/// How often the servers are read again: often while something downloads.
fn every(downloading: bool) -> Duration {
    Duration::from_secs(if downloading { 30 } else { 300 })
}

impl App {
    pub(super) fn servers_mode(&self) -> bool {
        self.servers.config.mode == LibraryMode::Servers
    }

    pub(super) fn jellyfin(&self) -> Option<Jellyfin> {
        let j = &self.servers.config.jellyfin;
        j.signed_in().then(|| Jellyfin::new(j, &self.http))
    }

    fn arr(&self, kind: Kind) -> Option<Arr> {
        let account = match kind {
            Kind::Sonarr => &self.servers.config.sonarr,
            Kind::Radarr => &self.servers.config.radarr,
        };
        account
            .is_set()
            .then(|| Arr::new(kind, account, &self.http))
    }

    pub(super) fn servers_action(&mut self, action: ServersAction) {
        match action {
            ServersAction::SetMode(mode) => self.set_library_mode(mode),
            ServersAction::Check(kind) => self.check_arr(kind),
            ServersAction::SignIn => self.jellyfin_sign_in(),
            ServersAction::SignOut => {
                if let Some(j) = self.jellyfin() {
                    self.rt.spawn(async move {
                        let _ = j.sign_out().await;
                    });
                }
                let j = &mut self.servers.config.jellyfin;
                (j.token, j.user_id, j.server_name) = (None, None, None);
                self.servers.refreshed = None;
            }
            ServersAction::Refresh => self.refresh_servers(),
            ServersAction::Add => self.add_to_servers(),
            ServersAction::Push => self.push_release(),
            ServersAction::Search { id, episodes } => self.search_servers(id, episodes),
            ServersAction::AskRemoveQueued { kind, id, title } => {
                self.confirm_delete = Some((super::DeleteTarget::Queued(kind, id), title));
            }
            ServersAction::RemoveQueuedMany {
                kind,
                ids,
                blocklist,
            } => {
                let Some(arr) = self.arr(kind) else { return };
                let n = ids.len();
                self.spawn(async move {
                    let result = arr
                        .remove_queued_many(&ids, blocklist)
                        .await
                        .map(|()| {
                            format!(
                                "Removed {n} from {}'s queue{}",
                                kind.name(),
                                if blocklist {
                                    " and blocklisted them"
                                } else {
                                    ""
                                }
                            )
                        })
                        .map_err(|e| format!("Couldn't remove them: {e:#}"));
                    Msg::Servers(ServersMsg::Removed(result))
                });
            }
            ServersAction::RemoveQueued {
                kind,
                id,
                blocklist,
            } => {
                self.confirm_delete = None;
                let Some(arr) = self.arr(kind) else { return };
                self.spawn(async move {
                    let result = arr
                        .remove_queued(id, blocklist)
                        .await
                        .map(|()| {
                            format!(
                                "Removed from {}'s queue{}",
                                kind.name(),
                                if blocklist { " and blocklisted" } else { "" }
                            )
                        })
                        .map_err(|e| format!("Couldn't remove it: {e:#}"));
                    Msg::Servers(ServersMsg::Removed(result))
                });
            }
        }
    }

    pub(super) fn handle_servers_msg(&mut self, msg: ServersMsg) {
        match msg {
            ServersMsg::Checked(kind, result) => {
                let loaded = match result {
                    Ok(about) => {
                        // The first profile and folder, until others are chosen.
                        let account = match kind {
                            Kind::Sonarr => &mut self.servers.config.sonarr,
                            Kind::Radarr => &mut self.servers.config.radarr,
                        };
                        if account
                            .quality_profile
                            .is_none_or(|p| !about.profiles.iter().any(|x| x.id == p))
                        {
                            account.quality_profile = about.profiles.first().map(|p| p.id);
                        }
                        if account
                            .root_folder
                            .as_ref()
                            .is_none_or(|f| !about.folders.iter().any(|x| &x.path == f))
                        {
                            account.root_folder = about.folders.first().map(|f| f.path.clone());
                        }
                        self.servers.refreshed = None;
                        Loaded::Ready(about)
                    }
                    Err(e) => Loaded::Failed(e),
                };
                self.servers.about.insert(kind, loaded);
            }
            ServersMsg::SignedIn(result) => {
                self.servers.signing_in = false;
                match result {
                    Ok((token, user, server)) => {
                        let j = &mut self.servers.config.jellyfin;
                        j.token = Some(token);
                        j.user_id = Some(user);
                        j.server_name = Some(server);
                        self.servers.password.clear();
                        self.servers.jellyfin_error = None;
                        self.servers.refreshed = None;
                    }
                    Err(e) => self.servers.jellyfin_error = Some(e),
                }
            }
            ServersMsg::Built { seq, result } => {
                if seq != self.servers.seq {
                    return;
                }
                self.servers.refreshing = false;
                self.servers.refreshed = Some(Instant::now());
                // The connection failed (on a phone, the app may have been in
                // the background): again in a minute, not at the next read.
                let again = Some(Instant::now() + Duration::from_secs(60));
                self.servers.retry_at = match &result {
                    Ok((_, built)) if built.dropped => again,
                    Ok(_) => None,
                    Err(_) => again,
                };
                match result {
                    Ok((mapping, built)) => {
                        self.servers.mapping = Some(mapping);
                        if !built.parts.is_empty() {
                            let now = chrono::Utc::now().timestamp();
                            for (id, p) in &built.parts {
                                self.servers.parts.insert(*id, (p.clone(), now));
                            }
                            let file = parts_file(&self.state_dir);
                            if let Ok(json) = serde_json::to_vec(&self.servers.parts) {
                                let _ = crate::fsutil::write_private(&file, &json);
                            }
                        }
                        if !self.servers_mode() {
                            return;
                        }
                        let jellyfin = self.servers.config.jellyfin.signed_in();
                        self.library.series = build::merge(
                            &self.library.series,
                            built.series,
                            jellyfin,
                            &built.unread,
                        );
                        self.library.save_or_warn();
                        self.servers.progress = built.progress;
                        self.servers.problems = built.problems;
                        self.servers.queue = built.queue;
                        self.servers.queue_read = Some(Instant::now());
                        self.pages.last_stats = None;
                    }
                    Err(e) => self.servers.problems = vec![e],
                }
            }
            ServersMsg::Queue(queue, problems) => {
                self.servers.queue_loading = false;
                self.servers.queue_read = Some(Instant::now());
                if !self.servers_mode() {
                    return;
                }
                self.servers.progress = build::progress(&self.library.series, &queue);
                self.servers.queue = queue;
                for p in problems {
                    if !self.servers.problems.contains(&p) {
                        self.servers.problems.push(p);
                    }
                }
                self.pages.last_stats = None;
            }
            ServersMsg::Removed(result) => {
                match result {
                    Ok(said) => self.set_info(said),
                    Err(e) => self.set_error(e),
                }
                self.servers.queue_read = None;
            }
            ServersMsg::Done { id, result, marks } => {
                self.servers.busy.remove(&id);
                match result {
                    Ok(said) => {
                        if let Some(marks) = marks {
                            self.mark_monitored(&marks);
                        }
                        self.set_info(said);
                    }
                    Err(e) => self.set_error(e),
                }
                self.refresh_servers();
            }
        }
    }

    /// Now and then: save what changed, and read the servers again.
    pub(super) fn servers_tick(&mut self) {
        if self.servers.config != self.servers.saved {
            match self.servers.config.save(&self.state_dir) {
                Ok(()) => self.servers.saved = self.servers.config.clone(),
                Err(e) => tracing::warn!("couldn't save the servers: {e:#}"),
            }
        }
        if !self.servers_mode() || !self.servers.config.any() {
            return;
        }
        // The queues, every few seconds while they're on screen.
        if matches!(self.page, Page::Downloads)
            && !self.servers.queue_loading
            && !self.servers.refreshing
            && self
                .servers
                .queue_read
                .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
        {
            self.request_queue();
        }
        if self.servers.refreshing {
            return;
        }
        let due = self
            .servers
            .refreshed
            .is_none_or(|t| t.elapsed() > every(!self.servers.progress.is_empty()))
            || self.servers.retry_at.is_some_and(|t| Instant::now() >= t);
        if due {
            self.refresh_servers();
        }
    }

    /// Switch libraries: each keeps its own list (the files on this
    /// computer aren't touched either way).
    fn set_library_mode(&mut self, mode: LibraryMode) {
        if self.servers.config.mode == mode {
            return;
        }
        self.library.save_or_warn();
        // 18+ series are kept here whichever library is shown: they go along.
        let adult: Vec<crate::library::Series> = self
            .library
            .series
            .iter()
            .filter(|s| s.adult)
            .cloned()
            .collect();
        self.servers.config.mode = mode;
        let root = self.library_dir();
        self.library = Library::load(library_file(&self.state_dir, mode), &root);
        if !adult.is_empty() {
            for s in adult {
                match self
                    .library
                    .series
                    .iter_mut()
                    .find(|o| o.anilist_id == s.anilist_id)
                {
                    // (Unless the servers have it themselves.)
                    Some(o) if o.remote.is_some() => {}
                    Some(o) => *o = s,
                    None => self.library.series.push(s),
                }
            }
            self.library.save_or_warn();
        }
        self.pages.stats.clear();
        self.pages.last_stats = None;
        self.servers.progress.clear();
        self.servers.problems.clear();
        self.servers.refreshed = None;
        if let Page::Anime(ap) = &self.page
            && ap.opened_from_library()
        {
            self.page = Page::Home;
        }
    }

    fn check_arr(&mut self, kind: Kind) {
        let Some(arr) = self.arr(kind) else {
            self.servers.about.insert(
                kind,
                Loaded::Failed(format!("Enter {}'s address and API key", kind.name())),
            );
            return;
        };
        self.servers.about.insert(kind, Loaded::Loading);
        self.spawn(async move {
            let result = arr.about().await.map_err(|e| format!("{e:#}"));
            Msg::Servers(ServersMsg::Checked(kind, result))
        });
    }

    fn jellyfin_sign_in(&mut self) {
        let j = &self.servers.config.jellyfin;
        if j.url.trim().is_empty() || j.username.trim().is_empty() {
            self.servers.jellyfin_error = Some("Enter Jellyfin's address and your username".into());
            return;
        }
        let mut account = j.clone();
        account.token = None;
        let jelly = Jellyfin::new(&account, &self.http);
        let (user, password) = (j.username.trim().to_owned(), self.servers.password.clone());
        self.servers.signing_in = true;
        self.servers.jellyfin_error = None;
        self.spawn(async move {
            let result = jelly
                .sign_in(&user, &password)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::Servers(ServersMsg::SignedIn(result))
        });
    }

    /// Read Sonarr's and Radarr's queues again.
    fn request_queue(&mut self) {
        let arrs: Vec<Arr> = [Kind::Sonarr, Kind::Radarr]
            .into_iter()
            .filter_map(|k| self.arr(k))
            .collect();
        self.servers.queue_loading = true;
        self.spawn(async move {
            let (mut queue, mut problems) = (Vec::new(), Vec::new());
            for arr in arrs {
                match arr.queue().await {
                    Ok(q) => queue.extend(q.into_iter().map(|q| (arr.kind, q))),
                    Err(e) => problems.push(format!("{} queue: {e:#}", arr.kind.name())),
                }
            }
            Msg::Servers(ServersMsg::Queue(queue, problems))
        });
    }

    /// Read everything again.
    pub(super) fn refresh_servers(&mut self) {
        if !self.servers.config.any() {
            return;
        }
        self.servers.seq += 1;
        self.servers.refreshing = true;
        self.servers.retry_at = None;
        let seq = self.servers.seq;
        let (client, config, have) = (
            self.http.clone(),
            self.servers.config.clone(),
            self.servers.mapping.clone(),
        );
        let file = self.state_dir.join("anime-lists.json");
        // Titles AniList gave before needn't be asked for again.
        let known: HashSet<u64> = self
            .library
            .series
            .iter()
            .filter(|s| s.remote.is_some() && (s.romaji.is_some() || s.cover.is_some()))
            .map(|s| s.anilist_id)
            .collect();
        let now = chrono::Utc::now().timestamp();
        let parts = build::Parts {
            known: self
                .servers
                .parts
                .iter()
                .map(|(id, (p, _))| (*id, p.clone()))
                .collect(),
            fresh: self
                .servers
                .parts
                .iter()
                .filter(|(_, (_, at))| now - at < PARTS_FOR)
                .map(|(id, _)| *id)
                .collect(),
        };
        self.spawn(async move {
            let result = async {
                let mapping = match have {
                    Some(m) => m,
                    None => Arc::new(mapping::load(&client, &file).await?),
                };
                let built = build::build(&client, &config, &mapping, &known, &parts).await;
                anyhow::Ok((mapping, built))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::Servers(ServersMsg::Built { seq, result })
        });
    }

    /// The library's numbers, as the servers have them.
    pub(super) fn servers_stats(&mut self) {
        let mut stats = HashMap::new();
        for s in &self.library.series {
            let Some(r) = &s.remote else { continue };
            let episodes: BTreeMap<u32, (String, u64)> = r
                .episodes
                .iter()
                .filter_map(|(n, e)| Some((*n, e.file.clone()?)))
                .collect();
            let progress = self
                .servers
                .progress
                .get(&s.anilist_id)
                .cloned()
                .unwrap_or_default();
            stats.insert(
                s.anilist_id,
                SeriesStats {
                    files: episodes.values().cloned().collect(),
                    bytes: episodes.values().map(|(_, len)| len).sum(),
                    episodes,
                    pending: 0,
                    downloading: !progress.is_empty(),
                    progress,
                },
            );
        }
        self.pages.stats = stats;
    }

    /// Where an anime is in Sonarr or Radarr, if it is: its own entry, or
    /// (a sequel not out yet, say) as a later part of a series there, by
    /// what AniList said follows what. The anime it follows, if so.
    pub(super) fn server_owner(&self, id: u64) -> Option<(Kind, u64, Option<u32>, Option<u64>)> {
        if let Some(r) = self.library.find(id).and_then(|s| s.remote.as_ref()) {
            return Some(match r.arr {
                ArrRef::Sonarr { id, season, .. } => (Kind::Sonarr, id, Some(season), None),
                ArrRef::Radarr { id, .. } => (Kind::Radarr, id, None, None),
            });
        }
        self.library.series.iter().find_map(|s| {
            let r = s.remote.as_ref()?;
            let (part, _) = self.servers.parts.get(&s.anilist_id)?;
            if !part.sequels.contains(&id) {
                return None;
            }
            match r.arr {
                ArrRef::Sonarr { id, season, .. } => {
                    Some((Kind::Sonarr, id, Some(season), Some(s.anilist_id)))
                }
                ArrRef::Radarr { .. } => None,
            }
        })
    }

    /// Which server an anime goes to, and where it is there.
    pub(super) fn server_place(&self, id: u64) -> Option<(Kind, mapping::Place)> {
        let place = self.servers.mapping.as_ref()?.place(id)?;
        if place.movie && place.tmdb_movie.is_some() {
            Some((Kind::Radarr, place))
        } else if place.tvdb.is_some() && !place.movie {
            Some((Kind::Sonarr, place))
        } else {
            None
        }
    }

    /// Why the anime on the page can't be added, if it can't.
    pub(super) fn why_not_addable(&self, id: u64) -> Option<String> {
        if self.servers.mapping.is_none() {
            return Some("Reading which anime is which on TheTVDB and TheMovieDB…".into());
        }
        let Some((kind, _)) = self.server_place(id) else {
            return Some(
                "The anime-lists mapping doesn't know where this is on TheTVDB or TheMovieDB; \
                 add it in Sonarr or Radarr yourself"
                    .into(),
            );
        };
        let account = match kind {
            Kind::Sonarr => &self.servers.config.sonarr,
            Kind::Radarr => &self.servers.config.radarr,
        };
        if !account.is_set() {
            return Some(format!("Set up {} in Settings first", kind.name()));
        }
        if account.quality_profile.is_none() || account.root_folder.is_none() {
            return Some(format!(
                "Check {} in Settings (to choose its quality profile and folder) first",
                kind.name()
            ));
        }
        None
    }

    fn add_to_servers(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let (id, title) = (ap.anime.anilist_id, ap.anime.title.clone());
        if let Some(why) = self.why_not_addable(id) {
            self.set_error(why);
            return;
        }
        let Some((kind, place)) = self.server_place(id) else {
            return;
        };
        let Some(arr) = self.arr(kind) else { return };
        let account = match kind {
            Kind::Sonarr => self.servers.config.sonarr.clone(),
            Kind::Radarr => self.servers.config.radarr.clone(),
        };
        self.servers.busy.insert(id);
        self.spawn(async move {
            let result = match kind {
                Kind::Sonarr => {
                    arr.add_show(place.tvdb.unwrap_or(0), place.season, &account, false)
                        .await
                }
                Kind::Radarr => {
                    arr.add_movie(place.tmdb_movie.unwrap_or(0), &account, false)
                        .await
                }
            }
            .map(|_| format!("Added {title} to {}", kind.name()))
            .map_err(|e| format!("Couldn't add it: {e:#}"));
            Msg::Servers(ServersMsg::Done {
                id,
                result,
                marks: None,
            })
        });
    }

    /// The release chosen on the page, as Sonarr and Radarr take them:
    /// a title and a .torrent link (and magnet) each.
    fn chosen_for_servers(&self) -> Result<Vec<(String, String, String, u64)>, String> {
        let Page::Anime(ap) = &self.page else {
            return Err("No release chosen".into());
        };
        if let Some(search) = &ap.nyaa {
            let Some(r) = search.chosen.and_then(|i| search.releases.get(i)) else {
                return Err("Choose a release first".into());
            };
            return Ok(match &r.kind {
                ReleaseKind::Torrent { item, .. } => vec![(
                    item.title.clone(),
                    crate::nyaa::torrent_url(item.torrent()),
                    item.magnet(),
                    item.size,
                )],
                ReleaseKind::Episodes(items) => items
                    .iter()
                    .map(|(_, i)| {
                        (
                            i.title.clone(),
                            crate::nyaa::torrent_url(i.torrent()),
                            i.magnet(),
                            i.size,
                        )
                    })
                    .collect(),
                ReleaseKind::Stream { site, .. } => {
                    return Err(format!(
                        "{}'s episodes are downloaded here, not by Sonarr",
                        site.name()
                    ));
                }
            });
        }
        let Some(r) = super::chosen_release(ap) else {
            return Err("No release chosen".into());
        };
        let Some(nyaa) = r.nyaa_id() else {
            return Err(format!(
                "{}'s release is on a private tracker; Sonarr and Radarr can only be handed \
                 Nyaa's",
                r.group
            ));
        };
        // The torrent's own name says best what's in it.
        let nyaa = crate::nyaa::TorrentId::nyaa(nyaa);
        let title = match self.pages.release_files.get(&nyaa) {
            Some(super::library_ui::ReleaseFiles::Ready(listing)) => listing.name.clone(),
            _ => return Err("Still reading the release; try again in a moment".into()),
        };
        Ok(vec![(
            title,
            crate::nyaa::torrent_url(nyaa),
            r.info_hash_hex()
                .map(crate::nyaa::magnet)
                .unwrap_or_default(),
            r.total_size(),
        )])
    }

    fn push_release(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let id = ap.anime.anilist_id;
        let releases = match self.chosen_for_servers() {
            Ok(r) => r,
            Err(e) => {
                self.set_error(e);
                return;
            }
        };
        // There already (maybe as a later part of a series there), or to be
        // added first.
        let (kind, add) = match self.server_owner(id) {
            Some((kind, ..)) => (kind, None),
            None => {
                if let Some(why) = self.why_not_addable(id) {
                    self.set_error(why);
                    return;
                }
                let Some((kind, place)) = self.server_place(id) else {
                    return;
                };
                (kind, Some(place))
            }
        };
        let Some(arr) = self.arr(kind) else {
            self.set_error(format!("Set up {} in Settings first", kind.name()));
            return;
        };
        let account = match kind {
            Kind::Sonarr => self.servers.config.sonarr.clone(),
            Kind::Radarr => self.servers.config.radarr.clone(),
        };
        self.servers.busy.insert(id);
        self.spawn(async move {
            let result = async {
                // It must be there for the release to be matched to it.
                if let Some(place) = add {
                    match kind {
                        Kind::Sonarr => {
                            arr.add_show(place.tvdb.unwrap_or(0), place.season, &account, false)
                                .await?
                        }
                        Kind::Radarr => {
                            arr.add_movie(place.tmdb_movie.unwrap_or(0), &account, false)
                                .await?
                        }
                    };
                }
                let (mut sent, mut refused) = (0, Vec::new());
                for (title, url, magnet, size) in &releases {
                    let r = arr.push(title, url, magnet, *size).await?;
                    if r.approved {
                        sent += 1;
                    } else {
                        refused.push(format!("{title}: {}", r.rejections.join("; ")));
                    }
                }
                let name = kind.name();
                anyhow::Ok(match (sent, refused.is_empty()) {
                    (_, true) => format!("{name} is downloading it"),
                    (0, false) => anyhow::bail!("{name} didn't take it: {}", refused.join(" · ")),
                    (n, false) => format!(
                        "{name} is downloading {n} of {}; it didn't take {}",
                        releases.len(),
                        refused.join(" · ")
                    ),
                })
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::Servers(ServersMsg::Done {
                id,
                result,
                marks: None,
            })
        });
    }

    fn search_servers(&mut self, id: u64, episodes: Vec<u32>) {
        let Some(remote) = self.library.find(id).and_then(|s| s.remote.clone()) else {
            return;
        };
        let (kind, owner) = match remote.arr {
            ArrRef::Sonarr { id, .. } => (Kind::Sonarr, id),
            ArrRef::Radarr { id, .. } => (Kind::Radarr, id),
        };
        let Some(arr) = self.arr(kind) else { return };
        // The episodes asked for, or all that are missing.
        let wanted: Vec<u64> = remote
            .episodes
            .iter()
            .filter(|(n, e)| {
                if episodes.is_empty() {
                    e.file.is_none()
                } else {
                    episodes.contains(n)
                }
            })
            .filter_map(|(_, e)| e.arr_id)
            .collect();
        if kind == Kind::Sonarr && wanted.is_empty() {
            self.set_info("Nothing to search for");
            return;
        }
        let n = wanted.len();
        // Only what's watched for is searched for.
        let marks = (kind == Kind::Sonarr).then(|| super::manage_ui::Monitored {
            episodes: wanted.clone(),
            on: true,
            season: None,
        });
        self.servers.busy.insert(id);
        self.spawn(async move {
            let result = async {
                if kind == Kind::Sonarr {
                    arr.monitor(&wanted, true).await?;
                }
                arr.search(owner, &wanted).await
            }
            .await
            .map(|()| match (kind, n) {
                (Kind::Radarr, _) => "Radarr is searching for it".to_owned(),
                (_, 1) => "Sonarr is searching for the episode".to_owned(),
                (_, n) => format!("Sonarr is searching for {n} episodes"),
            })
            .map_err(|e| format!("Couldn't search: {e:#}"));
            Msg::Servers(ServersMsg::Done { id, result, marks })
        });
    }

    /// Tell Jellyfin how far an episode's been watched.
    pub(super) fn report_watched(&self, id: u64, episode: u32, position: f64, done: bool) {
        let Some(j) = self.jellyfin() else { return };
        let Some(item) = self
            .library
            .find(id)
            .and_then(|s| s.remote.as_ref()?.episodes.get(&episode)?.jellyfin.clone())
        else {
            return;
        };
        self.rt.spawn(async move {
            if let Err(e) = j.set_watched(&item, position, done).await {
                tracing::warn!("couldn't tell Jellyfin: {e:#}");
            }
        });
    }
}

/// "4 min ago".
pub(super) fn ago(t: std::time::Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}
