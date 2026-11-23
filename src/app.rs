//! The app: searching, downloading (torrents, debrid, Usenet), the
//! library, remuxing, the playlist and your lists. The GUI (`gui`, iced)
//! draws it and turns clicks into its actions.
//!
//! The UI thread never blocks: network and torrent work runs on the tokio
//! runtime and reports back through `Msg`s. Clicks are collected as `Action`s
//! while drawing and handled after the frame is laid out.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::{
    Engine, Listing, Source, TorrentHandle, TorrentId, info_hash_of, sanitize_folder_name,
};
use crate::fallback::{self, NyaaRelease, ReleaseKind, SearchPlan};
use crate::library::{AnimeRef, Library};
use crate::nyaa::{self, NyaaItem};
use crate::playlist::{Item, Playlist};
use crate::remux::Analysis;
use crate::seadex::{self, Anime, Release};
use crate::usenet::indexers::NzbLink;

mod debrid_ui;
mod discover_ui;
mod discuss_ui;
mod drive_ui;
pub(crate) mod gui;
mod handover;
mod library_ui;
mod manage_ui;
mod playlist_page;
mod remux_page;
mod servers_ui;
mod tracking_ui;
mod usenet_ui;
mod vpn_ui;
mod watching;
use debrid_ui::DebridState;
use drive_ui::DriveState;
use handover::Handover;
use library_ui::{LibraryState, PagesState};
use playlist_page::PlaylistPage;
use remux_page::RemuxPage;
use usenet_ui::UsenetState;
use vpn_ui::VpnState;

/// How many Nyaa releases to list.
const MAX_NYAA_ROWS: usize = 40;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "webm", "mov", "wmv", "ts", "m2ts",
];

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
    player: crate::player::Settings,
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
            player: Default::default(),
        }
    }
}

impl Settings {
    /// `settings.json` in the state folder; the first time, what the egui
    /// app kept in eframe's `app.ron` there.
    fn load(state_dir: &Path) -> Self {
        let file = state_dir.join("settings.json");
        if let Ok(bytes) = std::fs::read(&file) {
            match serde_json::from_slice(&bytes) {
                Ok(s) => return s,
                Err(e) => tracing::warn!("couldn't read {}: {e}", file.display()),
            }
        }
        let old = std::fs::read_to_string(state_dir.join("app.ron"))
            .ok()
            .and_then(|text| ron::from_str::<std::collections::HashMap<String, String>>(&text).ok())
            .and_then(|stored| ron::from_str(stored.get("app")?).ok());
        old.unwrap_or_default()
    }

    fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// How background work wakes the GUI (to show its results), and opens links
/// in the browser.
#[derive(Clone)]
pub struct Waker {
    wake: Arc<dyn Fn() + Send + Sync>,
    open: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Waker {
    pub fn new(
        wake: impl Fn() + Send + Sync + 'static,
        open: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            wake: Arc::new(wake),
            open: Arc::new(open),
        }
    }

    /// Opens links with the system's browser.
    pub fn with_browser(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::new(wake, |url| {
            if let Err(e) = open_folder(Path::new(url)) {
                tracing::warn!("couldn't open {url}: {e}");
            }
        })
    }

    pub fn request_repaint(&self) {
        (self.wake)();
    }

