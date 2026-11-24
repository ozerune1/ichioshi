//! Playing an episode on the phone: where from (its file, an offline copy,
//! or Jellyfin's stream) and where to start, for the UI's player; and how
//! far it got, saved.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::Core;
use crate::app::App;
use crate::app::watching::episode_files;

/// Play episode `episode` of series `id`: the UI gets "play" with where
/// from (or the app says why it can't). In a watch party, the host's
/// episode waits until everyone has it.
pub(super) fn play(app: &mut App, core: &mut Core, id: u64, episode: u32) {
    if app.party_play(id, episode) {
        return;
    }
    let emit = core.emit.clone();
    open(
        app,
        core,
        id,
        episode,
        None,
        Arc::new(move |p| emit("play", p)),
    );
}

/// What to play goes to the UI's player (or, the party's, straight to the
/// phone's player: see `party_poll`).
type Deliver = Arc<dyn Fn(Value) + Send + Sync>;

/// Play it: from where you left it, or (`party`) where the party's player
/// is, playing or paused, and at its speed (none: your own).
fn open(
    app: &mut App,
    core: &mut Core,
    id: u64,
    episode: u32,
    party: Option<(Option<f64>, bool, Option<f64>)>,
    deliver: Deliver,
) {
    let Some(series) = app.library.find(id) else {
        return;
    };
    let title = series.title().to_owned();
    let start = match party {
        Some((Some(at), _, _)) => at,
        _ => series
            .watched
            .get(&episode)
            .and_then(crate::library::Watch::resume_at)
            .unwrap_or(0.0),
    };
    let paused = party.is_some_and(|(_, playing, _)| !playing);
    let speed = party.and_then(|(_, _, speed)| speed);
    // Following the host's player, only the host controls it.
    let role = if party.is_some()
        && !matches!(app.party.role, Some(crate::app::party_ui::Role::Host(_)))
    {
        "member"
    } else {
        ""
    };
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
    let send = move |url: String, kind: &str| {
        deliver(json!({
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
                "party": role,
                "paused": paused,
                "speed": speed,
        }));
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

/// The watch party and the phone's player, a few times a second (from the
/// phone app, not its web UI, which sleeps while the player's open): what
/// the player's doing (`player`: from PlayerPlugin's "partyState"), and
/// what it's to do ("control", for "partyControl"). `active`: there's a
/// party (else it's asked less often).
pub(super) fn party_poll(app: &mut App, core: &mut Core, player: Option<Value>) -> Value {
    use crate::app::party_ui::{PartyAction, PlayerCmd, Watching};
    let watching = player
        .as_ref()
        .filter(|p| p["open"] == true)
        .map(|p| Watching {
            series: p["id"].as_u64().unwrap_or(0),
            episode: p["episode"].as_u64().unwrap_or(0) as u32,
            position: p["position"].as_f64().unwrap_or(0.0),
            playing: p["paused"] != true && p["ended"] != true,
            speed: p["speed"].as_f64().unwrap_or(1.0),
        });
    // Pausing, following the host: asks the host.
    if let Some(asked) = player.as_ref().and_then(|p| p["asked"].as_str()) {
        let ask = if asked == "play" {
            crate::party::Ask::Play
        } else {
            crate::party::Ask::Pause
        };
        app.party_action(PartyAction::Ask(ask));
    }
    let control = match app.party_player(watching) {
        Some(PlayerCmd::Open {
            series,
            episode,
            at,
            playing,
            speed,
        }) => {
            // Straight to the phone's player (the web UI sleeps while one's
            // open); unless it's only known later, then through the UI.
            let slot: Arc<Mutex<(bool, Option<Value>)>> = Arc::default();
            let (s, emit) = (slot.clone(), core.emit.clone());
            let deliver: Deliver = Arc::new(move |p| {
                let mut s = s.lock().unwrap();
                if s.0 {
                    emit("play", p);
                } else {
                    s.1 = Some(p);
                }
            });
            open(
                app,
                core,
                series,
                episode,
                Some((at, playing, speed)),
                deliver,
            );
            let mut s = slot.lock().unwrap();
            s.0 = true;
            if let Some(p) = s.1.take() {
                return json!({"active": true, "control": Value::Null, "play": p});
            }
            Value::Null
        }
        Some(PlayerCmd::Seek(to)) => json!({"seek": to}),
        Some(PlayerCmd::Speed(speed)) => json!({"speed": speed}),
        Some(PlayerCmd::Pause) => json!({"paused": true}),
        Some(PlayerCmd::Play) => json!({"paused": false}),
        Some(PlayerCmd::Close) => json!({"close": true}),
        None => Value::Null,
    };
    json!({"active": app.party.role.is_some(), "control": control})
}
