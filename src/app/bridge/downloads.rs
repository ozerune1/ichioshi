//! Downloads: Sonarr's and Radarr's queues (on your servers), episodes
//! being saved offline, and what's downloading on the phone itself.

use std::collections::HashMap;

use librqbit::TorrentStatsState;
use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::offline_ui::{OfflineAction, Saving};
use crate::app::servers_ui::{ServersAction, ago};
use crate::app::{Action, EngineState, Picked, fmt_bytes, fmt_duration, fmt_speed};
use crate::engine::TorrentHandle;
use crate::servers::arr::{Kind, Queued};

/// Things picked (to remove together): each card's key, and what it is.
type Picks = HashMap<String, Picked>;

pub(super) fn page(ui: &mut Ui) -> Value {
    let app = ui.app;
    let mut picks = Picks::new();
    let mut sections = Vec::new();
    if app.servers_mode() {
        let s = &app.servers;
        for kind in [Kind::Sonarr, Kind::Radarr] {
            let set = match kind {
                Kind::Sonarr => s.config.sonarr.is_set(),
                Kind::Radarr => s.config.radarr.is_set(),
            };
            if !set {
                continue;
            }
            let items: Vec<&Queued> = s
                .queue
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, q)| q)
                .collect();
            let cards: Vec<Value> = items
                .iter()
                .map(|q| {
                    let key = format!("q{}{}", kind.name(), q.id);
                    picks.insert(key.clone(), Picked::Queued(kind, q.id));
                    queued(ui, kind, q, key)
                })
                .collect();
            sections.push(json!({
                "title": kind.name(),
                "note": match items.len() { 1 => "1 in its queue".to_owned(), n => format!("{n} in its queue") },
                "empty": items.is_empty().then(|| format!("Nothing in {}'s queue.", kind.name())),
                "cards": cards,
                "queue": true,
            }));
        }
        let saving = &app.offline.jobs;
        if !saving.is_empty() {
            let cards: Vec<Value> = saving.iter().map(|j| offline(ui, j)).collect();
            sections.push(json!({
                "title": "Saving offline",
                "note": format!("{} from Jellyfin", saving.len()),
                "cards": cards,
            }));
        }
    }
    // What's downloading here.
    let usenet = app.usenet.manager.views();
    let debrid = app.debrid.manager.views();
    let torrents = app.engine().map(|e| e.torrents()).unwrap_or_default();
    let mut own = Vec::new();
    for v in &usenet {
        let key = format!("u{}", v.job.id);
        picks.insert(key.clone(), Picked::Usenet(v.job.id));
        own.push(usenet_card(ui, v, key));
    }
    for v in &debrid {
        let key = format!("d{}", v.job.id);
        picks.insert(key.clone(), Picked::Debrid(v.job.id));
        own.push(debrid_card(ui, v, key));
    }
    for (id, handle) in torrents.iter().rev() {
        let key = format!("t{id}");
        picks.insert(key.clone(), Picked::Torrent(*id));
        own.push(torrent_card(ui, *id, handle, key));
    }
    let order = match (app.usenet.active(), app.debrid.service()) {
        (true, Some(s)) => format!("from Usenet first, then {s}"),
        (true, None) => "from Usenet first, then BitTorrent".to_owned(),
        (false, Some(s)) => format!("through {s}"),
        (false, None) => "as torrents · nothing is ever seeded".to_owned(),
    };
    let mut notes = Vec::new();
    match &app.engine {
        EngineState::Starting => {
            notes.push(json!({"text": "Restoring downloads…", "tone": "muted"}))
        }
        EngineState::Failed(e) => {
            notes.push(json!({"text": format!("The torrent engine failed: {e}"), "tone": "error"}))
        }
        EngineState::WaitingForVpn(reason) => notes.push(json!({"text": reason, "tone": "warn"})),
        _ => {}
    }
    if !own.is_empty() || !app.servers_mode() {
        sections.push(json!({
            "title": if app.servers_mode() { "On this phone" } else { "" },
            "note": format!("{} active · {order}", app.active_downloads()),
            "cards": own,
            "empty": (own.is_empty() && !matches!(app.engine, EngineState::Starting | EngineState::WaitingForVpn(_)))
                .then(|| "Nothing downloading. Finished downloads go into your library.".to_owned()),
        }));
    }
    let summary = if app.servers_mode() {
        let s = &app.servers;
        format!(
            "{} in your servers' queues · {}",
            s.queue.len(),
            match s.queue_read {
                Some(t) => format!("read {}", ago(t)),
                None => "reading…".into(),
            }
        )
    } else {
        format!("{} active · {order}", app.active_downloads())
    };
    let set_up = (app.servers_mode() && !app.servers.config.any())
        .then(|| "Set up Sonarr or Radarr in More → Library to see their queues.".to_owned());
    let remove = ui.input(move |app, keys| {
        let items: Vec<Picked> = keys
            .split(',')
            .filter_map(|k| picks.get(k).cloned())
            .collect();
        if !items.is_empty() {
            app.perform(Action::AskRemoveMany(items));
        }
    });
    json!({
        "kind": "downloads",
        "summary": summary,
        "set_up": set_up,
        "notes": notes,
        "sections": sections,
        "remove_many": remove,
    })
}