    pub fn open_url(&self, url: &str) {
        (self.open)(url);
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
    Tracking(tracking_ui::TrackingMsg),
    Servers(servers_ui::ServersMsg),
    Manage(manage_ui::ManageMsg),
    Engine {
        seq: u64,
        result: Result<Arc<Engine>, String>,
    },
    VpnFixDone(Result<(), String>),
    SearchDone {
        seq: u64,
        result: Result<Vec<Anime>, String>,
    },
    ListingDone {
        seq: u64,
        subtitle: String,
        result: Result<Listing, String>,
        nzbs: Vec<NzbLink>,
    },
    NyaaDone {
        seq: u64,
        result: Result<Vec<NyaaRelease>, String>,
    },
    DownloadStarted(Result<String, String>),
    FolderPicked(Option<PathBuf>),
    LibraryFolderPicked(Option<PathBuf>),
    RemuxProbed {
        seq: u64,
        result: Result<Analysis, String>,
    },
    RemuxTitle {
        seq: u64,
        found: Option<AnimeRef>,
    },
    RemuxSamples {
        seq: u64,
        samples: Vec<(crate::remux::TrackSig, crate::remux::SubtitleSample)>,
    },
    RemuxProgress {
        seq: u64,
        index: usize,
        fraction: f32,
    },
    RemuxFileDone {
        seq: u64,
        index: usize,
        result: Result<(), String>,
    },
    RemuxFinished {
        seq: u64,
    },
    RemuxFolderPicked(Option<PathBuf>),
    PlaylistAdd {
        items: Vec<Item>,
        errors: Vec<String>,
    },
    PlaylistExportProgress {
        seq: u64,
        fraction: f32,
    },
    PlaylistExportDone {
        seq: u64,
        result: Result<PathBuf, String>,
    },
    PlaylistFolderPicked(Option<PathBuf>),
    DriveClientFile(Result<crate::gdrive::OAuthClient, String>),
    DriveSignInUrl {
        seq: u64,
        url: String,
    },
    DriveSignedIn {
        seq: u64,
        result: Result<(String, String), String>,
    },
    DriveFolders {
        seq: u64,
        result: Result<Vec<crate::gdrive::Entry>, String>,
        expired: bool,
    },
    DriveLinkChecked {
        result: Result<crate::gdrive::Folder, String>,
        expired: bool,
    },
    DriveUploadProgress {
        seq: u64,
        fraction: f32,
    },
    DriveUploadDone {
        seq: u64,
        result: Result<crate::gdrive::Uploaded, String>,
        expired: bool,
    },
    DebridAccount {
        seq: u64,
        provider: crate::debrid::Provider,
        result: Result<crate::debrid::Account, String>,
    },
    NzbLookup {
        hash: String,
        links: Vec<NzbLink>,
    },
    Moved {
        key: String,
        name: String,
        result: Result<Vec<(PathBuf, PathBuf)>, String>,
    },
    DefaultsApplied {
        anilist_id: u64,
        rel: String,
        applied: crate::library::Applied,
    },
    DefaultsDone,
    HandedOver(Result<String, String>),
    ReleaseFiles {
        id: u64,
        result: Result<Listing, String>,
    },
    Details {
        id: u64,
        result: Result<Box<crate::library::Details>, String>,
    },
    SeriesEntry {
        id: u64,
        result: Result<Option<seadex::Entry>, String>,
    },
    Discovered {
        filter: crate::browse::Filter,
        result: Result<Vec<crate::browse::Media>, String>,
    },
    Schedule {
        week: chrono::NaiveDate,
        result: Result<Vec<crate::browse::Airing>, String>,
    },
    /// MyAnimeList's episodes of an anime (and its ID there).
    MalEpisodes {
        id: u64,
        result: Result<(Option<u64>, Vec<crate::discuss::MalEpisode>), String>,
    },
    Discussion {
        seq: u64,
        site: crate::discuss::Site,
        /// The next page of what's there (else the first).
        more: bool,
        result: Result<Option<Box<crate::discuss::Thread>>, String>,
    },
    UsenetChecked {
        id: u64,
        seq: u64,
        result: Result<(), String>,
    },
    Info(String),
    Error(String),
}

/// Things the user clicked this frame.
#[derive(Clone)]
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
    Remove {
        id: TorrentId,
        delete_files: bool,
    },
    AskDelete {
        id: TorrentId,
        name: String,
    },
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
    PlaylistMove {
        from: usize,
        to: usize,
    },
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
    DebridRemove {
        id: u64,
        delete_files: bool,
    },
    DebridAskDelete {
        id: u64,
        name: String,
    },
    DebridEditFiles(u64),
    DebridOpenRemux(u64),
    VpnFix(crate::vpn::Fix),
    UsenetPause(u64),
    UsenetResume(u64),
    UsenetRemove {
        id: u64,
        delete_files: bool,
    },
    UsenetAskDelete {
        id: u64,
        name: String,
    },
    UsenetEditFiles(u64),
    UsenetOpenRemux(u64),
    UsenetOpenProviders,
    UsenetCheck(u64),
    GoLibrary,
    GoDownloads,
    GoDiscover,
    GoCalendar,
    /// An anime from Discover or the calendar.
    OpenMedia(Box<crate::browse::Media>, Origin),
    /// An episode's discussion on AniList and MyAnimeList.
    OpenDiscussion {
        id: u64,
        episode: u32,
    },
    DiscussionMore,
    OpenSeries(u64),
    AddToLibrary,
    RemoveSeries {
        id: u64,
        delete_files: bool,
    },
    AskRemoveSeries(u64),
    OpenLanguages,
    SetSeriesPrefs {
        id: u64,
        prefs: Option<crate::prefs::LangPrefs>,
    },
    RemuxLibrary {
        id: u64,
        files: Vec<PathBuf>,
    },
    DownloadEpisodes(Vec<u32>),
    /// Library episodes of a series, to the end of the playlist.
    AddToPlaylist {
        id: u64,
        files: Vec<PathBuf>,
    },
    /// For the GUI's player.
    Player(watching::PlayerAction),
    /// Remove several downloads at once: deleting their files, or (queue
    /// items) blocklisting their releases.
    RemoveMany {
        items: Vec<Picked>,
        delete_files: bool,
        blocklist: bool,
    },
    /// Ask before that.
    AskRemoveMany(Vec<Picked>),
    /// Ask before deleting a remuxed copy (or a series' folder of them).
    AskDeleteRemux(PathBuf),
    DeleteRemux(PathBuf),
    Tracking(tracking_ui::TrackingAction),
    Servers(servers_ui::ServersAction),
    Manage(manage_ui::ManageAction),
}

/// A download picked on the Downloads page (to remove several at once).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Picked {
    Torrent(TorrentId),
    Debrid(u64),
    Usenet(u64),
    /// In Sonarr's or Radarr's queue.
    Queued(crate::servers::arr::Kind, u64),
}

