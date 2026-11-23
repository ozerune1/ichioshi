//! A series on Sonarr (or a film on Radarr), managed from its page as in
//! Sonarr: its settings, searching its indexers, its files, its history;
//! and an episode's details. Built from the settings' items.

use serde_json::{Value, json};

use super::form::{Change, boxed, buttons, check, field, said, select, text};
use super::ui::Ui;
use crate::app::discover_ui::Loaded;
use crate::app::manage_ui::{Edit, Looking, Manage, ManageAction, Tab};
use crate::app::{Action, App, fmt_bytes};
use crate::library::Remote;
use crate::servers::arr::Kind;

fn manage(a: ManageAction) -> Option<Action> {
    Some(Action::Manage(a))
}

fn age(hours: f64) -> String {
    if hours < 48.0 {
        format!("{:.0} h", hours.max(0.0))
    } else if hours < 24.0 * 60.0 {
        format!("{:.0} d", hours / 24.0)
    } else {
        format!("{:.0} mo", hours / 24.0 / 30.0)
    }
}

fn when(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%-d %b %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn event_name(kind: &str) -> &str {
    match kind {
        "grabbed" => "Grabbed",
        "downloadFolderImported" | "movieFolderImported" => "Imported",
        "downloadFailed" => "Download failed",
        "episodeFileDeleted" | "movieFileDeleted" => "File deleted",
        "episodeFileRenamed" | "movieFileRenamed" => "File renamed",
        "downloadIgnored" => "Ignored",
        "seriesFolderImported" => "Imported from the folder",
        other => other,
    }
}

/// The panel: what and where it is, links, its tabs and the tab's items.
pub(super) fn panel(ui: &mut Ui, m: &Manage) -> Value {
    let app = ui.app;
    let remote = app.library.find(m.anilist).and_then(|s| s.remote.as_ref());
    let name = m.kind.name();
    let record = match &m.record {
        Loaded::Ready(r) => Some(r),
        _ => None,
    };
    let title = record
        .and_then(|r| r["title"].as_str())
        .unwrap_or(name)
        .to_owned();
    let place = match (m.season, m.follows) {
        (_, Some(prev)) => format!(
            "after {} (its episodes show here once {name} lists them)",
            app.library
                .find(prev)
                .map(|s| s.title().to_owned())
                .unwrap_or_default()
        ),
        (Some(s), None) => match remote.map(|r| r.arr) {
            Some(crate::library::ArrRef::Sonarr { offset, .. }) if offset > 0 => {
                format!("season {s}, from its episode {}", offset + 1)
            }
            _ => format!("season {s}"),
        },
        (None, None) => "film".into(),
    };
    let mut links = Vec::new();
    if let Some(slug) = record.and_then(|r| r["titleSlug"].as_str()) {
        let account = match m.kind {
            Kind::Sonarr => &app.servers.config.sonarr,
            Kind::Radarr => &app.servers.config.radarr,
        };
        let arr = crate::servers::arr::Arr::new(m.kind, account, &app.http);
        let url = arr.page(slug);
        links.push(json!({"label": format!("Open in {name} ↗"), "id": ui.edit(move |app| app.ctx.open_url(&url))}));
    }
    if let (Some(item), Some(j)) = (remote.and_then(|r| r.jellyfin.as_ref()), app.jellyfin()) {
        let url = j.page(item);
        links.push(json!({"label": "Open in Jellyfin ↗", "id": ui.edit(move |app| app.ctx.open_url(&url))}));
    }
    let tabs: Vec<Value> = [
        (Tab::Overview, "Overview"),
        (Tab::Search, "Releases"),
        (Tab::Files, "Files"),
        (Tab::History, "History"),
    ]
    .into_iter()
    .map(|(tab, label)| json!({"label": label, "on": m.tab == tab, "id": ui.act(Action::Manage(ManageAction::Tab(tab)))}))
    .collect();
    let items = match m.tab {
        Tab::Overview => overview(ui, m, remote),
        Tab::Search => releases(ui, m),
        Tab::Files => files(ui, m, remote),
        Tab::History => history(m, remote),
    };
    json!({
        "kind": name,
        "title": title,
        "place": place,
        "links": links,
        "tabs": tabs,
        "items": items,
    })
}

fn overview(ui: &mut Ui, m: &Manage, remote: Option<&Remote>) -> Vec<Value> {
    let app = ui.app;
    let name = m.kind.name();
    let record = match &m.record {
        Loaded::Loading => return vec![said(format!("Reading it from {name}…"), "muted")],
        Loaded::Failed(e) => {
            return vec![said(format!("Couldn't read it from {name}: {e}"), "error")];
        }
        Loaded::Ready(r) => r,
    };
    let about = match app.servers.about.get(&m.kind) {
        Some(Loaded::Ready(a)) => Some(a),
        _ => None,
    };
    let profile = record["qualityProfileId"].as_u64();
    let profile_name = about
        .and_then(|a| a.profiles.iter().find(|p| Some(p.id) == profile))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("profile {}", profile.unwrap_or(0)));
    let monitored = record["monitored"] == true;
    let mut facts = vec![format!("Quality: {profile_name}")];
    let tag_ids: Vec<u64> = record["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    if let (false, Some(Loaded::Ready(list))) = (tag_ids.is_empty(), &m.tags) {
        let names: Vec<&str> = list
            .iter()
            .filter(|t| tag_ids.contains(&t.id))
            .map(|t| t.label.as_str())
            .collect();
        if !names.is_empty() {
            facts.push(format!("Tags: {}", names.join(", ")));
        }
    }
    match m.kind {
        Kind::Sonarr => {
            facts.push(format!(
                "Type: {}",
                record["seriesType"].as_str().unwrap_or("?")
            ));
            if record["monitorNewItems"] == "none" {
                facts.push("New seasons not watched for".into());
            }
        }
        Kind::Radarr => facts.push(format!(
            "Available when: {}",
            record["minimumAvailability"].as_str().unwrap_or("?")
        )),
    }
    let mut items = vec![
        check(
            ui,
            match m.kind {
                Kind::Sonarr => "Watch for this series",
                Kind::Radarr => "Watch for this film",
            },
            monitored,
            |app, on| app.perform(Action::Manage(ManageAction::SetMonitored(on))),
        ),
        said(facts.join(" · "), "muted"),
        said(
            record["path"].as_str().unwrap_or_default().to_owned(),
            "faint",
        ),
    ];
    let idle = !m.busy;
    let mut row = Vec::new();
    if let Some(r) = remote.filter(|_| m.kind == Kind::Sonarr) {
        let watched = r.episodes.values().filter(|e| e.monitored).count();
        let missing = r.episodes.values().filter(|e| e.file.is_none()).count();
        items.push(said(
            format!(
                "{watched} of its {} episodes watched for · {missing} missing",
                r.episodes.len()
            ),
            "muted",
        ));
        row.push(
            ui.button(
                "Watch for all its episodes",
                (idle && watched < r.episodes.len())
                    .then(|| Action::Manage(ManageAction::SetEpisodesMonitored(Vec::new(), true))),
            ),
        );
        row.push(
            ui.button(
                "Watch for none",
                (idle && watched > 0)
                    .then(|| Action::Manage(ManageAction::SetEpisodesMonitored(Vec::new(), false))),
            ),
        );
        row.push(ui.button(
            format!("Search for the {missing} missing"),
            (idle && missing > 0).then(|| Action::Manage(ManageAction::AutoSearch(Vec::new()))),
        ));
    }
    if m.kind == Kind::Radarr {
        row.push(ui.button(
            "Search for it",
            idle.then(|| Action::Manage(ManageAction::AutoSearch(Vec::new()))),
        ));
    }
    row.push(ui.button(
        "Refresh & scan",
        idle.then_some(Action::Manage(ManageAction::Refresh)),
    ));
    row.push(ui.button(
        "Rename files…",
        idle.then_some(Action::Manage(ManageAction::PreviewRenames)),
    ));
    row.push(ui.button(
        "Import by hand…",
        Some(Action::Import(crate::app::import_ui::ImportAction::Open {
            kind: m.kind,
            owner: m.owner,
            anilist: Some(m.anilist),
            download: None,
        })),
    ));
    row.push(
        ui.button(
            "Edit…",
            m.edit
                .is_none()
                .then_some(Action::Manage(ManageAction::StartEdit)),
        ),
    );
    let mut delete = ui.button("Delete…", manage(ManageAction::AskDelete));
    delete["kind"] = json!("danger");
    row.push(delete);
    items.push(json!({"t": "buttons", "buttons": row, "primary": false, "small": true}));
    if let Some(e) = &m.edit {
        items.push(boxed(edit(ui, m, e)));
    }
    if let Some(r) = &m.renames {
        items.push(boxed(renames(ui, r)));
    }
    items
}

