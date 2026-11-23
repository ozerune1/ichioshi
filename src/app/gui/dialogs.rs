//! Dialogs over the page: confirming deletes, the library's languages,
//! Usenet providers, and choosing a Google Drive folder.

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

use super::library::rule;
use super::series::lang_editor;
use super::{Gui, Message, act, edit};
use crate::app::usenet_ui::{Check, Dialog};
use crate::app::{Action, DeleteTarget};
use crate::gdrive::Place;
use crate::usenet::Provider;

use super::style::*;

/// Change the provider being edited.
fn on_form(f: impl Fn(&mut Provider) + Send + Sync + 'static) -> Message {
    edit(move |g| {
        if let Some(Dialog {
            editing: Some((p, _)),
            ..
        }) = &mut g.app.usenet.dialog
        {
            f(p);
        }
    })
}

/// Change the provider list (and save it).
fn on_providers(f: impl Fn(&mut Vec<Provider>) + Send + Sync + 'static) -> Message {
    edit(move |g| {
        f(&mut g.app.usenet.config.providers);
        g.app.usenet.save();
    })
}

fn buttons<'a>(items: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut r = Row::new().spacing(10).push(space::horizontal());
    for i in items {
        r = r.push(i);
    }
    r.into()
}

impl Gui {
    /// The open dialog, if any, over `base`.
    pub(super) fn dialogs<'a>(&'a self, base: Element<'a, Message>) -> Element<'a, Message> {
        let a = &self.app;
        if let Some((target, name)) = &a.confirm_delete {
            return self.confirm_delete(base, target.clone(), name);
        }
        if let Some(d) = &a.discussion {
            return self.discussion(base, d);
        }
        if let Some(i) = &a.import {
            return self.import_dialog(base, i);
        }
        if let Some(prefs) = &a.pages.languages {
            let content = column![
                dialog_title(
                    "Languages".into(),
                    Some(
                        "Which audio and subtitles play by default, for every series without \
                         its own. Every track stays in the files."
                            .into()
                    )
                ),
                gap(16.0),
                lang_editor(prefs, |p| edit(
                    move |g| g.app.pages.languages = Some(p.clone())
                )),
                gap(20.0),
                buttons(vec![
                    secondary("Cancel", Some(edit(|g| g.app.pages.languages = None))),
                    primary(
                        "Save",
                        Some(edit(|g| {
                            if let Some(p) = g.app.pages.languages.take() {
                                g.app.settings.prefs = p;
                                g.app.recheck_defaults();
                                g.app.pages.last_stats = None;
                            }
                        }))
                    ),
                ]),
            ];
            return dialog(
                base,
                content.into(),
                520.0,
                edit(|g| g.app.pages.languages = None),
            );
        }
        if let Some(d) = &a.usenet.dialog {
            return dialog(
                base,
                self.usenet_dialog(d),
                640.0,
                edit(|g| g.app.usenet.dialog = None),
            );
        }
        if a.drive.browser.is_some() {
            return dialog(
                base,
                self.drive_browser(),
                580.0,
                act(Action::DriveCloseBrowser),
            );
        }
        base
    }

