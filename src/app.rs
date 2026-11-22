//! The egui front end.
//!
//! The UI thread never blocks: network and torrent work runs on the tokio
//! runtime and reports back through `Msg`s. Clicks are collected as `Action`s
//! while drawing and handled after the frame is laid out.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use eframe::egui::{self, Align, Key, Layout, RichText, Sense, TextWrapMode, Ui, vec2};
use eframe::egui::containers::menu::{MenuButton, MenuConfig};
use egui_extras::{Column, TableBuilder};
use librqbit::TorrentStatsState;
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Listing, Source, TorrentHandle, TorrentId, info_hash_of, sanitize_folder_name};
use crate::fallback::{self, NyaaRelease, ReleaseKind, SearchPlan};
use crate::nyaa::{self, NyaaItem};
use crate::library::{AnimeRef, Library};
use crate::playlist::{Item, Playlist};
use crate::remux::Analysis;
use crate::seadex::{self, Anime, Release};
use crate::usenet::indexers::NzbLink;

mod debrid_ui;
mod drive_ui;
mod handover;
mod library_ui;
mod playlist_page;
mod remux_page;
mod usenet_ui;
mod vpn_ui;
use debrid_ui::DebridState;
use handover::Handover;
use library_ui::{LibraryState, PagesState};
use drive_ui::DriveState;
use playlist_page::PlaylistPage;
use remux_page::RemuxPage;
use usenet_ui::UsenetState;
use vpn_ui::VpnState;

/// How many Nyaa releases to list.
const MAX_NYAA_ROWS: usize = 40;

const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "m4v", "avi", "webm", "mov", "wmv", "ts", "m2ts"];

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// Where everything went before the library (now: playlist exports).
    download_dir: PathBuf,
    /// The library: one folder per series.
    library_dir: PathBuf,
    prefer_dual_audio: bool,
    /// Which audio and subtitles play by default, unless a series says otherwise.
    prefs: crate::prefs::LangPrefs,
    /// Old downloads have been moved into the library.
    migrated: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            library_dir: default_library_dir(),
            prefer_dual_audio: false,
            prefs: Default::default(),
            migrated: false,
        }
    }
}

/// `~/Videos/Anime` (or the platform's videos folder). Tests never touch it.
fn default_library_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join("seadex-downloader-test-library");
    }
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Anime")
}

/// Results of background work, sent back to the UI thread.
enum Msg {
    Engine { seq: u64, result: Result<Arc<Engine>, String> },
    VpnFixDone(Result<(), String>),
    SearchDone { seq: u64, result: Result<Vec<Anime>, String> },
    ListingDone { seq: u64, subtitle: String, result: Result<Listing, String>, nzbs: Vec<NzbLink> },
    NyaaDone { seq: u64, result: Result<Vec<NyaaRelease>, String> },
    DownloadStarted(Result<String, String>),
    FolderPicked(Option<PathBuf>),
    LibraryFolderPicked(Option<PathBuf>),
    RemuxProbed { seq: u64, result: Result<Analysis, String> },
    RemuxTitle { seq: u64, found: Option<AnimeRef> },
    RemuxSamples { seq: u64, samples: Vec<(crate::remux::TrackSig, crate::remux::SubtitleSample)> },
    RemuxProgress { seq: u64, index: usize, fraction: f32 },
    RemuxFileDone { seq: u64, index: usize, result: Result<(), String> },
    RemuxFinished { seq: u64 },
    RemuxFolderPicked(Option<PathBuf>),
    PlaylistAdd { items: Vec<Item>, errors: Vec<String> },
    PlaylistExportProgress { seq: u64, fraction: f32 },
    PlaylistExportDone { seq: u64, result: Result<PathBuf, String> },
    PlaylistFolderPicked(Option<PathBuf>),
    DriveClientFile(Result<crate::gdrive::OAuthClient, String>),
    DriveSignInUrl { seq: u64, url: String },
    DriveSignedIn { seq: u64, result: Result<(String, String), String> },
    DriveFolders { seq: u64, result: Result<Vec<crate::gdrive::Entry>, String>, expired: bool },
    DriveLinkChecked { result: Result<crate::gdrive::Folder, String>, expired: bool },
    DriveUploadProgress { seq: u64, fraction: f32 },
    DriveUploadDone { seq: u64, result: Result<crate::gdrive::Uploaded, String>, expired: bool },
    DebridAccount { seq: u64, provider: crate::debrid::Provider, result: Result<crate::debrid::Account, String> },
    NzbLookup { hash: String, links: Vec<NzbLink> },
    Moved { key: String, name: String, result: Result<Vec<(PathBuf, PathBuf)>, String> },
    DefaultsApplied { anilist_id: u64, rel: String, applied: crate::library::Applied },
    DefaultsDone,
    HandedOver(Result<String, String>),
    Details { id: u64, result: Result<crate::library::Details, String> },
    SeriesEntry { id: u64, result: Result<Option<seadex::Entry>, String> },
    UsenetChecked { id: u64, seq: u64, result: Result<(), String> },
    Info(String),
    Error(String),
}

/// Things the user clicked this frame.
enum Action {
    Search,
    OpenAnime(usize),
    Back,
    SearchNyaa,
    ChooseFiles,
    StartDownload,
    EditFiles(TorrentHandle),
    Pause(TorrentHandle),
    Resume(TorrentHandle),
    Remove { id: TorrentId, delete_files: bool },
    AskDelete { id: TorrentId, name: String },
    OpenFolder(PathBuf),
    PickFolder,
    PickLibraryFolder,
    OpenRemux(TorrentHandle),
    StartRemux,
    CancelRemux,
    PickRemuxFolder,
    AddRemuxesToPlaylist,
    OpenPlaylist,
    PlaylistAddFiles,
    PlaylistRemove(usize),
    PlaylistMove { from: usize, to: usize },
    PlaylistClear,
    PlaylistPickFolder,
    StartExport,
    CancelExport,
    DriveLoadClient,
    DriveSignIn,
    DriveCancelSignIn,
    DriveSignOut,
    DriveChooseFolder,
    DriveBrowseInto(crate::gdrive::Entry),
    DriveBrowseBack(usize),
    DriveUseFolder,
    DriveUseLink,
    DriveCloseBrowser,
    DriveSetAutoUpload(bool),
    DriveUpload(PathBuf),
    DriveCancelUpload,
    DebridSetEnabled(bool),
    DebridCheck,
    DebridPause(u64),
    DebridResume(u64),
    DebridRemove { id: u64, delete_files: bool },
    DebridAskDelete { id: u64, name: String },
    DebridEditFiles(u64),
    DebridOpenRemux(u64),
    VpnFix(crate::vpn::Fix),
    UsenetPause(u64),
    UsenetResume(u64),
    UsenetRemove { id: u64, delete_files: bool },
    UsenetAskDelete { id: u64, name: String },
    UsenetEditFiles(u64),
    UsenetOpenRemux(u64),
    UsenetOpenProviders,
    UsenetCheck(u64),
    GoLibrary,
    OpenSeries(u64),
    AddToLibrary,
    RemoveSeries { id: u64, delete_files: bool },
    AskRemoveSeries(u64),
    OpenLanguages,
    SetSeriesPrefs { id: u64, prefs: Option<crate::prefs::LangPrefs> },
    RemuxLibrary { id: u64, files: Vec<PathBuf> },
}

/// What the delete confirmation is about.
#[derive(Clone, Copy)]
enum DeleteTarget {
    Torrent(TorrentId),
    Debrid(u64),
    Usenet(u64),
    /// A series' whole folder.
    Series(u64),
}

enum EngineState {
    /// Not running: downloads go through a debrid service, and there are no
    /// torrents from before to show.
    Off,
    /// Torrents must go through the VPN, which isn't connected; the text says
    /// why. The engine is stopped, so nothing goes out.
    WaitingForVpn(String),
    Starting,
    Ready(Arc<Engine>),
    Failed(String),
}

// Only one page exists at a time, so the variants' size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
enum Page {
    Home,
    Results,
    Anime(AnimePage),
    Files(FilesPage),
    Remux(RemuxPage),
    Playlist(PlaylistPage),
}

struct AnimePage {
    anime: Anime,
    /// Index into the SeaDex entry's releases.
    chosen: Option<usize>,
    /// Set when SeaDex has no Nyaa release for this anime.
    nyaa: Option<NyaaSearch>,
    /// Opened from the library (so Back goes there).
    from_library: bool,
    /// Its SeaDex entry is being looked up.
    entry_loading: bool,
}

/// The Nyaa fallback: search results ranked by release-group tier.
struct NyaaSearch {
    /// What's in the search box.
    query: String,
    /// The automatic search (names + season filter), used while `query` is unedited.
    default_plan: SearchPlan,
    seq: u64,
    loading: bool,
    searched: bool,
    releases: Vec<NyaaRelease>,
    chosen: Option<usize>,
    error: Option<String>,
}

impl NyaaSearch {
    fn new(plan: SearchPlan) -> Self {
        Self {
            query: plan.display_query(),
            default_plan: plan,
            seq: 0,
            loading: false,
            searched: false,
            releases: Vec::new(),
            chosen: None,
            error: None,
        }
    }

    fn plan(&self) -> SearchPlan {
        if self.query.trim() == self.default_plan.display_query() {
            self.default_plan.clone()
        } else {
            SearchPlan::custom(&self.query)
        }
    }
}

/// What the picker page picks from.
enum Picker {
    /// Files inside one torrent.
    Files(Listing),
    /// A release that has one torrent per episode.
    Episodes {
        items: Vec<(u32, NyaaItem)>,
        title: String,
        /// Folder (inside the download folder) the episodes go in.
        folder: String,
    },
}

struct Row {
    label: String,
    len: u64,
    seeders: Option<u32>,
}

struct FilesPage {
    /// The anime page to go back to; `None` when opened from the downloads panel.
    back: Option<AnimePage>,
    subtitle: String,
    picker: Picker,
    rows: Vec<Row>,
    /// Parallel to `rows`.
    checked: Vec<bool>,
    filter: String,
    /// Where the release is on Usenet (files pickers only).
    nzbs: Vec<NzbLink>,
    /// Where the download goes (the series' library folder), unless it's
    /// already downloading somewhere.
    folder: Option<PathBuf>,
}