/// Its settings, being changed.
fn edit(ui: &mut Ui, m: &Manage, e: &Edit) -> Vec<Value> {
    let app = ui.app;
    let about = match app.servers.about.get(&m.kind) {
        Some(Loaded::Ready(a)) => a,
        Some(Loaded::Failed(err)) => {
            return vec![said(
                format!("Couldn't read {}'s settings: {err}", m.kind.name()),
                "error",
            )];
        }
        _ => {
            return vec![said(
                format!("Reading {}'s profiles and folders…", m.kind.name()),
                "muted",
            )];
        }
    };
    let change = |f: Box<dyn Fn(&mut Edit)>| -> Change {
        let e = e.clone();
        Box::new(move |app: &mut App| {
            let mut e = e.clone();
            f(&mut e);
            app.perform(Action::Manage(ManageAction::ChangeEdit(e)));
        })
    };
    let profiles = about
        .profiles
        .iter()
        .map(|p| {
            let id = p.id;
            (
                p.name.clone(),
                e.profile == id,
                change(Box::new(move |e| e.profile = id)),
            )
        })
        .collect();
    let mut items = vec![select(ui, "Quality profile", profiles)];
    match m.kind {
        Kind::Sonarr => {
            let types = [
                ("anime", "Anime (absolute numbers)"),
                ("standard", "Standard (season and episode)"),
                ("daily", "Daily (dates)"),
            ]
            .into_iter()
            .map(|(v, l)| {
                (
                    l.to_owned(),
                    e.series_type == v,
                    change(Box::new(move |e| e.series_type = v.into())),
                )
            })
            .collect();
            items.push(select(ui, "Series type", types));
            let new = [
                ("all", "Watch for new seasons"),
                ("none", "Don't watch for new seasons"),
            ]
            .into_iter()
            .map(|(v, l)| {
                (
                    l.to_owned(),
                    e.monitor_new == v,
                    change(Box::new(move |e| e.monitor_new = v.into())),
                )
            })
            .collect();
            items.push(select(ui, "New seasons", new));
            let ec = e.clone();
            items.push(check(
                ui,
                "Season folders",
                e.season_folder,
                move |app, on| {
                    let mut e = ec.clone();
                    e.season_folder = on;
                    app.perform(Action::Manage(ManageAction::ChangeEdit(e)));
                },
            ));
        }
        Kind::Radarr => {
            let avail = [
                ("announced", "Announced"),
                ("inCinemas", "In cinemas"),
                ("released", "Released"),
            ]
            .into_iter()
            .map(|(v, l)| {
                (
                    l.to_owned(),
                    e.availability == v,
                    change(Box::new(move |e| e.availability = v.into())),
                )
            })
            .collect();
            items.push(select(ui, "Available when", avail));
        }
    }
    let roots = about
        .folders
        .iter()
        .map(|f| {
            let path = f.path.trim_end_matches('/').to_owned();
            (
                f.to_string(),
                e.root == path,
                change(Box::new(move |e| e.root = path.clone())),
            )
        })
        .collect();
    items.push(select(ui, "Root folder", roots));
    // Its tags: the server's, picked; or a new one.
    match &m.tags {
        Some(Loaded::Ready(list)) => {
            let options = list
                .iter()
                .map(|t| {
                    let (id, on) = (t.id, e.tags.contains(&t.id));
                    (
                        t.label.clone(),
                        on,
                        change(Box::new(move |e| {
                            if on {
                                e.tags.retain(|x| *x != id);
                            } else {
                                e.tags.push(id);
                            }
                        })),
                    )
                })
                .collect::<Vec<_>>();
            items.push(text("Tags"));
            if !options.is_empty() {
                items.push(super::form::chips(ui, options));
            }
        }
        Some(Loaded::Failed(err)) => items.push(said(err.clone(), "error")),
        _ => items.push(said("Reading its tags…", "muted")),
    }
    let en = e.clone();
    items.push(field(
        ui,
        "New tag",
        "",
        &e.new_tag,
        false,
        move |app, v| {
            let mut e = en.clone();
            e.new_tag = v;
            app.perform(Action::Manage(ManageAction::ChangeEdit(e)));
        },
    ));
    let add = ui.button(
        "Add tag",
        (!e.new_tag.trim().is_empty()).then_some(Action::Manage(ManageAction::AddTag)),
    );
    items.push(buttons(vec![add], false));
    let em = e.clone();
    items.push(check(
        ui,
        "Move its files to a new root folder",
        e.move_files,
        move |app, on| {
            let mut e = em.clone();
            e.move_files = on;
            app.perform(Action::Manage(ManageAction::ChangeEdit(e)));
        },
    ));
    let mut save = ui.button(
        "Save",
        (!m.busy).then_some(Action::Manage(ManageAction::SaveEdit)),
    );
    save["kind"] = json!("primary");
    let cancel = ui.button("Cancel", manage(ManageAction::CancelEdit));
    items.push(buttons(vec![cancel, save], false));
    items
}

