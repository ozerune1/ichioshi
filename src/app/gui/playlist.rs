//! The playlist: episodes in order, exported as a dated zip (and uploaded
//! to Google Drive, if that's set up).

use chrono::Local;
use iced::widget::{Column, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

use super::series::with_tip;
use super::{Gui, Message, act, edit};
use crate::app::playlist_page::PlaylistPage;
use crate::app::{Action, Page, fmt_bytes};
use crate::playlist;

use super::style::*;

impl Gui {
    pub(super) fn playlist_page<'a>(&'a self, pp: &'a PlaylistPage) -> Element<'a, Message> {
        let exporting = pp.export.as_ref().is_some_and(|r| r.running());
        let items = &self.app.playlist.items;
        let names = playlist::export_names(items);
        let sizes: Vec<Option<u64>> = items
            .iter()
            .map(|i| std::fs::metadata(&i.path).ok().map(|m| m.len()))
            .collect();
        let missing = sizes.iter().filter(|s| s.is_none()).count();
        let total: u64 = sizes.iter().flatten().sum();

        let mut tools = row![desktop_only(secondary(
            if pp.adding {
                "Adding…"
            } else {
                "Add files…"
            },
            (!exporting && !pp.adding).then(|| act(Action::PlaylistAddFiles))
        ))]
        .spacing(10)
        .align_y(Alignment::Center);
        if !items.is_empty() {
            tools = tools.push(with_tip(
                secondary("Clear", (!exporting).then(|| act(Action::PlaylistClear))),
                "Removes everything from the playlist; the files themselves are kept.".into(),
            ));
        }
        let mut c = column![
            page_title(
                "Playlist",
                format!(
                    "{} episode{} · {}",
                    items.len(),
                    if items.len() == 1 { "" } else { "s" },
                    fmt_bytes(total)
                )
            ),
            weak(
                "Episodes in the order you want them. Exporting makes a zip named after the date \
                 and time, holding a folder of the same name with the files numbered “1) title”."
            ),
            tools,
        ]
        .spacing(12);

        // The episodes.
        let mut list = Column::new().spacing(6);
        if items.is_empty() {
            list = list.push(weak(
                "Nothing here yet. Add episodes with “+ Playlist” on a series page, after \
                 remuxing, or from files above.",
            ));
        }
        for (i, (item, name)) in items.iter().zip(&names).enumerate() {
            let shown = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(stem, _)| stem)
                .to_owned();
            let size: Element<'_, Message> = match sizes[i] {
                Some(s) => text(fmt_bytes(s)).size(12).color(FAINT).into(),
                None => text("File missing").size(12).color(RED).into(),
            };
            let enabled = !exporting;
            let name = with_tip(
                text(shown).size(14).color(TEXT).into(),
                item.path.display().to_string(),
            );
            let tools = row![
                size,
                small(
                    "↑",
                    (enabled && i > 0).then(|| act(Action::PlaylistMove { from: i, to: i - 1 }))
                ),
                small(
                    "↓",
                    (enabled && i + 1 < items.len())
                        .then(|| act(Action::PlaylistMove { from: i, to: i + 1 }))
                ),
                small("Remove", enabled.then(|| act(Action::PlaylistRemove(i)))),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            // A phone's: the episode, then its size and buttons.
            let entry: Element<'_, Message> = if compact() {
                column![name, tools].spacing(6).into()
            } else {
                row![name, space::horizontal(), tools]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .into()
            };
            list = list.push(
                container(entry)
                    .padding([8, 14])
                    .width(Fill)
                    .style(|_| card_style(SURFACE)),
            );
        }
        c = c.push(list);

        // Exporting.
        if !items.is_empty() {
            let name = playlist::export_name(&Local::now());
            let mut export = column![
                labelled(
                    "Export to",
                    field("Folder", &pp.out_dir)
                        .on_input(|t| edit(move |g| {
                            if let Page::Playlist(pp) = &mut g.app.page {
                                pp.out_dir = t.clone();
                            }
                        }))
                        .width(Fill)
                        .into(),
                    Some(desktop_only(secondary(
                        "Browse…",
                        (!exporting).then(|| act(Action::PlaylistPickFolder))
                    ))),
                ),
                weak(format!(
                    "Creates “{name}.zip” ({}) with a “{name}” folder inside; the time is taken \
                     when you export.",
                    fmt_bytes(total)
                )),
            ]
            .spacing(10);
            let run: Element<'_, Message> = match &pp.export {
                Some(run) if run.running() => row![
                    bar(run.progress, ACCENT, 8.0).length(if compact() { 150 } else { 300 }),
                    text(format!("{:.0}%", run.progress * 100.0))
                        .size(13)
                        .font(SEMI)
                        .color(TEXT),
                    secondary("Cancel", Some(act(Action::CancelExport))),
                ]
                .spacing(12)
                .align_y(Alignment::Center)
                .into(),
                _ => {
                    let can = missing == 0 && !pp.out_dir.trim().is_empty();
                    let mut r = row![primary(
                        format!(
                            "Export {} episode{}",
                            items.len(),
                            if items.len() == 1 { "" } else { "s" }
                        ),
                        can.then(|| act(Action::StartExport))
                    )]
                    .spacing(12)
                    .align_y(Alignment::Center);
                    if missing > 0 {
                        r = r.push(error_text(format!(
                            "{missing} file{} missing: remove or re-add {}",
                            if missing == 1 { " is" } else { "s are" },
                            if missing == 1 { "it" } else { "them" }
                        )));
                    }
                    match pp.export.as_ref().and_then(|r| r.result.as_ref()) {
                        Some(Ok(path)) => {
                            r = r
                                .push(
                                    text(format!(
                                        "Saved {}",
                                        path.file_name().unwrap_or_default().to_string_lossy()
                                    ))
                                    .size(13)
                                    .color(GREEN),
                                )
                                .push(desktop_only(small(
                                    "Open folder",
                                    Some(act(Action::OpenFolder(
                                        path.parent().map(|p| p.to_owned()).unwrap_or_default(),
                                    ))),
                                )));
                        }
                        Some(Err(e)) if e != "cancelled" => {
                            r = r.push(error_text(format!("Export failed: {e}")));
                        }
                        _ => {}
                    }
                    r.wrap().vertical_spacing(8).into()
                }
            };
            export = export.push(run);
            c = c.push(gap(8.0)).push(card(
                column![text("Export").size(17).font(SEMI).color(TEXT), export].spacing(14),
            ));
        }
        let exported = pp
            .export
            .as_ref()
            .and_then(|r| r.result.as_ref())
            .and_then(|r| r.as_ref().ok());
        c = c.push(self.remuxes_section(pp));
        if DESKTOP {
            c = c.push(card(
                column![
                    text("Google Drive").size(17).font(SEMI).color(TEXT),
                    self.drive_section(exported),
                ]
                .spacing(14),
            ));
        }
        page(c)
    }

    /// The remuxed copies kept in the library's Remuxed folder: to add to
    /// the playlist, open, or delete once you're done with them.
    fn remuxes_section<'a>(&'a self, pp: &'a PlaylistPage) -> Element<'a, Message> {
        let root = self.app.remux_root();
        let total: u64 = pp.remuxes.iter().map(|r| r.bytes).sum();
        let count: usize = pp.remuxes.iter().map(|r| r.files.len()).sum();
        let mut c = column![
            row![
                column![
                    text("Remuxed copies").size(17).font(SEMI).color(TEXT),
                    weak(format!(
                        "{count} episode{} · {} · kept in {} until you delete them (the \
                         episodes in your library are separate, and stay)",
                        if count == 1 { "" } else { "s" },
                        fmt_bytes(total),
                        root.display()
                    )),
                ]
                .spacing(4)
                .width(Fill),
                desktop_only(secondary(
                    "Open folder",
                    root.is_dir().then(|| act(Action::OpenFolder(root.clone())))
                )),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(12);
        if pp.remuxes.is_empty() {
            c = c.push(weak(
                "None yet. “Remux…” on a series page makes copies with just the tracks you \
                 choose.",
            ));
        }
        for r in &pp.remuxes {
            let mut files = Column::new().spacing(4);
            for (path, len) in &r.files {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let listed = self.app.playlist.items.iter().any(|i| &i.path == path);
                let add: Element<'_, Message> = if listed {
                    text("In playlist").size(12).color(FAINT).into()
                } else {
                    small(
                        "+ Playlist",
                        Some(edit({
                            let path = path.clone();
                            move |g| g.app.add_files_to_playlist(vec![path.clone()])
                        })),
                    )
                };
                let name = text(name.into_owned()).size(13).color(TEXT).width(Fill);
                let tools = row![
                    text(fmt_bytes(*len)).size(12).color(FAINT),
                    add,
                    small("Delete…", Some(act(Action::AskDeleteRemux(path.clone())))),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                // A phone's: the file, then its size and buttons.
                files = files.push(if compact() {
                    Element::from(column![name, tools].spacing(4))
                } else {
                    row![name, tools]
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .into()
                });
            }
            c = c.push(
                container(
                    column![
                        row![
                            column![
                                text(&r.name).size(14).font(SEMI).color(TEXT),
                                weak(format!(
                                    "{} episode{} · {}",
                                    r.files.len(),
                                    if r.files.len() == 1 { "" } else { "s" },
                                    fmt_bytes(r.bytes)
                                )),
                            ]
                            .spacing(2)
                            .width(Fill),
                            desktop_only(small(
                                "Open folder",
                                Some(act(Action::OpenFolder(r.dir.clone())))
                            )),
                            small(
                                "Delete all…",
                                Some(act(Action::AskDeleteRemux(r.dir.clone())))
                            ),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                        files,
                    ]
                    .spacing(8),
                )
                .padding([10, 14])
                .width(Fill)
                .style(|_| card_style(SURFACE)),
            );
        }
        card(c)
    }
}
