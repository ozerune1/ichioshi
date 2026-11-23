//! The phone app's view of the core (each page, as JSON), and the commands
//! its UI sends.

use serde::Deserialize;
use serde_json::{Value, json};

use super::ui::{Handler, Handles, Ui};
use super::{Core, browse, dialogs, discussion, downloads, library, pages, play, series, settings};
use crate::app::watching::PlayerAction;
use crate::app::{Action, App, Origin, Page};

/// What the UI asks the app to do.
#[derive(Debug, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// A button (its handle).
    Press {
        id: String,
    },
    /// Text typed into a field (its handle).
    Input {
        id: String,
        value: String,
    },
    /// One of the tabs along the bottom.
    Tab {
        tab: Tab,
    },
    /// Android's back button (the UI's own sheets and groups first).
    Back,
    DismissNote,
    /// How far the player got: saved, and told to your lists and servers.
    Played {
        id: u64,
        episode: u32,
        position: f64,
        duration: f64,
        /// What next: "next", "prev", "auto" (it ended).
        #[serde(default)]
        then: Option<String>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    Library,
    Discover,
    Downloads,
    More,
}

pub(super) fn command(app: &mut App, core: &mut Core, command: Command) -> Value {
    match command {
        Command::Press { id } => match core.handles.get(&id) {
            Some(Handler::Act(a)) => {
                let a = a.clone();
                act(app, core, a);
            }
            Some(Handler::Edit(f)) => f(app),
            Some(Handler::Input(_)) | None => {}
        },
        Command::Input { id, value } => {
            if let Some(Handler::Input(f)) = core.handles.get(&id) {
                f(app, value);
            }
        }
        Command::Tab { tab } => match tab {
            Tab::Library => app.perform(Action::GoLibrary),
            Tab::Discover => app.perform(Action::GoDiscover),
            Tab::Downloads => app.perform(Action::GoDownloads),
            Tab::More => {
                app.cancel_listing();
                app.page = Page::Settings;
            }
        },
        Command::Back => {
            if app.close_any_dialog() {
                return Value::Null;
            }
            match &app.page {
                Page::Home => return json!({"exit": true}),
                Page::Anime(_) | Page::Files(_) | Page::Remux(_) => app.perform(Action::Back),
                Page::Playlist(_) => app.page = Page::Settings,
                Page::Calendar => app.perform(Action::GoDiscover),
                _ => app.perform(Action::GoLibrary),
            }
        }
        Command::DismissNote => app.status = None,
        Command::Played {
            id,
            episode,
            position,
            duration,
            then,
        } => play::played(app, core, id, episode, position, duration, then.as_deref()),
    }
    Value::Null
}

/// An action, as the desktop GUI does it (playing is the phone app's).
fn act(app: &mut App, core: &mut Core, action: Action) {
    match action {
        Action::Player(PlayerAction::Play { id, episode }) => play::play(app, core, id, episode),
        // The phone's installer installs the downloaded APK.
        Action::Update(crate::app::update_ui::UpdateAction::Install) => {
            if let crate::app::update_ui::Status::Ready { path, .. } = &app.update.status {
                (core.emit)("install", json!({"path": path}));
            }
        }
        other => app.perform(other),
    }
}

/// Is something moving on its own (downloads), so the view changes?
pub(super) fn moving(app: &App) -> bool {
    app.active_downloads() > 0 || app.update.downloading()
}

/// The tab a page is under.
fn tab(app: &App) -> Tab {
    let in_library = |id: u64| app.library.find(id).is_some();
    match &app.page {
        Page::Home | Page::Remux(_) => Tab::Library,
        Page::Discover | Page::Calendar | Page::Results | Page::Files(_) => Tab::Discover,
        Page::Anime(ap) => {
            let found = matches!(
                ap.origin,
                Origin::Discover | Origin::Calendar | Origin::Results
            );
            if found && !in_library(ap.anime.anilist_id) {
                Tab::Discover
            } else {
                Tab::Library
            }
        }
        Page::Downloads => Tab::Downloads,
        Page::Playlist(_) | Page::Settings | Page::Party => Tab::More,
    }
}

pub(super) fn view(app: &App, handles: &mut Handles) -> Value {
    let mut ui = Ui::new(app, handles);
    let page = match &app.page {
        Page::Home => library::page(&mut ui),
        Page::Downloads => downloads::page(&mut ui),
        Page::Settings | Page::Party => settings::page(&mut ui),
        Page::Discover => browse::discover(&mut ui),
        Page::Calendar => browse::calendar(&mut ui),
        Page::Anime(ap) => series::page(&mut ui, ap),
        Page::Results => browse::search(&mut ui),
        Page::Files(fp) => browse::files(&mut ui, fp),
        Page::Remux(rp) => pages::remux(&mut ui, rp),
        Page::Playlist(pp) => pages::playlist(&mut ui, pp),
    };
    let dialog = dialogs::dialog(&mut ui);
    let discussion = discussion::view(&mut ui);
    let search = ui.edit(|app| {
        app.cancel_listing();
        app.page = Page::Results;
    });
    json!({
        "page": page,
        "dialog": dialog,
        "discussion": discussion,
        "search": search,
        "tab": match tab(app) {
            Tab::Library => "library",
            Tab::Discover => "discover",
            Tab::Downloads => "downloads",
            Tab::More => "more",
        },
        "servers": app.servers_mode(),
        "downloading": app.active_downloads(),
        "note": app.status.as_ref().map(|s| json!({"text": s.text, "error": s.error})),
    })
}
