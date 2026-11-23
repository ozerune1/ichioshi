//! A series' page: what it is, what to do with it (play, download, add,
//! your lists), its episodes, and the release downloads come from.

use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::library_ui::{Coverage, EpisodeRow};
use crate::app::offline_ui::{OfflineAction, Saving};
use crate::app::tracking_ui::TrackingAction;
use crate::app::watching::{PlayerAction, clock, up_next};
use crate::app::{
    Action, AnimePage, MAX_NYAA_ROWS, Page, chosen_release, fmt_bytes, nyaa_release_label,
    release_label,
};
use crate::fallback::ReleaseKind;
use crate::library::ArrRef;
use crate::servers::arr::Kind;
use crate::tracking::{Change, Status};

pub(super) fn page(ui: &mut Ui, ap: &AnimePage) -> Value {
    let app = ui.app;
    let anime = &ap.anime;
    let id = anime.anilist_id;
    let details = app.details(id);
    let facts = match details {
        Some(d) => crate::app::library_ui::facts(d),
        None if app.pages.details_loading.contains(&id) => "Getting details from AniList…".into(),
        None => anime.summary(),
    };
    let mut links = vec![link(ui, "AniList ↗", anime.anilist_url())];
    if anime.entry.is_some() {
        links.push(link(ui, "releases.moe ↗", anime.seadex_url()));
    }
    let (rows, extras) = app.episode_rows(ap);
    json!({
        "kind": "series",
        "id": id,
        "title": anime.title,
        "alt": anime.alt_title.clone().or(details.and_then(|d| d.native.clone())),
        "cover": anime.cover_large.clone().or(details.and_then(|d| d.cover.clone())),
        "banner": details.and_then(|d| d.banner.clone()),
        "facts": facts,
        "genres": details.map(|d| d.genres.iter().take(6).cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "links": links,
        "description": details.and_then(|d| d.description.clone()),
        "actions": actions(ui, ap, &rows),
        "lists": lists(ui, id),
        "library": library_box(ui, ap),
        "episodes": episodes(ui, ap, &rows, &extras),
        "release": release(ui, ap),
    })
}

fn link(ui: &mut Ui, label: &str, url: String) -> Value {
    let id = ui.edit(move |app| app.ctx.open_url(&url));
    json!({"label": label, "id": id})
}

/// Episodes not here, out, and in the release: what "Download missing" gets.
fn missing(rows: &[EpisodeRow], coverage: &Coverage<'_>) -> Vec<u32> {
    rows.iter()
        .filter(|e| e.file.is_none() && e.downloading.is_none() && e.out && coverage.has(e.n))
        .map(|e| e.n)
        .collect()
}

/// Play, add, download (or send to Sonarr or Radarr), save offline.
fn actions(ui: &mut Ui, ap: &AnimePage, rows: &[EpisodeRow]) -> Vec<Value> {
    let app = ui.app;
    let id = ap.anime.anilist_id;
    let series = app.library.find(id);
    let coverage = app.coverage(ap);
    let missing = missing(rows, &coverage);
    let mut buttons = Vec::new();
    if let Some(s) = series {
        let disk = rows.iter().filter(|e| e.file.is_some()).map(|e| e.n);
        if let Some(next) = up_next(s, disk) {
            let resume = s
                .watched
                .get(&next)
                .and_then(crate::library::Watch::resume_at);
            let label = match (resume, s.one_episode()) {
                (Some(at), true) => format!("▶  Resume · {}", clock(at)),
                (None, true) => "▶  Play".to_owned(),
                (Some(at), false) => format!("▶  Resume episode {next} · {}", clock(at)),
                (None, false) => format!("▶  Play episode {next}"),
            };
            buttons.push(ui.button(
                label,
                Some(Action::Player(PlayerAction::Play { id, episode: next })),
            ));
        }
    } else if app.manage.as_ref().is_some_and(|m| m.anilist == id) {
        // In Sonarr as a later part of a series there: nothing to add.
    } else if app.servers_mode() {
        let to = match app.server_place(id) {
            Some((Kind::Radarr, _)) => "Radarr",
            _ => "Sonarr",
        };
        let why = app.why_not_addable(id);
        let busy = app.servers.busy.contains(&id);
        buttons.push(ui.button_why(
            format!("+  Add to {to}"),
            (why.is_none() && !busy).then_some(Action::AddToLibrary),
            why,
        ));
    } else {
        buttons.push(ui.button("+  Add to library", Some(Action::AddToLibrary)));
    }
    if app.servers_mode() {
        let to = match series.and_then(|s| s.remote.as_ref()).map(|r| r.arr) {
            Some(ArrRef::Radarr { .. }) => "Radarr",
            Some(_) => "Sonarr",
            None => match app.server_place(id) {
                Some((Kind::Radarr, _)) => "Radarr",
                _ => "Sonarr",
            },
        };
        let busy = app.servers.busy.contains(&id);
        let ready = !matches!(
            coverage,
            Coverage::Nothing(_) | Coverage::Loading | Coverage::Failed(_)
        );
        buttons.push(ui.button_why(
            format!("↓  Send the release to {to}"),
            (ready && !busy).then_some(Action::DownloadEpisodes(Vec::new())),
            (!ready).then(|| coverage.why_not()),
        ));
        if let Some(r) = series.and_then(|s| s.remote.as_ref()) {
            let savable = r
                .episodes
                .iter()
                .filter(|(n, e)| {
                    e.jellyfin.is_some()
                        && app.offline_copy(id, **n).is_none()
                        && app.offline_job(id, **n).is_none()
                })
                .count();
            if savable > 0 {
                buttons.push(ui.button(
                    format!("↓  Save {savable} offline"),
                    Some(Action::Offline(OfflineAction::Save {
                        id,
                        episodes: Vec::new(),
                    })),
                ));
            }
        }
        return buttons;
    }
    let label = match missing.len() {
        0 => "↓  Download series".to_owned(),
        n => format!("↓  Download {n} missing"),
    };
    let can = !missing.is_empty();
    let have = rows.iter().filter(|e| e.file.is_some()).count();
    let why = (!can && !rows.is_empty() && have < rows.len()).then(|| coverage.why_not());
    buttons.push(ui.button_why(
        label,
        can.then(|| Action::DownloadEpisodes(missing.clone())),
        why,
    ));
    buttons
}

/// Your lists: status, score, episodes watched; what each list says.
fn lists(ui: &mut Ui, id: u64) -> Value {
    let app = ui.app;
    let t = &app.tracking;
    if !t.any() {
        return Value::Null;
    }
    let change = |c: Change| Action::Tracking(TrackingAction::Change { id, change: c });
    let ready = !t.pending(id);
    let shown = t.shown(id);
    let statuses: Vec<Value> = Status::ALL
        .into_iter()
        .map(|s| {
            json!({
                "label": s.name(),
                "on": shown.is_some_and(|e| e.status == s),
                "id": ready.then(|| ui.act(change(Change::Status(s)))),
            })
        })
        .collect();
    let mut panel = json!({
        "status": statuses,
        "key": shown.map(|e| format!("{:?}", e.status).to_lowercase()),
        "refresh": ui.button("Refresh", Some(Action::Tracking(TrackingAction::Refresh))),
    });
    if let Some(e) = shown {
        let now = e.score.round().clamp(0.0, 10.0) as u8;
        let scores: Vec<Value> = (0..=10u8)
            .rev()
            .map(|n| {
                json!({
                    "label": if n == 0 { "No score".to_owned() } else { format!("★ {n}") },
                    "on": n == now,
                    "id": ui.act(change(Change::Score(f32::from(n)))),
                })
            })
            .collect();
        let episodes = app.episode_count(id);
        let max = episodes.filter(|n| *n > 0).unwrap_or(9999);
        let p = e.progress;
        panel["score"] = json!(scores);
        panel["progress"] = json!({
            "text": match episodes {
                Some(n) => format!("{p} / {n}"),
                None => format!("{p} watched"),
            },
            "minus": ui.button("−", (p > 0).then(|| change(Change::Progress(p - 1)))),
            "plus": ui.button("+1", (p < max).then(|| change(Change::Progress(p + 1)))),
        });
    }
    // What each list says.
    let episodes = app.episode_count(id);
    let says: Vec<String> = t
        .services()
        .into_iter()
        .map(|service| {
            let line = match (t.entry(service, id), t.key(service, id)) {
                _ if !t.lists.contains_key(&service) => "reading your list…".to_owned(),
                (Some(e), _) => {
                    let progress = match episodes {
                        Some(n) => format!("{}/{n}", e.progress),
                        None => format!("{} watched", e.progress),
                    };
                    format!("{} · ★ {} · {progress}", e.status.name(), e.score_text())
                }
                (None, None) if t.no_mal.contains(&id) => "not on MyAnimeList".to_owned(),
                (None, None) => "finding it…".to_owned(),
                (None, Some(_)) => "not on your list".to_owned(),
            };
            format!("{}: {line}", service.name())
        })
        .collect();
    panel["says"] = json!(says);
    panel
}

/// Where it is: in the library (and its languages), not yet, on your
/// servers, or not on them.
fn library_box(ui: &mut Ui, ap: &AnimePage) -> Value {
    let app = ui.app;
    let anime = &ap.anime;
    let id = anime.anilist_id;
    if app.servers_mode() {
        if let Some(m) = app.manage.as_ref().filter(|m| m.anilist == id) {
            return json!({"t": "manage", "panel": super::manage::panel(ui, m)});
        }
        if let Some(r) = app.library.find(id).and_then(|s| s.remote.as_ref()) {
            let kind = match r.arr {
                ArrRef::Sonarr { season, offset, .. } => {
                    if offset > 0 {
                        format!("Sonarr · season {season}, from its episode {}", offset + 1)
                    } else {
                        format!("Sonarr · season {season}")
                    }
                }
                ArrRef::Radarr { .. } => "Radarr · film".to_owned(),
            };
            let have = r.episodes.values().filter(|e| e.file.is_some()).count();
            return json!({
                "t": "remote",
                "title": format!("On {kind}"),
                "note": format!("{have} of its {} episodes there", r.episodes.len()),
            });
        }
        let why = app.why_not_addable(id).unwrap_or_else(|| {
            "Adding it puts it in Sonarr or Radarr, watched for (not searched for: send them \
             the release above, or have them search)."
                .into()
        });
        return json!({"t": "not_on_servers", "title": "Not on your servers", "note": why});
    }
    let Some(series) = app.library.find(id) else {
        let folder = app
            .library
            .folder_for(&crate::app::new_series(anime), &app.library_dir());
        return json!({
            "t": "not_in_library",
            "title": "Not in your library",
            "note": format!(
                "It would go in {}. Downloading episodes adds it too.",
                folder.display()
            ),
            "add": ui.button("+ Add to library", Some(Action::AddToLibrary)),
        });
    };
    // Its own languages, or everyone's.
    let own = series.prefs.clone();
    let everyone = app.settings.prefs.clone();
    let toggle = ui.act(Action::SetSeriesPrefs {
        id,
        prefs: if own.is_some() {
            None
        } else {
            Some(everyone.clone())
        },
    });
    json!({
        "t": "in_library",
        "title": "In your library",
        "note": series.folder.display().to_string(),
        "remove": ui.button("Remove", Some(Action::RemoveSeries { id, delete_files: false })),
        "delete": ui.button("Delete folder…", Some(Action::AskRemoveSeries(id))),
        "languages": {
            "own": own.is_some(),
            "toggle": toggle,
            "summary": match &own {
                Some(p) => p.summary(),
                None => format!("Everyone's: {} (in Settings)", everyone.summary()),
            },
        },
    })
}

/// The episodes, with what to do with each.
fn episodes(ui: &mut Ui, ap: &AnimePage, rows: &[EpisodeRow], extras: &[(String, u64)]) -> Value {
    let app = ui.app;
    let id = ap.anime.anilist_id;
    let series = app.library.find(id);
    let coverage = app.coverage(ap);
    let have = rows.iter().filter(|e| e.file.is_some()).count();
    let bytes: u64 = rows
        .iter()
        .filter_map(|e| e.file.as_ref().map(|f| f.1))
        .sum();
    let downloading = rows.iter().filter(|e| e.downloading.is_some()).count();
    let mut summary = format!(
        "{have} of {} {}",
        rows.len(),
        if app.servers_mode() {
            "on your servers"
        } else {
            "in your library"
        }
    );
    if bytes > 0 {
        summary += &format!(" · {}", fmt_bytes(bytes));
    }
    if downloading > 0 {
        summary += &format!(" · {downloading} downloading");
    }
    // The library's files: all of them to the playlist, or to remux.
    let mut tools = Vec::new();
    if let Some(s) = series.filter(|s| s.remote.is_none()) {
        let files: Vec<std::path::PathBuf> = rows
            .iter()
            .filter_map(|e| e.file.as_ref().map(|(rel, _)| s.folder.join(rel)))
            .collect();
        let new: Vec<std::path::PathBuf> = files
            .iter()
            .filter(|p| !app.playlist.items.iter().any(|i| &i.path == *p))
            .cloned()
            .collect();
        if !files.is_empty() {
            tools.push(ui.button(
                match new.len() {
                    0 => "All in the playlist".to_owned(),
                    n => format!("+ Playlist ({n})"),
                },
                (!new.is_empty()).then(|| Action::AddToPlaylist { id, files: new }),
            ));
            tools.push(ui.button("Remux all…", Some(Action::RemuxLibrary { id, files })));
        }
    }
    let list: Vec<Value> = rows.iter().map(|e| episode(ui, id, e, &coverage)).collect();
    json!({
        "summary": summary,
        "tools": tools,
        "rows": list,
        "extras": extras.iter().map(|(rel, len)| json!({"name": rel, "size": fmt_bytes(*len)})).collect::<Vec<_>>(),
        "empty": rows.is_empty(),
    })
}

fn episode(ui: &mut Ui, id: u64, e: &EpisodeRow, coverage: &Coverage<'_>) -> Value {
    let app = ui.app;
    let n = e.n;
    let series = app.library.find(id);
    let watch = series.and_then(|s| s.watched.get(&n));
    let done = watch.is_some_and(|w| w.done);
    let mal = app.episode_scores.episode(id, n);
    let mut tags = Vec::new();
    if e.file.is_some()
        && let Some(r) = app.episode_release(id, n).filter(|r| !r.is_empty())
    {
        if let Some(g) = &r.group {
            tags.push(json!({"label": g, "tone": "plain"}));
        }
        if let Some(s) = &r.source {
            tags.push(json!({"label": s, "tone": if s == "BD" { "accent" } else { "muted" }}));
        }
        match r.dual_audio {
            Some(true) => tags.push(json!({"label": "Dual audio", "tone": "good"})),
            Some(false) => tags.push(json!({"label": "Japanese only", "tone": "faint"})),
            None => {}
        }
        match r.censored {
            Some(true) => tags.push(json!({"label": "Censored", "tone": "warn"})),
            Some(false) => tags.push(json!({"label": "Uncensored", "tone": "good"})),
            None => {}
        }
    }
    let progress = watch
        .filter(|w| !w.done && w.position > 0.0 && w.duration > 0.0)
        .map(|w| json!({"fraction": w.position / w.duration, "text": format!("Watched to {}", clock(w.position))}));
    let airs = e
        .airs
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%-d %b %Y")
                .to_string()
        });
    let play_label = |resume: bool| if resume { "▶ Resume" } else { "▶ Play" };
    let resumable = watch.and_then(crate::library::Watch::resume_at).is_some();
    let mut buttons: Vec<Value> = Vec::new();
    let mut play = Value::Null;
    let mut state = Value::Null;
    if let Some(p) = e.downloading {
        state = json!({"downloading": p});
    } else if let (Some((_, len)), Some(r)) = (&e.file, series.and_then(|s| s.remote.as_ref())) {
        // On the servers: it plays from Jellyfin (or its offline copy).
        let on_jellyfin = r.episodes.get(&n).is_some_and(|x| x.jellyfin.is_some());
        let copy = app.offline_copy(id, n).is_some();
        if on_jellyfin || copy {
            play = ui.button(
                play_label(resumable),
                Some(Action::Player(PlayerAction::Play { id, episode: n })),
            );
        } else {
            state = json!({"text": "Not in Jellyfin yet"});
        }
        buttons.extend(offline(ui, id, n, on_jellyfin));
        let _ = len;
    } else if let (Some((rel, _)), Some(s)) = (&e.file, series) {
        let path = s.folder.join(rel);
        play = ui.button(
            play_label(resumable),
            Some(Action::Player(PlayerAction::Play { id, episode: n })),
        );
        let listed = app.playlist.items.iter().any(|i| i.path == path);
        buttons.push(ui.button(
            if listed { "In playlist" } else { "+ Playlist" },
            (!listed).then(|| Action::AddToPlaylist {
                id,
                files: vec![path],
            }),
        ));
    } else if !e.out {
        state = json!({"text": "Not out yet"});
    } else if series.and_then(|s| s.remote.as_ref()).is_some_and(|r| {
        matches!(r.arr, ArrRef::Radarr { .. })
            || r.episodes.get(&n).is_some_and(|e| e.arr_id.is_some())
    }) {
        buttons.push(ui.button(
            "⌕ Search",
            (!app.servers.busy.contains(&id)).then(|| {
                Action::Servers(crate::app::servers_ui::ServersAction::Search {
                    id,
                    episodes: vec![n],
                })
            }),
        ));
    } else if app.servers_mode() {
    } else {
        let why = (!coverage.has(n)).then(|| coverage.why_not());
        buttons.push(ui.button_why(
            "↓ Download",
            coverage.has(n).then(|| Action::DownloadEpisodes(vec![n])),
            why,
        ));
    }
    if e.out {
        buttons.push(ui.button("Discuss", Some(Action::OpenDiscussion { id, episode: n })));
    }
    // Sonarr: watched for or not; its details and releases.
    let mut monitor = Value::Null;
    if let Some(m) = app.manage.as_ref().filter(|m| m.kind == Kind::Sonarr)
        && let Some(r) = app.library.find(m.anilist).and_then(|s| s.remote.as_ref())
        && let Some(ep) = r.episodes.get(&n).filter(|ep| ep.arr_id.is_some())
    {
        use crate::app::manage_ui::{Looking, ManageAction};
        let on = ep.monitored;
        monitor = json!({
            "on": on,
            "id": ui.act(Action::Manage(ManageAction::SetEpisodesMonitored(vec![n], !on))),
        });
        let open = m.episode.as_ref().is_some_and(|(e, _)| *e == n);
        buttons.push(ui.button(
            if open { "Hide details" } else { "Details" },
            Some(Action::Manage(ManageAction::Episode(n))),
        ));
        buttons.push(ui.button(
            "Releases…",
            Some(Action::Manage(ManageAction::Look(Looking::Episode(n)))),
        ));
    }
    let details = super::manage::episode_details(ui, n);
    json!({
        "n": n,
        "details": details,
        "title": e.title,
        "present": e.file.is_some() || e.downloading.is_some(),
        "done": done,
        "filler": mal.is_some_and(|m| m.filler),
        "recap": mal.is_some_and(|m| m.recap),
        "score": mal.and_then(|m| m.score).map(|v| format!("★ {v:.2}")),
        "tags": tags,
        "progress": progress,
        "airs": airs,
        "state": state,
        "play": play,
        "buttons": buttons,
        "monitor": monitor,
    })
}