fn renames(ui: &mut Ui, renames: &Loaded<Vec<crate::servers::arr::Rename>>) -> Vec<Value> {
    match renames {
        Loaded::Loading => vec![said("Working out the new names…", "muted")],
        Loaded::Failed(e) => vec![said(format!("Couldn't work them out: {e}"), "error")],
        Loaded::Ready(list) if list.is_empty() => {
            let close = ui.button("Close", manage(ManageAction::CancelRenames));
            vec![
                said("Its files are named as its settings say already.", "muted"),
                buttons(vec![close], false),
            ]
        }
        Loaded::Ready(list) => {
            let mut items: Vec<Value> = list
                .iter()
                .flat_map(|r| [said(r.from.clone(), "faint"), text(format!("→ {}", r.to))])
                .collect();
            let cancel = ui.button("Cancel", manage(ManageAction::CancelRenames));
            let mut go = ui.button(
                format!(
                    "Rename {} file{}",
                    list.len(),
                    if list.len() == 1 { "" } else { "s" }
                ),
                manage(ManageAction::Rename),
            );
            go["kind"] = json!("primary");
            items.push(buttons(vec![cancel, go], false));
            items
        }
    }
}

/// What its indexers have, best first; grabbing one sends it to the
/// download client.
fn releases(ui: &mut Ui, m: &Manage) -> Vec<Value> {
    let what = match m.looking {
        Looking::Season => format!("season {}", m.season.unwrap_or(0)),
        Looking::Episode(n) => format!("episode {n}"),
        Looking::Film => "the film".into(),
    };
    let mut items = vec![said(
        format!(
            "What {}'s indexers have for {what}, best first (as it would choose). Grabbing one \
             sends it to its download client.",
            m.kind.name()
        ),
        "muted",
    )];
    if matches!(m.looking, Looking::Episode(_)) {
        let season = ui.button(
            "The whole season",
            manage(ManageAction::Look(Looking::Season)),
        );
        items.push(buttons(vec![season], false));
    }
    match &m.releases {
        None | Some(Loaded::Loading) => items.push(said(
            "Searching its indexers (this can take a while)…",
            "muted",
        )),
        Some(Loaded::Failed(e)) => items.push(said(format!("Couldn't search: {e}"), "error")),
        Some(Loaded::Ready(list)) if list.is_empty() => {
            items.push(said("Its indexers have nothing for it.", "muted"))
        }
        Some(Loaded::Ready(list)) => {
            for (i, r) in list.iter().enumerate() {
                let mut facts = vec![fmt_bytes(r.size)];
                if let Some(h) = r.age_hours {
                    facts.push(age(h));
                }
                if let Some(s) = r.seeders {
                    facts.push(format!(
                        "{s} seeders · {} leechers",
                        r.leechers.unwrap_or(0)
                    ));
                }
                if let Some(i) = &r.indexer {
                    facts.push(i.clone());
                }
                if !r.languages.is_empty() {
                    facts.push(r.languages.join(", "));
                }
                if let Some(s) = r.score.filter(|s| *s != 0) {
                    facts.push(format!("score {s:+}"));
                }
                let mut card = vec![
                    said(
                        r.quality.clone().unwrap_or_else(|| "?".into()),
                        if r.approved { "good" } else { "warn" },
                    ),
                    text(r.title.clone()),
                    said(facts.join(" · "), "muted"),
                ];
                if !r.rejections.is_empty() {
                    card.push(said(r.rejections.join(" · "), "warn"));
                }
                if m.grabbed.contains(&r.guid) {
                    card.push(said("Sent ✓", "good"));
                } else if m.grabbing.contains(&r.guid) {
                    card.push(said("Sending…", "muted"));
                } else {
                    let grab = ui.button(
                        if r.approved { "Grab" } else { "Grab anyway" },
                        manage(ManageAction::Grab(i)),
                    );
                    card.push(
                        json!({"t": "buttons", "buttons": [grab], "primary": false, "small": true}),
                    );
                }
                items.push(boxed(card));
            }
        }
    }
    items
}