/// What the delete confirmation is about.
#[derive(Clone)]
enum DeleteTarget {
    Torrent(TorrentId),
    Debrid(u64),
    Usenet(u64),
    /// A series' whole folder.
    Series(u64),
    /// A remuxed copy, or a folder of them.
    Remux(PathBuf),
    /// Something in Sonarr's or Radarr's queue.
    Queued(crate::servers::arr::Kind, u64),
    /// Several downloads (or queue items) at once.
    Many(Vec<Picked>),
    /// Files Sonarr or Radarr has (from the page's series).
    ArrFiles(Vec<u64>),
    /// The page's series in Sonarr (or film in Radarr).
    ArrOwner,
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
    /// Everything downloading (and whatever's not in the library).
    Downloads,
    Anime(AnimePage),
    Files(FilesPage),
    Remux(RemuxPage),
    Playlist(PlaylistPage),
    /// Folders, languages, accounts and services (the iced GUI's page).
    Settings,
    /// AniList's lists of anime.
    Discover,
    /// What's airing, day by day.
    Calendar,
}

struct AnimePage {
    anime: Anime,
    /// Index into the SeaDex entry's releases.
    chosen: Option<usize>,
    /// Set when SeaDex has no Nyaa release for this anime.
    nyaa: Option<NyaaSearch>,
    /// Where it was opened from (Back goes there).
    origin: Origin,
    /// Its SeaDex entry is being looked up.
    entry_loading: bool,
}

impl AnimePage {
    fn opened_from_library(&self) -> bool {
        self.origin == Origin::Library
    }
}

/// The page an anime's page was opened from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Library,
    Results,
    Discover,
    Calendar,
}

impl Origin {
    fn name(self) -> &'static str {
        match self {
            Origin::Library => "Library",
            Origin::Results => "Results",
            Origin::Discover => "Discover",
            Origin::Calendar => "Calendar",
        }
    }
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
        let checked = match listing
            .existing
            .as_ref()
            .and_then(|e| e.only_files.as_ref())
        {
            Some(only) => listing
                .files
                .iter()
                .map(|f| only.contains(&f.index))
                .collect(),
            None => vec![true; listing.files.len()],
        };
        let rows = listing
            .files
            .iter()
            .map(|f| Row {
                label: f.path.clone(),
                len: f.len,
                seeders: None,
            })
            .collect();
        Self {
            back,
            subtitle,
            picker: Picker::Files(listing),
            rows,
            checked,
            filter: String::new(),
            nzbs: Vec::new(),
            folder: None,
        }
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
            picker: Picker::Episodes {
                items,
                title,
                folder,
            },
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
            Picker::Files(listing) if listing.existing.is_some() => {
                listing.output_folder(library_dir)
            }
            _ if self.folder.is_some() => self.folder.clone().unwrap(),
            Picker::Files(listing) => listing.output_folder(library_dir),
            Picker::Episodes { folder, .. } => library_dir.join(folder),
        }
    }

    /// Checked positions in `rows`.
    fn selected(&self) -> Vec<usize> {
        self.checked
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(|(i, _)| i)
            .collect()
    }

    /// Torrent file indices to download (file pickers only).
    fn selected_indices(&self) -> Vec<usize> {
        match &self.picker {
            Picker::Files(listing) => self
                .selected()
                .into_iter()
                .map(|i| listing.files[i].index)
                .collect(),
            Picker::Episodes { .. } => Vec::new(),
        }
    }

    /// Positions in `rows` that match the filter.
    fn visible(&self) -> Vec<usize> {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
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
    ctx: Waker,
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
    /// AniList and MyAnimeList accounts, and your lists on them.
    tracking: tracking_ui::TrackingState,
    /// Jellyfin, Sonarr and Radarr, when the library is there.
    servers: servers_ui::ServersState,
    /// The page's series, managed as in Sonarr (on the servers).
    manage: Option<manage_ui::Manage>,
    /// AniList's lists to discover anime in.
    discover: discover_ui::DiscoverState,
    /// What's airing.
    calendar: discover_ui::CalendarState,
    /// MyAnimeList's episode scores and discussion topics.
    episode_scores: discuss_ui::EpisodeScores,
    /// An episode's discussion, open over its page.
    discussion: Option<discuss_ui::Discussion>,
    discussion_seq: u64,
    /// The network the engine runs (or is starting) on.
    engine_net: Option<crate::vpn::Network>,
    /// Bumped by each engine start or stop; a start that finds it changed
    /// has been superseded.
    engine_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Engine starts and stops take turns.
    engine_lock: Arc<tokio::sync::Mutex<()>>,
    state_dir: PathBuf,
    /// The settings as last saved.
    saved_settings: String,
}