/// The series a download is for (its poster).
fn cover_for(ui: &Ui, hash: &str) -> Option<String> {
    let s = ui
        .app
        .library
        .series
        .iter()
        .find(|s| s.downloads.contains(&hash.to_owned()))?;
    s.cover
        .clone()
        .or(s.details.as_ref().and_then(|d| d.cover.clone()))
}

fn queue_status(q: &Queued) -> (&'static str, &'static str) {
    let failed = q.state.starts_with("failed") || q.status == "failed";
    let s = match (q.state.as_str(), q.status.as_str()) {
        _ if failed => "Failed",
        ("importPending", _) => "Waiting to import",
        ("importing", _) => "Importing",
        ("imported", _) => "Imported",
        (_, "queued") => "Queued",
        (_, "paused") => "Paused",
        (_, "delay") => "Delayed (by a delay profile)",
        (_, "completed") => "Downloaded",
        (_, "warning") => "Downloading, with a warning",
        _ => "Downloading",
    };
    let tone = if failed {
        "error"
    } else if q.problem.is_some() || q.state == "importPending" {
        "warn"
    } else {
        "good"
    };
    (s, tone)
}

/// "00:12:34" (or "1.02:03:04", with days) as "12 min left".
fn time_left(t: &str) -> Option<String> {
    let (days, hms) = match t.split_once('.') {
        Some((d, rest)) if !d.contains(':') => (d.parse::<u64>().ok()?, rest),
        _ => (0, t),
    };
    let mut p = hms.split(':').map(|x| x.parse::<f64>().ok());
    let (h, m, s) = (p.next()??, p.next()??, p.next()??);
    let secs = days as f64 * 86_400.0 + h * 3600.0 + m * 60.0 + s;
    Some(format!("{} left", fmt_duration(secs)))
}

fn queued(ui: &mut Ui, kind: Kind, q: &Queued, key: String) -> Value {
    let app = ui.app;
    let found = crate::servers::build::queued_episode(&app.library.series, kind, q);
    let series = found.map(|(s, _)| s);
    let title = series
        .map(|s| s.title().to_owned())
        .or_else(|| q.show.clone())
        .unwrap_or_else(|| q.title.clone());
    let which = match (found, kind) {
        (_, Kind::Radarr) => "Film".to_owned(),
        (Some((_, n)), _) => format!("Episode {n}"),
        (None, _) => match (q.season, q.number) {
            (Some(s), Some(e)) => format!("S{s:02}E{e:02}"),
            _ => "Episode".to_owned(),
        },
    };
    let which = match &q.episode_title {
        Some(t) if kind == Kind::Sonarr => format!("{which} · {t}"),
        _ => which,
    };
    let (status, tone) = queue_status(q);
    let mut detail = vec![format!("{:.0}%", q.fraction() * 100.0), status.to_owned()];
    if let Some(t) = q.time_left.as_deref().and_then(time_left)
        && q.left > 0.0
    {
        detail.push(t);
    }
    if q.size > 0.0 {
        detail.push(fmt_bytes(q.size as u64));
    }
    if let Some(quality) = &q.quality {
        detail.push(quality.clone());
    }
    if let Some(client) = &q.client {
        detail.push(match q.protocol.as_str() {
            "" => client.clone(),
            p => format!("{client} ({p})"),
        });
    }
    let mut buttons = Vec::new();
    if let Some(s) = series {
        buttons.push(ui.button("Open series", Some(Action::OpenSeries(s.anilist_id))));
    }
    if let Some(d) = q
        .download_id
        .clone()
        .filter(|_| q.state.starts_with("import") || q.problem.is_some())
    {
        buttons.push(ui.button(
            "Import…",
            Some(Action::Import(crate::app::import_ui::ImportAction::Open {
                kind,
                owner: q.owner,
                anilist: series.map(|s| s.anilist_id),
                download: Some(d),
            })),
        ));
    }
    let remove = ui.button(
        "Remove…",
        Some(Action::Servers(ServersAction::AskRemoveQueued {
            kind,
            id: q.id,
            title: q.title.clone(),
        })),
    );
    json!({
        "key": key,
        "source": kind.name(),
        "tone": tone,
        "title": title,
        "which": which,
        "fraction": q.fraction(),
        "detail": detail.join(" · "),
        "release": q.title,
        "problem": q.problem,
        "cover": series.and_then(|s| s.cover.clone().or(s.details.as_ref().and_then(|d| d.cover.clone()))),
        "buttons": buttons,
        "remove": remove,
    })
}