fn files(ui: &mut Ui, m: &Manage, remote: Option<&Remote>) -> Vec<Value> {
    let list = match &m.files {
        None | Some(Loaded::Loading) => return vec![said("Reading its files…", "muted")],
        Some(Loaded::Failed(e)) => {
            return vec![said(format!("Couldn't read its files: {e}"), "error")];
        }
        Some(Loaded::Ready(list)) => list,
    };
    let mine: Vec<(u32, &crate::servers::arr::FileInfo)> = match remote {
        Some(r) => r
            .episodes
            .iter()
            .filter_map(|(n, e)| {
                let id = e.file_id?;
                Some((*n, list.iter().find(|f| f.id == id)?))
            })
            .collect(),
        None => Vec::new(),
    };
    if mine.is_empty() {
        return vec![said("None of its episodes have files yet.", "muted")];
    }
    let all: Vec<u64> = mine.iter().map(|(_, f)| f.id).collect();
    let picked: Vec<u64> = all
        .iter()
        .copied()
        .filter(|id| m.picked_files.contains(id))
        .collect();
    let total: u64 = mine.iter().map(|(_, f)| f.size).sum();
    let every = picked.len() == all.len();
    let all2 = all.clone();
    let mut items = vec![
        check(ui, "Select all", every, move |app, on| {
            for id in &all2 {
                app.perform(Action::Manage(ManageAction::PickFile(*id, on)));
            }
        }),
        said(
            format!(
                "{} file{} · {}",
                all.len(),
                if all.len() == 1 { "" } else { "s" },
                fmt_bytes(total)
            ),
            "muted",
        ),
    ];
    if !picked.is_empty() {
        let mut del = ui.button(
            format!("Delete {} selected…", picked.len()),
            manage(ManageAction::AskDeleteFiles(picked.clone())),
        );
        del["kind"] = json!("danger");
        items.push(buttons(vec![del], false));
    }
    for (n, f) in mine {
        let mut facts = vec![fmt_bytes(f.size)];
        facts.extend(f.quality.clone());
        facts.extend(f.group.clone());
        facts.extend(f.resolution.clone());
        facts.extend(f.video.clone());
        if let Some(a) = &f.audio {
            facts.push(match &f.audio_languages {
                Some(l) => format!("{a} ({l})"),
                None => a.clone(),
            });
        }
        if let Some(s) = &f.subtitles {
            facts.push(format!("subtitles: {s}"));
        }
        let id = f.id;
        let on = m.picked_files.contains(&id);
        let card = vec![
            check(ui, &format!("Episode {n}"), on, move |app, v| {
                app.perform(Action::Manage(ManageAction::PickFile(id, v)))
            }),
            said(
                f.path.rsplit('/').next().unwrap_or(&f.path).to_owned(),
                "muted",
            ),
            said(facts.join(" · "), "faint"),
            json!({"t": "buttons", "buttons": [{"label": "Delete…", "kind": "danger", "id": ui.act(Action::Manage(ManageAction::AskDeleteFiles(vec![id])))}], "primary": false, "small": true}),
        ];
        items.push(boxed(card));
    }
    items
}

