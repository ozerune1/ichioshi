//! Searching: anime on AniList (with what SeaDex has for each), and
//! picking which files of a release to download.

use iced::widget::{Column, column, container, mouse_area, row, space, text};
use iced::{Alignment, Element, Fill, Length};

use super::series::{header_row, with_tip};
use super::{Gui, Message, act, edit};
use crate::app::{Action, FilesPage, Page, Picker, fmt_bytes, is_video};
use crate::seadex::Anime;

use super::style::*;

/// Change the file picker's state, if it's still showing.
fn on_files(f: impl Fn(&mut FilesPage) + Send + Sync + 'static) -> Message {
    edit(move |g| {
        if let Page::Files(fp) = &mut g.app.page {
            f(fp);
        }
    })
}

impl Gui {
    pub(super) fn search_page(&self) -> Element<'_, Message> {
        let a = &self.app;
        let search = row![
            field("Search anime, e.g. Frieren", &a.query)
                .id("search")
                .on_input(|q| edit(move |g| g.app.query = q.clone()))
                .on_submit(act(Action::Search))
                .size(16)
                .padding([12, 16]),
            primary(
                if a.searching {
                    "Searching…"
                } else {
                    "Search"
                },
                (!a.searching && !a.query.trim().is_empty()).then(|| act(Action::Search)),
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let note = if a.last_query.is_empty() {
            "Anime from AniList, with the releases SeaDex (releases.moe) rates best.".to_owned()
        } else {
            let with_entry = a.results.iter().filter(|r| r.entry.is_some()).count();
            format!(
                "{} results for “{}”, {with_entry} with a SeaDex entry",
                a.results.len(),
                a.last_query
            )
        };
        let mut c = column![page_title("Search", note), gap(18.0), search, gap(22.0)];
        if !a.last_query.is_empty() && a.results.is_empty() && !a.searching {
            c = c.push(weak(format!("No anime found for “{}”.", a.last_query)));
        }
        let dual = a.settings.prefer_dual_audio || a.settings.prefs.wants_dub();
        let mut list = Column::new().spacing(10);
        for (i, anime) in a.results.iter().enumerate() {
            list = list.push(self.result_card(i, anime, dual));
        }
        page(c.push(list))
    }

    fn result_card<'a>(&'a self, i: usize, anime: &'a Anime, dual: bool) -> Element<'a, Message> {
        let cover = self.images.get(
            anime
                .cover_small
                .as_deref()
                .or(anime.cover_large.as_deref()),
        );
        let in_library = self.app.library.find(anime.anilist_id).is_some();
        let (badge_text, color) = match &anime.entry {
            None => (
                "Not on SeaDex · Nyaa releases ranked by group".to_owned(),
                MUTED,
            ),
            Some(entry) => match entry.best_nyaa(dual) {
                Some(b) => {
                    let r = &entry.releases[b];
                    let kind = if r.is_best { "Best" } else { "Alt" };
                    let audio = if r.dual_audio { " · dual audio" } else { "" };
                    (format!("★ {kind} on Nyaa: {}{audio}", r.group), GOLD)
                }
                None => (
                    "On SeaDex, but not on Nyaa · Nyaa releases ranked by group".to_owned(),
                    MUTED,
                ),
            },
        };
        let mut titles = column![text(anime.title.clone()).size(16).font(SEMI).color(TEXT)]
            .spacing(4)
            .width(Fill);
        if let Some(alt) = &anime.alt_title {
            titles = titles.push(weak(alt.clone()));
        }
        titles = titles
            .push(text(anime.summary()).size(13).color(MUTED))
            .push(text(badge_text).size(13).font(MEDIUM).color(color));
        let mut r = row![picture(cover, 60.0, 86.0, 8.0), titles]
            .spacing(16)
            .align_y(Alignment::Center);
        if in_library {
            r = r.push(badge("In your library", GREEN));
        }
        mouse_area(
            container(r)
                .padding(12)
                .width(Fill)
                .style(|_| card_style(SURFACE)),
        )
        .on_press(act(Action::OpenAnime(i)))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
    }

    // ── Choosing files ────────────────────────────────────────────────

    pub(super) fn files_page<'a>(&'a self, fp: &'a FilesPage) -> Element<'a, Message> {
        let a = &self.app;
        let mut head = column![
            row![
                link("← Back", Some(act(Action::Back))),
                text("Choose episodes").size(26).font(BOLD).color(TEXT),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            text(fp.title().to_owned()).size(15).font(SEMI).color(TEXT),
        ]
        .spacing(6);
        if !fp.subtitle.is_empty() {
            head = head.push(weak(fp.subtitle.clone()));
        }
        if fp.existing() {
            head = head.push(
                text(
                    "This torrent is already in your downloads. Saving changes which files it \
                     fetches.",
                )
                .size(13)
                .color(GOLD),
            );
        } else if a.via_usenet(fp) {
            let names: Vec<&str> = fp.nzbs.iter().map(|l| l.indexer.name()).collect();
            head = head.push(
                text(format!(
                    "On Usenet ({}): downloads from your Usenet providers",
                    names.join(", ")
                ))
                .size(13)
                .color(GREEN),
            );
        } else if let Some(nzbs) = a.episode_nzbs(fp) {
            let on = nzbs
                .iter()
                .filter(|l| l.is_some_and(|l| !l.is_empty()))
                .count();
            if nzbs.iter().any(Option::is_none) {
                head = head.push(weak("Checking which episodes are on Usenet…"));
            } else if on > 0 {
                let mut names: Vec<&str> = nzbs
                    .iter()
                    .flatten()
                    .flat_map(|l| l.iter().map(|x| x.indexer.name()))
                    .collect();
                names.sort_unstable();
                names.dedup();
                head = head.push(
                    text(format!(
                        "On Usenet ({}): {on} of {} episodes download from your Usenet providers",
                        names.join(", "),
                        nzbs.len()
                    ))
                    .size(13)
                    .color(GREEN),
                );
            }
        }

        // Selecting.
        let visible = fp.visible();
        let mut tools = row![
            small("All", Some(on_files(|fp| fp.checked.fill(true)))),
            small("None", Some(on_files(|fp| fp.checked.fill(false)))),
            small(
                "Invert",
                Some(on_files(|fp| fp.checked.iter_mut().for_each(|c| *c = !*c)))
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if matches!(fp.picker, Picker::Files(_)) {
            tools = tools.push(with_tip(
                small(
                    "Videos only",
                    Some(on_files(|fp| {
                        let videos: Vec<bool> =
                            fp.rows.iter().map(|r| is_video(&r.label)).collect();
                        fp.checked = videos;
                    })),
                ),
                "Select video files and deselect everything else".into(),
            ));
        }
        tools = tools.push(space().width(12)).push(
            field("Filter, e.g. S01E0 or NCOP", &fp.filter)
                .id("filter")
                .on_input(|f| on_files(move |fp| fp.filter = f.clone()))
                .width(260)
                .padding([7, 12])
                .size(13),
        );
        if !fp.filter.trim().is_empty() {
            let shown = visible.clone();
            let hide = visible.clone();
            tools = tools
                .push(small(
                    format!("Select {} shown", visible.len()),
                    Some(on_files(move |fp| {
                        shown.iter().for_each(|&i| fp.checked[i] = true)
                    })),
                ))
                .push(small(
                    "Deselect shown",
                    Some(on_files(move |fp| {
                        hide.iter().for_each(|&i| fp.checked[i] = false)
                    })),
                ));
        }

        // The files (or episodes).
        let heading = match fp.picker {
            Picker::Files(_) => "File",
            Picker::Episodes { .. } => "Episode",
        };
        let seeders = matches!(fp.picker, Picker::Episodes { .. });
        let usenet_hashes: Option<Vec<String>> = match &fp.picker {
            Picker::Episodes { items, .. } if a.usenet.active() => {
                Some(items.iter().map(|(_, i)| i.info_hash.clone()).collect())
            }
            _ => None,
        };
        let shown_label = format!("{heading} ({} shown)", visible.len());
        let mut headers: Vec<&str> = vec![&shown_label, "Size"];
        let mut widths = vec![Length::Fill, Length::Fixed(90.0)];
        if seeders {
            headers.push("Seeders");
            widths.push(Length::Fixed(70.0));
        }
        if usenet_hashes.is_some() {
            headers.push("Usenet");
            widths.push(Length::Fixed(80.0));
        }
        let mut rows = Column::new().spacing(2);
        for &i in &visible {
            let r = &fp.rows[i];
            let on = fp.checked[i];
            let mut cells = row![
                container(check(r.label.clone(), on, move |v| on_files(move |fp| {
                    fp.checked[i] = v
                })))
                .width(widths[0]),
                container(text(fmt_bytes(r.len)).size(12).color(MUTED)).width(widths[1]),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            if seeders {
                cells = cells.push(
                    container(
                        text(r.seeders.map(|s| s.to_string()).unwrap_or_default())
                            .size(12)
                            .color(MUTED),
                    )
                    .width(widths[2]),
                );
            }
            if let Some(hashes) = &usenet_hashes {
                cells =
                    cells.push(container(self.nzb_cell(&hashes[i])).width(*widths.last().unwrap()));
            }
            rows = rows.push(
                container(cells)
                    .padding([6, 12])
                    .width(Fill)
                    .style(move |_| {
                        fill(if on {
                            ACCENT.scale_alpha(0.08)
                        } else {
                            SURFACE.scale_alpha(0.6)
                        })
                    }),
            );
        }

        // Downloading.
        let (count, size) = fp
            .rows
            .iter()
            .zip(&fp.checked)
            .filter(|(_, c)| **c)
            .fold((0usize, 0u64), |(n, s), (r, _)| (n + 1, s + r.len));
        let existing = fp.existing();
        let (on, off) = match a.episode_nzbs(fp) {
            Some(nzbs) => fp.selected().iter().fold((0, 0), |(on, off), &i| {
                if nzbs[i].is_some_and(|l| !l.is_empty()) {
                    (on + 1, off)
                } else {
                    (on, off + 1)
                }
            }),
            None => (0, count),
        };
        let usenet = a.via_usenet(fp) || (on > 0 && off == 0);
        let ready = existing || usenet || a.download_ready().is_ok();
        let label = match a.debrid.service() {
            _ if existing => "Update download".to_owned(),
            _ if usenet => "↓  Download from Usenet".to_owned(),
            service if on > 0 => {
                let rest = service.map_or("as torrents".to_owned(), |s| format!("via {s}"));
                format!("↓  Download: {on} from Usenet, {off} {rest}")
            }
            Some(service) => format!("↓  Download via {service}"),
            None => "↓  Download".to_owned(),
        };
        let folder = fp.output_folder(&a.library_dir()).display().to_string();
        let mut footer = row![
            column![
                text(format!(
                    "{count} of {} {} · {}",
                    fp.rows.len(),
                    fp.unit(),
                    fmt_bytes(size)
                ))
                .size(14)
                .font(SEMI)
                .color(TEXT),
                row![
                    weak(format!("Saves to {folder}")),
                    desktop_only(link(
                        "Change…",
                        (!existing).then(|| act(Action::PickFolder))
                    )),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            ]
            .spacing(2)
            .width(Fill),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        if !ready && let Err(why) = a.download_ready() {
            footer = footer.push(weak(why));
        }
        footer = footer.push(primary(
            if a.starting_download {
                "Starting…".to_owned()
            } else {
                label
            },
            (count > 0 && ready && !a.starting_download).then(|| act(Action::StartDownload)),
        ));
        column![
            container(column![head, gap(12.0), tools].spacing(0)).padding([24, 36]),
            iced::widget::scrollable(
                container(column![header_row(&headers, &widths), rows].spacing(4)).padding([0, 36])
            )
            .height(Fill)
            .style(scroll_style),
            container(footer)
                .padding([16, 36])
                .width(Fill)
                .style(|_| container::Style {
                    background: Some(fill_color(SIDEBAR)),
                    border: iced::Border {
                        color: LINE,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    ..Default::default()
                }),
        ]
        .into()
    }

    /// On Usenet or not, for one torrent.
    fn nzb_cell<'a>(&self, hash: &str) -> Element<'a, Message> {
        match self.app.usenet.links(hash) {
            None => text("checking…").size(12).color(FAINT).into(),
            Some(links) if !links.is_empty() => badge("NZB", GREEN),
            Some(_) => text("—").size(12).color(FAINT).into(),
        }
    }
}