impl FilesPage {
    fn new(listing: Listing, back: Option<AnimePage>, subtitle: String) -> Self {
        let checked = match listing.existing.as_ref().and_then(|e| e.only_files.as_ref()) {
            Some(only) => listing.files.iter().map(|f| only.contains(&f.index)).collect(),
            None => vec![true; listing.files.len()],
        };
        let rows = listing
            .files
            .iter()
            .map(|f| Row { label: f.path.clone(), len: f.len, seeders: None })
            .collect();
        Self { back, subtitle, picker: Picker::Files(listing), rows, checked, filter: String::new(), nzbs: Vec::new(), folder: None }
    }

    fn episodes(
        items: Vec<(u32, NyaaItem)>,
        title: String,
        folder: String,
        back: Option<AnimePage>,
        subtitle: String,
    ) -> Self {
        let rows = items
            .iter()
            .map(|(ep, item)| Row {
                label: format!("Episode {ep:02}  ·  {}", item.title),
                len: item.size,
                seeders: Some(item.seeders),
            })
            .collect::<Vec<_>>();
        let checked = vec![true; rows.len()];
        Self {
            back,
            subtitle,
            picker: Picker::Episodes { items, title, folder },
            rows,
            checked,
            filter: String::new(),
            nzbs: Vec::new(),
            folder: None,
        }
    }

    fn title(&self) -> &str {
        match &self.picker {
            Picker::Files(listing) => &listing.name,
            Picker::Episodes { title, .. } => title,
        }
    }

    fn existing(&self) -> bool {
        matches!(&self.picker, Picker::Files(l) if l.existing.is_some())
    }