impl App {
    /// The app, its state in `state_dir`; `ctx` wakes the GUI when
    /// background work is done.
    pub fn create(rt: tokio::runtime::Handle, state_dir: PathBuf, ctx: Waker) -> Self {
        let settings = Settings::load(&state_dir);
        let saved_settings = settings.to_json();
        let http = reqwest::Client::builder()
            .user_agent(concat!("seadex-downloader/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build the HTTP client");
        let (tx, rx) = channel();
        let debrid = DebridState::load(&state_dir, rt.clone(), ctx.clone());
        let vpn = VpnState::load(&state_dir, &rt, ctx.clone());
        let usenet = UsenetState::load(&state_dir, rt.clone(), ctx.clone(), http.clone());
        let servers = servers_ui::ServersState::load(&state_dir);
        let library = Library::load(
            servers_ui::library_file(&state_dir, servers.config.mode),
            &expand_home(&settings.library_dir),
        );

        let mut app = Self {
            rt,
            ctx,
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
            drive: DriveState::load(
                &state_dir,
                concat!("seadex-downloader/", env!("CARGO_PKG_VERSION")),
            ),
            debrid,
            vpn,
            usenet,
            tracking: tracking_ui::TrackingState::load(&state_dir),
            servers,
            manage: None,
            discover: Default::default(),
            calendar: Default::default(),
            episode_scores: Default::default(),
            discussion: None,
            discussion_seq: 0,
            engine_net: None,
            engine_seq: Arc::default(),
            engine_lock: Arc::default(),
            state_dir,
            saved_settings,
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
                None => Err(format!(
                    "Add your {} API key in Settings first.",
                    provider.name()
                )),
            };
        }
        if let Some(reason) = &self.vpn.waiting {
            return Err(format!("Torrents are waiting for the VPN: {reason}"));
        }
        match &self.engine {
            EngineState::Ready(_) => Ok(()),
            EngineState::Failed(_) => Err("The torrent engine failed to start.".into()),
            EngineState::WaitingForVpn(_) => Err("Connecting to the VPN…".into()),
            EngineState::Starting | EngineState::Off => {
                Err("Waiting for the torrent engine…".into())
            }
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
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    /// Stop downloads cleanly, as the app closes.
    fn shut_down(&mut self) {
        // Debrid and Usenet downloads resume from their partial files next time.
        self.debrid.manager.stop();
        self.usenet.manager.stop();
        // Pause torrents cleanly so they resume quickly next time.
        if let Some(engine) = self.engine() {
            let stop = async {
                let _ = tokio::time::timeout(Duration::from_secs(3), engine.stop()).await;
            };
            // On a runtime thread already (the iced GUI's), wait without
            // blocking the runtime.
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::block_in_place(|| self.rt.block_on(stop));
            } else {
                self.rt.block_on(stop);
            }
        }
    }

    /// Close whatever dialog is open (Escape).
    fn close_dialogs(&mut self) {
        self.confirm_delete = None;
        self.pages.languages = None;
        self.usenet.dialog = None;
        self.drive.browser = None;
        self.discussion = None;
    }

    /// Write the settings to `settings.json` if they changed.
    fn save_settings(&mut self) {
        let json = self.settings.to_json();
        if json == self.saved_settings {
            return;
        }
        let file = self.state_dir.join("settings.json");
        match crate::fsutil::write_private(&file, json.as_bytes()) {
            Ok(()) => self.saved_settings = json,
            Err(e) => tracing::warn!("couldn't save {}: {e:#}", file.display()),
        }
    }

    fn set_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.status = Some(Status { text, error: true });
    }

