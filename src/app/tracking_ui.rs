//! Signing in to AniList and MyAnimeList, and changing your list entries
//! (status, score, episodes watched) from the library: on each series card
//! and each series page. A change goes to every list you're signed in to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{App, Msg, Page};
use crate::tracking::{Change, Config, Endpoints, Entry, Service, SignedOut, anilist, mal};

/// How often the lists are read again (changes made on the sites show up).
const REFRESH: Duration = Duration::from_secs(15 * 60);
/// How soon a list that couldn't be read is tried again.
const LIST_RETRY: Duration = Duration::from_secs(60);

pub(super) struct SignIn {
    pub(super) service: Service,
    seq: u64,
    cancel: Arc<AtomicBool>,
    pub(super) url: Option<String>,
}

pub(super) struct TrackingState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) endpoints: Endpoints,
    pub(super) anilist: Option<Arc<anilist::Session>>,
    mal: Option<Arc<mal::Session>>,
    /// The lists: AniList's by AniList ID, MyAnimeList's by its own.
    pub(super) lists: HashMap<Service, HashMap<u64, Entry>>,
    fetching: HashSet<Service>,
    fetched: HashMap<Service, Instant>,
    /// Lists that couldn't be read: when to try again.
    retry_at: HashMap<Service, Instant>,
    /// AniList IDs being looked up for MyAnimeList's, and ones it hasn't got.
    looking_up: HashSet<u64>,
    pub(super) no_mal: HashSet<u64>,
    lookup_failed: Option<Instant>,
    pub(super) saving: HashSet<(Service, u64)>,
    pub(super) errors: HashMap<Service, String>,
    pub(super) signing_in: Option<SignIn>,
    seq: u64,
    /// Episodes finished in the player, to put on the lists once they're
    /// read (by AniList ID: the latest episode).
    pub(super) to_sync: BTreeMap<u64, u32>,
    /// The API clients this build carries (`crate::tracking::built_in`).
    pub(crate) built_in: HashMap<Service, crate::tracking::Client>,
    /// Services whose own-client form is open, though one's built in.
    pub(crate) own_client: HashSet<Service>,
}

pub(super) enum Account {
    AniList(anilist::SignedIn),
    Mal(mal::Tokens, String),
}

pub(super) struct Failure {
    message: String,
    /// What went wrong at bottom ("connection aborted").
    cause: String,
    signed_out: bool,
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure {
            signed_out: e.is::<SignedOut>(),
            message: format!("{e:#}"),
            cause: e.root_cause().to_string(),
        }
    }
}