fn offline(ui: &mut Ui, j: &crate::app::offline_ui::OfflineJob) -> Value {
    let app = ui.app;
    let series = app.library.find(j.anilist);
    let len = j.len.load(std::sync::atomic::Ordering::Relaxed);
    let have = j.have.load(std::sync::atomic::Ordering::Relaxed);
    let speed = *j.speed.lock().unwrap();
    let (status, tone) = match &j.state {
        Saving::Waiting => ("Waiting its turn".to_owned(), "muted"),
        Saving::Getting if len == 0 => ("Asking Jellyfin for it…".to_owned(), "accent"),
        Saving::Getting => {
            let mut s = format!(
                "{:.0}% · {} of {}",
                j.fraction() * 100.0,
                fmt_bytes(have),
                fmt_bytes(len)
            );
            if speed > 0.0 {
                s += &format!(
                    " · {} · {} left",
                    fmt_speed(speed / 1_048_576.0),
                    fmt_duration((len - have.min(len)) as f64 / speed)
                );
            }
            (s, "accent")
        }
        Saving::Failed(e) => (format!("Couldn't save it: {e}"), "error"),
    };
    let mut buttons = Vec::new();
    if matches!(j.state, Saving::Failed(_)) {
        buttons.push(ui.button("Retry", Some(Action::Offline(OfflineAction::Retry(j.id)))));
    }
    buttons.push(ui.button("Stop", Some(Action::Offline(OfflineAction::Cancel(j.id)))));
    json!({
        "source": "Jellyfin",
        "tone": tone,
        "title": series.map_or("An episode", |s| s.title()),
        "which": format!("Episode {}", j.episode),
        "fraction": j.fraction(),
        "detail": status,
        "cover": series.and_then(|s| s.cover.clone().or(s.details.as_ref().and_then(|d| d.cover.clone()))),
        "buttons": buttons,
    })
}