fn history(m: &Manage, remote: Option<&Remote>) -> Vec<Value> {
    let list = match &m.history {
        None | Some(Loaded::Loading) => return vec![said("Reading its history…", "muted")],
        Some(Loaded::Failed(e)) => {
            return vec![said(format!("Couldn't read its history: {e}"), "error")];
        }
        Some(Loaded::Ready(list)) => list,
    };
    let number = |id: u64| {
        remote.and_then(|r| {
            r.episodes
                .iter()
                .find(|(_, e)| e.arr_id == Some(id))
                .map(|(n, _)| *n)
        })
    };
    let mine: Vec<_> = list
        .iter()
        .filter(|e| m.kind == Kind::Radarr || e.episode.is_none_or(|id| number(id).is_some()))
        .collect();
    if mine.is_empty() {
        return vec![said("Nothing's happened to it yet.", "muted")];
    }
    mine.into_iter()
        .take(100)
        .map(|e| {
            let tone = match e.kind.as_str() {
                "grabbed" => "accent",
                k if k.contains("Imported") => "good",
                k if k.contains("Failed") => "error",
                _ => "muted",
            };
            let mut line = vec![when(e.at)];
            if let Some(n) = e.episode.and_then(number) {
                line.push(format!("Episode {n}"));
            }
            line.extend(e.quality.clone());
            line.extend(e.detail.clone());
            boxed(vec![
                said(event_name(&e.kind).to_owned(), tone),
                text(e.title.clone()),
                said(line.join(" · "), "faint"),
            ])
        })
        .collect()
}

