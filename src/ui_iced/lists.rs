//! Your AniList and MyAnimeList lists in the iced GUI: read now and then,
//! changed from the library (status, score) and by the player (episodes
//! watched). A change goes to every list you're signed in to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;

use super::Message;
use crate::library::Library;
use crate::tracking::{Change, Config, Endpoints, Entry, Service, SignedOut, anilist, mal};

const REFRESH: Duration = Duration::from_secs(15 * 60);

/// What went wrong, and whether the account needs signing in again.
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub signed_out: bool,
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure {
            signed_out: e.is::<SignedOut>(),
            message: format!("{e:#}"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    Listed(Service, Result<HashMap<u64, Entry>, Failure>),
    MalIds(Vec<u64>, Result<HashMap<u64, u64>, String>),
    Saved {
        service: Service,
        key: u64,
        before: Option<Entry>,
        result: Result<Entry, Failure>,
    },
}

pub struct Lists {
    pub config: Config,
    file: PathBuf,
    pub endpoints: Endpoints,
    http: reqwest::Client,
    anilist: Option<Arc<anilist::Session>>,
    mal: Option<Arc<mal::Session>>,
    /// AniList's entries by AniList ID; MyAnimeList's by its own.
    pub lists: HashMap<Service, HashMap<u64, Entry>>,
    fetching: HashSet<Service>,
    fetched: HashMap<Service, Instant>,
    looking_up: bool,
    lookup_failed: Option<Instant>,
    no_mal: HashSet<u64>,
    pub saving: HashSet<(Service, u64)>,
    /// Episodes finished in the player, waiting for the lists to be read.
    pub to_sync: BTreeMap<u64, u32>,
}

impl Lists {
    pub fn load(state_dir: &Path, http: reqwest::Client) -> Self {
        let file = state_dir.join("tracking.json");
        Self {
            config: Config::load(&file),
            file,
            endpoints: Endpoints::default(),
            http,
            anilist: None,
            mal: None,
            lists: HashMap::new(),
            fetching: HashSet::new(),
            fetched: HashMap::new(),
            looking_up: false,
            lookup_failed: None,
            no_mal: HashSet::new(),
            saving: HashSet::new(),
            to_sync: BTreeMap::new(),
        }
    }

    fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("couldn't save {}: {e:#}", self.file.display());
        }
    }

    pub fn services(&self) -> Vec<Service> {
        [Service::AniList, Service::Mal]
            .into_iter()
            .filter(|s| self.config.signed_in(*s))
            .collect()
    }

    fn key(&self, service: Service, id: u64) -> Option<u64> {
        match service {
            Service::AniList => Some(id),
            Service::Mal => self.config.mal_ids.get(&id).copied(),
        }
    }

    pub fn entry(&self, service: Service, id: u64) -> Option<&Entry> {
        self.lists.get(&service)?.get(&self.key(service, id)?)
    }

    /// The entries to show, by AniList ID: AniList's, else MyAnimeList's.
    pub fn shown(&self, library: &Library) -> HashMap<u64, Entry> {
        let services = self.services();
        library
            .series
            .iter()
            .filter_map(|s| {
                services
                    .iter()
                    .find_map(|sv| self.entry(*sv, s.anilist_id))
                    .map(|e| (s.anilist_id, e.clone()))
            })
            .collect()
    }

    /// Still finding out what the lists say about it.
    fn pending(&self, id: u64) -> bool {
        self.services().iter().any(|s| !self.lists.contains_key(s))
            || (self.config.signed_in(Service::Mal)
                && self.key(Service::Mal, id).is_none()
                && !self.no_mal.contains(&id))
    }

    /// Now and then: sessions, the lists, MyAnimeList IDs, and finished
    /// episodes once the lists are in.
    pub fn tick(
        &mut self,
        library: &Library,
        episodes: impl Fn(u64) -> Option<u32>,
    ) -> Task<Message> {
        if self.anilist.is_none()
            && let (Some(token), Some(user)) =
                (&self.config.anilist.token, &self.config.anilist.user)
        {
            let s = anilist::Session::new(
                self.http.clone(),
                self.endpoints.anilist_api.clone(),
                token.clone(),
                user.id,
            );
            self.anilist = Some(Arc::new(s));
        }
        if self.mal.is_none()
            && let Some(tokens) = &self.config.mal.tokens
        {
            let m = &self.config.mal;
            let s = mal::Session::new(
                self.http.clone(),
                self.endpoints.clone(),
                m.client_id.trim().to_owned(),
                m.client_secret.trim().to_owned(),
                tokens.clone(),
            );
            self.mal = Some(Arc::new(s));
        }
        if let Some(tokens) = self.mal.as_ref().and_then(|s| s.take_refreshed()) {
            self.config.mal.tokens = Some(tokens);
            self.save();
        }
        let mut tasks = Vec::new();
        for service in self.services() {
            if self.fetching.contains(&service)
                || self
                    .fetched
                    .get(&service)
                    .is_some_and(|at| at.elapsed() < REFRESH)
            {
                continue;
            }
            self.fetched.insert(service, Instant::now());
            self.fetching.insert(service);
            tasks.push(match service {
                Service::AniList => {
                    let s = self.anilist.clone().expect("signed in");
                    Task::perform(
                        async move { s.list().await.map_err(Failure::from) },
                        move |r| Message::Lists(Msg::Listed(Service::AniList, r)),
                    )
                }
                Service::Mal => {
                    let s = self.mal.clone().expect("signed in");
                    Task::perform(
                        async move { s.list().await.map_err(Failure::from) },
                        move |r| Message::Lists(Msg::Listed(Service::Mal, r)),
                    )
                }
            });
        }
        if self.config.signed_in(Service::Mal)
            && !self.looking_up
            && self
                .lookup_failed
                .is_none_or(|at| at.elapsed() > Duration::from_secs(60))
        {
            let want: Vec<u64> = library
                .series
                .iter()
                .map(|s| s.anilist_id)
                .filter(|id| !self.config.mal_ids.contains_key(id) && !self.no_mal.contains(id))
                .collect();
            if !want.is_empty() {
                self.looking_up = true;
                let (http, api, asked) = (
                    self.http.clone(),
                    self.endpoints.anilist_api.clone(),
                    want.clone(),
                );
                tasks.push(Task::perform(
                    async move {
                        anilist::mal_ids(&http, &api, &want)
                            .await
                            .map_err(|e| format!("{e:#}"))
                    },
                    move |r| Message::Lists(Msg::MalIds(asked.clone(), r)),
                ));
            }
        }
        let ready: Vec<(u64, u32)> = self
            .to_sync
            .iter()
            .filter(|(id, _)| !self.pending(**id))
            .map(|(id, e)| (*id, *e))
            .collect();
        for (id, episode) in ready {
            self.to_sync.remove(&id);
            let (task, saved) = self.change(id, Change::Watched(episode), episodes(id));
            tasks.push(task);
            if !saved.is_empty() {
                let names: Vec<&str> = saved.iter().map(|s| s.name()).collect();
                let note = format!("Episode {episode} watched: updated {}", names.join(" and "));
                tasks.push(Task::done(Message::Note(note, false)));
            }
        }
        Task::batch(tasks)
    }

    /// An episode was finished in the player.
    pub fn watched(&mut self, id: u64, episode: u32, episodes: Option<u32>) {
        if !self.config.sync_watched
            || self.services().is_empty()
            || episodes.is_some_and(|n| n > 0 && episode > n)
        {
            return;
        }
        let e = self.to_sync.entry(id).or_insert(episode);
        *e = (*e).max(episode);
    }

    /// Change an entry on every list (each keeps what isn't changed);
    /// shown at once, put back if saving fails.
    pub fn change(
        &mut self,
        id: u64,
        change: Change,
        episodes: Option<u32>,
    ) -> (Task<Message>, Vec<Service>) {
        let mut tasks = Vec::new();
        let mut saved = Vec::new();
        for service in self.services() {
            let Some(key) = self.key(service, id) else {
                continue;
            };
            let before = self.entry(service, id).cloned();
            let after = change.apply(before.as_ref(), episodes);
            if before.as_ref() == Some(&after) {
                continue;
            }
            self.lists
                .entry(service)
                .or_default()
                .insert(key, after.clone());
            self.saving.insert((service, key));
            saved.push(service);
            let to_msg = move |result| {
                Message::Lists(Msg::Saved {
                    service,
                    key,
                    before: before.clone(),
                    result,
                })
            };
            tasks.push(match service {
                Service::AniList => {
                    let Some(s) = self.anilist.clone() else {
                        continue;
                    };
                    Task::perform(
                        async move { s.save(key, &after).await.map_err(Failure::from) },
                        to_msg,
                    )
                }
                Service::Mal => {
                    let Some(s) = self.mal.clone() else { continue };
                    Task::perform(
                        async move { s.save(key, &after).await.map_err(Failure::from) },
                        to_msg,
                    )
                }
            });
        }
        (Task::batch(tasks), saved)
    }

    /// News from the sites; an error to show, if any.
    pub fn handle(&mut self, msg: Msg) -> Option<String> {
        match msg {
            Msg::Listed(service, result) => {
                self.fetching.remove(&service);
                match result {
                    Ok(list) => {
                        self.lists.insert(service, list);
                        None
                    }
                    Err(f) => Some(self.failed(service, f)),
                }
            }
            Msg::MalIds(asked, result) => {
                self.looking_up = false;
                match result {
                    Ok(found) => {
                        for id in asked {
                            match found.get(&id) {
                                Some(m) => {
                                    self.config.mal_ids.insert(id, *m);
                                }
                                None => {
                                    self.no_mal.insert(id);
                                }
                            }
                        }
                        self.save();
                        None
                    }
                    Err(e) => {
                        self.lookup_failed = Some(Instant::now());
                        tracing::warn!("looking up MyAnimeList IDs: {e}");
                        None
                    }
                }
            }
            Msg::Saved {
                service,
                key,
                before,
                result,
            } => {
                self.saving.remove(&(service, key));
                match result {
                    Ok(entry) => {
                        self.lists.entry(service).or_default().insert(key, entry);
                        None
                    }
                    Err(f) => {
                        let list = self.lists.entry(service).or_default();
                        match before {
                            Some(b) => list.insert(key, b),
                            None => list.remove(&key),
                        };
                        Some(self.failed(service, f))
                    }
                }
            }
        }
    }

    fn failed(&mut self, service: Service, f: Failure) -> String {
        if f.signed_out {
            match service {
                Service::AniList => {
                    self.config.anilist.token = None;
                    self.config.anilist.user = None;
                    self.anilist = None;
                }
                Service::Mal => {
                    self.config.mal.tokens = None;
                    self.config.mal.user = None;
                    self.mal = None;
                }
            }
            self.lists.remove(&service);
            self.save();
            format!("{} signed you out; sign in again", service.name())
        } else {
            format!("{}: {}", service.name(), f.message)
        }
    }
}
