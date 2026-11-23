//! Dialogs, as sheets the phone app shows over the page: confirming a
//! deletion, the languages, Usenet providers, importing by hand.

use serde_json::{Value, json};

use super::form::{check, chips, field, said, select, text};
use super::ui::Ui;
use crate::app::import_ui::ImportAction;
use crate::app::usenet_ui::{Check, Dialog};
use crate::app::{Action, App, DeleteTarget, Picked};
use crate::prefs::{LANGUAGES, LangPrefs, SubType};
use crate::remux::language_name;
use crate::servers::arr::Kind;
use crate::usenet::Provider;

/// A sheet: what it's about, its items, its buttons; and closing it.
fn sheet(
    ui: &mut Ui,
    title: String,
    note: Option<String>,
    items: Vec<Value>,
    buttons: Vec<Value>,
    close: impl Fn(&mut App) + 'static,
) -> Value {
    json!({
        "title": title,
        "note": note,
        "items": items,
        "buttons": buttons,
        "close": ui.edit(close),
    })
}

fn button(ui: &mut Ui, label: impl Into<String>, kind: &str, action: Option<Action>) -> Value {
    let mut b = ui.button(label, action);
    b["kind"] = json!(kind);
    b
}

fn button_edit(
    ui: &mut Ui,
    label: impl Into<String>,
    kind: &str,
    f: Option<Box<dyn Fn(&mut App)>>,
) -> Value {
    let id = f.map(|f| ui.edit(f));
    json!({"label": label.into(), "kind": kind, "id": id})
}

/// The dialog open, if any.
pub(super) fn dialog(ui: &mut Ui) -> Value {
    let app = ui.app;
    if let Some((target, name)) = &app.confirm_delete {
        return confirm_delete(ui, target.clone(), name.clone());
    }
    if let Some(prefs) = app.pages.languages.clone() {
        return languages(ui, prefs);
    }
    if let Some(i) = &app.import {
        let _ = i;
        return import(ui);
    }
    if let Some(d) = &app.usenet.dialog {
        return usenet(ui, d);
    }
    Value::Null
}