pub(super) enum TrackingMsg {
    SignInUrl {
        seq: u64,
        url: String,
    },
    SignedIn {
        seq: u64,
        result: Result<Account, String>,
    },
    Listed {
        service: Service,
        result: Result<HashMap<u64, Entry>, Failure>,
    },
    Saved {
        service: Service,
        key: u64,
        before: Option<Entry>,
        result: Result<Entry, Failure>,
    },
    MalIds {
        asked: Vec<u64>,
        result: Result<HashMap<u64, u64>, String>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum TrackingAction {
    SignIn(Service),
    CancelSignIn,
    SignOut(Service),
    Change { id: u64, change: Change },
    Refresh,
}

impl TrackingState {
    pub(super) fn load(state_dir: &Path) -> Self {
        let file = state_dir.join("tracking.json");
        Self {
            config: Config::load(&file),
            file,
            endpoints: Endpoints::default(),
            anilist: None,
            mal: None,
            lists: HashMap::new(),
            fetching: HashSet::new(),
            fetched: HashMap::new(),
            retry_at: HashMap::new(),
            looking_up: HashSet::new(),
            no_mal: HashSet::new(),
            lookup_failed: None,
            saving: HashSet::new(),
            errors: HashMap::new(),
            signing_in: None,
            seq: 0,
            to_sync: BTreeMap::new(),
            built_in: [Service::AniList, Service::Mal]
                .into_iter()
                .filter_map(|s| Some((s, crate::tracking::built_in(s)?)))
                .collect(),
            own_client: HashSet::new(),
        }
    }

    /// The API client to sign in (and stay signed in) with: yours, else the
    /// one built in.
    pub(crate) fn client(&self, service: Service) -> Option<crate::tracking::Client> {
        let (id, secret) = match service {
            Service::AniList => (
                &self.config.anilist.client_id,
                &self.config.anilist.client_secret,
            ),
            Service::Mal => (&self.config.mal.client_id, &self.config.mal.client_secret),
        };
        crate::tracking::client(id, secret, self.built_in.get(&service))
    }

    /// Back to the build's own API client: yours forgotten.
    pub(crate) fn use_built_in(&mut self, service: Service) {
        self.own_client.remove(&service);
        let c = &mut self.config;
        match service {
            Service::AniList => (c.anilist.client_id.clear(), c.anilist.client_secret.clear()),
            Service::Mal => (c.mal.client_id.clear(), c.mal.client_secret.clear()),
        };
        self.save();
    }

    /// Signing in is one button: a client's built in, and you haven't given
    /// (or asked to give) your own.
    pub(crate) fn just_sign_in(&self, service: Service) -> bool {
        let own = match service {
            Service::AniList => &self.config.anilist.client_id,
            Service::Mal => &self.config.mal.client_id,
        };
        self.built_in.contains_key(&service)
            && own.trim().is_empty()
            && !self.own_client.contains(&service)
    }

    pub(super) fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("couldn't save {}: {e:#}", self.file.display());
        }
    }

    /// Signed in to AniList as "jake", with `list` as the list (tests and
    /// screenshots: nothing is fetched).
    #[cfg(test)]
    pub(super) fn pretend_lists(&mut self, list: HashMap<u64, Entry>) {
        self.config.anilist.token = Some("token".into());
        self.config.anilist.user = Some(crate::tracking::anilist::User {
            id: 1,
            name: "jake".into(),
        });
        self.endpoints.anilist_api = "http://127.0.0.1:9".into();
        self.fetched.insert(Service::AniList, Instant::now());
        self.lists.insert(Service::AniList, list);
    }

    pub(super) fn signed_in(&self, service: Service) -> bool {
        self.config.signed_in(service)
    }

    pub(super) fn any(&self) -> bool {
        !self.services().is_empty()
    }

    /// The services signed in to, AniList first.
    pub(super) fn services(&self) -> Vec<Service> {
        [Service::AniList, Service::Mal]
            .into_iter()
            .filter(|s| self.signed_in(*s))
            .collect()
    }

    /// The ID a service knows an anime by.
    pub(super) fn key(&self, service: Service, anilist_id: u64) -> Option<u64> {
        match service {
            Service::AniList => Some(anilist_id),
            Service::Mal => self.config.mal_ids.get(&anilist_id).copied(),
        }
    }

    pub(super) fn entry(&self, service: Service, anilist_id: u64) -> Option<&Entry> {
        self.lists
            .get(&service)?
            .get(&self.key(service, anilist_id)?)
    }

    /// The entry to show: AniList's when you're signed in to it, else
    /// MyAnimeList's.
    pub(super) fn shown(&self, anilist_id: u64) -> Option<&Entry> {
        self.services()
            .into_iter()
            .find_map(|s| self.entry(s, anilist_id))
    }

    /// Still working out where the anime is (lists loading, or its
    /// MyAnimeList ID being looked up).
    pub(super) fn pending(&self, anilist_id: u64) -> bool {
        self.services().iter().any(|s| !self.lists.contains_key(s))
            || (self.signed_in(Service::Mal)
                && self.key(Service::Mal, anilist_id).is_none()
                && !self.no_mal.contains(&anilist_id))
    }