/// An episode's details, under its row: what Sonarr says of it, its file,
/// what happened to it, and what to do with it.
pub(super) fn episode_details(ui: &mut Ui, n: u32) -> Option<Vec<Value>> {
    let app = ui.app;
    let m = app.manage.as_ref()?;
    let (open, record) = m.episode.as_ref()?;
    if *open != n {
        return None;
    }
    let r = app.library.find(m.anilist)?.remote.as_ref()?;
    let e = r.episodes.get(&n)?;
    let mut items = Vec::new();
    match record {
        Loaded::Loading => items.push(said("Reading it from Sonarr…", "muted")),
        Loaded::Failed(err) => items.push(said(format!("Couldn't read it: {err}"), "error")),
        Loaded::Ready(v) => {
            let mut head = vec![format!(
                "S{:02}E{:02}",
                v["seasonNumber"].as_u64().unwrap_or(0),
                v["episodeNumber"].as_u64().unwrap_or(0)
            )];
            if let Some(t) = v["title"].as_str().filter(|t| !t.is_empty()) {
                head.push(t.to_owned());
            }
            if let Some(at) = v["airDateUtc"]
                .as_str()
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            {
                head.push(format!("aired {}", when(at.timestamp())));
            }
            if let Some(min) = v["runtime"].as_u64().filter(|m| *m > 0) {
                head.push(format!("{min} min"));
            }
            items.push(text(head.join(" · ")));
            if let Some(o) = v["overview"].as_str().filter(|o| !o.is_empty()) {
                items.push(said(o.to_owned(), "muted"));
            }
        }
    }
    let file = e.file_id.and_then(|id| match &m.files {
        Some(Loaded::Ready(list)) => list.iter().find(|f| f.id == id),
        _ => None,
    });
    match (e.file_id, file) {
        (Some(id), Some(f)) => {
            let mut facts = vec![fmt_bytes(f.size)];
            facts.extend(f.quality.clone());
            facts.extend(f.group.clone());
            facts.extend(f.resolution.clone());
            facts.extend(f.video.clone());
            facts.extend(f.audio.clone());
            facts.extend(f.audio_languages.as_ref().map(|l| format!("audio: {l}")));
            facts.extend(f.subtitles.as_ref().map(|s| format!("subtitles: {s}")));
            items.push(said(f.path.clone(), "plain"));
            items.push(said(facts.join(" · "), "faint"));
            let del = json!({"label": "Delete file…", "kind": "danger", "id": ui.act(Action::Manage(ManageAction::AskDeleteFiles(vec![id])))});
            items.push(json!({"t": "buttons", "buttons": [del], "primary": false, "small": true}));
        }
        (Some(_), None) => items.push(said("Reading its file…", "muted")),
        (None, _) => items.push(said("No file yet.", "muted")),
    }
    if let (Some(Loaded::Ready(list)), Some(arr_id)) = (&m.history, e.arr_id) {
        let mine: Vec<_> = list.iter().filter(|h| h.episode == Some(arr_id)).collect();
        for h in mine.iter().take(8) {
            let mut line = vec![when(h.at)];
            line.extend(h.quality.clone());
            line.extend(h.detail.clone());
            items.push(said(
                format!(
                    "{} · {} · {}",
                    event_name(&h.kind),
                    h.title,
                    line.join(" · ")
                ),
                "faint",
            ));
        }
        if mine.is_empty() {
            items.push(said("Nothing's happened to it yet.", "muted"));
        }
    }
    let on = e.monitored;
    items.push(check(ui, "Watch for it", on, move |app, v| {
        app.perform(Action::Manage(ManageAction::SetEpisodesMonitored(
            vec![n],
            v,
        )))
    }));
    let search = ui.button("⌕ Search for it", manage(ManageAction::AutoSearch(vec![n])));
    let look = ui.button("Releases…", manage(ManageAction::Look(Looking::Episode(n))));
    items.push(json!({"t": "buttons", "buttons": [search, look], "primary": false, "small": true}));
    Some(items)
}