/// Pause or resume, files, remux, remove (keeping or deleting).
#[allow(clippy::too_many_arguments)]
fn tools(
    ui: &mut Ui,
    pause: Option<Action>,
    resume: Option<(&'static str, Action)>,
    files: Action,
    remux: Action,
    remove: Action,
    delete: Action,
) -> Vec<Value> {
    let mut b = Vec::new();
    if let Some(a) = pause {
        b.push(ui.button("Pause", Some(a)));
    }
    if let Some((label, a)) = resume {
        b.push(ui.button(label, Some(a)));
    }
    b.push(ui.button("Episodes…", Some(files)));
    b.push(ui.button("Remux…", Some(remux)));
    b.push(ui.button("Remove", Some(remove)));
    b.push(json!({"danger": true, "label": "Delete…", "id": ui.act(delete)}));
    b
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &Ui,
    key: String,
    source: &str,
    tone: &str,
    name: &str,
    hash: &str,
    fraction: f32,
    detail: String,
    error: Option<String>,
    note: Option<String>,
    buttons: Vec<Value>,
    files: Vec<(f32, String)>,
) -> Value {
    json!({
        "key": key,
        "source": source,
        "tone": tone,
        "title": name,
        "fraction": fraction,
        "detail": detail,
        "problem": error,
        "note": note,
        "cover": cover_for(ui, hash),
        "buttons": buttons,
        "files": files.into_iter().map(|(f, p)| json!({"fraction": f, "path": p})).collect::<Vec<_>>(),
    })
}

fn torrent_card(
    ui: &mut Ui,
    id: crate::engine::TorrentId,
    handle: &TorrentHandle,
    key: String,
) -> Value {
    let stats = handle.stats();
    let name = handle.name().unwrap_or_else(|| format!("Torrent #{id}"));
    let state = match &stats.state {
        TorrentStatsState::Initializing { .. } => "Checking files",
        _ if stats.finished => "Done · not seeding",
        TorrentStatsState::Live => "Downloading",
        TorrentStatsState::Paused => "Paused",
        TorrentStatsState::Error => "Error",
    };
    let fraction = if stats.total_bytes == 0 {
        0.0
    } else {
        stats.progress_bytes as f32 / stats.total_bytes as f32
    };
    let mut detail = format!(
        "{} of {} · {state}",
        fmt_bytes(stats.progress_bytes),
        fmt_bytes(stats.total_bytes)
    );
    if let Some(live) = &stats.live
        && !stats.finished
    {
        detail.push_str(&format!(" · {}", fmt_speed(live.download_speed.mbps)));
        if let Some(eta) = &live.time_remaining {
            detail.push_str(&format!(" · {eta} left"));
        }
        detail.push_str(&format!(" · {} peers", live.snapshot.peer_stats.live));
    }
    let (pause, resume) = match stats.state {
        _ if stats.finished => (None, None),
        TorrentStatsState::Live => (Some(Action::Pause(handle.clone())), None),
        TorrentStatsState::Paused | TorrentStatsState::Error => {
            (None, Some(("Resume", Action::Resume(handle.clone()))))
        }
        TorrentStatsState::Initializing { .. } => (None, None),
    };
    let only = handle.only_files();
    let mut files = Vec::new();
    let _ = handle.with_metadata(|m| {
        for (i, file) in m.file_infos.iter().enumerate() {
            if file.attrs.padding || only.as_ref().is_some_and(|o| !o.contains(&i)) {
                continue;
            }
            let have = stats.file_progress.get(i).copied().unwrap_or(0);
            let fraction = if file.len == 0 {
                1.0
            } else {
                have as f32 / file.len as f32
            };
            files.push((fraction, file.relative_filename.display().to_string()));
        }
    });
    let buttons = tools(
        ui,
        pause,
        resume,
        Action::EditFiles(handle.clone()),
        Action::OpenRemux(handle.clone()),
        Action::Remove {
            id,
            delete_files: false,
        },
        Action::AskDelete {
            id,
            name: name.clone(),
        },
    );
    let hash = handle.info_hash().as_string().to_ascii_lowercase();
    card(
        ui,
        key,
        "BitTorrent",
        "blue",
        &name,
        &hash,
        fraction,
        detail,
        stats.error.clone(),
        None,
        buttons,
        files,
    )
}

fn debrid_card(ui: &mut Ui, v: &crate::debrid::jobs::View, key: String) -> Value {
    use crate::debrid::jobs::Phase;
    let job = &v.job;
    let service = job.provider.name();
    let total = job.wanted_bytes();
    let have = v.have_bytes();
    let local = if total == 0 {
        0.0
    } else {
        have as f32 / total as f32
    };
    let sizes = format!("{} of {}", fmt_bytes(have), fmt_bytes(total));
    let (fraction, detail) = match &v.live.phase {
        Phase::Adding => (0.0, format!("Sending it to {service}…")),
        Phase::Remote {
            status,
            progress,
            speed,
            seeders,
        } => {
            let mut d = format!("{service} is getting it");
            if !status.is_empty() {
                d.push_str(&format!(" · {status}"));
            }
            if let Some(s) = speed.filter(|s| *s > 0) {
                d.push_str(&format!(" · {}", fmt_speed(s as f64 / 1_048_576.0)));
            }
            if let Some(n) = seeders {
                d.push_str(&format!(" · {n} seeders"));
            }
            (progress.unwrap_or(0.0), d)
        }
        Phase::Downloading => {
            let mut d = format!("{sizes} · Downloading from {service}");
            if v.live.speed > 0.0 {
                d.push_str(&format!(" · {}", fmt_speed(v.live.speed / 1_048_576.0)));
                let left = total.saturating_sub(have) as f64 / v.live.speed;
                d.push_str(&format!(" · {} left", fmt_duration(left)));
            }
            (local, d)
        }
        Phase::Done => (local, format!("{sizes} · Done · via {service}")),
        Phase::Paused => (local, format!("{sizes} · Paused")),
        Phase::Failed(_) => (local, sizes),
    };
    let (pause, resume) = match &v.live.phase {
        Phase::Adding | Phase::Remote { .. } | Phase::Downloading => {
            (Some(Action::DebridPause(job.id)), None)
        }
        Phase::Paused => (None, Some(("Resume", Action::DebridResume(job.id)))),
        Phase::Failed(_) => (None, Some(("Retry", Action::DebridResume(job.id)))),
        Phase::Done => (None, None),
    };
    let files = job
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.wanted)
        .map(|(i, f)| {
            let fraction = if f.len == 0 {
                1.0
            } else {
                v.live.have[i] as f32 / f.len as f32
            };
            (fraction, f.path.clone())
        })
        .collect();
    let buttons = tools(
        ui,
        pause,
        resume,
        Action::DebridEditFiles(job.id),
        Action::DebridOpenRemux(job.id),
        Action::DebridRemove {
            id: job.id,
            delete_files: false,
        },
        Action::DebridAskDelete {
            id: job.id,
            name: job.name.clone(),
        },
    );
    let error = match &v.live.phase {
        Phase::Failed(e) => Some(e.clone()),
        _ => None,
    };
    card(
        ui,
        key,
        service,
        "accent",
        &job.name,
        &job.info_hash,
        fraction,
        detail,
        error,
        v.live.note.clone(),
        buttons,
        files,
    )
}