    fn unit(&self) -> &'static str {
        match self.picker {
            Picker::Files(_) => "files",
            Picker::Episodes { .. } => "episodes",
        }
    }

    fn output_folder(&self, library_dir: &Path) -> PathBuf {
        match &self.picker {
            Picker::Files(listing) if listing.existing.is_some() => listing.output_folder(library_dir),
            _ if self.folder.is_some() => self.folder.clone().unwrap(),
            Picker::Files(listing) => listing.output_folder(library_dir),
            Picker::Episodes { folder, .. } => library_dir.join(folder),
        }
    }

    /// Checked positions in `rows`.
    fn selected(&self) -> Vec<usize> {
        self.checked.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| i).collect()
    }

    /// Torrent file indices to download (file pickers only).
    fn selected_indices(&self) -> Vec<usize> {
        match &self.picker {
            Picker::Files(listing) => self.selected().into_iter().map(|i| listing.files[i].index).collect(),
            Picker::Episodes { .. } => Vec::new(),
        }
    }

    /// Positions in `rows` that match the filter.
    fn visible(&self) -> Vec<usize> {
        let terms: Vec<String> = self.filter.split_whitespace().map(str::to_lowercase).collect();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let label = r.label.to_lowercase();
                terms.iter().all(|t| label.contains(t))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

struct Status {
    text: String,
    error: bool,
}

pub struct App {
    rt: tokio::runtime::Handle,
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    http: reqwest::Client,
    engine: EngineState,
    settings: Settings,

    query: String,
    last_query: String,
    results: Vec<Anime>,
    searching: bool,
    search_seq: u64,

    page: Page,
    nyaa_seq: u64,
    listing_seq: u64,
    loading_listing: bool,
    starting_download: bool,

    confirm_delete: Option<(DeleteTarget, String)>,
    status: Option<Status>,

    /// Your series, their folders and preferences.
    library: Library,
    /// Moving downloads into the library, setting default tracks.
    lib: LibraryState,
    /// The library and series pages.
    pages: PagesState,
    /// Failed downloads handed on to the next way of getting them.
    handover: Handover,
    remux_seq: u64,
    /// Remuxed episodes to export together.
    playlist: Playlist,
    playlist_seq: u64,
    /// Google Drive sign-in and upload folder.
    drive: DriveState,
    /// Debrid service settings and downloads.
    debrid: DebridState,
    /// The VPN setting and connection.
    vpn: VpnState,
    /// Usenet providers, lookups and downloads.
    usenet: UsenetState,
    /// The network the engine runs (or is starting) on.
    engine_net: Option<crate::vpn::Network>,
    /// Bumped by each engine start or stop; a start that finds it changed
    /// has been superseded.
    engine_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Engine starts and stops take turns.
    engine_lock: Arc<tokio::sync::Mutex<()>>,
    state_dir: PathBuf,
}

impl App {
    /// `state_dir` holds the torrent engine's state (which torrents are in
    /// the session and how far along they are).
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle, state_dir: PathBuf) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("seadex-downloader/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build the HTTP client");
        let (tx, rx) = channel();
        let debrid = DebridState::load(&state_dir, rt.clone(), cc.egui_ctx.clone());
        let vpn = VpnState::load(&state_dir, &rt, cc.egui_ctx.clone());
        let usenet = UsenetState::load(&state_dir, rt.clone(), cc.egui_ctx.clone(), http.clone());
        let library = Library::load(state_dir.join("library.json"), &expand_home(&settings.library_dir));

        let mut app = Self {
            rt,
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            http,
            engine: EngineState::Off,
            settings,
            query: String::new(),
            last_query: String::new(),
            results: Vec::new(),
            searching: false,
            search_seq: 0,
            page: Page::Home,
            nyaa_seq: 0,
            listing_seq: 0,
            loading_listing: false,
            starting_download: false,
            confirm_delete: None,
            status: None,
            library,
            lib: LibraryState::default(),
            pages: PagesState::default(),
            handover: Handover::default(),
            remux_seq: 0,
            playlist: Playlist::load(state_dir.join("playlist.json")),
            playlist_seq: 0,
            drive: DriveState::load(&state_dir, concat!("seadex-downloader/", env!("CARGO_PKG_VERSION"))),
            debrid,
            vpn,
            usenet,
            engine_net: None,
            engine_seq: Arc::default(),
            engine_lock: Arc::default(),
            state_dir,
        };

        // With a debrid service, BitTorrent only starts to show torrents from before.
        if !app.debrid.config.enabled || crate::engine::has_saved_torrents(&app.state_dir) {
            app.start_engine();
        }
        if app.debrid.config.active() {
            app.debrid_check();
        }
        app
    }

    /// Whether a new download can start now, or why not.
    fn download_ready(&self) -> Result<(), String> {
        if self.debrid.config.enabled {
            let provider = self.debrid.config.provider;
            return match self.debrid.config.key(provider) {
                Some(_) => Ok(()),
                None => Err(format!("Add your {} API key in Settings first.", provider.name())),
            };
        }
        if let Some(reason) = &self.vpn.waiting {
            return Err(format!("Torrents are waiting for the VPN: {reason}"));
        }
        match &self.engine {
            EngineState::Ready(_) => Ok(()),
            EngineState::Failed(_) => Err("The torrent engine failed to start.".into()),
            EngineState::WaitingForVpn(_) => Err("Connecting to the VPN…".into()),
            EngineState::Starting | EngineState::Off => Err("Waiting for the torrent engine…".into()),
        }
    }

    /// Run `fut` on the runtime and deliver its message to the UI.
    fn spawn(&self, fut: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
            ctx.request_repaint();
        });
    }

    fn engine(&self) -> Option<Arc<Engine>> {
        match &self.engine {
            EngineState::Ready(e) => Some(e.clone()),
            _ => None,
        }
    }

    /// The download folder setting, with a leading `~` expanded.
    fn download_dir(&self) -> PathBuf {
        expand_home(&self.settings.download_dir)
    }

    /// The library folder setting, with a leading `~` expanded.
    fn library_dir(&self) -> PathBuf {
        expand_home(&self.settings.library_dir)
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), error: false });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.status = Some(Status { text, error: true });
    }

    fn home_or_results(&self) -> Page {
        if self.last_query.is_empty() { Page::Home } else { Page::Results }
    }

    /// Forget any in-flight torrent fetch, so its result can't land on a different page.
    fn cancel_listing(&mut self) {
        self.listing_seq += 1;
        self.loading_listing = false;
    }

    // ── Background results ─────────────────────────────────────────────

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Engine { seq, result } => self.handle_engine_msg(seq, result),
                Msg::VpnFixDone(result) => self.handle_vpn_fix_done(result),
                Msg::SearchDone { seq, result } => {
                    if seq != self.search_seq {
                        continue;
                    }
                    self.searching = false;
                    match result {
                        Ok(results) => {
                            self.results = results;
                            self.cancel_listing();
                            self.page = Page::Results;
                        }
                        Err(e) => self.set_error(e),
                    }
                }
                Msg::ListingDone { seq, subtitle, result, nzbs } => {
                    if seq != self.listing_seq {
                        continue;
                    }
                    self.loading_listing = false;
                    match result {
                        Ok(listing) => {
                            if matches!(self.page, Page::Anime(_))
                                && let Page::Anime(ap) = std::mem::replace(&mut self.page, Page::Home)
                            {
                                let folder = self.library.folder_for(&new_series(&ap.anime), &self.library_dir());
                                let mut page = FilesPage::new(listing, Some(ap), subtitle);
                                page.nzbs = nzbs;
                                page.folder = Some(folder);
                                self.page = Page::Files(page);
                            }
                        }
                        Err(e) => self.set_error(e),
                    }
                }
                Msg::NyaaDone { seq, result } => {
                    let Page::Anime(AnimePage { nyaa: Some(search), .. }) = &mut self.page else { continue };
                    if search.seq != seq {
                        continue;
                    }
                    search.loading = false;
                    search.searched = true;
                    match result {
                        Ok(releases) => {
                            search.chosen = releases.first().filter(|r| r.usable()).map(|_| 0);
                            search.releases = releases;
                            search.error = None;
                        }
                        Err(e) => {
                            search.releases.clear();
                            search.chosen = None;
                            search.error = Some(e);
                        }
                    }
                }
                Msg::DownloadStarted(result) => {
                    self.starting_download = false;
                    match result {
                        Ok(name) => {
                            self.set_info(format!("Downloading {name}"));
                            if matches!(self.page, Page::Files(_))
                                && let Page::Files(fp) = std::mem::replace(&mut self.page, Page::Home)
                            {
                                self.page = match fp.back {
                                    Some(ap) => Page::Anime(ap),
                                    None => self.home_or_results(),
                                };
                            }
                        }
                        Err(e) => self.set_error(e),
                    }
                }
                // "Change…" on the files page: this download only.
                Msg::FolderPicked(Some(dir)) => {
                    if let Page::Files(fp) = &mut self.page {
                        fp.folder = Some(dir);
                    }
                }
                Msg::FolderPicked(None) | Msg::LibraryFolderPicked(None) => {}
                Msg::LibraryFolderPicked(Some(dir)) => self.settings.library_dir = dir,
                Msg::Info(text) => self.set_info(text),
                Msg::Error(text) => self.set_error(text),
                msg @ (Msg::RemuxProbed { .. }
                | Msg::RemuxTitle { .. }
                | Msg::RemuxSamples { .. }
                | Msg::RemuxProgress { .. }
                | Msg::RemuxFileDone { .. }
                | Msg::RemuxFinished { .. }
                | Msg::RemuxFolderPicked(_)) => self.handle_remux_msg(msg),
                msg @ (Msg::PlaylistAdd { .. }
                | Msg::PlaylistExportProgress { .. }
                | Msg::PlaylistExportDone { .. }
                | Msg::PlaylistFolderPicked(_)) => self.handle_playlist_msg(msg),
                msg @ (Msg::DriveClientFile(_)
                | Msg::DriveSignInUrl { .. }
                | Msg::DriveSignedIn { .. }
                | Msg::DriveFolders { .. }
                | Msg::DriveLinkChecked { .. }
                | Msg::DriveUploadProgress { .. }
                | Msg::DriveUploadDone { .. }) => self.handle_drive_msg(msg),
                msg @ Msg::DebridAccount { .. } => self.handle_debrid_msg(msg),
                msg @ (Msg::NzbLookup { .. } | Msg::UsenetChecked { .. }) => self.handle_usenet_msg(msg),
                Msg::Moved { key, name, result } => self.handle_moved(key, name, result),
                Msg::DefaultsApplied { anilist_id, rel, applied } => self.handle_defaults_applied(anilist_id, rel, applied),
                Msg::DefaultsDone => self.defaults_done(),
                Msg::HandedOver(result) => self.handled_over(result),
                Msg::Details { id, result } => self.handle_details(id, result),
                Msg::SeriesEntry { id, result } => self.handle_series_entry(id, result),
            }
        }
    }

    // ── Actions ────────────────────────────────────────────────────────

    fn perform(&mut self, action: Action) {
        match action {
            Action::Search => self.start_search(),
            Action::OpenAnime(i) => {
                if let Some(anime) = self.results.get(i).cloned() {
                    self.show_anime(anime, false);
                }
            }
            Action::SearchNyaa => self.search_nyaa(),
            Action::Back => {
                self.cancel_listing();
                self.page = match std::mem::replace(&mut self.page, Page::Home) {
                    Page::Files(FilesPage { back: Some(ap), .. }) => Page::Anime(ap),
                    Page::Anime(AnimePage { from_library: true, .. }) => Page::Home,
                    Page::Remux(RemuxPage { back: Some(ap), .. }) => Page::Anime(ap),
                    Page::Files(_) | Page::Anime(_) | Page::Remux(_) | Page::Playlist(_) => self.home_or_results(),
                    other => other,
                };
            }
            Action::ChooseFiles => self.choose_files(),
            Action::StartDownload => self.start_download(),
            Action::EditFiles(handle) => {
                let Some(engine) = self.engine() else { return };
                match engine.listing_for(&handle) {
                    Ok(listing) => {
                        self.cancel_listing();
                        let subtitle = "Change which files this torrent downloads".to_owned();
                        self.page = Page::Files(FilesPage::new(listing, None, subtitle));
                    }
                    Err(e) => self.set_error(format!("Can't edit files yet: {e:#}")),
                }
            }
            Action::Pause(handle) => {
                let Some(engine) = self.engine() else { return };
                self.spawn(async move {
                    match engine.pause(&handle).await {
                        Ok(()) => Msg::Info("Paused".into()),
                        Err(e) => Msg::Error(format!("Could not pause: {e:#}")),
                    }
                });
            }
            Action::Resume(handle) => {
                let Some(engine) = self.engine() else { return };
                self.spawn(async move {
                    match engine.resume(&handle).await {
                        Ok(()) => Msg::Info("Resumed".into()),
                        Err(e) => Msg::Error(format!("Could not resume: {e:#}")),
                    }
                });
            }
            Action::Remove { id, delete_files } => {
                let Some(engine) = self.engine() else { return };
                self.spawn(async move {
                    match engine.remove(id, delete_files).await {
                        Ok(()) if delete_files => Msg::Info("Removed and deleted files".into()),
                        Ok(()) => Msg::Info("Removed (files kept on disk)".into()),
                        Err(e) => Msg::Error(format!("Could not remove: {e:#}")),
                    }
                });
            }
            Action::AskDelete { id, name } => self.confirm_delete = Some((DeleteTarget::Torrent(id), name)),
            Action::OpenFolder(path) => {
                if let Err(e) = open_folder(&path) {
                    self.set_error(format!("Could not open {}: {e}", path.display()));
                }
            }
            Action::OpenRemux(handle) => self.open_remux(handle),
            Action::StartRemux => self.start_remux(),
            Action::CancelRemux => self.cancel_remux(),
            Action::PickRemuxFolder => self.pick_remux_folder(),
            Action::AddRemuxesToPlaylist => self.add_remuxes_to_playlist(),
            Action::OpenPlaylist => self.open_playlist(),
            Action::PlaylistAddFiles => self.add_playlist_files(),
            Action::PlaylistRemove(i) => self.playlist.remove(i),
            Action::PlaylistMove { from, to } => self.playlist.move_item(from, to),
            Action::PlaylistClear => self.playlist.clear(),
            Action::PlaylistPickFolder => self.pick_export_folder(),
            Action::StartExport => self.start_export(),
            Action::CancelExport => self.cancel_export(),
            Action::DriveLoadClient => self.drive_load_client(),
            Action::DriveSignIn => self.drive_sign_in(),
            Action::DriveCancelSignIn => self.drive_cancel_sign_in(),
            Action::DriveSignOut => self.drive_sign_out(),
            Action::DriveChooseFolder => self.open_folder_browser(),
            Action::DriveBrowseInto(entry) => self.browse_into(entry),
            Action::DriveBrowseBack(depth) => self.browse_back(depth),
            Action::DriveUseFolder => self.use_current_drive_folder(),
            Action::DriveUseLink => self.use_drive_link(),
            Action::DriveCloseBrowser => self.drive.browser = None,
            Action::DriveSetAutoUpload(on) => self.set_auto_upload(on),
            Action::DriveUpload(path) => self.start_upload(path),
            Action::DriveCancelUpload => self.cancel_upload(),
            Action::DebridSetEnabled(on) => self.set_debrid_enabled(on),
            Action::VpnFix(fix) => self.vpn_fix(fix),
            Action::UsenetPause(id) => self.usenet.manager.pause(id),
            Action::UsenetResume(id) => self.usenet.manager.resume(id),
            Action::UsenetRemove { id, delete_files } => self.usenet_remove(id, delete_files),
            Action::UsenetAskDelete { id, name } => self.confirm_delete = Some((DeleteTarget::Usenet(id), name)),
            Action::UsenetEditFiles(id) => self.usenet_edit_files(id),
            Action::UsenetOpenRemux(id) => self.usenet_open_remux(id),
            Action::UsenetOpenProviders => self.usenet.dialog = Some(Default::default()),
            Action::UsenetCheck(id) => self.usenet_check(id),
            Action::GoLibrary => {
                self.cancel_listing();
                self.page = Page::Home;
            }
            Action::OpenSeries(id) => self.open_series(id),
            Action::AddToLibrary => self.add_to_library(),
            Action::RemoveSeries { id, delete_files } => self.remove_series(id, delete_files),
            Action::AskRemoveSeries(id) => {
                if let Some(s) = self.library.find(id) {
                    self.confirm_delete = Some((DeleteTarget::Series(id), s.title().to_owned()));
                }
            }
            Action::OpenLanguages => self.pages.languages = Some(self.settings.prefs.clone()),
            Action::SetSeriesPrefs { id, prefs } => self.set_series_prefs(id, prefs),
            Action::RemuxLibrary { id, files } => self.remux_library(id, files),
            Action::DebridCheck => self.debrid_check(),
            Action::DebridPause(id) => self.debrid.manager.pause(id),
            Action::DebridResume(id) => self.debrid.manager.resume(id),
            Action::DebridRemove { id, delete_files } => self.debrid_remove(id, delete_files),
            Action::DebridAskDelete { id, name } => self.confirm_delete = Some((DeleteTarget::Debrid(id), name)),
            Action::DebridEditFiles(id) => self.debrid_edit_files(id),
            Action::DebridOpenRemux(id) => self.debrid_open_remux(id),
            Action::PickLibraryFolder => {
                let start = self.library_dir();
                let start = start.ancestors().find(|p| p.is_dir()).map(Path::to_owned).unwrap_or(start);
                self.spawn(async move {
                    let picked = rfd::AsyncFileDialog::new().set_title("Library folder").set_directory(&start).pick_folder().await;
                    Msg::LibraryFolderPicked(picked.map(|f| f.path().to_owned()))
                });
            }
            Action::PickFolder => {
                let start = match &self.page {
                    Page::Files(fp) => fp.output_folder(&self.library_dir()),
                    _ => self.library_dir(),
                };
                let start = start.ancestors().find(|p| p.is_dir()).map(Path::to_owned).unwrap_or(start);
                self.spawn(async move {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Save this download in")
                        .set_directory(&start)
                        .pick_folder()
                        .await;
                    Msg::FolderPicked(picked.map(|f| f.path().to_owned()))
                });
            }
        }
    }

    /// Show an anime's page, with its best release picked.
    fn show_anime(&mut self, anime: Anime, from_library: bool) {
        let dual = self.prefer_dual_audio(anime.anilist_id);
        let chosen = anime.entry.as_ref().and_then(|e| e.best_nyaa(dual));
        // Nothing on SeaDex to download: find the best release on Nyaa instead.
        let nyaa = chosen.is_none().then(|| NyaaSearch::new(SearchPlan::for_anime(&anime)));
        let search_now = nyaa.is_some();
        self.cancel_listing();
        self.page = Page::Anime(AnimePage { anime, chosen, nyaa, from_library, entry_loading: false });
        if search_now {
            self.search_nyaa();
        }
    }

    /// Whether to pick dual-audio releases for an anime: the setting, or
    /// languages that want a dub.
    fn prefer_dual_audio(&self, anilist_id: u64) -> bool {
        let prefs = self.library.find(anilist_id).and_then(|s| s.prefs.as_ref()).unwrap_or(&self.settings.prefs);
        self.settings.prefer_dual_audio || prefs.wants_dub()
    }

    fn start_search(&mut self) {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.search_seq += 1;
        let seq = self.search_seq;
        self.searching = true;
        self.last_query = query.clone();
        let client = self.http.clone();
        self.spawn(async move {
            let result = seadex::search(&client, &query)
                .await
                .map_err(|e| format!("Search failed: {e:#}"));
            Msg::SearchDone { seq, result }
        });
    }

    /// Search Nyaa for the anime on the current page and rank what it finds.
    fn search_nyaa(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let prefer_dual_audio = self.prefer_dual_audio(ap.anime.anilist_id);
        let Page::Anime(AnimePage { nyaa: Some(search), .. }) = &mut self.page else { return };
        if search.query.trim().is_empty() {
            return;
        }
        let plan = search.plan();
        self.nyaa_seq += 1;
        let seq = self.nyaa_seq;
        search.seq = seq;
        search.loading = true;
        search.error = None;
        let client = self.http.clone();
        self.spawn(async move {
            let result = fallback::find_releases(&client, &plan, prefer_dual_audio)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::NyaaDone { seq, result }
        });
    }

    /// Go from the chosen release to picking episodes. For a single torrent,
    /// fetch its .torrent from Nyaa (or, failing that, its metadata via
    /// magnet link) and list its files; a release with one torrent per
    /// episode lists the episodes straight away.
    fn choose_files(&mut self) {
        // Releases on Usenet don't need the torrent engine or a debrid key.
        if !self.usenet.active()
            && let Err(why) = self.download_ready()
        {
            self.set_error(why);
            return;
        }
        let Page::Anime(ap) = &self.page else { return };
        let (nyaa_id, info_hash, subtitle) = match &ap.nyaa {
            Some(search) => {
                let Some(release) = search.chosen.and_then(|i| search.releases.get(i)) else { return };
                let subtitle = nyaa_release_label(release);
                match &release.kind {
                    ReleaseKind::Torrent { item, .. } => (item.id, valid_info_hash(&item.info_hash), subtitle),
                    ReleaseKind::Episodes(items) => {
                        let items = items.clone();
                        let group = release.group.clone();
                        let Page::Anime(ap) = std::mem::replace(&mut self.page, Page::Home) else { unreachable!() };
                        let title = format!("{} · {group}", ap.anime.title);
                        let folder = sanitize_folder_name(&format!("{} [{group}]", ap.anime.title));
                        self.cancel_listing();
                        let series_folder = self.library.folder_for(&new_series(&ap.anime), &self.library_dir());
                        let mut page = FilesPage::episodes(items, title, folder, Some(ap), subtitle);
                        page.folder = Some(series_folder);
                        self.page = Page::Files(page);
                        return;
                    }
                }
            }
            None => {
                let Some(release) = chosen_release(ap) else { return };
                let Some(id) = release.nyaa_id() else { return };
                (id, release.info_hash_hex().map(str::to_owned), release_label(release))
            }
        };
        let client = self.http.clone();
        // With a Usenet provider, a release on Usenet comes from there.
        let usenet = self.usenet.active().then(|| {
            let cached = info_hash.as_deref().and_then(|h| self.usenet.links(h)).map(<[NzbLink]>::to_vec);
            (self.usenet.indexers.clone(), self.usenet.manager.clone(), cached)
        });
        let debrid = self.debrid.config.enabled.then(|| self.debrid.manager.clone());
        let engine = self.engine();

        self.listing_seq += 1;
        let seq = self.listing_seq;
        self.loading_listing = true;
        self.spawn(async move {
            let failed = |e: anyhow::Error| format!("Could not load the torrent: {e:#}");
            if let Some((indexers, usenet, cached)) = usenet {
                let links = match (cached, &info_hash) {
                    (Some(links), _) => links,
                    (None, Some(hash)) => indexers.lookup(hash, Some(nyaa_id)).await,
                    (None, None) => Vec::new(),
                };
                // A download stays with whatever started it.
                let elsewhere = info_hash.as_deref().is_some_and(|h| {
                    usenet.job_for(h).is_none()
                        && (debrid.as_ref().is_some_and(|d| d.job_for(h).is_some())
                            || engine.as_ref().is_some_and(|e| e.existing(h).is_some()))
                });
                if !links.is_empty() && !elsewhere {
                    let result: anyhow::Result<Listing> = async {
                        let bytes = nyaa::fetch_torrent(&client, nyaa_id).await?;
                        let mut listing = crate::engine::listing_from_bytes(bytes)?;
                        listing.existing = info_hash_of(&listing.torrent_bytes).and_then(|h| usenet.existing(&h));
                        Ok(listing)
                    }
                    .await;
                    return Msg::ListingDone { seq, subtitle, result: result.map_err(failed), nzbs: links };
                }
            }
            let result: anyhow::Result<Listing> = match (debrid, engine) {
                // No BitTorrent: read the .torrent file here.
                (Some(manager), _) => async {
                    let bytes = nyaa::fetch_torrent(&client, nyaa_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e:#} (debrid downloads need the .torrent file)"))?;
                    let mut listing = crate::engine::listing_from_bytes(bytes)?;
                    listing.existing = info_hash_of(&listing.torrent_bytes).and_then(|h| manager.existing(&h));
                    Ok(listing)
                }
                .await,
                (None, Some(engine)) => async {
                    let source = match nyaa::fetch_torrent(&client, nyaa_id).await {
                        Ok(bytes) => Source::TorrentFile(bytes),
                        Err(e) => match info_hash {
                            Some(hash) => {
                                tracing::warn!("{e:#}; trying the magnet link instead");
                                Source::Magnet(nyaa::magnet(&hash))
                            }
                            None => return Err(e),
                        },
                    };
                    engine.inspect(source).await
                }
                .await,
                (None, None) => Err(anyhow::anyhow!("this release isn't on Usenet, and the torrent engine isn't ready")),
            };
            Msg::ListingDone { seq, subtitle, result: result.map_err(failed), nzbs: Vec::new() }
        });
    }

    /// For a picker of one torrent per episode, with Usenet set up: each
    /// episode's NZBs (`None` while being looked up), in row order.
    fn episode_nzbs(&self, fp: &FilesPage) -> Option<Vec<Option<&[NzbLink]>>> {
        let Picker::Episodes { items, .. } = &fp.picker else { return None };
        self.usenet.active().then(|| items.iter().map(|(_, i)| self.usenet.links(&i.info_hash)).collect())
    }

    /// Whether the release on the files page downloads from Usenet.
    fn via_usenet(&self, fp: &FilesPage) -> bool {
        matches!(fp.picker, Picker::Files(_)) && !fp.nzbs.is_empty() && self.usenet.active() && !fp.existing()
    }

    fn start_download(&mut self) {
        let Page::Files(fp) = &self.page else { return };
        let selected = fp.selected();
        if selected.is_empty() {
            self.set_error(format!("Select at least one of the {}", fp.unit()));
            return;
        }
        let output_folder = fp.output_folder(&self.library_dir());
        if let Some(ap) = &fp.back {
            let hashes: Vec<String> = match &fp.picker {
                Picker::Files(listing) => info_hash_of(&listing.torrent_bytes).into_iter().collect(),
                Picker::Episodes { items, .. } => selected.iter().map(|&i| items[i].1.info_hash.clone()).collect(),
            };
            // Downloading something adds its series to the library.
            let new = new_series(&ap.anime);
            let root = self.library_dir();
            self.library.record(hashes, &new, &root);
        }
        let Page::Files(fp) = &self.page else { return };
        let provider = self.debrid.config.provider;
        match &fp.picker {
            Picker::Files(listing) => {
                let files = fp.selected_indices();
                let name = listing.name.clone();
                // A download stays with whatever started it; new ones follow the setting.
                let hash = info_hash_of(&listing.torrent_bytes).unwrap_or_default();
                if self.usenet.manager.job_for(&hash).is_some() || self.via_usenet(fp) {
                    let result = self
                        .usenet
                        .manager
                        .add(listing, &files, output_folder, fp.nzbs.clone())
                        .map(|()| name)
                        .map_err(|e| format!("Could not start the download: {e:#}"));
                    self.starting_download = true;
                    let _ = self.tx.send(Msg::DownloadStarted(result));
                    return;
                }
                let debrid_job = self.debrid.manager.job_for(&hash).is_some();
                if debrid_job || (!fp.existing() && self.debrid.config.enabled) {
                    if !debrid_job && let Err(why) = self.download_ready() {
                        self.set_error(why);
                        return;
                    }
                    let result = self
                        .debrid
                        .manager
                        .add(provider, listing, &files, output_folder)
                        .map(|()| name)
                        .map_err(|e| format!("Could not start the download: {e:#}"));
                    self.starting_download = true;
                    let _ = self.tx.send(Msg::DownloadStarted(result));
                    return;
                }
                let Some(engine) = self.engine() else {
                    self.set_error("The torrent engine isn't ready yet");
                    return;
                };
                let torrent_bytes = listing.torrent_bytes.clone();
                self.starting_download = true;
                self.spawn(async move {
                    let result = engine
                        .download(torrent_bytes, Some(files), output_folder)
                        .await
                        .map(|()| name)
                        .map_err(|e| format!("Could not start the download: {e:#}"));
                    Msg::DownloadStarted(result)
                });
            }
            Picker::Episodes { items, title, .. } => {
                let items: Vec<(u32, NyaaItem)> = selected.iter().map(|&i| items[i].clone()).collect();
                let title = title.clone();
                let client = self.http.clone();
                // One torrent per episode: those on Usenet come from there (with
                // a provider); the rest go to the debrid service, or the engine.
                let usenet = self.usenet.active().then(|| {
                    let cached: std::collections::HashMap<String, Vec<NzbLink>> = items
                        .iter()
                        .filter_map(|(_, i)| self.usenet.links(&i.info_hash).map(|l| (i.info_hash.to_ascii_lowercase(), l.to_vec())))
                        .collect();
                    (self.usenet.indexers.clone(), self.usenet.manager.clone(), cached)
                });
                let backend = if self.debrid.config.enabled {
                    match self.download_ready() {
                        Ok(()) => Some(Err(self.debrid.manager.clone())),
                        Err(why) if usenet.is_none() => {
                            self.set_error(why);
                            return;
                        }
                        Err(_) => None,
                    }
                } else {
                    match self.engine() {
                        Some(engine) => Some(Ok(engine)),
                        None if usenet.is_none() => {
                            self.set_error("The torrent engine isn't ready yet");
                            return;
                        }
                        None => None,
                    }
                };
                self.starting_download = true;
                self.spawn(async move {
                    let mut failed = Vec::new();
                    for (ep, item) in &items {
                        let result = async {
                            if let Some((indexers, manager, cached)) = &usenet {
                                let links = match cached.get(&item.info_hash.to_ascii_lowercase()) {
                                    Some(links) => links.clone(),
                                    None => indexers.lookup(&item.info_hash, Some(item.id)).await,
                                };
                                if !links.is_empty() {
                                    let listing = crate::engine::listing_from_bytes(nyaa::fetch_torrent(&client, item.id).await?)?;
                                    let all: Vec<usize> = listing.files.iter().map(|f| f.index).collect();
                                    return manager.add(&listing, &all, output_folder.clone(), links);
                                }
                            }
                            let bytes = nyaa::fetch_torrent(&client, item.id).await?;
                            match &backend {
                                Some(Ok(engine)) => engine.download(bytes, None, output_folder.clone()).await,
                                Some(Err(manager)) => {
                                    let listing = crate::engine::listing_from_bytes(bytes)?;
                                    let all: Vec<usize> = listing.files.iter().map(|f| f.index).collect();
                                    manager.add(provider, &listing, &all, output_folder.clone())
                                }
                                None => Err(anyhow::anyhow!("not on Usenet, and torrents can't start yet")),
                            }
                        }
                        .await;
                        if let Err(e) = result {
                            failed.push(format!("episode {ep:02}: {e:#}"));
                        }
                    }
                    let started = items.len() - failed.len();
                    let what = if started == 1 { "1 episode".to_owned() } else { format!("{started} episodes") };
                    Msg::DownloadStarted(if failed.is_empty() {
                        Ok(format!("{what} of {title}"))
                    } else {
                        Err(format!(
                            "Started {started} of {} episodes. Failed: {}",
                            items.len(),
                            failed.join("; ")
                        ))
                    })
                });
            }
        }
    }

    // ── Drawing ────────────────────────────────────────────────────────

    fn top_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("SeaDex Downloader");
            ui.add_space(8.0);
            if ui.selectable_label(matches!(self.page, Page::Home), "Library").clicked() {
                actions.push(Action::GoLibrary);
            }
            ui.add_space(8.0);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search anime, e.g. Frieren")
                    .desired_width(380.0),
            );
            if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(Action::Search);
            }
            if ui
                .add_enabled(!self.query.trim().is_empty(), egui::Button::new("Search"))
                .clicked()
            {
                actions.push(Action::Search);
            }
            if self.searching {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Stay open while options are clicked and the key is pasted;
                // a click outside or Esc closes it.
                let stay_open = MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                MenuButton::new("⚙ Settings").config(stay_open).ui(ui, |ui| self.settings_menu(ui, actions));
                let playlist = format!("Playlist ({})", self.playlist.items.len());
                if ui.button(playlist).on_hover_text("Remuxed episodes to export together").clicked() {
                    actions.push(Action::OpenPlaylist);
                }
                // Uploads carry on in the background; show them from anywhere.
                if let Some(fraction) = self.drive.upload_progress() {
                    let left = self.drive.upload_time_left().unwrap_or_default();
                    ui.add(egui::ProgressBar::new(fraction).desired_width(90.0).text(format!("⬆ {:.0}%", fraction * 100.0)))
                        .on_hover_text(format!("Uploading to Google Drive: {left}"));
                    ui.weak(left);
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                }
            });
        });
        ui.add_space(6.0);
    }

    fn settings_menu(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.set_min_width(380.0);
        self.debrid_settings(ui, actions);
        ui.separator();
        self.usenet_settings(ui, actions);
        ui.separator();
        if !self.debrid.config.enabled {
            self.vpn_settings(ui, actions);
            ui.separator();
        }
        ui.strong("Languages");
        ui.horizontal(|ui| {
            ui.label(self.settings.prefs.summary());
            if ui.small_button("Change…").clicked() {
                actions.push(Action::OpenLanguages);
            }
        });
        ui.weak("Which tracks play by default; series can have their own. Every track is kept.");
        ui.separator();
        ui.strong("Library folder");
        ui.horizontal(|ui| {
            let mut text = self.settings.library_dir.display().to_string();
            if ui
                .add(egui::TextEdit::singleline(&mut text).desired_width(280.0))
                .changed()
            {
                self.settings.library_dir = PathBuf::from(text);
            }
            if ui.button("Browse…").clicked() {
                actions.push(Action::PickLibraryFolder);
            }
        });
        ui.weak("Each series gets its own folder here; downloads go straight into it.");
        ui.separator();
        ui.checkbox(&mut self.settings.prefer_dual_audio, "Prefer dual-audio releases")
            .on_hover_text(
                "Between equally good releases (two SeaDex \"best\" picks, or two Nyaa releases \
                 from the same tier), pick the dual-audio one by default.",
            );
        ui.separator();
        if self.debrid.config.enabled {
            ui.weak("Nothing is seeded from this computer (TorBox is also told not to seed).");
        } else {
            ui.weak("Uploading is disabled: nothing is ever seeded, and each torrent");
            ui.weak("stops as soon as the files you picked are complete.");
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            // VPN matters only for torrents downloaded here, not via a debrid service.
            let local = !self.debrid.config.enabled;
            match &self.engine {
                EngineState::Off => {}
                EngineState::WaitingForVpn(_) => {
                    ui.spinner();
                    ui.label("Connecting to the VPN…");
                }
                EngineState::Ready(_) if local && self.vpn.waiting.is_some() => {
                    status_dot(ui, ui.visuals().warn_fg_color);
                    ui.label(RichText::new("Torrents waiting for the VPN").color(ui.visuals().warn_fg_color));
                }
                EngineState::Starting => {
                    ui.spinner();
                    ui.label("Starting torrent engine…");
                }
                EngineState::Ready(_) => {
                    status_dot(ui, GREEN);
                    ui.label(RichText::new("Engine ready").color(GREEN));
                }
                EngineState::Failed(e) => {
                    let red = ui.visuals().error_fg_color;
                    status_dot(ui, red);
                    ui.label(RichText::new("Engine failed").color(red)).on_hover_text(e);
                }
            }
            if local && self.vpn.config.enabled && !matches!(self.engine, EngineState::Off) {
                ui.separator();
                self.vpn_status(ui);
            }
            if self.debrid.service().is_some() {
                if !matches!(self.engine, EngineState::Off) {
                    ui.separator();
                }
                self.debrid_status(ui);
            }
            if self.usenet.active() {
                ui.separator();
                self.usenet_status(ui);
            }
            let mut clear = false;
            if let Some(status) = &self.status {
                ui.separator();
                let color = if status.error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.add(egui::Label::new(RichText::new(&status.text).color(color)).truncate())
                    .on_hover_text(&status.text);
                clear = ui.small_button("✖").on_hover_text("Dismiss").clicked();
            }
            if clear {
                self.status = None;
            }
        });
    }

    fn downloads_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.add_space(6.0);
        ui.heading("Downloads");
        ui.separator();
        let jobs = self.debrid.manager.views();
        let usenet_jobs = self.usenet.manager.views();
        let torrents = self.engine().map(|e| e.torrents()).unwrap_or_default();
        match &self.engine {
            EngineState::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Restoring downloads…");
                });
            }
            EngineState::Failed(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Torrent engine failed: {e}"));
            }
            EngineState::WaitingForVpn(reason) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(reason);
                });
            }
            _ => {}
        }
        if let Some(reason) = &self.vpn.waiting {
            if !self.debrid.config.enabled {
                self.vpn_banner(ui, reason, actions);
                ui.add_space(6.0);
            } else if torrents.iter().any(|(_, h)| !h.stats().finished) {
                // Downloads go through the service; only unfinished old torrents wait.
                ui.weak("Unfinished torrents from before carry on once your VPN connects.");
                ui.add_space(6.0);
            }
        }
        if jobs.is_empty() && usenet_jobs.is_empty() && torrents.is_empty() {
            if !matches!(self.engine, EngineState::Starting | EngineState::WaitingForVpn(_)) {
                ui.weak("Nothing here yet. Search for an anime, pick a release and choose episodes.");
            }
            return;
        }
        let mut any_active = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for view in &usenet_jobs {
                    any_active |= usenet_ui::usenet_card(ui, view, actions);
                    ui.add_space(6.0);
                }
                for view in &jobs {
                    any_active |= debrid_ui::debrid_card(ui, view, actions);
                    ui.add_space(6.0);
                }
                for (id, handle) in torrents.iter().rev() {
                    any_active |= torrent_card(ui, *id, handle, actions);
                    ui.add_space(6.0);
                }
            });
        ui.ctx().request_repaint_after(if any_active {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(2)
        });
    }

    fn results_page(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.results.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(format!("No anime found for “{}”.", self.last_query));
            });
            return;
        }
        let with_entry = self.results.iter().filter(|a| a.entry.is_some()).count();
        ui.label(format!(
            "{} results for “{}”, {with_entry} with a SeaDex entry",
            self.results.len(),
            self.last_query
        ));
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, anime) in self.results.iter().enumerate() {
                    let card = egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            cover(ui, anime.cover_small.as_deref(), vec2(52.0, 74.0));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&anime.title).strong().size(15.0));
                                if let Some(alt) = &anime.alt_title {
                                    ui.weak(alt);
                                }
                                ui.label(anime.summary());
                                ui.label(result_badge(anime, self.settings.prefer_dual_audio || self.settings.prefs.wants_dub()));
                            });
                        });
                    });
                    let response = card
                        .response
                        .interact(Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.hovered() {
                        ui.painter().rect_stroke(
                            response.rect,
                            4.0,
                            ui.visuals().widgets.hovered.bg_stroke,
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        actions.push(Action::OpenAnime(i));
                    }
                    ui.add_space(4.0);
                }
            });
    }

    fn anime_page(&self, ui: &mut Ui, ap: &mut AnimePage, actions: &mut Vec<Action>) {
        let back = if ap.from_library { "⏴ Library" } else { "⏴ Results" };
        if ui.button(back).clicked() {
            actions.push(Action::Back);
        }
        ui.add_space(6.0);
        let dual = self.prefer_dual_audio(ap.anime.anilist_id);
        let AnimePage { anime, chosen, nyaa, entry_loading, .. } = ap;
        let entry = anime.entry.as_ref();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let details_cover = self.details(anime.anilist_id).and_then(|d| d.cover.as_deref());
                    cover(ui, anime.cover_large.as_deref().or(details_cover), vec2(130.0, 185.0));
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.heading(&anime.title);
                        if let Some(alt) = &anime.alt_title {
                            ui.weak(alt);
                        }
                        ui.label(anime.summary());
                        self.details_section(ui, anime.anilist_id);
                        ui.horizontal(|ui| {
                            ui.hyperlink_to("AniList ↗", anime.anilist_url());
                            if entry.is_some() {
                                ui.hyperlink_to("releases.moe ↗", anime.seadex_url());
                            }
                        });
                        if let Some(entry) = entry {
                            if entry.incomplete {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    "⚠ SeaDex marks this entry as incomplete.",
                                );
                            }
                            if !entry.theoretical_best.is_empty() {
                                ui.label(format!("Theoretical best: {}", entry.theoretical_best));
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                self.description_section(ui, anime.anilist_id);
                ui.add_space(6.0);
                self.series_library_section(ui, anime, actions);
                ui.add_space(10.0);
                if *entry_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking up its releases on SeaDex…");
                    });
                }

                match (nyaa, entry) {
                    (Some(search), _) => self.nyaa_section(ui, entry.is_some(), search, actions),
                    (None, Some(entry)) => self.seadex_section(ui, entry, chosen, dual, actions),
                    (None, None) => {}
                }

                if let Some(entry) = entry
                    && !entry.notes.is_empty()
                {
                    ui.add_space(10.0);
                    egui::CollapsingHeader::new(RichText::new("SeaDex notes").strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label(&entry.notes);
                        });
                }
            });
    }

    /// Whether a release is on Usenet, for the release lists.
    fn usenet_cell(&self, ui: &mut Ui, hashes: &[&str]) {
        let found: Vec<Option<&[NzbLink]>> = hashes.iter().map(|h| self.usenet.links(h)).collect();
        if found.iter().any(Option::is_none) {
            ui.add(egui::Spinner::new().size(12.0)).on_hover_text("Checking the Usenet indexers…");
            return;
        }
        match found.as_slice() {
            [Some(links)] => usenet_ui::nzb_badge(ui, links),
            many => {
                let on = many.iter().filter(|l| l.is_some_and(|l| !l.is_empty())).count();
                if on == 0 {
                    ui.weak("—");
                } else {
                    ui.colored_label(GREEN, format!("NZB {on}/{}", many.len()))
                        .on_hover_text("Episodes on Usenet download from there; the rest as torrents.");
                }
            }
        }
    }

    fn seadex_section(&self, ui: &mut Ui, entry: &seadex::Entry, chosen: &mut Option<usize>, dual: bool, actions: &mut Vec<Action>) {
        ui.heading("Releases");
        ui.add_space(4.0);
        let usenet = self.usenet.active();
        egui::Grid::new("releases")
            .striped(true)
            .num_columns(if usenet { 8 } else { 7 })
            .spacing(vec2(14.0, 6.0))
            .show(ui, |ui| {
                for header in ["Release", "SeaDex", "Audio", "Files", "Size", "Tags", "Source"] {
                    ui.strong(header);
                }
                if usenet {
                    ui.strong("Usenet");
                }
                ui.end_row();
                for i in entry.ranked(dual) {
                    let r = &entry.releases[i];
                    let nyaa_id = r.nyaa_id();
                    ui.add_enabled_ui(nyaa_id.is_some(), |ui| {
                        ui.radio_value(chosen, Some(i), RichText::new(&r.group).strong());
                    });
                    if r.is_best {
                        ui.label(RichText::new("★ Best").color(GOLD));
                    } else {
                        ui.weak("Alt");
                    }
                    ui.label(if r.dual_audio { "Dual audio" } else { "Japanese" });
                    ui.label(r.files.len().to_string());
                    ui.label(fmt_bytes(r.total_size()));
                    ui.label(r.tags.join(", "));
                    match nyaa_id {
                        Some(id) => {
                            ui.hyperlink_to("Nyaa ↗", nyaa::view_url(id));
                        }
                        None => {
                            ui.weak(format!("{} (private)", r.tracker))
                                .on_hover_text("Private tracker: this app downloads from Nyaa only.");
                        }
                    }
                    if usenet {
                        match r.info_hash_hex().filter(|_| nyaa_id.is_some()) {
                            Some(hash) => self.usenet_cell(ui, &[hash]),
                            None => {
                                ui.weak("—");
                            }
                        }
                    }
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
        self.choose_button(ui, chosen.is_some(), actions);
    }

    /// The fallback for anime with nothing on SeaDex to download.
    fn nyaa_section(&self, ui: &mut Ui, has_entry: bool, search: &mut NyaaSearch, actions: &mut Vec<Action>) {
        ui.heading("Best on Nyaa");
        ui.weak(if has_entry {
            "SeaDex has no Nyaa release for this, so Nyaa releases are ranked by TRaSH Guides release-group tiers."
        } else {
            "Not on SeaDex, so Nyaa releases are ranked by TRaSH Guides release-group tiers."
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut search.query)
                    .hint_text("Search Nyaa")
                    .desired_width(320.0),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(Action::SearchNyaa);
            }
            let can_search = !search.loading && !search.query.trim().is_empty();
            if ui.add_enabled(can_search, egui::Button::new("Search Nyaa")).clicked() {
                actions.push(Action::SearchNyaa);
            }
            if search.loading {
                ui.spinner();
            }
        });
        if search.query.trim() == search.default_plan.display_query()
            && let Some(season) = search.default_plan.season
        {
            ui.weak(format!("Showing season {season} only. Edit the search to see everything."));
        }
        if let Some(e) = &search.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if search.searched && !search.loading && search.releases.is_empty() && search.error.is_none() {
            ui.label("Nothing found on Nyaa. Try another name for the show, e.g. its English title.");
        }

        if !search.releases.is_empty() {
            ui.add_space(8.0);
            // Above the (long) list, so it's visible without scrolling.
            ui.horizontal(|ui| {
                self.choose_button(ui, search.chosen.is_some(), actions);
                if let Some(r) = search.chosen.and_then(|i| search.releases.get(i)) {
                    ui.add(egui::Label::new(RichText::new(nyaa_release_label(r)).strong()).truncate());
                }
            });
            ui.add_space(6.0);
            let NyaaSearch { releases, chosen, .. } = search;
            let usenet = self.usenet.active();
            egui::Grid::new("nyaa_releases")
                .striped(true)
                .num_columns(if usenet { 9 } else { 8 })
                .spacing(vec2(14.0, 6.0))
                .show(ui, |ui| {
                    for header in ["Release", "Ranking", "Episodes", "Quality", "Audio", "Size", "Seeders", "Source"] {
                        ui.strong(header);
                    }
                    if usenet {
                        ui.strong("Usenet");
                    }
                    ui.end_row();
                    for (i, r) in releases.iter().enumerate().take(MAX_NYAA_ROWS) {
                        let problem = r.problem();
                        ui.add_enabled_ui(problem.is_none(), |ui| {
                            ui.radio_value(chosen, Some(i), RichText::new(&r.group).strong())
                                .on_hover_text(r.title());
                        });
                        let rank = if r.rank.is_blocked() {
                            RichText::new(&r.rank.label).color(ui.visuals().error_fg_color)
                        } else if r.rank.score > 0 {
                            RichText::new(format!("★ {}", r.rank.label)).color(GOLD)
                        } else {
                            RichText::new(&r.rank.label).weak()
                        };
                        let rank = ui.label(rank);
                        if let Some(problem) = problem {
                            rank.on_hover_text(problem);
                        }
                        if r.incomplete {
                            ui.weak(format!("{} (incomplete)", r.episodes_label()))
                                .on_hover_text("Has far fewer episodes than other releases, so it ranks lower.");
                        } else {
                            ui.label(r.episodes_label());
                        }
                        ui.label(r.quality_label());
                        ui.label(if r.dual_audio { "Dual audio" } else { "" });
                        ui.label(fmt_bytes(r.size));
                        if r.max_seeders == 0 {
                            ui.colored_label(ui.visuals().error_fg_color, "0");
                        } else {
                            ui.label(r.seeders_label());
                        }
                        match &r.kind {
                            ReleaseKind::Torrent { item, .. } => {
                                ui.hyperlink_to("Nyaa ↗", nyaa::view_url(item.id));
                            }
                            ReleaseKind::Episodes(_) => {
                                ui.weak("Per episode");
                            }
                        }
                        if usenet {
                            let hashes: Vec<&str> = match &r.kind {
                                ReleaseKind::Torrent { item, .. } => vec![item.info_hash.as_str()],
                                ReleaseKind::Episodes(items) => items.iter().map(|(_, i)| i.info_hash.as_str()).collect(),
                            };
                            self.usenet_cell(ui, &hashes);
                        }
                        ui.end_row();
                    }
                });
            if releases.len() > MAX_NYAA_ROWS {
                ui.weak(format!("{} lower-ranked releases not shown.", releases.len() - MAX_NYAA_ROWS));
            }
        }
    }

    fn choose_button(&self, ui: &mut Ui, has_choice: bool, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            // Releases on Usenet don't need the torrent engine or a debrid key.
            let ready = if self.usenet.active() { Ok(()) } else { self.download_ready() };
            let enabled = has_choice && ready.is_ok() && !self.loading_listing;
            let button = egui::Button::new(RichText::new("Choose episodes ⏵").strong()).min_size(vec2(170.0, 32.0));
            if ui.add_enabled(enabled, button).clicked() {
                actions.push(Action::ChooseFiles);
            }
            if self.loading_listing {
                ui.spinner();
                ui.label("Fetching the torrent from Nyaa…");
            } else if let Err(why) = &ready {
                ui.weak(why);
            } else if !has_choice {
                ui.weak("No release to download.");
            }
        });
    }

    fn files_page(&self, ui: &mut Ui, fp: &mut FilesPage, actions: &mut Vec<Action>) {
        egui::Panel::top("files_header")
            .resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("⏴ Back").clicked() {
                        actions.push(Action::Back);
                    }
                    ui.heading("Choose episodes");
                });
                ui.add(egui::Label::new(RichText::new(fp.title()).strong()).truncate());
                if !fp.subtitle.is_empty() {
                    ui.weak(&fp.subtitle);
                }
                if fp.existing() {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "This torrent is already in your downloads. Saving changes which files it fetches.",
                    );
                } else if self.via_usenet(fp) {
                    let names: Vec<&str> = fp.nzbs.iter().map(|l| l.indexer.name()).collect();
                    ui.colored_label(GREEN, format!("On Usenet ({}): downloads from your Usenet providers", names.join(", ")));
                } else if let Some(nzbs) = self.episode_nzbs(fp) {
                    let on = nzbs.iter().filter(|l| l.is_some_and(|l| !l.is_empty())).count();
                    if nzbs.iter().any(Option::is_none) {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak("Checking which episodes are on Usenet…");
                        });
                    } else if on > 0 {
                        let mut names: Vec<&str> = nzbs.iter().flatten().flat_map(|l| l.iter().map(|x| x.indexer.name())).collect();
                        names.sort_unstable();
                        names.dedup();
                        ui.colored_label(
                            GREEN,
                            format!("On Usenet ({}): {on} of {} episodes download from your Usenet providers", names.join(", "), nzbs.len()),
                        );
                    }
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("All").clicked() {
                        fp.checked.fill(true);
                    }
                    if ui.button("None").clicked() {
                        fp.checked.fill(false);
                    }
                    if ui.button("Invert").clicked() {
                        fp.checked.iter_mut().for_each(|c| *c = !*c);
                    }
                    if matches!(fp.picker, Picker::Files(_))
                        && ui
                            .button("Videos only")
                            .on_hover_text("Select video files and deselect everything else")
                            .clicked()
                    {
                        for (c, r) in fp.checked.iter_mut().zip(&fp.rows) {
                            *c = is_video(&r.label);
                        }
                    }
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut fp.filter)
                            .hint_text("Filter, e.g. S01E0 or NCOP")
                            .desired_width(220.0),
                    );
                    if !fp.filter.trim().is_empty() {
                        let visible = fp.visible();
                        if ui.button(format!("Select {} shown", visible.len())).clicked() {
                            visible.iter().for_each(|&i| fp.checked[i] = true);
                        }
                        if ui.button("Deselect shown").clicked() {
                            visible.iter().for_each(|&i| fp.checked[i] = false);
                        }
                    }
                });
            });

        egui::Panel::bottom("files_footer")
            .resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 6)))
            .show(ui, |ui| {
                let (count, size) = fp
                    .rows
                    .iter()
                    .zip(&fp.checked)
                    .filter(|(_, c)| **c)
                    .fold((0usize, 0u64), |(n, s), (r, _)| (n + 1, s + r.len));
                let existing = fp.existing();
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Picked episodes on Usenet download from there; the rest as torrents.
                        let (on, off) = match self.episode_nzbs(fp) {
                            Some(nzbs) => fp.selected().iter().fold((0, 0), |(on, off), &i| {
                                if nzbs[i].is_some_and(|l| !l.is_empty()) { (on + 1, off) } else { (on, off + 1) }
                            }),
                            None => (0, count),
                        };
                        let usenet = self.via_usenet(fp) || (on > 0 && off == 0);
                        let ready = existing || usenet || self.download_ready().is_ok();
                        let label = match self.debrid.service() {
                            _ if existing => "Update download".to_owned(),
                            _ if usenet => "⬇ Download from Usenet".to_owned(),
                            service if on > 0 => {
                                let rest = service.map_or("as torrents".to_owned(), |s| format!("via {s}"));
                                format!("⬇ Download: {on} from Usenet, {off} {rest}")
                            }
                            Some(service) => format!("⬇ Download via {service}"),
                            None => "⬇ Download".to_owned(),
                        };
                        let button = egui::Button::new(RichText::new(label).strong()).min_size(vec2(150.0, 32.0));
                        if ui
                            .add_enabled(count > 0 && ready && !self.starting_download, button)
                            .clicked()
                        {
                            actions.push(Action::StartDownload);
                        }
                        if self.starting_download {
                            ui.spinner();
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{count} of {} {} · {}",
                                    fp.rows.len(),
                                    fp.unit(),
                                    fmt_bytes(size)
                                ))
                                .strong(),
                            );
                            ui.separator();
                            ui.label("Save to");
                            if !existing && ui.small_button("Change…").clicked() {
                                actions.push(Action::PickFolder);
                            }
                            let folder = fp.output_folder(&self.library_dir());
                            let folder = folder.display().to_string();
                            ui.add(egui::Label::new(RichText::new(&folder).monospace()).truncate())
                                .on_hover_text(&folder);
                        });
                    });
                });
            });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            let visible = fp.visible();
            let heading = match fp.picker {
                Picker::Files(_) => "File",
                Picker::Episodes { .. } => "Episode",
            };
            let show_seeders = matches!(fp.picker, Picker::Episodes { .. });
            let usenet_hashes: Option<Vec<String>> = match &fp.picker {
                Picker::Episodes { items, .. } if self.usenet.active() => Some(items.iter().map(|(_, i)| i.info_hash.clone()).collect()),
                _ => None,
            };
            let FilesPage { rows, checked, .. } = fp;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .auto_shrink([false, false])
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::remainder().clip(true))
                .column(Column::auto().at_least(90.0));
            if show_seeders {
                table = table.column(Column::auto().at_least(70.0));
            }
            if usenet_hashes.is_some() {
                table = table.column(Column::auto().at_least(60.0));
            }
            table
                .header(22.0, |mut header| {
                    header.col(|ui| {
                        ui.strong(format!("{heading} ({} shown)", visible.len()));
                    });
                    header.col(|ui| {
                        ui.strong("Size");
                    });
                    if show_seeders {
                        header.col(|ui| {
                            ui.strong("Seeders");
                        });
                    }
                    if usenet_hashes.is_some() {
                        header.col(|ui| {
                            ui.strong("Usenet");
                        });
                    }
                })
                .body(|body| {
                    body.rows(24.0, visible.len(), |mut row| {
                        let i = visible[row.index()];
                        let r = &rows[i];
                        row.col(|ui| {
                            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                            ui.checkbox(&mut checked[i], r.label.as_str()).on_hover_text(&r.label);
                        });
                        row.col(|ui| {
                            ui.label(fmt_bytes(r.len));
                        });
                        if let Some(seeders) = r.seeders {
                            row.col(|ui| {
                                ui.label(seeders.to_string());
                            });
                        }
                        if let Some(hashes) = &usenet_hashes {
                            row.col(|ui| self.usenet_cell(ui, &[hashes[i].as_str()]));
                        }
                    });
                });
        });
    }

    fn delete_modal(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((target, name)) = &self.confirm_delete else { return };
        let (target, name) = (*target, name.clone());
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Delete downloaded files?");
            ui.add_space(6.0);
            match target {
                DeleteTarget::Series(id) => {
                    let folder = self.library.find(id).map(|s| s.folder.display().to_string()).unwrap_or_default();
                    ui.label(format!("This removes “{name}” from your library and deletes its folder, {folder}, with everything in it. It can't be undone."));
                }
                _ => {
                    ui.label(format!("This removes “{name}” and deletes its files from disk. It can't be undone."));
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let delete = egui::Button::new(
                    RichText::new("Delete files").color(ui.visuals().error_fg_color),
                );
                if ui.add(delete).clicked() {
                    actions.push(match target {
                        DeleteTarget::Torrent(id) => Action::Remove { id, delete_files: true },
                        DeleteTarget::Debrid(id) => Action::DebridRemove { id, delete_files: true },
                        DeleteTarget::Usenet(id) => Action::UsenetRemove { id, delete_files: true },
                        DeleteTarget::Series(id) => Action::RemoveSeries { id, delete_files: true },
                    });
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.confirm_delete = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.handle_messages();
        self.sync_engine();
        self.request_nzb_lookups();
        self.tidy_library();
        self.apply_defaults();
        self.refresh_library_stats();
        self.request_details();
        let mut actions = Vec::new();

        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui, &mut actions));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("downloads")
            .resizable(true)
            .default_size(400.0)
            .show(ui, |ui| self.downloads_panel(ui, &mut actions));

        // Take the page out so the page functions can borrow it next to `self`.
        let mut page = std::mem::replace(&mut self.page, Page::Home);
        egui::CentralPanel::default_margins().show(ui, |ui| match &mut page {
            Page::Home => self.library_page(ui, &mut actions),
            Page::Results => self.results_page(ui, &mut actions),
            Page::Anime(ap) => self.anime_page(ui, ap, &mut actions),
            Page::Files(fp) => self.files_page(ui, fp, &mut actions),
            Page::Remux(rp) => self.remux_page(ui, rp, &mut actions),
            Page::Playlist(pp) => self.playlist_page(ui, pp, &mut actions),
        });
        self.page = page;

        let ctx = ui.ctx().clone();
        self.delete_modal(&ctx, &mut actions);
        self.usenet_dialog(&ctx, &mut actions);
        self.languages_dialog(&ctx);
        self.folder_browser_modal(&ctx, &mut actions);

        for action in actions {
            self.perform(action);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn on_exit(&mut self) {
        // Debrid and Usenet downloads resume from their partial files next time.
        self.debrid.manager.stop();
        self.usenet.manager.stop();
        // Pause torrents cleanly so they resume quickly next time.
        if let Some(engine) = self.engine() {
            self.rt.block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(3), engine.stop()).await;
            });
        }
    }
}

