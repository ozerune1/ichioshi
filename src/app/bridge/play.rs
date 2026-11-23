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
    // The languages (the series' own, or everyone's) and the player's
    // settings; the episodes either side.
    let prefs = series
        .prefs
        .clone()
        .unwrap_or_else(|| app.settings.prefs.clone());
    let alang = mpv_lang(&prefs.audio);
    let slang = prefs.subtitles.as_deref().map(mpv_lang).unwrap_or_default();
    let p = &app.settings.player;
    let (skip_openings, skip_endings) = (p.skip_openings, p.skip_endings);
    let next = neighbour(app, id, episode, true);
    let prev = neighbour(app, id, episode, false);
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
                "alang": alang,
                "slang": slang,
                "skipOpenings": skip_openings,
                "skipEndings": skip_endings,
                "hasNext": next.is_some(),
                "hasPrev": prev.is_some(),
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

/// How far the player got with an episode; then the next or previous one
/// (`then`: "next", "prev", or "auto": it ended, and autoplay's on).
pub(super) fn played(
    app: &mut App,
    core: &mut Core,
    id: u64,
    episode: u32,
    position: f64,
    duration: f64,
    then: Option<&str>,
) {
    if duration > 0.0 {
        let done = position >= duration * 0.9;
        app.save_watch(id, episode, position, duration, done);
    }
    let go = match then {
        Some("next") => neighbour(app, id, episode, true),
        Some("prev") => neighbour(app, id, episode, false),
        Some("auto") if app.settings.player.autoplay => neighbour(app, id, episode, true),
        _ => None,
    };
    if let Some(n) = go {
        play(app, core, id, n);
    }
}

/// The next (or previous) episode there is to play: on disk, or on
/// Jellyfin (or saved from it).
fn neighbour(app: &App, id: u64, episode: u32, forward: bool) -> Option<u32> {
    let series = app.library.find(id)?;
    let there: std::collections::BTreeSet<u32> = match &series.remote {
        Some(r) => r
            .episodes
            .iter()
            .filter(|(n, e)| e.jellyfin.is_some() || app.offline_copy(id, **n).is_some())
            .map(|(n, _)| *n)
            .collect(),
        None => episode_files(series, &app.busy_files())
            .into_keys()
            .collect(),
    };
    if forward {
        there.range(episode + 1..).next().copied()
    } else {
        there.range(..episode).next_back().copied()
    }
}

/// A language as mpv takes it: its ISO 639-2 code, and the two-letter one
/// (files tag either).
fn mpv_lang(code: &str) -> String {
    let two = match code {
        "jpn" => "ja",
        "eng" => "en",
        "spa" => "es",
        "por" => "pt",
        "fre" => "fr",
        "ger" => "de",
        "ita" => "it",
        "rus" => "ru",
        "ara" => "ar",
        "chi" => "zh",
        "kor" => "ko",
        "ind" => "id",
        "may" => "ms",
        "tha" => "th",
        "vie" => "vi",
        "pol" => "pl",
        "tur" => "tr",
        "dut" => "nl",
        "swe" => "sv",
        "hin" => "hi",
        _ => "",
    };
    if two.is_empty() {
        code.to_owned()
    } else {
        format!("{code},{two}")
    }
}