    fn home_or_results(&self) -> Page {
        if self.last_query.is_empty() {
            Page::Home
        } else {
            Page::Results
        }
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
                Msg::ListingDone {
                    seq,
                    subtitle,
                    result,
                    nzbs,
                } => {
                    if seq != self.listing_seq {
                        continue;
                    }
                    self.loading_listing = false;
                    match result {
                        Ok(listing) => {
                            if matches!(self.page, Page::Anime(_))
                                && let Page::Anime(ap) =
                                    std::mem::replace(&mut self.page, Page::Home)
                            {
                                let folder = self
                                    .library
                                    .folder_for(&new_series(&ap.anime), &self.library_dir());
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
                    let Page::Anime(AnimePage {
                        nyaa: Some(search), ..
                    }) = &mut self.page
                    else {
                        continue;
                    };
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
                                && let Page::Files(fp) =
                                    std::mem::replace(&mut self.page, Page::Home)
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
                msg @ (Msg::NzbLookup { .. } | Msg::UsenetChecked { .. }) => {
                    self.handle_usenet_msg(msg)
                }
                Msg::Moved { key, name, result } => self.handle_moved(key, name, result),
                Msg::DefaultsApplied {
                    anilist_id,
                    rel,
                    applied,
                } => self.handle_defaults_applied(anilist_id, rel, applied),
                Msg::DefaultsDone => self.defaults_done(),
                Msg::HandedOver(result) => self.handled_over(result),
                Msg::Tracking(msg) => self.handle_tracking_msg(msg),
                Msg::Servers(msg) => self.handle_servers_msg(msg),
                Msg::Manage(msg) => self.handle_manage_msg(msg),
                Msg::ReleaseFiles { id, result } => self.handle_release_files(id, result),
                Msg::Details { id, result } => self.handle_details(id, result),
                Msg::SeriesEntry { id, result } => self.handle_series_entry(id, result),
                Msg::Discovered { filter, result } => self.handle_discovered(filter, result),
                Msg::Schedule { week, result } => self.handle_schedule(week, result),
                Msg::MalEpisodes { id, result } => self.handle_mal_episodes(id, result),
                Msg::Discussion {
                    seq,
                    site,
                    more,
                    result,
                } => self.handle_discussion(seq, site, more, result),
            }
        }
    }

    // ── Actions ────────────────────────────────────────────────────────

    fn perform(&mut self, action: Action) {
        match action {
            Action::Search => self.start_search(),
            Action::OpenAnime(i) => {
                if let Some(anime) = self.results.get(i).cloned() {
                    self.show_anime(anime, Origin::Results);
                }
            }
            Action::SearchNyaa => self.search_nyaa(),
            Action::Back => {
                self.cancel_listing();
                self.page = match std::mem::replace(&mut self.page, Page::Home) {
                    Page::Files(FilesPage { back: Some(ap), .. }) => Page::Anime(ap),
                    Page::Anime(AnimePage { origin, .. }) if origin != Origin::Results => {
                        match origin {
                            Origin::Discover => Page::Discover,
                            Origin::Calendar => Page::Calendar,
                            _ => Page::Home,
                        }
                    }
                    Page::Remux(RemuxPage { back: Some(ap), .. }) => Page::Anime(ap),
                    Page::Files(_)
                    | Page::Anime(_)
                    | Page::Remux(_)
                    | Page::Playlist(_)
                    | Page::Downloads => self.home_or_results(),
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
            Action::AskDelete { id, name } => {
                self.confirm_delete = Some((DeleteTarget::Torrent(id), name))
            }
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
            Action::AskRemoveMany(items) => {
                let n = items.len();
                self.confirm_delete = Some((
                    DeleteTarget::Many(items),
                    format!("{n} download{}", if n == 1 { "" } else { "s" }),
                ));
            }
            Action::RemoveMany {
                items,
                delete_files,
                blocklist,
            } => {
                self.confirm_delete = None;
                let mut queued: std::collections::HashMap<crate::servers::arr::Kind, Vec<u64>> =
                    Default::default();
                for item in items {
                    match item {
                        Picked::Torrent(id) => self.perform(Action::Remove { id, delete_files }),
                        Picked::Debrid(id) => self.debrid_remove(id, delete_files),
                        Picked::Usenet(id) => self.usenet_remove(id, delete_files),
                        Picked::Queued(kind, id) => queued.entry(kind).or_default().push(id),
                    }
                }
                for (kind, ids) in queued {
                    self.servers_action(servers_ui::ServersAction::RemoveQueuedMany {
                        kind,
                        ids,
                        blocklist,
                    });
                }
            }
            Action::AskDeleteRemux(path) => {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.confirm_delete = Some((DeleteTarget::Remux(path), name));
            }
            Action::DeleteRemux(path) => {
                self.confirm_delete = None;
                self.delete_remux(&path);
            }
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
            Action::UsenetAskDelete { id, name } => {
                self.confirm_delete = Some((DeleteTarget::Usenet(id), name))
            }
            Action::UsenetEditFiles(id) => self.usenet_edit_files(id),
            Action::UsenetOpenRemux(id) => self.usenet_open_remux(id),
            Action::UsenetOpenProviders => self.usenet.dialog = Some(Default::default()),
            Action::UsenetCheck(id) => self.usenet_check(id),
            Action::GoLibrary => {
                self.cancel_listing();
                self.page = Page::Home;
            }
            Action::GoDownloads => {
                self.cancel_listing();
                self.page = Page::Downloads;
            }
            Action::GoDiscover => {
                self.cancel_listing();
                self.page = Page::Discover;
            }
            Action::GoCalendar => {
                self.cancel_listing();
                self.page = Page::Calendar;
            }
            Action::OpenMedia(media, origin) => self.open_media(*media, origin),
            Action::OpenDiscussion { id, episode } => self.open_discussion(id, episode),
            Action::DiscussionMore => self.discussion_more(),
            Action::OpenSeries(id) => self.open_series(id),
            Action::AddToLibrary if self.servers_mode() => {
                self.servers_action(servers_ui::ServersAction::Add)
            }
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
            // The servers download whole releases themselves.
            Action::DownloadEpisodes(_) if self.servers_mode() => {
                self.servers_action(servers_ui::ServersAction::Push)
            }
            Action::DownloadEpisodes(episodes) => self.download_episodes(episodes),
            Action::AddToPlaylist { id, files } => self.add_episodes_to_playlist(id, files),
            // The GUI plays (it has the player).
            Action::Player(_) => {}
            Action::Tracking(action) => self.tracking_action(action),
            Action::Servers(action) => self.servers_action(action),
            Action::Manage(action) => self.manage_action(action),
            Action::DebridCheck => self.debrid_check(),
            Action::DebridPause(id) => self.debrid.manager.pause(id),
            Action::DebridResume(id) => self.debrid.manager.resume(id),
            Action::DebridRemove { id, delete_files } => self.debrid_remove(id, delete_files),
            Action::DebridAskDelete { id, name } => {
                self.confirm_delete = Some((DeleteTarget::Debrid(id), name))
            }
            Action::DebridEditFiles(id) => self.debrid_edit_files(id),
            Action::DebridOpenRemux(id) => self.debrid_open_remux(id),
            Action::PickLibraryFolder => {
                let start = self.library_dir();
                let start = start
                    .ancestors()
                    .find(|p| p.is_dir())
                    .map(Path::to_owned)
                    .unwrap_or(start);
                self.spawn(async move {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Library folder")
                        .set_directory(&start)
                        .pick_folder()
                        .await;
                    Msg::LibraryFolderPicked(picked.map(|f| f.path().to_owned()))
                });
            }
            Action::PickFolder => {
                let start = match &self.page {
                    Page::Files(fp) => fp.output_folder(&self.library_dir()),
                    _ => self.library_dir(),
                };
                let start = start
                    .ancestors()
                    .find(|p| p.is_dir())
                    .map(Path::to_owned)
                    .unwrap_or(start);
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
    fn show_anime(&mut self, anime: Anime, origin: Origin) {
        let dual = self.prefer_dual_audio(anime.anilist_id);
        let chosen = anime.entry.as_ref().and_then(|e| e.best_nyaa(dual));
        // Nothing on SeaDex to download: find the best release on Nyaa instead.
        let nyaa = chosen
            .is_none()
            .then(|| NyaaSearch::new(SearchPlan::for_anime(&anime)));
        let search_now = nyaa.is_some();
        self.cancel_listing();
        self.page = Page::Anime(AnimePage {
            anime,
            chosen,
            nyaa,
            origin,
            entry_loading: false,
        });
        if search_now {
            self.search_nyaa();
        }
    }

    /// Whether to pick dual-audio releases for an anime: the setting, or
    /// languages that want a dub.
    fn prefer_dual_audio(&self, anilist_id: u64) -> bool {
        let prefs = self
            .library
            .find(anilist_id)
            .and_then(|s| s.prefs.as_ref())
            .unwrap_or(&self.settings.prefs);
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
        let Page::Anime(AnimePage {
            nyaa: Some(search), ..
        }) = &mut self.page
        else {
            return;
        };
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
                let Some(release) = search.chosen.and_then(|i| search.releases.get(i)) else {
                    return;
                };
                let subtitle = nyaa_release_label(release);
                match &release.kind {
                    ReleaseKind::Torrent { item, .. } => {
                        (item.id, valid_info_hash(&item.info_hash), subtitle)
                    }
                    ReleaseKind::Episodes(items) => {
                        let items = items.clone();
                        let group = release.group.clone();
                        let Page::Anime(ap) = std::mem::replace(&mut self.page, Page::Home) else {
                            unreachable!()
                        };
                        let title = format!("{} · {group}", ap.anime.title);
                        let folder = sanitize_folder_name(&format!("{} [{group}]", ap.anime.title));
                        self.cancel_listing();
                        let series_folder = self
                            .library
                            .folder_for(&new_series(&ap.anime), &self.library_dir());
                        let mut page =
                            FilesPage::episodes(items, title, folder, Some(ap), subtitle);
                        page.folder = Some(series_folder);
                        self.page = Page::Files(page);
                        return;
                    }
                }
            }
            None => {
                let Some(release) = chosen_release(ap) else {
                    return;
                };
                let Some(id) = release.nyaa_id() else { return };
                (
                    id,
                    release.info_hash_hex().map(str::to_owned),
                    release_label(release),
                )
            }
        };
        let client = self.http.clone();
        // With a Usenet provider, a release on Usenet comes from there.
        let usenet = self.usenet.active().then(|| {
            let cached = info_hash
                .as_deref()
                .and_then(|h| self.usenet.links(h))
                .map(<[NzbLink]>::to_vec);
            (
                self.usenet.indexers.clone(),
                self.usenet.manager.clone(),
                cached,
            )
        });
        let debrid = self
            .debrid
            .config
            .enabled
            .then(|| self.debrid.manager.clone());
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
                        listing.existing =
                            info_hash_of(&listing.torrent_bytes).and_then(|h| usenet.existing(&h));
                        Ok(listing)
                    }
                    .await;
                    return Msg::ListingDone {
                        seq,
                        subtitle,
                        result: result.map_err(failed),
                        nzbs: links,
                    };
                }
            }
            let result: anyhow::Result<Listing> = match (debrid, engine) {
                // No BitTorrent: read the .torrent file here.
                (Some(manager), _) => {
                    async {
                        let bytes = nyaa::fetch_torrent(&client, nyaa_id).await.map_err(|e| {
                            anyhow::anyhow!("{e:#} (debrid downloads need the .torrent file)")
                        })?;
                        let mut listing = crate::engine::listing_from_bytes(bytes)?;
                        listing.existing =
                            info_hash_of(&listing.torrent_bytes).and_then(|h| manager.existing(&h));
                        Ok(listing)
                    }
                    .await
                }
                (None, Some(engine)) => {
                    async {
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
                    .await
                }
                (None, None) => Err(anyhow::anyhow!(
                    "this release isn't on Usenet, and the torrent engine isn't ready"
                )),
            };
            Msg::ListingDone {
                seq,
                subtitle,
                result: result.map_err(failed),
                nzbs: Vec::new(),
            }
        });
    }

    /// For a picker of one torrent per episode, with Usenet set up: each
    /// episode's NZBs (`None` while being looked up), in row order.
    fn episode_nzbs(&self, fp: &FilesPage) -> Option<Vec<Option<&[NzbLink]>>> {
        let Picker::Episodes { items, .. } = &fp.picker else {
            return None;
        };
        self.usenet.active().then(|| {
            items
                .iter()
                .map(|(_, i)| self.usenet.links(&i.info_hash))
                .collect()
        })
    }

    /// Whether the release on the files page downloads from Usenet.
    fn via_usenet(&self, fp: &FilesPage) -> bool {
        matches!(fp.picker, Picker::Files(_))
            && !fp.nzbs.is_empty()
            && self.usenet.active()
            && !fp.existing()
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
                Picker::Files(listing) => {
                    info_hash_of(&listing.torrent_bytes).into_iter().collect()
                }
                Picker::Episodes { items, .. } => selected
                    .iter()
                    .map(|&i| items[i].1.info_hash.clone())
                    .collect(),
            };
            // Downloading something adds its series to the library.
            let new = new_series(&ap.anime);
            let root = self.library_dir();
            self.library.record(hashes, &new, &root);
        }
        let Page::Files(fp) = &self.page else { return };
        match &fp.picker {
            Picker::Files(listing) => {
                let (listing, files, nzbs) =
                    (listing.clone(), fp.selected_indices(), fp.nzbs.clone());
                self.download_files(&listing, files, nzbs, output_folder);
            }
            Picker::Episodes { items, title, .. } => {
                let items: Vec<(u32, NyaaItem)> =
                    selected.iter().map(|&i| items[i].clone()).collect();
                let title = title.clone();
                self.download_episode_torrents(items, title, output_folder);
            }
        }
    }

    /// Download `files` (torrent file indices) of a torrent into `folder`:
    /// from Usenet if it's there (and you have a provider), else through the
    /// debrid service if that's the setting, else BitTorrent here. A torrent
    /// already downloading stays with whatever is downloading it, and gets
    /// this selection.
    fn download_files(
        &mut self,
        listing: &Listing,
        files: Vec<usize>,
        nzbs: Vec<NzbLink>,
        folder: PathBuf,
    ) {
        let name = listing.name.clone();
        let hash = info_hash_of(&listing.torrent_bytes).unwrap_or_default();
        let provider = self.debrid.config.provider;
        let in_engine = self.engine().is_some_and(|e| e.existing(&hash).is_some());
        let in_usenet = self.usenet.manager.job_for(&hash).is_some();
        let in_debrid = self.debrid.manager.job_for(&hash).is_some();
        let fresh = !in_engine && !in_usenet && !in_debrid;
        if in_usenet || (fresh && self.usenet.active() && !nzbs.is_empty()) {
            let result = self
                .usenet
                .manager
                .add(listing, &files, folder, nzbs)
                .map(|()| name)
                .map_err(|e| format!("Could not start the download: {e:#}"));
            self.starting_download = true;
            let _ = self.tx.send(Msg::DownloadStarted(result));
            return;
        }
        if in_debrid || (fresh && self.debrid.config.enabled) {
            if !in_debrid && let Err(why) = self.download_ready() {
                self.set_error(why);
                return;
            }
            let result = self
                .debrid
                .manager
                .add(provider, listing, &files, folder)
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
                .download(torrent_bytes, Some(files), folder)
                .await
                .map(|()| name)
                .map_err(|e| format!("Could not start the download: {e:#}"));
            Msg::DownloadStarted(result)
        });
    }

    /// Download a release that has one torrent per episode: each episode
    /// from Usenet if it's there, else the debrid service or BitTorrent here.
    fn download_episode_torrents(
        &mut self,
        items: Vec<(u32, NyaaItem)>,
        title: String,
        output_folder: PathBuf,
    ) {
        let provider = self.debrid.config.provider;
        let client = self.http.clone();
        // One torrent per episode: those on Usenet come from there (with
        // a provider); the rest go to the debrid service, or the engine.
        let usenet = self.usenet.active().then(|| {
            let cached: std::collections::HashMap<String, Vec<NzbLink>> = items
                .iter()
                .filter_map(|(_, i)| {
                    self.usenet
                        .links(&i.info_hash)
                        .map(|l| (i.info_hash.to_ascii_lowercase(), l.to_vec()))
                })
                .collect();
            (
                self.usenet.indexers.clone(),
                self.usenet.manager.clone(),
                cached,
            )
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
                            let listing = crate::engine::listing_from_bytes(
                                nyaa::fetch_torrent(&client, item.id).await?,
                            )?;
                            let all: Vec<usize> = listing.files.iter().map(|f| f.index).collect();
                            return manager.add(&listing, &all, output_folder.clone(), links);
                        }
                    }
                    let bytes = nyaa::fetch_torrent(&client, item.id).await?;
                    match &backend {
                        Some(Ok(engine)) => {
                            engine.download(bytes, None, output_folder.clone()).await
                        }
                        Some(Err(manager)) => {
                            let listing = crate::engine::listing_from_bytes(bytes)?;
                            let all: Vec<usize> = listing.files.iter().map(|f| f.index).collect();
                            manager.add(provider, &listing, &all, output_folder.clone())
                        }
                        None => Err(anyhow::anyhow!(
                            "not on Usenet, and torrents can't start yet"
                        )),
                    }
                }
                .await;
                if let Err(e) = result {
                    failed.push(format!("episode {ep:02}: {e:#}"));
                }
            }
            let started = items.len() - failed.len();
            let what = if started == 1 {
                "1 episode".to_owned()
            } else {
                format!("{started} episodes")
            };
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

    // ── Drawing ────────────────────────────────────────────────────────

    /// Downloads under way (or waiting).
    fn active_downloads(&self) -> usize {
        let torrents = self.engine().map_or(0, |e| {
            e.torrents()
                .iter()
                .filter(|(_, h)| !h.stats().finished)
                .count()
        });
        let debrid = self
            .debrid
            .manager
            .views()
            .iter()
            .filter(|v| v.live.active())
            .count();
        let usenet = self
            .usenet
            .manager
            .views()
            .iter()
            .filter(|v| v.live.active() || v.live.phase == crate::usenet::jobs::Phase::Queued)
            .count();
        // On the servers: what's in Sonarr's and Radarr's queues too.
        let queued = if self.servers_mode() {
            self.servers.queue.len()
        } else {
            0
        };
        torrents + debrid + usenet + queued
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
    parts.push(if r.is_best {
        "SeaDex best".into()
    } else {
        "SeaDex alt".into()
    });
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

pub(crate) fn fmt_bytes(bytes: u64) -> String {
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
                .map(|(i, p)| TorrentFile {
                    index: i * 2,
                    path: (*p).into(),
                    len: 10,
                })
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

    /// Settings come from `settings.json`, or the first time from what
    /// eframe kept in `app.ron`; they're saved only when they change.
    #[test]
    fn settings_carry_over_from_eframe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.ron"),
            r#"{
    "egui": "(options:())",
    "app": "(download_dir:\"/dl\",library_dir:\"/lib/Anime\",prefer_dual_audio:true,prefs:(audio:\"jpn\",subtitles:Some(\"eng\"),sub_type:Auto),migrated:true,player:(deband:true,skip_openings:true,skip_endings:false,autoplay:false,volume:80.0))",
}"#,
        )
        .unwrap();
        let s = Settings::load(dir.path());
        assert_eq!(s.library_dir, PathBuf::from("/lib/Anime"));
        assert_eq!(s.download_dir, PathBuf::from("/dl"));
        assert!(s.prefer_dual_audio && s.migrated);
        assert!(s.player.skip_openings && !s.player.autoplay);
        assert_eq!(s.prefs.subtitles.as_deref(), Some("eng"));

        std::fs::write(dir.path().join("settings.json"), {
            let mut changed = s;
            changed.library_dir = PathBuf::from("/elsewhere");
            changed.to_json()
        })
        .unwrap();
        assert_eq!(
            Settings::load(dir.path()).library_dir,
            PathBuf::from("/elsewhere")
        );
        // Nothing there: the defaults.
        let empty = tempfile::tempdir().unwrap();
        assert!(!Settings::load(empty.path()).migrated);
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