    fn sign_out_locally(&mut self, service: Service) {
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
        self.fetched.remove(&service);
        self.save();
    }
}

impl App {
    /// Every frame: sessions for the accounts, the lists now and then, and
    /// MyAnimeList IDs for the library's series.
    pub(super) fn tracking_tick(&mut self) {
        let t = &mut self.tracking;
        if t.anilist.is_none()
            && let (Some(token), Some(user)) = (&t.config.anilist.token, &t.config.anilist.user)
        {
            let session = anilist::Session::new(
                self.http.clone(),
                t.endpoints.anilist_api.clone(),
                token.clone(),
                user.id,
            );
            t.anilist = Some(Arc::new(session));
        }
        if t.mal.is_none()
            && let Some(tokens) = &t.config.mal.tokens
        {
            let (client_id, secret) = t.client(Service::Mal).unwrap_or_default();
            let session = mal::Session::new(
                self.http.clone(),
                t.endpoints.clone(),
                client_id,
                secret,
                tokens.clone(),
            );
            t.mal = Some(Arc::new(session));
        }
        if let Some(tokens) = t.mal.as_ref().and_then(|s| s.take_refreshed()) {
            t.config.mal.tokens = Some(tokens);
            t.save();
        }
        let due: Vec<Service> = t
            .services()
            .into_iter()
            .filter(|s| {
                !t.fetching.contains(s)
                    && t.fetched.get(s).is_none_or(|at| at.elapsed() > REFRESH)
                    && t.retry_at.get(s).is_none_or(|at| Instant::now() >= *at)
            })
            .collect();
        for service in due {
            self.fetch_list(service);
        }
        if self.tracking.signed_in(Service::Mal) {
            self.look_up_mal_ids();
        }
        self.sync_watched_now();
    }

    /// An episode was finished in the player: put it on the lists.
    pub(super) fn sync_watched(&mut self, id: u64, episode: u32) {
        if !self.tracking.config.sync_watched || !self.tracking.any() {
            return;
        }
        // Numbered past the end (counted from the show's start, say): which
        // episode it is here isn't clear, so leave the lists be.
        if self.episode_count(id).is_some_and(|n| n > 0 && episode > n) {
            return;
        }
        let e = self.tracking.to_sync.entry(id).or_insert(episode);
        *e = (*e).max(episode);
    }

    /// Sync finished episodes once the lists (and MyAnimeList IDs) are in:
    /// saving before then could overwrite what's on them.
    fn sync_watched_now(&mut self) {
        let ready: Vec<(u64, u32)> = self
            .tracking
            .to_sync
            .iter()
            .filter(|(id, _)| !self.tracking.pending(**id))
            .map(|(id, ep)| (*id, *ep))
            .collect();
        for (id, episode) in ready {
            self.tracking.to_sync.remove(&id);
            let saved = self.change_list_entry(id, Change::Watched(episode));
            if !saved.is_empty() {
                let names: Vec<&str> = saved.iter().map(|s| s.name()).collect();
                self.set_info(format!(
                    "Episode {episode} watched: updated {}",
                    names.join(" and ")
                ));
            }
        }
    }

    fn fetch_list(&mut self, service: Service) {
        let t = &mut self.tracking;
        t.fetched.insert(service, Instant::now());
        match service {
            Service::AniList => {
                let Some(s) = t.anilist.clone() else { return };
                t.fetching.insert(service);
                self.spawn(async move {
                    let result = s.list().await.map_err(Failure::from);
                    Msg::Tracking(TrackingMsg::Listed { service, result })
                });
            }
            Service::Mal => {
                let Some(s) = t.mal.clone() else { return };
                t.fetching.insert(service);
                self.spawn(async move {
                    let result = s.list().await.map_err(Failure::from);
                    Msg::Tracking(TrackingMsg::Listed { service, result })
                });
            }
        }
    }

