//! Signing in to AniList and MyAnimeList, and changing your list entries
//! (status, score, episodes watched) from the library: on each series card
//! and each series page. A change goes to every list you're signed in to.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};

use super::{Action, App, Msg, Page};
use crate::tracking::{
    self, Change, Config, Endpoints, Entry, Service, SignedOut, Status, anilist, mal,
};

/// How often the lists are read again (changes made on the sites show up).
const REFRESH: Duration = Duration::from_secs(15 * 60);

pub(super) struct SignIn {
    service: Service,
    seq: u64,
    cancel: Arc<AtomicBool>,
    pub(super) url: Option<String>,
}

pub(super) struct TrackingState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) endpoints: Endpoints,
    anilist: Option<Arc<anilist::Session>>,
    mal: Option<Arc<mal::Session>>,
    /// The lists: AniList's by AniList ID, MyAnimeList's by its own.
    pub(super) lists: HashMap<Service, HashMap<u64, Entry>>,
    fetching: HashSet<Service>,
    fetched: HashMap<Service, Instant>,
    /// AniList IDs being looked up for MyAnimeList's, and ones it hasn't got.
    looking_up: HashSet<u64>,
    no_mal: HashSet<u64>,
    lookup_failed: Option<Instant>,
    pub(super) saving: HashSet<(Service, u64)>,
    pub(super) errors: HashMap<Service, String>,
    pub(super) signing_in: Option<SignIn>,
    seq: u64,
    pub(super) dialog: bool,
}

pub(super) enum Account {
    AniList(anilist::SignedIn),
    Mal(mal::Tokens, String),
}