fn confirm_delete(ui: &mut Ui, target: DeleteTarget, name: String) -> Value {
    let app = ui.app;
    let close = |app: &mut App| app.confirm_delete = None;
    let cancel = button_edit(ui, "Cancel", "secondary", Some(Box::new(close)));
    if let DeleteTarget::Queued(kind, id) = target {
        use crate::app::servers_ui::ServersAction;
        let n = kind.name();
        let remove = |blocklist| {
            Some(Action::Servers(ServersAction::RemoveQueued {
                kind,
                id,
                blocklist,
            }))
        };
        let buttons = vec![
            cancel,
            button(ui, "Remove and blocklist", "secondary", remove(true)),
            button(ui, "Remove", "danger", remove(false)),
        ];
        return sheet(
            ui,
            format!("Remove from {n}'s queue?"),
            Some(format!(
                "This stops downloading “{name}” and takes it out of {n}'s download client, with \
                 what it's downloaded so far. Blocklisting the release as well keeps {n} from \
                 grabbing it again (it looks for another)."
            )),
            Vec::new(),
            buttons,
            close,
        );
    }
    if let DeleteTarget::ArrFiles(ids) = &target {
        use crate::app::manage_ui::ManageAction;
        let buttons = vec![
            cancel,
            button(
                ui,
                "Delete",
                "danger",
                Some(Action::Manage(ManageAction::DeleteFiles(ids.clone()))),
            ),
        ];
        return sheet(
            ui,
            format!("Delete {name}?"),
            Some("They're deleted from disk on the server. It can't be undone.".into()),
            Vec::new(),
            buttons,
            close,
        );
    }
    if let DeleteTarget::ArrOwner = &target {
        use crate::app::manage_ui::ManageAction;
        let (kind, others) = match &app.manage {
            Some(m) => {
                let others: Vec<String> = app
                    .library
                    .series
                    .iter()
                    .filter(|s| s.anilist_id != m.anilist)
                    .filter(|s| {
                        s.remote.as_ref().is_some_and(|r| match r.arr {
                            crate::library::ArrRef::Sonarr { id, .. } => {
                                m.kind == Kind::Sonarr && id == m.owner
                            }
                            crate::library::ArrRef::Radarr { id, .. } => {
                                m.kind == Kind::Radarr && id == m.owner
                            }
                        })
                    })
                    .map(|s| s.title().to_owned())
                    .collect();
                (m.kind.name(), others)
            }
            None => ("Sonarr", Vec::new()),
        };
        let mut note =
            format!("This deletes “{name}” from {kind}: it stops watching for it and forgets it.");
        if !others.is_empty() {
            note += &format!(
                " It's the whole series there, so these go too: {}.",
                others.join(", ")
            );
        }
        note += " Deleting its files too can't be undone.";
        let exclude = app.manage.as_ref().is_some_and(|m| m.exclude);
        let delete = |delete_files| {
            Some(Action::Manage(ManageAction::Delete {
                delete_files,
                exclude,
            }))
        };
        let items = vec![check(
            ui,
            "Don't let import lists add it back",
            exclude,
            |app, on| app.perform(Action::Manage(ManageAction::SetExclude(on))),
        )];
        let buttons = vec![
            cancel,
            button(ui, "Delete, keep files", "secondary", delete(false)),
            button(ui, "Delete with files", "danger", delete(true)),
        ];
        return sheet(
            ui,
            format!("Delete from {kind}?"),
            Some(note),
            items,
            buttons,
            close,
        );
    }
    if let DeleteTarget::Many(items) = &target {
        let queued = items.iter().all(|p| matches!(p, Picked::Queued(..)));
        let remove = |delete_files: bool, blocklist: bool| {
            Some(Action::RemoveMany {
                items: items.clone(),
                delete_files,
                blocklist,
            })
        };
        if queued {
            let buttons = vec![
                cancel,
                button(ui, "Remove and blocklist", "secondary", remove(false, true)),
                button(ui, "Remove", "danger", remove(false, false)),
            ];
            return sheet(
                ui,
                format!("Remove {name} from the queue?"),
                Some(
                    "This stops downloading them and takes them out of the download client, with \
                     what they've downloaded so far. Blocklisting their releases as well keeps \
                     Sonarr and Radarr from grabbing them again."
                        .into(),
                ),
                Vec::new(),
                buttons,
                close,
            );
        }
        let buttons = vec![cancel, button(ui, "Delete", "danger", remove(true, false))];
        return sheet(
            ui,
            format!("Delete {name}?"),
            Some("This removes them and deletes their files from disk. It can't be undone.".into()),
            Vec::new(),
            buttons,
            close,
        );
    }
    let note = match &target {
        DeleteTarget::Series(id) => {
            let folder = app
                .library
                .find(*id)
                .map(|s| s.folder.display().to_string())
                .unwrap_or_default();
            format!(
                "This removes “{name}” from your library and deletes its folder, {folder}, with \
                 everything in it. It can't be undone."
            )
        }
        DeleteTarget::Remux(path) if path.is_dir() => format!(
            "This deletes the remuxed copies of “{name}” ({}). The episodes in your library \
             stay. It can't be undone.",
            path.display()
        ),
        DeleteTarget::Remux(_) => format!(
            "This deletes the remuxed copy “{name}”. The episode in your library stays. It can't \
             be undone."
        ),
        _ => format!("This removes “{name}” and deletes its files from disk. It can't be undone."),
    };
    let title = if matches!(target, DeleteTarget::Remux(_)) {
        "Delete remuxed copies?"
    } else {
        "Delete downloaded files?"
    };
    let confirm = match target {
        DeleteTarget::Torrent(id) => Action::Remove {
            id,
            delete_files: true,
        },
        DeleteTarget::Debrid(id) => Action::DebridRemove {
            id,
            delete_files: true,
        },
        DeleteTarget::Usenet(id) => Action::UsenetRemove {
            id,
            delete_files: true,
        },
        DeleteTarget::Series(id) => Action::RemoveSeries {
            id,
            delete_files: true,
        },
        DeleteTarget::Remux(path) => Action::DeleteRemux(path),
        DeleteTarget::Queued(..)
        | DeleteTarget::Many(_)
        | DeleteTarget::ArrFiles(_)
        | DeleteTarget::ArrOwner => {
            unreachable!("asked above")
        }
    };
    let buttons = vec![cancel, button(ui, "Delete", "danger", Some(confirm))];
    sheet(ui, title.into(), Some(note), Vec::new(), buttons, close)
}