    fn look_up_mal_ids(&mut self) {
        let t = &self.tracking;
        if !t.looking_up.is_empty()
            || t.lookup_failed
                .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        let mut want: Vec<u64> = self.library.series.iter().map(|s| s.anilist_id).collect();
        if let Page::Anime(ap) = &self.page {
            want.push(ap.anime.anilist_id);
        }
        want.retain(|id| !t.config.mal_ids.contains_key(id) && !t.no_mal.contains(id));
        want.sort_unstable();
        want.dedup();
        if want.is_empty() {
            return;
        }
        self.tracking.looking_up.extend(&want);
        let (http, api) = (
            self.http.clone(),
            self.tracking.endpoints.anilist_api.clone(),
        );
        self.spawn(async move {
            let result = anilist::mal_ids(&http, &api, &want)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::Tracking(TrackingMsg::MalIds {
                asked: want,
                result,
            })
        });
    }

    pub(super) fn handle_tracking_msg(&mut self, msg: TrackingMsg) {
        match msg {
            TrackingMsg::SignInUrl { seq, url } => {
                if let Some(job) = &mut self.tracking.signing_in
                    && job.seq == seq
                {
                    job.url = Some(url.clone());
                    self.ctx.open_url(&url);
                }
            }
            TrackingMsg::SignedIn { seq, result } => {
                if self
                    .tracking
                    .signing_in
                    .as_ref()
                    .is_none_or(|j| j.seq != seq)
                {
                    return;
                }
                let Some(job) = self.tracking.signing_in.take() else {
                    return;
                };
                let t = &mut self.tracking;
                match result {
                    Ok(Account::AniList(s)) => {
                        let name = s.user.name.clone();
                        t.config.anilist.token = Some(s.token);
                        t.config.anilist.expires_at = s.expires_at;
                        t.config.anilist.user = Some(s.user);
                        t.anilist = None;
                        t.fetched.remove(&Service::AniList);
                        t.errors.remove(&Service::AniList);
                        t.save();
                        self.set_info(format!("Signed in to AniList as {name}"));
                    }
                    Ok(Account::Mal(tokens, name)) => {
                        t.config.mal.tokens = Some(tokens);
                        t.config.mal.user = Some(name.clone());
                        t.mal = None;
                        t.fetched.remove(&Service::Mal);
                        t.errors.remove(&Service::Mal);
                        t.save();
                        self.set_info(format!("Signed in to MyAnimeList as {name}"));
                    }
                    Err(e) if job.cancel.load(Ordering::Relaxed) => {
                        tracing::debug!("sign-in cancelled: {e}")
                    }
                    Err(e) => {
                        self.tracking.errors.insert(job.service, e.clone());
                        self.set_error(format!("{} sign-in: {e}", job.service.name()));
                    }
                }
            }
            TrackingMsg::Listed { service, result } => {
                self.tracking.fetching.remove(&service);
                match result {
                    Ok(list) => {
                        self.tracking.lists.insert(service, list);
                        self.tracking.errors.remove(&service);
                        self.tracking.retry_at.remove(&service);
                    }
                    Err(f) if f.signed_out => self.tracking_failed(service, f),
                    Err(f) => {
                        // Again in a minute, not at the next refresh: the
                        // connection may have dropped (on a phone, while the
                        // app was in the background).
                        self.tracking.fetched.remove(&service);
                        self.tracking
                            .retry_at
                            .insert(service, Instant::now() + LIST_RETRY);
                        let message = format!(
                            "Couldn't read your list: {}. Trying again in a minute.",
                            f.cause
                        );
                        self.tracking_failed(service, Failure { message, ..f });
                    }
                }
            }
            TrackingMsg::Saved {
                service,
                key,
                before,
                result,
            } => {
                self.tracking.saving.remove(&(service, key));
                match result {
                    Ok(entry) => {
                        self.tracking
                            .lists
                            .entry(service)
                            .or_default()
                            .insert(key, entry);
                    }
                    Err(f) => {
                        let list = self.tracking.lists.entry(service).or_default();
                        match before {
                            Some(b) => list.insert(key, b),
                            None => list.remove(&key),
                        };
                        self.tracking_failed(service, f);
                    }
                }
            }
            TrackingMsg::MalIds { asked, result } => {
                let t = &mut self.tracking;
                for id in &asked {
                    t.looking_up.remove(id);
                }
                match result {
                    Ok(found) => {
                        for id in asked {
                            match found.get(&id) {
                                Some(mal) => {
                                    t.config.mal_ids.insert(id, *mal);
                                }
                                None => {
                                    t.no_mal.insert(id);
                                }
                            }
                        }
                        t.save();
                    }
                    Err(e) => {
                        t.lookup_failed = Some(Instant::now());
                        tracing::warn!("looking up MyAnimeList IDs: {e}");
                    }
                }
            }
        }
    }