/// Draw one torrent in the downloads panel. Returns whether it is busy
/// (so the panel refreshes faster).
fn torrent_card(ui: &mut Ui, id: TorrentId, handle: &TorrentHandle, actions: &mut Vec<Action>) -> bool {
    let stats = handle.stats();
    let name = handle.name().unwrap_or_else(|| format!("Torrent #{id}"));
    // Finished torrents are stopped by the engine right away (no seeding).
    let (state, busy) = match &stats.state {
        TorrentStatsState::Initializing { .. } => ("Checking files", true),
        _ if stats.finished => ("Done · not seeding", false),
        TorrentStatsState::Live => ("Downloading", true),
        TorrentStatsState::Paused => ("Paused", false),
        TorrentStatsState::Error => ("Error", false),
    };

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(&name).strong()).truncate())
            .on_hover_text(&name);

        let fraction = if stats.total_bytes == 0 {
            0.0
        } else {
            stats.progress_bytes as f32 / stats.total_bytes as f32
        };
        ui.add(egui::ProgressBar::new(fraction).text(format!(
            "{:.1}%  ·  {} of {}",
            fraction * 100.0,
            fmt_bytes(stats.progress_bytes),
            fmt_bytes(stats.total_bytes)
        )));

        let mut detail = state.to_owned();
        if let Some(live) = &stats.live
            && !stats.finished
        {
            detail.push_str(&format!("  ·  ⬇ {}", fmt_speed(live.download_speed.mbps)));
            if let Some(eta) = &live.time_remaining {
                detail.push_str(&format!("  ·  {eta} left"));
            }
            detail.push_str(&format!("  ·  {} peers", live.snapshot.peer_stats.live));
        }
        ui.label(RichText::new(detail).weak());
        if let Some(err) = &stats.error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }

        ui.horizontal_wrapped(|ui| {
            match stats.state {
                _ if stats.finished => {}
                TorrentStatsState::Live => {
                    if ui.button("⏸ Pause").clicked() {
                        actions.push(Action::Pause(handle.clone()));
                    }
                }
                TorrentStatsState::Paused | TorrentStatsState::Error => {
                    if ui.button("▶ Resume").clicked() {
                        actions.push(Action::Resume(handle.clone()));
                    }
                }
                TorrentStatsState::Initializing { .. } => {}
            }
            if ui
                .button("Episodes…")
                .on_hover_text("Change which files this torrent downloads")
                .clicked()
            {
                actions.push(Action::EditFiles(handle.clone()));
            }
            if ui
                .button("Remux…")
                .on_hover_text("Make copies with only the audio and subtitle languages you want")
                .clicked()
            {
                actions.push(Action::OpenRemux(handle.clone()));
            }
            if ui.button("🗁 Folder").clicked() {
                actions.push(Action::OpenFolder(handle.output_folder().to_owned()));
            }
            ui.menu_button("Remove", |ui| {
                if ui.button("Remove from list (keep files)").clicked() {
                    actions.push(Action::Remove { id, delete_files: false });
                }
                if ui.button("Remove and delete files…").clicked() {
                    actions.push(Action::AskDelete { id, name: name.clone() });
                }
            });
        });

        egui::CollapsingHeader::new("Files")
            .id_salt(("torrent_files", id))
            .show(ui, |ui| file_progress(ui, handle, &stats.file_progress));
    });
    busy
}