fn usenet_card(ui: &mut Ui, v: &crate::usenet::jobs::View, key: String) -> Value {
    use crate::usenet::jobs::Phase;
    let job = &v.job;
    let total = job.wanted_bytes();
    let have = v.have_bytes().min(total);
    let source = v.source().unwrap_or("Usenet");
    let fraction = if total == 0 {
        0.0
    } else {
        have as f32 / total as f32
    };
    let sizes = format!("{} of {}", fmt_bytes(have), fmt_bytes(total));
    let detail = match &v.live.phase {
        Phase::Queued => format!("{sizes} · Waiting for the download before it"),
        Phase::Fetching => format!("Getting the NZB from {source}"),
        Phase::Downloading => {
            let mut d = format!("{sizes} · Downloading from Usenet ({source})");
            if v.live.speed > 0.0 {
                d.push_str(&format!(" · {}", fmt_speed(v.live.speed / 1_048_576.0)));
                d.push_str(&format!(
                    " · {} left",
                    fmt_duration(total.saturating_sub(have) as f64 / v.live.speed)
                ));
            }
            d
        }
        Phase::Repairing(what) => format!("Repairing with PAR2: {what}"),
        Phase::Done => format!("{sizes} · Done · from Usenet ({source})"),
        Phase::Paused => format!("{sizes} · Paused"),
        Phase::Failed(_) => sizes,
    };
    let mut note = v.live.note.clone();
    if v.live.missing > 0 && !matches!(v.live.phase, Phase::Done) {
        note = Some(format!(
            "{} articles aren't on any of your providers; they'll be repaired with PAR2",
            v.live.missing
        ));
    }
    let (pause, resume) = match &v.live.phase {
        Phase::Queued | Phase::Fetching | Phase::Downloading | Phase::Repairing(_) => {
            (Some(Action::UsenetPause(job.id)), None)
        }
        Phase::Paused => (None, Some(("Resume", Action::UsenetResume(job.id)))),
        Phase::Failed(_) => (None, Some(("Retry", Action::UsenetResume(job.id)))),
        Phase::Done => (None, None),
    };
    let files = job
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.wanted)
        .map(|(i, f)| {
            let fraction = if f.len == 0 {
                1.0
            } else {
                (v.live.have[i] as f32 / f.len as f32).min(1.0)
            };
            (fraction, f.path.clone())
        })
        .collect();
    let buttons = tools(
        ui,
        pause,
        resume,
        Action::UsenetEditFiles(job.id),
        Action::UsenetOpenRemux(job.id),
        Action::UsenetRemove {
            id: job.id,
            delete_files: false,
        },
        Action::UsenetAskDelete {
            id: job.id,
            name: job.name.clone(),
        },
    );
    let error = match &v.live.phase {
        Phase::Failed(e) => Some(e.clone()),
        _ => None,
    };
    card(
        ui,
        key,
        "Usenet",
        "good",
        &job.name,
        &job.info_hash,
        fraction,
        detail,
        error,
        note,
        buttons,
        files,
    )
}
