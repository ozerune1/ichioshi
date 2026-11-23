//! Playing an episode on the phone: where from (its file, an offline copy,
//! or Jellyfin's stream) and where to start, for the UI's player; and how
//! far it got, saved.

use serde_json::json;

use super::Core;
use crate::app::App;
use crate::app::watching::episode_files;

/// Play episode `episode` of series `id`: the UI gets "play" with where
/// from (or the app says why it can't).
pub(super) fn play(app: &mut App, core: &mut Core, id: u64, episode: u32) {
    let Some(series) = app.library.find(id) else {
        return;
    };
    let title = series.title().to_owned();
    let start = series
        .watched
        .get(&episode)
        .and_then(crate::library::Watch::resume_at)
        .unwrap_or(0.0);
    let name = series
        .details
        .as_ref()
        .and_then(|d| d.episode(episode))
        .and_then(|e| e.title.clone())
        .unwrap_or_else(|| format!("Episode {episode}"));
    let emit = core.emit.clone();
    let send = move |url: String, kind: &str| {
        emit(
            "play",
            json!({
                "id": id,
                "episode": episode,
                "title": title,
                "name": name,
                "url": url,
                "kind": kind,
                "start": start,
            }),
        );
    };
    // On the servers: its offline copy, else Jellyfin's stream.
    if app.servers_mode() {
        if let Some(path) = app.offline_copy(id, episode) {
            send(path.display().to_string(), "file");
            return;
        }
        let Some(j) = app.jellyfin() else {
            app.set_error("Sign in to Jellyfin in Settings to watch from your servers");
            return;
        };
        let item = series
            .remote
            .as_ref()
            .and_then(|r| r.episodes.get(&episode)?.jellyfin.clone());
        let Some(item) = item else {
            app.set_error(format!(
                "Jellyfin doesn't have episode {episode} (yet: it shows up once Jellyfin has \
                 scanned it)"
            ));
            return;
        };
        app.set_info("Opening it from Jellyfin…");
        let waker = app.ctx.clone();
        core.rt.spawn(async move {
            match j.playable(&item).await {
                Ok(p) => send(j.stream_url(&p), "stream"),
                Err(e) => tracing::warn!("couldn't open it from Jellyfin: {e:#}"),
            }
            waker.request_repaint();
        });
        return;
    }
    let busy = app.busy_files();
    match episode_files(series, &busy).remove(&episode) {
        Some(path) => send(path.display().to_string(), "file"),
        None => app.set_error(format!("Episode {episode} isn't in the library")),
    }
}

/// How far the player got with an episode.
pub(super) fn played(app: &mut App, id: u64, episode: u32, position: f64, duration: f64) {
    if duration <= 0.0 {
        return;
    }
    let done = position >= duration * 0.9;
    app.save_watch(id, episode, position, duration, done);
}