/// An episode's copy from Jellyfin: saved (remove it), saving, or not.
fn offline(ui: &mut Ui, id: u64, n: u32, on_jellyfin: bool) -> Vec<Value> {
    let app = ui.app;
    let off = |a: OfflineAction| Some(Action::Offline(a));
    if app.offline_copy(id, n).is_some() {
        return vec![
            json!({"label": "Offline ✓", "id": null}),
            ui.button("Remove copy", off(OfflineAction::Remove { id, episode: n })),
        ];
    }
    if let Some(j) = app.offline_job(id, n) {
        let job = j.id;
        return match &j.state {
            Saving::Waiting => vec![
                json!({"label": "Waiting to save", "id": null}),
                ui.button("Stop", off(OfflineAction::Cancel(job))),
            ],
            Saving::Getting => vec![
                json!({"label": format!("Saving {:.0}%", j.fraction() * 100.0), "id": null}),
                ui.button("Stop", off(OfflineAction::Cancel(job))),
            ],
            Saving::Failed(e) => vec![
                json!({"label": "Couldn't save", "id": null, "why": e}),
                ui.button("Retry", off(OfflineAction::Retry(job))),
            ],
        };
    }
    if on_jellyfin {
        return vec![ui.button(
            "↓ Save offline",
            off(OfflineAction::Save {
                id,
                episodes: vec![n],
            }),
        )];
    }
    Vec::new()
}