pub(super) struct Failure {
    message: String,
    signed_out: bool,
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure {
            signed_out: e.is::<SignedOut>(),
            message: format!("{e:#}"),
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
    OpenDialog,
    CloseDialog,
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
            looking_up: HashSet::new(),
            no_mal: HashSet::new(),
            lookup_failed: None,
            saving: HashSet::new(),
            errors: HashMap::new(),
            signing_in: None,
            seq: 0,
            dialog: false,
        }
    }

    fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("couldn't save {}: {e:#}", self.file.display());
        }
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
    fn pending(&self, anilist_id: u64) -> bool {
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
            let m = &t.config.mal;
            let session = mal::Session::new(
                self.http.clone(),
                t.endpoints.clone(),
                m.client_id.trim().to_owned(),
                m.client_secret.trim().to_owned(),
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
                !t.fetching.contains(s) && t.fetched.get(s).is_none_or(|at| at.elapsed() > REFRESH)
            })
            .collect();
        for service in due {
            self.fetch_list(service);
        }
        if self.tracking.signed_in(Service::Mal) {
            self.look_up_mal_ids();
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
                    self.ctx.open_url(egui::OpenUrl::new_tab(url));
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
                    }
                    Err(f) => self.tracking_failed(service, f),
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
            self.tracking.errors.insert(service, f.message.clone());
            self.set_error(format!("{}: {}", service.name(), f.message));
        }
    }

    pub(super) fn tracking_action(&mut self, action: TrackingAction) {
        match action {
            TrackingAction::OpenDialog => self.tracking.dialog = true,
            TrackingAction::CloseDialog => {
                self.tracking.dialog = false;
                self.tracking.save();
            }
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
            TrackingAction::Change { id, change } => self.change_list_entry(id, change),
            TrackingAction::Refresh => self.tracking.fetched.clear(),
        }
    }

    fn tracking_sign_in(&mut self, service: Service) {
        if self.tracking.signing_in.is_some() {
            return;
        }
        let (client_id, secret) = match service {
            Service::AniList => (
                self.tracking.config.anilist.client_id.trim().to_owned(),
                String::new(),
            ),
            Service::Mal => (
                self.tracking.config.mal.client_id.trim().to_owned(),
                self.tracking.config.mal.client_secret.trim().to_owned(),
            ),
        };
        if client_id.is_empty() {
            self.set_error(format!("Paste the {} client ID first", service.name()));
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
                Service::AniList => anilist::sign_in(&http, &ep, &client_id, &cancel, open)
                    .await
                    .map(Account::AniList),
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
    fn episode_count(&self, id: u64) -> Option<u32> {
        let shown = match &self.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.episodes,
            _ => None,
        };
        self.details(id)
            .and_then(|d| d.episodes)
            .or_else(|| self.library.find(id).and_then(|s| s.episodes))
            .or(shown)
    }

    /// Change an entry on every list signed in to (each keeps its other
    /// fields); shown at once, put back if saving fails.
    fn change_list_entry(&mut self, id: u64, change: Change) {
        let episodes = self.episode_count(id);
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
    }

    // ── Drawing ───────────────────────────────────────────────────────

    /// The accounts, in the settings menu.
    pub(super) fn tracking_settings(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.strong("Anime lists");
        for service in [Service::AniList, Service::Mal] {
            let who = match service {
                Service::AniList => self
                    .tracking
                    .config
                    .anilist
                    .user
                    .as_ref()
                    .map(|u| u.name.clone()),
                Service::Mal => self.tracking.config.mal.user.clone(),
            };
            match who.filter(|_| self.tracking.signed_in(service)) {
                Some(name) => ui.label(format!("{}: signed in as {name}", service.name())),
                None => ui.weak(format!("{}: not signed in", service.name())),
            };
        }
        if ui.button("AniList & MyAnimeList…").clicked() {
            actions.push(Action::Tracking(TrackingAction::OpenDialog));
            ui.close();
        }
        ui.weak("Change status, score and episodes watched from the library.");
    }

    /// Signing in and out.
    pub(super) fn tracking_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.tracking.dialog {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("tracking_accounts")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("AniList & MyAnimeList");
            ui.label("Sign in to change your lists from the library. Changes go to every list you're signed in to.");
            for service in [Service::AniList, Service::Mal] {
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.account_box(ui, service, actions);
                });
            }
            ui.add_space(8.0);
            if ui.button("Close").clicked() {
                actions.push(Action::Tracking(TrackingAction::CloseDialog));
            }
        });
        if modal.should_close() {
            actions.push(Action::Tracking(TrackingAction::CloseDialog));
        }
    }

    fn account_box(&mut self, ui: &mut Ui, service: Service, actions: &mut Vec<Action>) {
        let t = &mut self.tracking;
        ui.strong(service.name());
        if let Some(e) = t.errors.get(&service) {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if t.signed_in(service) {
            let who = match service {
                Service::AniList => t.config.anilist.user.as_ref().map(|u| u.name.clone()),
                Service::Mal => t.config.mal.user.clone(),
            }
            .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(format!("Signed in as {who}"));
                if ui.button("Sign out").clicked() {
                    actions.push(Action::Tracking(TrackingAction::SignOut(service)));
                }
            });
            return;
        }
        if let Some(job) = t.signing_in.as_ref().filter(|j| j.service == service) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Finish signing in in your browser…");
                if ui.button("Cancel").clicked() {
                    actions.push(Action::Tracking(TrackingAction::CancelSignIn));
                }
            });
            if let Some(url) = &job.url {
                ui.hyperlink_to("Open the sign-in page again ↗", url);
            }
            return;
        }
        let redirect = tracking::redirect_url(service);
        let (site, site_url, kind) = match service {
            Service::AniList => (
                "anilist.co/settings/developer",
                "https://anilist.co/settings/developer",
                "any name",
            ),
            Service::Mal => (
                "myanimelist.net/apiconfig",
                "https://myanimelist.net/apiconfig",
                "App Type \"other\", any name and description",
            ),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("1. Make an API client at");
            ui.hyperlink_to(format!("{site} ↗"), site_url);
            ui.label(format!("({kind})."));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("2. Give it the redirect URL");
            ui.code(&redirect);
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(redirect.clone());
            }
        });
        ui.horizontal(|ui| {
            ui.label("3. Its client ID:");
            let id = match service {
                Service::AniList => &mut t.config.anilist.client_id,
                Service::Mal => &mut t.config.mal.client_id,
            };
            ui.add(
                egui::TextEdit::singleline(id)
                    .desired_width(300.0)
                    .hint_text("Client ID"),
            );
        });
        if service == Service::Mal {
            ui.horizontal(|ui| {
                ui.label("    and its client secret, if it shows one:");
                ui.add(
                    egui::TextEdit::singleline(&mut t.config.mal.client_secret)
                        .password(true)
                        .desired_width(220.0)
                        .hint_text("Client secret"),
                );
            });
        }
        let busy = t.signing_in.is_some();
        let label = format!("Sign in with {}", service.name());
        if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
            actions.push(Action::Tracking(TrackingAction::SignIn(service)));
        }
    }

    /// Status, score and episodes watched, on a series page.
    pub(super) fn lists_section(&self, ui: &mut Ui, id: u64, actions: &mut Vec<Action>) {
        let t = &self.tracking;
        if !t.any() {
            return;
        }
        let episodes = self.episode_count(id);
        let shown = t.shown(id).cloned();
        let pending = t.pending(id);
        let change = |actions: &mut Vec<Action>, change: Change| {
            actions.push(Action::Tracking(TrackingAction::Change { id, change }))
        };
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong("Your lists");
                ui.add_space(6.0);
                ui.add_enabled_ui(!pending, |ui| {
                    let label = shown.as_ref().map_or("Add to list", |e| e.status.name());
                    egui::ComboBox::from_id_salt(("list-status", id))
                        .selected_text(label)
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for s in Status::ALL {
                                if ui
                                    .selectable_label(
                                        shown.as_ref().is_some_and(|e| e.status == s),
                                        s.name(),
                                    )
                                    .clicked()
                                {
                                    change(actions, Change::Status(s));
                                }
                            }
                        });
                    let Some(e) = &shown else { return };
                    ui.add_space(8.0);
                    ui.label("Score");
                    egui::ComboBox::from_id_salt(("list-score", id))
                        .selected_text(format!("★ {}", e.score_text()))
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            score_options(ui, e.score, |score| {
                                change(actions, Change::Score(score))
                            });
                        });
                    ui.add_space(8.0);
                    ui.label("Episodes");
                    let key = egui::Id::unique(("list-progress", id));
                    let mut value: u32 = ui.data(|d| d.get_temp(key)).unwrap_or(e.progress);
                    let max = episodes.filter(|n| *n > 0).unwrap_or(9999);
                    let r = ui.add(egui::DragValue::new(&mut value).range(0..=max).speed(0.1));
                    if (r.drag_stopped()
                        || r.lost_focus()
                        || (r.changed() && !r.dragged() && !r.has_focus()))
                        && value != e.progress
                    {
                        change(actions, Change::Progress(value));
                    }
                    if r.dragged() || r.has_focus() {
                        ui.data_mut(|d| d.insert_temp(key, value));
                    } else {
                        ui.data_mut(|d| d.remove::<u32>(key));
                    }
                    if let Some(n) = episodes {
                        ui.label(format!("/ {n}"));
                    }
                    if ui
                        .add_enabled(e.progress < max, egui::Button::new("+1").small())
                        .on_hover_text("One more episode watched")
                        .clicked()
                    {
                        change(actions, Change::Progress(e.progress + 1));
                    }
                });
                if pending {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("⟳")
                        .on_hover_text("Read your lists again")
                        .clicked()
                    {
                        actions.push(Action::Tracking(TrackingAction::Refresh));
                    }
                });
            });
            // What each list says.
            ui.horizontal_wrapped(|ui| {
                for service in t.services() {
                    let text = match (t.entry(service, id), t.key(service, id)) {
                        _ if !t.lists.contains_key(&service) => "reading your list…".to_owned(),
                        (Some(e), _) => entry_text(e, episodes),
                        (None, None) if t.no_mal.contains(&id) => "not on MyAnimeList".to_owned(),
                        (None, None) => "finding it…".to_owned(),
                        (None, Some(_)) => "not on your list".to_owned(),
                    };
                    let saving = t
                        .key(service, id)
                        .is_some_and(|k| t.saving.contains(&(service, k)));
                    ui.weak(format!("{}: {text}", service.name()));
                    if saving {
                        ui.spinner();
                    }
                    ui.add_space(12.0);
                }
                let entries: Vec<&Entry> = t
                    .services()
                    .into_iter()
                    .filter_map(|s| t.entry(s, id))
                    .collect();
                if entries.len() == 2 && !same(entries[0], entries[1]) {
                    ui.colored_label(ui.visuals().warn_fg_color, "⚠ Not the same on both")
                        .on_hover_text(
                            "A change here goes to both (each keeps what you don't change).",
                        );
                }
            });
        });
    }

    /// Status and score under a library card.
    pub(super) fn list_chips(&self, ui: &mut Ui, id: u64, actions: &mut Vec<Action>) {
        let t = &self.tracking;
        if !t.any() {
            return;
        }
        let shown = t.shown(id).cloned();
        let episodes = self.episode_count(id);
        let pending = t.pending(id);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_enabled_ui(!pending, |ui| {
                let status = shown
                    .as_ref()
                    .map_or("+ Add to list".to_owned(), |e| e.status.name().to_owned());
                ui.menu_button(RichText::new(status).small(), |ui| {
                    for s in Status::ALL {
                        if ui
                            .selectable_label(
                                shown.as_ref().is_some_and(|e| e.status == s),
                                s.name(),
                            )
                            .clicked()
                        {
                            actions.push(Action::Tracking(TrackingAction::Change {
                                id,
                                change: Change::Status(s),
                            }));
                        }
                    }
                });
                if let Some(e) = &shown {
                    ui.menu_button(
                        RichText::new(format!("★ {}", e.score_text())).small(),
                        |ui| {
                            score_options(ui, e.score, |score| {
                                actions.push(Action::Tracking(TrackingAction::Change {
                                    id,
                                    change: Change::Score(score),
                                }));
                            });
                        },
                    );
                    let progress = match episodes {
                        Some(n) => format!("{}/{n}", e.progress),
                        None => e.progress.to_string(),
                    };
                    ui.label(RichText::new(progress).small().weak())
                        .on_hover_text("Episodes watched");
                }
            });
        });
    }
}

fn score_options(ui: &mut Ui, current: f32, mut pick: impl FnMut(f32)) {
    if ui.selectable_label(current <= 0.0, "No score").clicked() {
        pick(0.0);
    }
    for n in (1..=10u32).rev() {
        let on = (current - n as f32).abs() < 0.05;
        if ui
            .selectable_label(on, format!("{n} · {}", tracking::score_word(n)))
            .clicked()
        {
            pick(n as f32);
        }
    }
}

fn entry_text(e: &Entry, episodes: Option<u32>) -> String {
    let progress = match episodes {
        Some(n) => format!("{}/{n}", e.progress),
        None => format!("{} watched", e.progress),
    };
    format!("{} · ★ {} · {progress}", e.status.name(), e.score_text())
}

/// The same entry, as far as each site can say (MyAnimeList's scores are
/// whole numbers).
fn same(a: &Entry, b: &Entry) -> bool {
    a.status == b.status
        && a.progress == b.progress
        && (a.score.round() - b.score.round()).abs() < 0.5
}