    fn tracking_failed(&mut self, service: Service, f: Failure) {
        if f.signed_out {
            self.tracking.sign_out_locally(service);
            self.tracking
                .errors
                .insert(service, "Signed out: sign in again".into());
            self.set_error(format!(
                "{} signed you out; sign in again (⚙ Settings)",
                service.name()
            ));
        } else {
            // Said once (not again each time it's retried).
            let again = self.tracking.errors.get(&service) == Some(&f.message);
            self.tracking.errors.insert(service, f.message.clone());
            if !again {
                self.set_error(format!("{}: {}", service.name(), f.message));
            }
        }
    }

    pub(super) fn tracking_action(&mut self, action: TrackingAction) {
        match action {
            TrackingAction::SignIn(service) => self.tracking_sign_in(service),
            TrackingAction::CancelSignIn => {
                if let Some(job) = self.tracking.signing_in.take() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
            TrackingAction::SignOut(service) => {
                self.tracking.sign_out_locally(service);
                self.tracking.errors.remove(&service);
                self.set_info(format!("Signed out of {}", service.name()));
            }
            TrackingAction::Change { id, change } => {
                self.change_list_entry(id, change);
            }
            TrackingAction::Refresh => self.tracking.fetched.clear(),
        }
    }

    fn tracking_sign_in(&mut self, service: Service) {
        if self.tracking.signing_in.is_some() {
            return;
        }
        let (client_id, secret) = self.tracking.client(service).unwrap_or_default();
        if client_id.is_empty() {
            self.set_error(format!("Paste the {} client ID first", service.name()));
            return;
        }
        if service == Service::AniList && secret.is_empty() {
            self.set_error("Paste the AniList client secret too");
            return;
        }
        self.tracking.save();
        self.tracking.seq += 1;
        let seq = self.tracking.seq;
        let cancel = Arc::new(AtomicBool::new(false));
        self.tracking.signing_in = Some(SignIn {
            service,
            seq,
            cancel: cancel.clone(),
            url: None,
        });
        self.tracking.errors.remove(&service);
        let (tx, ctx, http, ep) = (
            self.tx.clone(),
            self.ctx.clone(),
            self.http.clone(),
            self.tracking.endpoints.clone(),
        );
        self.rt.spawn(async move {
            let (url_tx, url_ctx) = (tx.clone(), ctx.clone());
            let open = move |url: String| {
                let _ = url_tx.send(Msg::Tracking(TrackingMsg::SignInUrl { seq, url }));
                url_ctx.request_repaint();
            };
            let result = match service {
                Service::AniList => {
                    anilist::sign_in(&http, &ep, &client_id, &secret, &cancel, open)
                        .await
                        .map(Account::AniList)
                }
                Service::Mal => mal::sign_in(&http, &ep, &client_id, &secret, &cancel, open)
                    .await
                    .map(|(tokens, name)| Account::Mal(tokens, name)),
            };
            let result = result.map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Tracking(TrackingMsg::SignedIn { seq, result }));
            ctx.request_repaint();
        });
    }

