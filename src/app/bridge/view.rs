//! The phone app's view of the core: each page as JSON, and the commands
//! its UI sends.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{Action, App, Page};

/// What the UI asks the app to do.
#[derive(Debug, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    GoLibrary,
    GoDiscover,
    GoDownloads,
    GoSettings,
    Back,
    OpenSeries { id: u64 },
    DismissNote,
}

pub(super) fn command(app: &mut App, command: Command) -> Result<(), String> {
    match command {
        Command::GoLibrary => app.perform(Action::GoLibrary),
        Command::GoDiscover => app.perform(Action::GoDiscover),
        Command::GoDownloads => app.perform(Action::GoDownloads),
        Command::GoSettings => {
            app.cancel_listing();
            app.page = Page::Settings;
        }
        Command::Back => app.perform(Action::Back),
        Command::OpenSeries { id } => app.perform(Action::OpenSeries(id)),
        Command::DismissNote => app.status = None,
    }
    Ok(())
}

/// Is something moving on its own (downloads), so the view changes?
pub(super) fn moving(app: &App) -> bool {
    app.active_downloads() > 0
}

pub(super) fn view(app: &App) -> Value {
    let page = match &app.page {
        Page::Home => json!({
            "kind": "library",
            "series": app.library.series.iter().map(|s| json!({
                "id": s.anilist_id,
                "title": s.title(),
            })).collect::<Vec<_>>(),
        }),
        Page::Downloads => json!({"kind": "downloads"}),
        Page::Settings => json!({"kind": "settings"}),
        Page::Discover | Page::Calendar => json!({"kind": "discover"}),
        Page::Anime(ap) => json!({"kind": "series", "title": ap.anime.title}),
        _ => json!({"kind": "other"}),
    };
    json!({
        "page": page,
        "note": app.status.as_ref().map(|s| json!({"text": s.text, "error": s.error})),
    })
}