/// Per-file progress for the files a torrent is set to download.
fn file_progress(ui: &mut Ui, handle: &TorrentHandle, progress: &[u64]) {
    let only = handle.only_files();
    let shown = handle.with_metadata(|m| {
        for (i, file) in m.file_infos.iter().enumerate() {
            if file.attrs.padding || only.as_ref().is_some_and(|o| !o.contains(&i)) {
                continue;
            }
            let have = progress.get(i).copied().unwrap_or(0);
            let fraction = if file.len == 0 { 1.0 } else { have as f32 / file.len as f32 };
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(64.0)
                        .text(format!("{:.0}%", fraction * 100.0)),
                );
                let path = file.relative_filename.display().to_string();
                ui.add(egui::Label::new(RichText::new(&path).weak()).truncate())
                    .on_hover_text(path);
            });
        }
    });
    if shown.is_err() {
        ui.weak("File list not available yet.");
    }
}

const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
const GOLD: egui::Color32 = egui::Color32::from_rgb(220, 170, 40);

fn status_dot(ui: &mut Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn cover(ui: &mut Ui, url: Option<&str>, size: egui::Vec2) {
    match url {
        Some(url) => {
            ui.add(
                egui::Image::new(url.to_owned())
                    .fit_to_exact_size(size)
                    .corner_radius(4),
            );
        }
        None => {
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
        }
    }
}

/// What the library needs to know to add an anime.
fn new_series(anime: &Anime) -> crate::library::NewSeries {
    crate::library::NewSeries {
        anilist_id: anime.anilist_id,
        english: anime.english.clone(),
        romaji: anime.romaji.clone(),
        cover: anime.cover_large.clone().or(anime.cover_small.clone()),
        format: anime.format.clone(),
        year: anime.year,
        episodes: anime.episodes,
    }
}

fn result_badge(anime: &Anime, prefer_dual_audio: bool) -> RichText {
    let Some(entry) = &anime.entry else {
        return RichText::new("Not on SeaDex · will rank Nyaa releases by group").weak();
    };
    match entry.best_nyaa(prefer_dual_audio) {
        Some(i) => {
            let r = &entry.releases[i];
            let kind = if r.is_best { "Best" } else { "Alt" };
            let audio = if r.dual_audio { " · dual audio" } else { "" };
            RichText::new(format!("★ {kind} on Nyaa: {}{audio}", r.group)).color(GOLD)
        }
        None => RichText::new("On SeaDex, but not on Nyaa · will rank Nyaa releases by group").weak(),
    }
}

fn nyaa_release_label(r: &NyaaRelease) -> String {
    let mut parts = vec![r.group.clone(), r.rank.label.clone()];
    let quality = r.quality_label();
    if !quality.is_empty() {
        parts.push(quality);
    }
    if r.dual_audio {
        parts.push("dual audio".into());
    }
    parts.push(r.episodes_label());
    parts.join(" · ")
}

/// A 40-character hex info hash, or `None`.
fn valid_info_hash(hash: &str) -> Option<String> {
    let h = hash.trim();
    (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_owned())
}

fn chosen_release(ap: &AnimePage) -> Option<&Release> {
    ap.anime.entry.as_ref()?.releases.get(ap.chosen?)
}

fn release_label(r: &Release) -> String {
    let mut parts = vec![r.group.clone()];
    parts.push(if r.is_best { "SeaDex best".into() } else { "SeaDex alt".into() });
    if r.dual_audio {
        parts.push("dual audio".into());
    }
    if !r.tags.is_empty() {
        parts.push(r.tags.join(", "));
    }
    parts.join(" · ")
}

fn is_video(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(ext)))
}

