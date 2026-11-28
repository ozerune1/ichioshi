//! The library: your series as posters, what's next to watch, and how the
//! library is (on this phone or your servers).

use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::watching::{PlayerAction, clock, up_next};
use crate::app::{Action, fmt_bytes};
use crate::library::Series;
use crate::tracking::Status;

pub(super) fn page(ui: &mut Ui) -> Value {
    let app = ui.app;
    let stats = &app.pages.stats;
    let episodes: usize = stats.values().map(|s| s.files.len()).sum();
    let bytes: u64 = stats.values().map(|s| s.bytes).sum();
    let summary = format!(
        "{} series · {episodes} episodes · {}",
        app.library.series.len(),
        fmt_bytes(bytes)
    );
    // How the library is: on your servers (read when), or here (and its
    // languages).
    let about = if app.servers_mode() {
        let s = &app.servers;
        let mut names = Vec::new();
        if s.config.sonarr.is_set() {
            names.push("Sonarr");
        }
        if s.config.radarr.is_set() {
            names.push("Radarr");
        }
        if s.config.jellyfin.signed_in() {
            names.push("Jellyfin");
        }
        let when = if s.refreshing {
            "reading…".to_owned()
        } else {
            s.refreshed
                .map(crate::app::servers_ui::ago)
                .unwrap_or_default()
        };
        let refresh = ui.button(
            "Read again",
            (s.config.any() && !s.refreshing)
                .then(|| Action::Servers(crate::app::servers_ui::ServersAction::Refresh)),
        );
        json!({
            "servers": names,
            "read": when,
            "refresh": refresh,
            "problems": s.problems,
        })
    } else {
        let change = ui.button("Change", Some(Action::OpenLanguages));
        json!({"languages": app.settings.prefs.summary(), "change": change})
    };

    let mut order: Vec<&Series> = app.library.series.iter().collect();
    order.sort_by(|x, y| y.added.cmp(&x.added).then(x.title().cmp(y.title())));

    // Continue watching: series being watched, with what's next.
    let mut shelf = Vec::new();
    for s in &order {
        let watching = app
            .tracking
            .shown(s.anilist_id)
            .is_some_and(|e| matches!(e.status, Status::Watching | Status::Rewatching))
            || (!app.tracking.any() && s.watched.values().any(|w| !w.done));
        if !watching {
            continue;
        }
        let disk = app.on_disk(s.anilist_id);
        let Some(next) = up_next(s, disk.iter().copied(), app.list_progress(s.anilist_id)) else {
            continue;
        };
        let details = s.details.as_ref();
        let watch = s.watched.get(&next);
        let resume = watch.and_then(crate::library::Watch::resume_at);
        let name = details
            .and_then(|d| d.episode(next))
            .and_then(|e| e.title.clone())
            .filter(|t| !t.starts_with("Episode "));
        let mut line = match resume {
            Some(at) => format!("Resume episode {next} from {}", clock(at)),
            None => format!("Up next · Episode {next}"),
        };
        if let Some(n) = name {
            line += &format!(" · {n}");
        }
        let id = s.anilist_id;
        shelf.push(json!({
            "id": id,
            "title": s.title(),
            "line": line,
            "art": details
                .and_then(|d| d.banner.clone())
                .or(s.cover.clone())
                .or(details.and_then(|d| d.cover.clone())),
            "progress": watch.map_or(0.0, |w| (w.position / w.duration.max(1.0)).min(1.0)),
            "open": ui.act(Action::OpenSeries(id)),
            "play": ui.act(Action::Player(PlayerAction::Play { id, episode: next })),
        }));
    }

    let cards: Vec<Value> = order.iter().map(|s| card(ui, s)).collect();
    // 18+ series kept off your lists have a tab of their own.
    let adult_tab = app.adult_on() && !app.settings.adult_lists && order.iter().any(|s| s.adult);
    json!({
        "kind": "library",
        "adult_tab": adult_tab,
        "summary": summary,
        "about": about,
        "lists": app.tracking.any(),
        // Signed in, but a list isn't read yet: statuses are still to come.
        "listsLoading": app
            .tracking
            .services()
            .iter()
            .any(|s| !app.tracking.lists.contains_key(s)),
        "continue": shelf,
        "series": cards,
        "empty": app.library.series.is_empty(),
    })
}

/// A poster: list status, how much is watched or downloading, what's next.
fn card(ui: &mut Ui, s: &Series) -> Value {
    let app = ui.app;
    let id = s.anilist_id;
    let details = s.details.as_ref();
    let stats = app.pages.stats.get(&id);
    let have = stats.map_or(0, |s| s.files.len());
    let total = s.episodes.or(details.and_then(|d| d.episodes));
    let done = s.watched.values().filter(|w| w.done).count();
    let watched = match total {
        Some(t) if t > 0 => done as f32 / t as f32,
        _ if have > 0 => done as f32 / have as f32,
        _ => 0.0,
    };
    let downloading = stats.filter(|s| s.downloading).map(|s| {
        let n = s.progress.len().max(1) as f32;
        s.progress.values().sum::<f32>() / n
    });
    let status = app.tracking.shown(id).map(|e| e.status);
    json!({
        "id": id,
        "title": s.title(),
        "cover": s.cover.clone().or(details.and_then(|d| d.cover.clone())),
        "meta": match (have, total) {
            (0, _) => "No episodes yet".to_owned(),
            (n, Some(t)) => format!("{n} of {t} episodes"),
            (n, None) => format!("{n} episodes"),
        },
        "airing": details.and_then(crate::app::library_ui::airing_line),
        "status": status.map(|s| json!({"name": s.name(), "key": format!("{s:?}").to_lowercase()})),
        "adult": s.adult,
        "type": s
            .format
            .clone()
            .or_else(|| details.and_then(|d| d.format.clone()))
            .map(|t| crate::browse::format_name(&t)),
        "watched": watched.min(1.0),
        "downloading": downloading,
        "open": ui.act(Action::OpenSeries(id)),
    })
}