    fn confirm_delete<'a>(
        &'a self,
        base: Element<'a, Message>,
        target: DeleteTarget,
        name: &str,
    ) -> Element<'a, Message> {
        let cancel = edit(|g| g.app.confirm_delete = None);
        // Sonarr's or Radarr's queue: removing it, maybe for good.
        if let DeleteTarget::Queued(kind, id) = target {
            use crate::app::servers_ui::ServersAction;
            let n = kind.name();
            let remove = |blocklist| {
                act(Action::Servers(ServersAction::RemoveQueued {
                    kind,
                    id,
                    blocklist,
                }))
            };
            let content = column![
                dialog_title(
                    format!("Remove from {n}'s queue?"),
                    Some(format!(
                        "This stops downloading “{name}” and takes it out of {n}'s download \
                         client, with what it's downloaded so far. Blocklisting the release \
                         as well keeps {n} from grabbing it again (it looks for another)."
                    ))
                ),
                gap(20.0),
                buttons(vec![
                    secondary("Cancel", Some(cancel.clone())),
                    secondary("Remove and blocklist", Some(remove(true))),
                    danger("Remove", Some(remove(false))),
                ]),
            ];
            return dialog(base, content.into(), 520.0, cancel);
        }
        // Sonarr's (or Radarr's) files, or the series itself.
        if let DeleteTarget::ArrFiles(ids) = &target {
            use crate::app::manage_ui::ManageAction;
            let content = column![
                dialog_title(
                    format!("Delete {name}?"),
                    Some("They're deleted from disk on the server. It can't be undone.".into())
                ),
                gap(20.0),
                buttons(vec![
                    secondary("Cancel", Some(cancel.clone())),
                    danger(
                        "Delete",
                        Some(act(Action::Manage(ManageAction::DeleteFiles(ids.clone()))))
                    ),
                ]),
            ];
            return dialog(base, content.into(), 480.0, cancel);
        }
        if let DeleteTarget::ArrOwner = &target {
            use crate::app::manage_ui::ManageAction;
            let (kind, others) = match &self.app.manage {
                Some(m) => {
                    // Other anime that are seasons of the same series.
                    let others: Vec<&str> = self
                        .app
                        .library
                        .series
                        .iter()
                        .filter(|s| s.anilist_id != m.anilist)
                        .filter(|s| {
                            s.remote.as_ref().is_some_and(|r| match r.arr {
                                crate::library::ArrRef::Sonarr { id, .. } => {
                                    m.kind == crate::servers::arr::Kind::Sonarr && id == m.owner
                                }
                                crate::library::ArrRef::Radarr { id, .. } => {
                                    m.kind == crate::servers::arr::Kind::Radarr && id == m.owner
                                }
                            })
                        })
                        .map(|s| s.title())
                        .collect();
                    (m.kind.name(), others)
                }
                None => ("Sonarr", Vec::new()),
            };
            let mut note = format!(
                "This deletes “{name}” from {kind}: it stops watching for it and forgets it."
            );
            if !others.is_empty() {
                note += &format!(
                    " It's the whole series there, so these go too: {}.",
                    others.join(", ")
                );
            }
            note += " Deleting its files too can't be undone.";
            let exclude = self.app.manage.as_ref().is_some_and(|m| m.exclude);
            let delete = |delete_files| {
                act(Action::Manage(ManageAction::Delete {
                    delete_files,
                    exclude,
                }))
            };
            let content = column![
                dialog_title(format!("Delete from {kind}?"), Some(note)),
                gap(14.0),
                check("Don't let import lists add it back", exclude, |on| act(
                    Action::Manage(ManageAction::SetExclude(on))
                )),
                gap(20.0),
                buttons(vec![
                    secondary("Cancel", Some(cancel.clone())),
                    secondary("Delete, keep files", Some(delete(false))),
                    danger("Delete with files", Some(delete(true))),
                ]),
            ];
            return dialog(base, content.into(), 560.0, cancel);
        }
        // Several at once: queue items (remove, maybe blocklisting) or
        // downloads here (deleting their files).
        if let DeleteTarget::Many(items) = &target {
            let queued = items
                .iter()
                .all(|p| matches!(p, crate::app::Picked::Queued(..)));
            let remove = |delete_files: bool, blocklist: bool| {
                act(Action::RemoveMany {
                    items: items.clone(),
                    delete_files,
                    blocklist,
                })
            };
            let (title, note, buttons_row) = if queued {
                (
                    format!("Remove {name} from the queue?"),
                    "This stops downloading them and takes them out of the download client, \
                     with what they've downloaded so far. Blocklisting their releases as well \
                     keeps Sonarr and Radarr from grabbing them again."
                        .to_owned(),
                    vec![
                        secondary("Cancel", Some(cancel.clone())),
                        secondary("Remove and blocklist", Some(remove(false, true))),
                        danger("Remove", Some(remove(false, false))),
                    ],
                )
            } else {
                (
                    format!("Delete {name}?"),
                    "This removes them and deletes their files from disk. It can't be undone."
                        .to_owned(),
                    vec![
                        secondary("Cancel", Some(cancel.clone())),
                        danger("Delete", Some(remove(true, false))),
                    ],
                )
            };
            let content = column![
                dialog_title(title, Some(note)),
                gap(20.0),
                buttons(buttons_row),
            ];
            return dialog(base, content.into(), 520.0, cancel);
        }
        let note = match target {
            DeleteTarget::Series(id) => {
                let folder = self
                    .app
                    .library
                    .find(id)
                    .map(|s| s.folder.display().to_string())
                    .unwrap_or_default();
                format!(
                    "This removes “{name}” from your library and deletes its folder, {folder}, \
                     with everything in it. It can't be undone."
                )
            }
            DeleteTarget::Remux(ref path) if path.is_dir() => format!(
                "This deletes the remuxed copies of “{name}” ({}). The episodes in your \
                 library stay. It can't be undone.",
                path.display()
            ),
            DeleteTarget::Remux(_) => format!(
                "This deletes the remuxed copy “{name}”. The episode in your library stays. It \
                 can't be undone."
            ),
            _ => format!(
                "This removes “{name}” and deletes its files from disk. It can't be undone."
            ),
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
            | DeleteTarget::ArrOwner => unreachable!("asked above"),
        };
        let content = column![
            dialog_title(title.into(), Some(note)),
            gap(20.0),
            buttons(vec![
                secondary("Cancel", Some(cancel.clone())),
                danger("Delete", Some(act(confirm))),
            ]),
        ];
        dialog(base, content.into(), 480.0, cancel)
    }

    /// Importing files into Sonarr or Radarr by hand.
    fn import_dialog<'a>(
        &'a self,
        base: Element<'a, Message>,
        i: &'a crate::app::import_ui::Importing,
    ) -> Element<'a, Message> {
        use crate::app::discover_ui::Loaded;
        use crate::app::import_ui::ImportAction;
        let imp = |a: ImportAction| act(Action::Import(a));
        let name = i.kind.name();
        let close = imp(ImportAction::Close);
        let mut c = column![dialog_title(
            "Import by hand".into(),
            Some(match &i.download {
                Some(_) => format!(
                    "The download's files, and what {name} takes each for. Pick what to \
                     import (and, where it couldn't tell, which episode it is)."
                ),
                None => format!(
                    "Files in a folder on the server (as {name} sees it, like \
                     /downloads/complete/…), and what {name} takes each for."
                ),
            })
        )]
        .spacing(14);
        if i.download.is_none() {
            c = c.push(
                row![
                    field("Folder on the server", &i.folder)
                        .id("import-folder")
                        .on_input(move |v| imp(ImportAction::Folder(v)))
                        .on_submit(imp(ImportAction::Scan))
                        .width(Fill),
                    secondary(
                        "Look",
                        (!i.folder.trim().is_empty()).then(|| imp(ImportAction::Scan))
                    ),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
        let choices = self.app.import_choices();
        match &i.candidates {
            None => {}
            Some(Loaded::Loading) => c = c.push(weak(format!("Asking {name} what's there…"))),
            Some(Loaded::Failed(e)) => c = c.push(error_text(format!("Couldn't look: {e}"))),
            Some(Loaded::Ready(list)) if list.is_empty() => {
                c = c.push(weak("Nothing there to import."));
            }
            Some(Loaded::Ready(list)) => {
                let mut rows = Column::new().spacing(8);
                for (n, cand) in list.iter().enumerate() {
                    let on = i.picked.contains(&n);
                    let mut facts = vec![crate::app::fmt_bytes(cand.size)];
                    facts.extend(cand.quality.clone());
                    facts.extend(cand.group.clone());
                    let which: Element<'_, Message> = if i.kind == crate::servers::arr::Kind::Radarr
                    {
                        text("The film").size(12).color(MUTED).into()
                    } else if !cand.labels.is_empty() && !i.assigned.contains_key(&n) {
                        text(cand.labels.join(", "))
                            .size(12)
                            .font(SEMI)
                            .color(TEXT)
                            .into()
                    } else if choices.is_empty() {
                        text("Which episode isn't known")
                            .size(12)
                            .color(GOLD)
                            .into()
                    } else {
                        #[derive(Clone, PartialEq)]
                        struct Choice(u64, String);
                        impl std::fmt::Display for Choice {
                            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                                f.write_str(&self.1)
                            }
                        }
                        let options: Vec<Choice> = choices
                            .iter()
                            .map(|(id, l)| Choice(*id, l.clone()))
                            .collect();
                        let picked = i
                            .assigned
                            .get(&n)
                            .and_then(|id| options.iter().find(|o| o.0 == *id).cloned());
                        dropdown(options, picked, move |ch: Choice| {
                            imp(ImportAction::Assign(n, ch.0))
                        })
                    };
                    let mut info = column![
                        text(clip(&cand.name, 90)).size(13).color(TEXT),
                        text(facts.join(" · ")).size(12).color(FAINT),
                    ]
                    .spacing(2)
                    .width(Fill);
                    if !cand.rejections.is_empty() {
                        info = info.push(text(cand.rejections.join(" · ")).size(12).color(GOLD));
                    }
                    rows = rows.push(
                        row![
                            check("", on, move |v| imp(ImportAction::Pick(n, v))),
                            info,
                            which
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    );
                }
                c = c.push(iced::widget::scrollable(rows).height(iced::Length::Shrink));
                c = c.push(
                    row![
                        chip("Move", !i.copy, Some(imp(ImportAction::Copy(false)))),
                        chip("Copy", i.copy, Some(imp(ImportAction::Copy(true)))),
                        weak(if i.copy {
                            "The files stay where they are too (a copy, or a hard link)."
                        } else {
                            "The files move into the series' folder."
                        }),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
        }
        let n = i.picked.len();
        c = c.push(buttons(vec![
            secondary("Cancel", Some(close.clone())),
            primary(
                format!("Import {n} file{}", if n == 1 { "" } else { "s" }),
                (n > 0 && !i.busy).then(|| imp(ImportAction::Import)),
            ),
        ]));
        dialog(base, c.into(), 760.0, close)
    }

    /// Usenet providers: add, edit, check, order.
    fn usenet_dialog<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        let title = dialog_title(
            "Usenet providers".into(),
            Some(
                "Articles come from the main providers, in this order. Backup (block) accounts \
                 are only asked for articles the main ones don't have."
                    .into(),
            ),
        );
        if let Some((p, new)) = &d.editing {
            return column![title, gap(16.0), provider_form(p, *new)].into();
        }
        let providers = &self.app.usenet.config.providers;
        let count = providers.len();
        let mut list = Column::new().spacing(10);
        if providers.is_empty() {
            list = list.push(weak("No providers yet."));
        }
        for (i, p) in providers.iter().enumerate() {
            let id = p.id;
            let security = if p.tls { "TLS" } else { "no encryption" };
            let role = if p.backup { " · backup" } else { "" };
            let check_state: Element<'_, Message> = match d.checks.get(&id) {
                Some(Check::Running(_)) => weak("Connecting…").into(),
                Some(Check::Done(Ok(()))) => text("✓ Connected and logged in")
                    .size(13)
                    .color(GREEN)
                    .into(),
                Some(Check::Done(Err(e))) => error_text(e.clone()).into(),
                None => small("Check", Some(act(Action::UsenetCheck(id)))),
            };
            let editing = p.clone();
            list = list.push(
                container(
                    column![
                        row![
                            check(p.label().to_owned(), p.enabled, move |on| on_providers(
                                move |ps| {
                                    if let Some(x) = ps.iter_mut().find(|x| x.id == id) {
                                        x.enabled = on;
                                    }
                                }
                            )),
                            weak(format!(
                                "{}:{} · {security} · {} connections{role}",
                                p.host, p.port, p.connections
                            )),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                        row![
                            check_state,
                            space::horizontal(),
                            small(
                                "Up",
                                (i > 0).then(|| on_providers(move |ps| ps.swap(i, i - 1)))
                            ),
                            small(
                                "Down",
                                (i + 1 < count).then(|| on_providers(move |ps| ps.swap(i, i + 1)))
                            ),
                            small(
                                "Edit",
                                Some(edit(move |g| {
                                    if let Some(d) = &mut g.app.usenet.dialog {
                                        d.editing = Some((editing.clone(), false));
                                    }
                                }))
                            ),
                            small(
                                "Remove",
                                Some(on_providers(move |ps| ps.retain(|x| x.id != id)))
                            ),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    ]
                    .spacing(10),
                )
                .padding(14)
                .width(Fill)
                .style(|_| card_style(SURFACE2.scale_alpha(0.5))),
            );
        }
        column![
            title,
            gap(16.0),
            list,
            gap(16.0),
            row![
                secondary(
                    "Add provider",
                    Some(edit(|g| {
                        if let Some(d) = &mut g.app.usenet.dialog {
                            d.editing = Some((Provider::default(), true));
                        }
                    }))
                ),
                space::horizontal(),
                primary("Done", Some(edit(|g| g.app.usenet.dialog = None))),
            ],
        ]
        .into()
    }

    /// Choosing the Google Drive folder uploads go to.
    fn drive_browser(&self) -> Element<'_, Message> {
        let Some(b) = &self.app.drive.browser else {
            return space().into();
        };
        let mut trail = Row::new().spacing(6).align_y(Alignment::Center);
        let last = b.trail.len().saturating_sub(1);
        for (depth, (name, _)) in b.trail.iter().enumerate() {
            if depth == last {
                trail = trail.push(text(name.clone()).size(14).font(SEMI).color(TEXT));
            } else {
                trail = trail
                    .push(link(
                        name.clone(),
                        Some(act(Action::DriveBrowseBack(depth))),
                    ))
                    .push(text("›").size(14).color(FAINT));
            }
        }
        let mut entries = Column::new().spacing(2);
        match &b.entries {
            None => entries = entries.push(weak("Loading…")),
            Some(Err(e)) => entries = entries.push(error_text(e.clone())),
            Some(Ok(list)) if list.is_empty() => {
                entries = entries.push(weak("No folders in here."))
            }
            Some(Ok(list)) => {
                for entry in list {
                    entries = entries.push(
                        iced::widget::button(
                            row![
                                text("▸").size(13).color(ACCENT),
                                text(entry.name.clone()).size(14).color(TEXT)
                            ]
                            .spacing(10),
                        )
                        .width(Fill)
                        .padding([8, 10])
                        .on_press(act(Action::DriveBrowseInto(entry.clone())))
                        .style(|_, status| iced::widget::button::Style {
                            background: Some(fill_color(
                                if matches!(status, iced::widget::button::Status::Hovered) {
                                    SURFACE2
                                } else {
                                    iced::Color::TRANSPARENT
                                },
                            )),
                            text_color: TEXT,
                            border: rounded(8.0),
                            ..Default::default()
                        }),
                    );
                }
            }
        }
        let here = matches!(b.trail.last(), Some((_, Place::Folder(_))));
        let use_label = match b.trail.last() {
            Some((name, Place::Folder(_))) => format!("Upload to “{name}”"),
            _ => "Open a folder to upload to it".to_owned(),
        };
        let mut c = column![
            dialog_title("Choose the upload folder".into(), None),
            gap(10.0),
            trail,
            gap(8.0),
            rule(),
            container(iced::widget::scrollable(entries).style(scroll_style)).height(300),
            rule(),
            gap(10.0),
            buttons(vec![
                secondary("Cancel", Some(act(Action::DriveCloseBrowser))),
                primary(use_label, here.then(|| act(Action::DriveUseFolder))),
            ]),
            gap(16.0),
            weak("Or paste a folder's link (e.g. one shared with you):"),
            row![
                field("https://drive.google.com/drive/folders/…", &b.link)
                    .on_input(|l| edit(move |g| {
                        if let Some(b) = &mut g.app.drive.browser {
                            b.link = l.clone();
                        }
                    }))
                    .on_submit(act(Action::DriveUseLink))
                    .width(Fill),
                secondary(
                    if b.checking_link {
                        "Checking…"
                    } else {
                        "Use link"
                    },
                    (!b.checking_link && !b.link.trim().is_empty())
                        .then(|| act(Action::DriveUseLink))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(0);
        if let Some(e) = &b.link_error {
            c = c.push(gap(6.0)).push(error_text(e.clone()));
        }
        c.into()
    }
}

/// A Usenet provider's server, account and connections.
fn provider_form<'a>(p: &Provider, new: bool) -> Element<'a, Message> {
    let line = |name: &'static str, input: Element<'a, Message>| {
        row![text(name).size(13).color(MUTED).width(110), input]
            .spacing(10)
            .align_y(Alignment::Center)
    };
    let number = |n: u16| if n == 0 { String::new() } else { n.to_string() };
    let digits = |s: &str| -> u16 {
        s.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    };
    let tls = p.tls;
    let save_ok = !p.host.trim().is_empty() && p.port > 0 && p.connections > 0;
    column![
        text(if new {
            "Add a provider"
        } else {
            "Edit provider"
        })
        .size(16)
        .font(SEMI)
        .color(TEXT),
        line(
            "Name",
            field("e.g. Eweka", &p.name)
                .on_input(|v| on_form(move |p| p.name = v.clone()))
                .into()
        ),
        line(
            "Server",
            field("news.example.com", &p.host)
                .on_input(|v| on_form(move |p| p.host = v.clone()))
                .into()
        ),
        line(
            "Port",
            row![
                field("563", &number(p.port))
                    .on_input(move |v| {
                        let port = digits(&v);
                        on_form(move |p| p.port = port)
                    })
                    .width(100),
                check("Encrypted (TLS)", tls, |on| on_form(move |p| {
                    p.tls = on;
                    p.port = match (on, p.port) {
                        (true, 119) => 563,
                        (false, 563) => 119,
                        (_, port) => port,
                    };
                })),
            ]
            .spacing(14)
            .align_y(Alignment::Center)
            .into()
        ),
        line(
            "Username",
            field("", &p.username)
                .on_input(|v| on_form(move |p| p.username = v.clone()))
                .into()
        ),
        line(
            "Password",
            field("", &p.password)
                .secure(true)
                .on_input(|v| on_form(move |p| p.password = v.clone()))
                .into()
        ),
        line(
            "Connections",
            row![
                field("20", &number(p.connections))
                    .on_input(move |v| {
                        let n = digits(&v).min(100);
                        on_form(move |p| p.connections = n)
                    })
                    .width(100),
                weak("How many your plan allows (often 20–60). More is faster."),
            ]
            .spacing(14)
            .align_y(Alignment::Center)
            .into()
        ),
        line(
            "",
            check(
                "Backup (block) account: only for articles the others lack",
                p.backup,
                |on| on_form(move |p| p.backup = on)
            )
        ),
        gap(8.0),
        buttons(vec![
            secondary(
                "Cancel",
                Some(edit(|g| {
                    if let Some(d) = &mut g.app.usenet.dialog {
                        d.editing = None;
                    }
                }))
            ),
            primary(
                "Save",
                save_ok.then(|| edit(|g| {
                    let Some(d) = &mut g.app.usenet.dialog else {
                        return;
                    };
                    let Some((mut p, new)) = d.editing.take() else {
                        return;
                    };
                    p.host = p.host.trim().to_owned();
                    let config = &mut g.app.usenet.config;
                    if new {
                        config.add(p);
                    } else if let Some(slot) = config.providers.iter_mut().find(|x| x.id == p.id) {
                        d.checks.remove(&p.id);
                        *slot = p;
                    }
                    g.app.usenet.save();
                }))
            ),
        ]),
    ]
    .spacing(12)
    .into()
}