    /// How many episodes an anime has, if known.
    pub(super) fn episode_count(&self, id: u64) -> Option<u32> {
        let shown = match &self.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.episodes,
            _ => None,
        };
        self.details(id)
            .and_then(|d| d.episodes)
            .or_else(|| self.library.find(id).and_then(|s| s.episodes))
            .or(shown)
    }

    /// Episodes watched as your list says (the one `shown`), for what plays
    /// next; none when the anime isn't on a list, or its episodes here are
    /// numbered past the list's count (from the show's start, say), so the
    /// numbers don't match.
    pub(super) fn list_progress(&self, id: u64) -> Option<u32> {
        let progress = self.tracking.shown(id)?.progress;
        let count = self.episode_count(id).filter(|n| *n > 0);
        if let Some(count) = count
            && self.on_disk(id).last().is_some_and(|n| *n > count)
        {
            return None;
        }
        Some(progress)
    }

    /// Episode `n` watched, as your lists say (their episodes watched reach
    /// it): its mark ("✓ AniList · MAL") and what that means. None when no
    /// list says so.
    pub(super) fn listed_watched(&self, id: u64, n: u32) -> Option<(String, String)> {
        let lists: Vec<Service> = self
            .tracking
            .services()
            .into_iter()
            .filter(|s| self.tracking.entry(*s, id).is_some_and(|e| e.progress >= n))
            .collect();
        if lists.is_empty() {
            return None;
        }
        let short: Vec<&str> = lists
            .iter()
            .map(|s| match s {
                Service::AniList => "AniList",
                Service::Mal => "MAL",
            })
            .collect();
        let long: Vec<&str> = lists.iter().map(|s| s.name()).collect();
        Some((
            format!("✓ {}", short.join(" · ")),
            if lists.len() == 1 {
                format!("Watched, as your {} list says", long[0])
            } else {
                format!("Watched, as your {} lists say", long.join(" and "))
            },
        ))
    }

    /// Change an entry on every list signed in to (each keeps its other
    /// fields); shown at once, put back if saving fails.
    fn change_list_entry(&mut self, id: u64, change: Change) -> Vec<Service> {
        let episodes = self.episode_count(id);
        let mut saved = Vec::new();
        for service in self.tracking.services() {
            let Some(key) = self.tracking.key(service, id) else {
                continue;
            };
            let before = self.tracking.entry(service, id).cloned();
            let after = change.apply(before.as_ref(), episodes);
            if before.as_ref() == Some(&after) {
                continue;
            }
            self.tracking
                .lists
                .entry(service)
                .or_default()
                .insert(key, after.clone());
            self.tracking.saving.insert((service, key));
            saved.push(service);
            match service {
                Service::AniList => {
                    let Some(s) = self.tracking.anilist.clone() else {
                        continue;
                    };
                    self.spawn(async move {
                        let result = s.save(key, &after).await.map_err(Failure::from);
                        Msg::Tracking(TrackingMsg::Saved {
                            service,
                            key,
                            before,
                            result,
                        })
                    });
                }
                Service::Mal => {
                    let Some(s) = self.tracking.mal.clone() else {
                        continue;
                    };
                    self.spawn(async move {
                        let result = s.save(key, &after).await.map_err(Failure::from);
                        Msg::Tracking(TrackingMsg::Saved {
                            service,
                            key,
                            before,
                            result,
                        })
                    });
                }
            }
        }
        saved
    }

    // ── Drawing ───────────────────────────────────────────────────────
}

/// The same entry, as far as each site can say (MyAnimeList's scores are
/// whole numbers).
pub(super) fn same(a: &Entry, b: &Entry) -> bool {
    a.status == b.status
        && a.progress == b.progress
        && (a.score.round() - b.score.round()).abs() < 0.5
}