/// Where downloads come from: SeaDex's releases, or (none on Nyaa)
/// Nyaa's, ranked by release group; to choose from.
fn release(ui: &mut Ui, ap: &AnimePage) -> Value {
    let app = ui.app;
    let source = match &ap.nyaa {
        Some(search) => search
            .chosen
            .and_then(|i| search.releases.get(i))
            .map(nyaa_release_label),
        None => chosen_release(ap).map(release_label),
    };
    let title = match &source {
        Some(label) => format!("Downloads come from {label}"),
        None if ap.entry_loading => "Looking up its releases on SeaDex…".to_owned(),
        None => "The release to download from".to_owned(),
    };
    let mut notes = Vec::new();
    if let Some(entry) = &ap.anime.entry {
        if entry.incomplete {
            notes.push(json!({"text": "⚠ SeaDex marks this entry as incomplete.", "tone": "warn"}));
        }
        if !entry.theoretical_best.is_empty() {
            notes.push(json!({"text": format!("Theoretical best: {}", entry.theoretical_best), "tone": "muted"}));
        }
    }
    let usenet = app.usenet.active();
    let mut cards = Vec::new();
    let mut search = Value::Null;
    if let Some(s) = &ap.nyaa {
        notes.push(json!({
            "text": if ap.anime.entry.is_some() {
                "SeaDex has no Nyaa release for this, so Nyaa's releases are ranked by TRaSH \
                 Guides release-group tiers."
            } else {
                "Not on SeaDex, so Nyaa's releases are ranked by TRaSH Guides release-group tiers."
            },
            "tone": "muted",
        }));
        search = json!({
            "query": ui.field(&s.query, |app, q| {
                if let Page::Anime(ap) = &mut app.page && let Some(s) = &mut ap.nyaa {
                    s.query = q;
                }
            }),
            "go": ui.button(
                if s.loading { "Searching…" } else { "Search Nyaa" },
                (!s.loading && !s.query.trim().is_empty()).then_some(Action::SearchNyaa),
            ),
        });
        if s.query.trim() == s.default_plan.display_query()
            && let Some(season) = s.default_plan.season
        {
            notes.push(json!({"text": format!("Showing season {season} only. Edit the search to see everything."), "tone": "faint"}));
        }
        if let Some(e) = &s.error {
            notes.push(json!({"text": e, "tone": "error"}));
        }
        if s.searched && !s.loading && s.releases.is_empty() && s.error.is_none() {
            notes.push(json!({"text": "Nothing found on Nyaa. Try another name for the show, e.g. its English title.", "tone": "plain"}));
        }
        for (i, r) in s.releases.iter().enumerate().take(MAX_NYAA_ROWS) {
            let problem = r.problem();
            let mut facts = vec![r.episodes_label(), r.quality_label()];
            if r.dual_audio {
                facts.push("Dual audio".into());
            }
            facts.push(fmt_bytes(r.size));
            facts.push(format!("{} seeders", r.seeders_label()));
            let mut card = json!({
                "group": r.group,
                "title": r.title(),
                "rank": r.rank.label,
                "best": r.rank.score > 0 && !r.rank.is_blocked(),
                "blocked": r.rank.is_blocked(),
                "facts": facts.into_iter().filter(|f| !f.is_empty()).collect::<Vec<_>>(),
                "chosen": s.chosen == Some(i),
                "problem": problem.map(|p| p.to_string()),
                "choose": problem.is_none().then(|| ui.edit(move |app| {
                    if let Page::Anime(ap) = &mut app.page && let Some(s) = &mut ap.nyaa {
                        s.chosen = Some(i);
                    }
                })),
            });
            if usenet && let ReleaseKind::Torrent { item, .. } = &r.kind {
                card["usenet"] = json!(nzb(app, &item.info_hash));
            }
            cards.push(card);
        }
        if s.releases.len() > MAX_NYAA_ROWS {
            notes.push(json!({"text": format!("{} lower-ranked releases not shown.", s.releases.len() - MAX_NYAA_ROWS), "tone": "faint"}));
        }
    } else if let Some(entry) = &ap.anime.entry {
        let dual = app.prefer_dual_audio(ap.anime.anilist_id);
        for i in entry.ranked(dual) {
            let r = &entry.releases[i];
            let nyaa = r.nyaa_id();
            let mut facts = vec![
                if r.dual_audio {
                    "Dual audio"
                } else {
                    "Japanese"
                }
                .to_owned(),
                format!("{} files", r.files.len()),
                fmt_bytes(r.total_size()),
            ];
            if !r.tags.is_empty() {
                facts.push(r.tags.join(", "));
            }
            let mut card = json!({
                "group": r.group,
                "rank": if r.is_best { "★ Best" } else { "Alt" },
                "best": r.is_best,
                "facts": facts,
                "chosen": ap.chosen == Some(i),
                "problem": nyaa.is_none().then(|| format!("{} (private): this app downloads from Nyaa only", r.tracker)),
                "choose": nyaa.map(|_| ui.edit(move |app| {
                    if let Page::Anime(ap) = &mut app.page {
                        ap.chosen = Some(i);
                    }
                })),
            });
            if usenet
                && nyaa.is_some()
                && let Some(hash) = r.info_hash_hex()
            {
                card["usenet"] = json!(nzb(app, &hash));
            }
            cards.push(card);
        }
    }
    // Choosing files by hand.
    let has_choice = match &ap.nyaa {
        Some(s) => s.chosen.is_some(),
        None => ap.chosen.is_some(),
    };
    let ready = if app.usenet.active() {
        Ok(())
    } else {
        app.download_ready()
    };
    let enabled = has_choice && ready.is_ok() && !app.loading_listing;
    let note = if app.loading_listing {
        "Fetching the torrent from Nyaa…".to_owned()
    } else if let Err(why) = &ready {
        why.clone()
    } else if !has_choice {
        "No release to download.".to_owned()
    } else {
        "Or pick which files to download yourself.".to_owned()
    };
    let choose = (!app.servers_mode() && (!cards.is_empty() || has_choice)).then(|| {
        json!({
            "button": ui.button("Choose files…", enabled.then_some(Action::ChooseFiles)),
            "note": note,
        })
    });
    json!({
        "title": title,
        "chosen": source.is_some(),
        "notes": notes,
        "search": search,
        "cards": cards,
        "choose": choose,
        "notes_seadex": ap.anime.entry.as_ref().map(|e| e.notes.clone()).filter(|n| !n.is_empty()),
    })
}

/// Whether a torrent's on Usenet: "NZB", "—", or "checking…".
fn nzb(app: &crate::app::App, hash: &str) -> &'static str {
    match app.usenet.links(hash) {
        None => "checking…",
        Some(links) if !links.is_empty() => "NZB",
        Some(_) => "—",
    }
}