#[cfg(test)]
pub(crate) fn fmt_bytes_pub(bytes: u64) -> String {
    fmt_bytes(bytes)
}

fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A path with a leading `~` expanded.
fn expand_home(dir: &Path) -> PathBuf {
    match (dir.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => dir.to_owned(),
    }
}

/// A time left, rounded the way people say it.
fn fmt_duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

fn fmt_speed(mib_per_sec: f64) -> String {
    if mib_per_sec >= 1.0 {
        format!("{mib_per_sec:.1} MiB/s")
    } else {
        format!("{:.0} KiB/s", mib_per_sec * 1024.0)
    }
}

/// Open a folder in the system file manager (its nearest existing parent if
/// it hasn't been created yet).
fn open_folder(path: &Path) -> std::io::Result<()> {
    let target = path
        .ancestors()
        .find(|p| p.is_dir())
        .unwrap_or(path)
        .to_owned();
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opener = "xdg-open";
    let mut child = std::process::Command::new(opener).arg(target).spawn()?;
    // Reap it so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod screens_test;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TorrentFile;

    fn listing(paths: &[&str]) -> Listing {
        Listing {
            name: "t".into(),
            torrent_bytes: vec![],
            files: paths
                .iter()
                .enumerate()
                .map(|(i, p)| TorrentFile { index: i * 2, path: (*p).into(), len: 10 })
                .collect(),
            multi_file: true,
            existing: None,
        }
    }

    #[test]
    fn filter_matches_all_terms_case_insensitively() {
        let mut fp = FilesPage::new(
            listing(&["Show S01E01.mkv", "Show S01E02.mkv", "Extras/NCOP1.mkv"]),
            None,
            String::new(),
        );
        fp.filter = "s01e0 MKV".into();
        assert_eq!(fp.visible(), vec![0, 1]);
        fp.filter = "ncop".into();
        assert_eq!(fp.visible(), vec![2]);
        fp.filter = "  ".into();
        assert_eq!(fp.visible(), vec![0, 1, 2]);
    }

    #[test]
    fn selection_maps_to_torrent_file_indices() {
        let mut fp = FilesPage::new(listing(&["a.mkv", "b.mkv", "c.mkv"]), None, String::new());
        assert_eq!(fp.selected_indices(), vec![0, 2, 4]);
        fp.checked[1] = false;
        assert_eq!(fp.selected_indices(), vec![0, 4]);
    }

    #[test]
    fn existing_selection_is_restored() {
        let mut l = listing(&["a.mkv", "b.mkv", "c.mkv"]);
        l.existing = Some(crate::engine::Existing {
            only_files: Some(vec![2]),
            output_folder: PathBuf::from("/x"),
        });
        let fp = FilesPage::new(l, None, String::new());
        assert_eq!(fp.checked, vec![false, true, false]);
    }

    #[test]
    fn helpers() {
        assert!(is_video("Show/Ep 01.MKV"));
        assert!(!is_video("Show/Ep 01.ass"));
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(fmt_speed(0.5), "512 KiB/s");
    }
}