/// The languages: audio, subtitles (or none) and their kind; saved for
/// every series without its own.
fn languages(ui: &mut Ui, prefs: LangPrefs) -> Value {
    let set = |p: LangPrefs| -> Box<dyn Fn(&mut App)> {
        Box::new(move |app: &mut App| app.pages.languages = Some(p.clone()))
    };
    let audio = LANGUAGES
        .iter()
        .map(|c| {
            (
                language_name(c),
                prefs.audio == *c,
                set(LangPrefs {
                    audio: (*c).into(),
                    ..prefs.clone()
                }),
            )
        })
        .collect();
    let mut subs = vec![(
        "None".to_owned(),
        prefs.subtitles.is_none(),
        set(LangPrefs {
            subtitles: None,
            ..prefs.clone()
        }),
    )];
    subs.extend(LANGUAGES.iter().map(|c| {
        (
            language_name(c),
            prefs.subtitles.as_deref() == Some(*c),
            set(LangPrefs {
                subtitles: Some((*c).into()),
                ..prefs.clone()
            }),
        )
    }));
    let mut items = vec![select(ui, "Audio", audio), select(ui, "Subtitles", subs)];
    if prefs.subtitles.is_some() {
        let kinds = SubType::ALL
            .into_iter()
            .map(|k| {
                (
                    k.name().to_owned(),
                    prefs.sub_type == k,
                    set(LangPrefs {
                        sub_type: k,
                        ..prefs.clone()
                    }),
                )
            })
            .collect();
        items.push(select(ui, "Kind", kinds));
        items.push(said(prefs.sub_type.help(), "faint"));
    }
    let close = |app: &mut App| app.pages.languages = None;
    let buttons = vec![
        button_edit(ui, "Cancel", "secondary", Some(Box::new(close))),
        button_edit(
            ui,
            "Save",
            "primary",
            Some(Box::new(|app: &mut App| {
                if let Some(p) = app.pages.languages.take() {
                    app.settings.prefs = p;
                    app.recheck_defaults();
                    app.pages.last_stats = None;
                }
            })),
        ),
    ];
    sheet(
        ui,
        "Languages".into(),
        Some("Which audio and subtitles play by default, for every series without its own. Every track stays in the files.".into()),
        items,
        buttons,
        close,
    )
}

/// Change the provider being edited.
fn on_form(f: impl Fn(&mut Provider) + 'static) -> impl Fn(&mut App) + 'static {
    move |app| {
        if let Some(Dialog {
            editing: Some((p, _)),
            ..
        }) = &mut app.usenet.dialog
        {
            f(p);
        }
    }
}

/// Change the provider list (and save it).
fn on_providers(f: impl Fn(&mut Vec<Provider>) + 'static) -> Box<dyn Fn(&mut App)> {
    Box::new(move |app| {
        f(&mut app.usenet.config.providers);
        app.usenet.save();
    })
}

/// Usenet providers: add, edit, check, order.
fn usenet(ui: &mut Ui, d: &Dialog) -> Value {
    let close = |app: &mut App| app.usenet.dialog = None;
    let note = Some(
        "Articles come from the main providers, in this order. Backup (block) accounts are only \
         asked for articles the main ones don't have."
            .to_owned(),
    );
    if let Some((p, new)) = &d.editing {
        let digits = |s: &str| -> u16 {
            s.chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        };
        let number = |n: u16| if n == 0 { String::new() } else { n.to_string() };
        let tls = p.tls;
        let items = vec![
            text(if *new {
                "Add a provider"
            } else {
                "Edit provider"
            }),
            field(ui, "Name", "e.g. Eweka", &p.name, false, |app, v| {
                on_form(move |p| p.name = v.clone())(app)
            }),
            field(
                ui,
                "Server",
                "news.example.com",
                &p.host,
                false,
                |app, v| on_form(move |p| p.host = v.clone())(app),
            ),
            field(ui, "Port", "563", &number(p.port), false, move |app, v| {
                let port = digits(&v);
                on_form(move |p| p.port = port)(app)
            }),
            check(ui, "Encrypted (TLS)", tls, |app, on| {
                on_form(move |p| {
                    p.tls = on;
                    p.port = match (on, p.port) {
                        (true, 119) => 563,
                        (false, 563) => 119,
                        (_, port) => port,
                    };
                })(app)
            }),
            field(ui, "Username", "", &p.username, false, |app, v| {
                on_form(move |p| p.username = v.clone())(app)
            }),
            field(ui, "Password", "", &p.password, true, |app, v| {
                on_form(move |p| p.password = v.clone())(app)
            }),
            field(
                ui,
                "Connections",
                "20",
                &number(p.connections),
                false,
                move |app, v| {
                    let n = digits(&v).min(100);
                    on_form(move |p| p.connections = n)(app)
                },
            ),
            said(
                "How many your plan allows (often 20–60). More is faster.",
                "faint",
            ),
            check(
                ui,
                "Backup (block) account: only for articles the others lack",
                p.backup,
                |app, on| on_form(move |p| p.backup = on)(app),
            ),
        ];
        let save_ok = !p.host.trim().is_empty() && p.port > 0 && p.connections > 0;
        let buttons = vec![
            button_edit(
                ui,
                "Cancel",
                "secondary",
                Some(Box::new(|app: &mut App| {
                    if let Some(d) = &mut app.usenet.dialog {
                        d.editing = None;
                    }
                })),
            ),
            button_edit(
                ui,
                "Save",
                "primary",
                save_ok.then(|| -> Box<dyn Fn(&mut App)> {
                    Box::new(|app: &mut App| {
                        let Some(d) = &mut app.usenet.dialog else {
                            return;
                        };
                        let Some((mut p, new)) = d.editing.take() else {
                            return;
                        };
                        p.host = p.host.trim().to_owned();
                        let config = &mut app.usenet.config;
                        if new {
                            config.add(p);
                        } else if let Some(slot) =
                            config.providers.iter_mut().find(|x| x.id == p.id)
                        {
                            d.checks.remove(&p.id);
                            *slot = p;
                        }
                        app.usenet.save();
                    })
                }),
            ),
        ];
        return sheet(ui, "Usenet providers".into(), note, items, buttons, close);
    }
    let providers = ui.app.usenet.config.providers.clone();
    let count = providers.len();
    let mut items = Vec::new();
    if providers.is_empty() {
        items.push(said("No providers yet.", "muted"));
    }
    for (i, p) in providers.iter().enumerate() {
        let id = p.id;
        let security = if p.tls { "TLS" } else { "no encryption" };
        let role = if p.backup { " · backup" } else { "" };
        let mut boxed = vec![
            check(ui, p.label(), p.enabled, move |app, on| {
                on_providers(move |ps| {
                    if let Some(x) = ps.iter_mut().find(|x| x.id == id) {
                        x.enabled = on;
                    }
                })(app)
            }),
            said(
                format!(
                    "{}:{} · {security} · {} connections{role}",
                    p.host, p.port, p.connections
                ),
                "muted",
            ),
        ];
        match d.checks.get(&id) {
            Some(Check::Running(_)) => boxed.push(said("Connecting…", "muted")),
            Some(Check::Done(Ok(()))) => boxed.push(said("✓ Connected and logged in", "good")),
            Some(Check::Done(Err(e))) => boxed.push(said(e.clone(), "error")),
            None => {}
        }
        let editing = p.clone();
        let mut row = Vec::new();
        if !matches!(d.checks.get(&id), Some(Check::Running(_))) {
            row.push(ui.button("Check", Some(Action::UsenetCheck(id))));
        }
        row.push(button_edit(
            ui,
            "Up",
            "secondary",
            (i > 0).then(|| on_providers(move |ps| ps.swap(i, i - 1))),
        ));
        row.push(button_edit(
            ui,
            "Down",
            "secondary",
            (i + 1 < count).then(|| on_providers(move |ps| ps.swap(i, i + 1))),
        ));
        row.push(button_edit(
            ui,
            "Edit",
            "secondary",
            Some(Box::new(move |app: &mut App| {
                if let Some(d) = &mut app.usenet.dialog {
                    d.editing = Some((editing.clone(), false));
                }
            })),
        ));
        row.push(button_edit(
            ui,
            "Remove",
            "danger",
            Some(on_providers(move |ps| ps.retain(|x| x.id != id))),
        ));
        boxed.push(json!({"t": "buttons", "buttons": row, "primary": false, "small": true}));
        items.push(json!({"t": "box", "items": boxed}));
    }
    let buttons = vec![
        button_edit(
            ui,
            "Add provider",
            "secondary",
            Some(Box::new(|app: &mut App| {
                if let Some(d) = &mut app.usenet.dialog {
                    d.editing = Some((Provider::default(), true));
                }
            })),
        ),
        button_edit(ui, "Done", "primary", Some(Box::new(close))),
    ];
    sheet(ui, "Usenet providers".into(), note, items, buttons, close)
}

/// Importing files into Sonarr or Radarr by hand.
fn import(ui: &mut Ui) -> Value {
    use crate::app::discover_ui::Loaded;
    let app = ui.app;
    let Some(i) = &app.import else {
        return Value::Null;
    };
    let imp = |a: ImportAction| Some(Action::Import(a));
    let name = i.kind.name();
    let note = match &i.download {
        Some(_) => format!(
            "The download's files, and what {name} takes each for. Pick what to import (and, \
             where it couldn't tell, which episode it is)."
        ),
        None => format!(
            "Files in a folder on the server (as {name} sees it, like /downloads/complete/…), \
             and what {name} takes each for."
        ),
    };
    let mut items = Vec::new();
    if i.download.is_none() {
        items.push(field(
            ui,
            "Folder on the server",
            "/downloads/complete/…",
            &i.folder,
            false,
            |app, v| app.perform(Action::Import(ImportAction::Folder(v))),
        ));
        let look = ui.button(
            "Look",
            (!i.folder.trim().is_empty()).then_some(Action::Import(ImportAction::Scan)),
        );
        items.push(json!({"t": "buttons", "buttons": [look], "primary": false}));
    }
    let choices = app.import_choices();
    match &i.candidates {
        None => {}
        Some(Loaded::Loading) => items.push(said(format!("Asking {name} what's there…"), "muted")),
        Some(Loaded::Failed(e)) => items.push(said(format!("Couldn't look: {e}"), "error")),
        Some(Loaded::Ready(list)) if list.is_empty() => {
            items.push(said("Nothing there to import.", "muted"))
        }
        Some(Loaded::Ready(list)) => {
            for (n, cand) in list.iter().enumerate() {
                let on = i.picked.contains(&n);
                let mut facts = vec![crate::app::fmt_bytes(cand.size)];
                facts.extend(cand.quality.clone());
                facts.extend(cand.group.clone());
                let mut boxed = vec![
                    check(ui, &cand.name, on, move |app, v| {
                        app.perform(Action::Import(ImportAction::Pick(n, v)))
                    }),
                    said(facts.join(" · "), "faint"),
                ];
                if !cand.rejections.is_empty() {
                    boxed.push(said(cand.rejections.join(" · "), "warn"));
                }
                if i.kind == Kind::Radarr {
                    boxed.push(said("The film", "muted"));
                } else if !cand.labels.is_empty() && !i.assigned.contains_key(&n) {
                    boxed.push(text(cand.labels.join(", ")));
                } else if choices.is_empty() {
                    boxed.push(said("Which episode isn't known", "warn"));
                } else {
                    let mut options: Vec<(String, bool, Box<dyn Fn(&mut App)>)> = vec![(
                        "Which episode?".into(),
                        !i.assigned.contains_key(&n),
                        Box::new(|_: &mut App| {}),
                    )];
                    for (id, label) in &choices {
                        let id = *id;
                        options.push((
                            label.clone(),
                            i.assigned.get(&n) == Some(&id),
                            Box::new(move |app: &mut App| {
                                app.perform(Action::Import(ImportAction::Assign(n, id)))
                            }),
                        ));
                    }
                    boxed.push(select(ui, "Episode", options));
                }
                items.push(json!({"t": "box", "items": boxed}));
            }
            items.push(chips(
                ui,
                vec![
                    (
                        "Move".into(),
                        !i.copy,
                        Box::new(|app: &mut App| {
                            app.perform(Action::Import(ImportAction::Copy(false)))
                        }),
                    ),
                    (
                        "Copy".into(),
                        i.copy,
                        Box::new(|app: &mut App| {
                            app.perform(Action::Import(ImportAction::Copy(true)))
                        }),
                    ),
                ],
            ));
            items.push(said(
                if i.copy {
                    "The files stay where they are too (a copy, or a hard link)."
                } else {
                    "The files move into the series' folder."
                },
                "faint",
            ));
        }
    }
    let n = i.picked.len();
    let busy = i.busy;
    let buttons = vec![
        button(ui, "Cancel", "secondary", imp(ImportAction::Close)),
        button(
            ui,
            format!("Import {n} file{}", if n == 1 { "" } else { "s" }),
            "primary",
            (n > 0 && !busy).then(|| Action::Import(ImportAction::Import)),
        ),
    ];
    sheet(
        ui,
        "Import by hand".into(),
        Some(note),
        items,
        buttons,
        |app| app.perform(Action::Import(ImportAction::Close)),
    )
}
