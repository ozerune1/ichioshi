//! An anime's page: what it is, your lists, every episode (to play or
//! download), the release episodes come from, and its place in the
//! library.

use std::path::PathBuf;

use iced::widget::{Column, Row, column, container, mouse_area, row, space, stack, text, tooltip};
use iced::{Alignment, Background, Color, Element, Fill, Length, gradient};

use super::library::{change, rule};
use super::{Gui, Message, act, edit};
use crate::app::library_ui::{Coverage, EpisodeRow as Episode};
use crate::app::watching::{PlayerAction, clock, up_next};
use crate::app::{
    Action, AnimePage, MAX_NYAA_ROWS, NyaaSearch, Page, chosen_release, fmt_bytes,
    nyaa_release_label, release_label,
};
use crate::fallback::{NyaaRelease, ReleaseKind};
use crate::library::Details;
use crate::prefs::{LANGUAGES, LangPrefs, SubType};
use crate::remux::language_name;
use crate::seadex::Entry;
use crate::tracking::Change;

use super::style::*;

/// Change the anime page's state, if it's still showing.
fn on_page(f: impl Fn(&mut AnimePage) + Send + Sync + 'static) -> Message {
    edit(move |g| {
        if let Page::Anime(ap) = &mut g.app.page {
            f(ap);
        }
    })
}

impl Gui {
    pub(super) fn series_page<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        let id = ap.anime.anilist_id;
        let mut body = Column::new().spacing(0);
        body = body.push(self.hero(ap));
        let side = if compact() { 14 } else { 36 };
        let mut rest = Column::new().spacing(28).padding([8, side]);
        rest = rest.push(self.series_actions(ap));
        if let Some(d) = self.app.details(id).and_then(|d| d.description.clone()) {
            rest = rest
                .push(container(text(d).size(14).line_height(1.55).color(MUTED)).max_width(920));
        }
        // On the servers, managing it comes first (as Sonarr's page).
        let servers = self.app.servers_mode();
        if servers {
            rest = rest.push(self.library_box(ap));
        }
        rest = rest.push(self.episodes(ap));
        rest = rest.push(self.release_section(ap));
        if !servers {
            rest = rest.push(self.library_box(ap));
        }
        rest = rest.push(gap(20.0));
        body = body.push(rest);
        iced::widget::scrollable(body)
            .height(Fill)
            .style(scroll_style)
            .into()
    }

    // ── Header ────────────────────────────────────────────────────────

    fn hero<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        // A phone's: smaller, with the facts under the poster.
        let small = compact();
        let hero: f32 = if small { 360.0 } else { 380.0 };
        let anime = &ap.anime;
        let id = anime.anilist_id;
        let details = self.app.details(id);
        let banner = self.images.get(details.and_then(|d| d.banner.as_deref()));
        let backdrop: Element<'_, Message> = match banner {
            Some(h) => iced::widget::image(h)
                .width(Fill)
                .height(hero)
                .content_fit(iced::ContentFit::Cover)
                .into(),
            None => container(space())
                .width(Fill)
                .height(hero)
                .style(|_| fill(SURFACE))
                .into(),
        };
        let shade = container(space())
            .width(Fill)
            .height(hero)
            .style(|_| container::Style {
                background: Some(Background::Gradient(
                    gradient::Linear::new(std::f32::consts::PI)
                        .add_stop(0.0, Color { a: 0.35, ..BG })
                        .add_stop(0.55, Color { a: 0.75, ..BG })
                        .add_stop(1.0, BG)
                        .into(),
                )),
                ..Default::default()
            });
        let back_to = ap.origin.name();
        let back = iced::widget::button(
            row![text("←").size(14), text(back_to).size(13).font(MEDIUM)].spacing(6),
        )
        .padding([7, 12])
        .on_press(act(Action::Back))
        .style(|_, _| iced::widget::button::Style {
            background: Some(fill_color(Color { a: 0.6, ..BG })),
            text_color: TEXT,
            border: rounded(8.0),
            ..Default::default()
        });
        let cover = self.images.get(
            anime
                .cover_large
                .as_deref()
                .or(details.and_then(|d| d.cover.as_deref())),
        );
        let mut genres = Row::new().spacing(6);
        for g in details
            .map(|d| d.genres.as_slice())
            .unwrap_or_default()
            .iter()
            .take(6)
        {
            genres = genres.push(
                container(text(g.clone()).size(12).font(MEDIUM).color(TEXT))
                    .padding([4, 10])
                    .style(|_| pill(Color::WHITE.scale_alpha(0.1))),
            );
        }
        let mut links = row![link("AniList ↗", Some(open_url(anime.anilist_url())))].spacing(4);
        if anime.entry.is_some() {
            links = links.push(link("releases.moe ↗", Some(open_url(anime.seadex_url()))));
        }
        let facts = match details {
            Some(d) => facts(d),
            None if self.app.pages.details_loading.contains(&id) => {
                "Getting details from AniList…".to_owned()
            }
            None => anime.summary(),
        };
        let mut titles = column![
            text(anime.title.clone())
                .size(if small { 24 } else { 34 })
                .font(BOLD)
                .color(Color::WHITE),
        ]
        .spacing(2)
        .width(Fill);
        if let Some(alt) = anime
            .alt_title
            .clone()
            .or(details.and_then(|d| d.native.clone()))
        {
            titles = titles.push(text(alt).size(14).color(MUTED));
        }
        let facts = text(facts)
            .size(13)
            .font(MEDIUM)
            .color(Color::from_rgb8(0xc9, 0xce, 0xda));
        if small {
            let tags = genres.push(links).wrap().vertical_spacing(6);
            return stack![
                backdrop,
                shade,
                // (The bar on top has the way back.)
                column![
                    space::vertical(),
                    row![picture(cover, 100.0, 142.0, 10.0), titles]
                        .spacing(14)
                        .align_y(Alignment::End),
                    facts,
                    tags,
                ]
                .spacing(10)
                .padding([16, 14])
                .height(hero),
            ]
            .into();
        }
        titles = titles
            .push(space().height(8))
            .push(facts)
            .push(space().height(8))
            .push(row![genres, links].spacing(14).align_y(Alignment::Center));
        stack![
            backdrop,
            shade,
            column![
                back,
                space::vertical(),
                row![picture(cover, 170.0, 242.0, 14.0), titles]
                    .spacing(26)
                    .align_y(Alignment::End)
            ]
            .padding([22, 36])
            .height(hero),
        ]
        .into()
    }

    /// Play, download what's missing, add to the library; your lists.
    fn series_actions<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        let id = ap.anime.anilist_id;
        let series = self.app.library.find(id);
        let (episodes, _) = self.app.episode_rows(ap);
        let coverage = self.app.coverage(ap);
        let missing = missing(&episodes, &coverage);
        let mut buttons = Row::new().spacing(10).align_y(Alignment::Center);
        if let Some(s) = series {
            let disk = episodes.iter().filter(|e| e.file.is_some()).map(|e| e.n);
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
                buttons = buttons.push(primary(
                    label,
                    Some(act(Action::Player(PlayerAction::Play {
                        id,
                        episode: next,
                    }))),
                ));
            }
        } else if self.app.manage.as_ref().is_some_and(|m| m.anilist == id) {
            // In Sonarr as a later part of a series there: nothing to add.
        } else if self.app.servers_mode() {
            let to = match self.app.server_place(id) {
                Some((crate::servers::arr::Kind::Radarr, _)) => "Radarr",
                _ => "Sonarr",
            };
            let why = self.app.why_not_addable(id);
            let busy = self.app.servers.busy.contains(&id);
            let add = primary(
                format!("+  Add to {to}"),
                (why.is_none() && !busy).then(|| act(Action::AddToLibrary)),
            );
            buttons = buttons.push(match why {
                Some(why) => with_tip(add, why),
                None => add,
            });
        } else {
            buttons = buttons.push(primary(
                "+  Add to library",
                Some(act(Action::AddToLibrary)),
            ));
        }
        if self.app.servers_mode() {
            // The servers download whole releases, with their own client.
            let to = match series.and_then(|s| s.remote.as_ref()).map(|r| r.arr) {
                Some(crate::library::ArrRef::Radarr { .. }) => "Radarr",
                Some(_) => "Sonarr",
                None => match self.app.server_place(id) {
                    Some((crate::servers::arr::Kind::Radarr, _)) => "Radarr",
                    _ => "Sonarr",
                },
            };
            let busy = self.app.servers.busy.contains(&id);
            let ready = !matches!(
                coverage,
                Coverage::Nothing(_) | Coverage::Loading | Coverage::Failed(_)
            );
            buttons = buttons.push(with_tip(
                secondary(
                    format!("↓  Send the release to {to}"),
                    (ready && !busy).then(|| act(Action::DownloadEpisodes(Vec::new()))),
                ),
                format!(
                    "{to} downloads the release chosen below (all of it) with its download \
                     client, and puts it in place."
                ),
            ));
            // Its episodes on Jellyfin, kept here to watch offline.
            if let Some(r) = series.and_then(|s| s.remote.as_ref()) {
                let savable = r
                    .episodes
                    .iter()
                    .filter(|(n, e)| {
                        e.jellyfin.is_some()
                            && self.app.offline_copy(id, **n).is_none()
                            && self.app.offline_job(id, **n).is_none()
                    })
                    .count();
                if savable > 0 {
                    buttons = buttons.push(with_tip(
                        secondary(
                            format!("↓  Save {savable} offline"),
                            Some(act(Action::Offline(OfflineAction::Save {
                                id,
                                episodes: Vec::new(),
                            }))),
                        ),
                        "Download its episodes from Jellyfin to this computer, to watch \
                         without it"
                            .into(),
                    ));
                }
            }
            let lists: Element<'_, Message> = if self.app.tracking.any() {
                self.lists_panel(id)
            } else {
                space().into()
            };
            return actions_and_lists(buttons, lists);
        }
        let download = match missing.len() {
            0 => "↓  Download series".to_owned(),
            n => format!("↓  Download {n} missing"),
        };
        let can = !missing.is_empty();
        let download = secondary(
            download,
            can.then(|| act(Action::DownloadEpisodes(missing.clone()))),
        );
        let have = episodes.iter().filter(|e| e.file.is_some()).count();
        buttons = buttons.push(if !can && !episodes.is_empty() && have < episodes.len() {
            with_tip(download, coverage_why(&coverage))
        } else {
            download
        });
        let lists: Element<'_, Message> = if self.app.tracking.any() {
            self.lists_panel(id)
        } else {
            space().into()
        };
        actions_and_lists(buttons, lists)
    }

    /// Your lists: status, score and episodes watched, and what each list
    /// says.
    fn lists_panel(&self, id: u64) -> Element<'_, Message> {
        let t = &self.app.tracking;
        let episodes = self.app.episode_count(id);
        let mut top = row![text("Your lists").size(12).font(SEMI).color(FAINT)]
            .spacing(10)
            .align_y(Alignment::Center);
        if let Some(e) = t.shown(id) {
            top = top.push(dot(status_color(e.status)));
        }
        top = top.push(self.list_pickers(id, false));
        if let Some(e) = t.shown(id) {
            let max = episodes.filter(|n| *n > 0).unwrap_or(9999);
            let p = e.progress;
            top = top.push(
                row![
                    small("−", (p > 0).then(|| change(id, Change::Progress(p - 1)))),
                    text(match episodes {
                        Some(n) => format!("{p} / {n}"),
                        None => format!("{p} watched"),
                    })
                    .size(13)
                    .color(TEXT),
                    small("+1", (p < max).then(|| change(id, Change::Progress(p + 1)))),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        top = top.push(link(
            "Refresh",
            Some(act(Action::Tracking(
                crate::app::tracking_ui::TrackingAction::Refresh,
            ))),
        ));
        // What each list says.
        let mut says: Vec<Element<'_, Message>> = Vec::new();
        let mut entries = Vec::new();
        for service in t.services() {
            let line = match (t.entry(service, id), t.key(service, id)) {
                _ if !t.lists.contains_key(&service) => "reading your list…".to_owned(),
                (Some(e), _) => {
                    entries.push(e);
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
            let saving = t
                .key(service, id)
                .is_some_and(|k| t.saving.contains(&(service, k)));
            says.push(
                text(format!(
                    "{}: {line}{}",
                    service.name(),
                    if saving { " · saving…" } else { "" }
                ))
                .size(12)
                .color(FAINT)
                .into(),
            );
        }
        if let [a, b] = entries.as_slice()
            && !crate::app::tracking_ui::same(a, b)
        {
            says.push(text("⚠ Not the same on both").size(12).color(GOLD).into());
        }
        // A phone's: wrapped, a list a line.
        let (top, says): (Element<'_, Message>, Element<'_, Message>) = if compact() {
            (
                top.wrap().vertical_spacing(8).into(),
                Column::with_children(says).spacing(4).into(),
            )
        } else {
            (top.into(), Row::with_children(says).spacing(14).into())
        };
        container(column![top, says].spacing(8))
            .padding([10, 14])
            .width(if compact() {
                Fill
            } else {
                iced::Length::Shrink
            })
            .style(|_| card_style(SURFACE))
            .into()
    }

    // ── Episodes ──────────────────────────────────────────────────────

    fn episodes<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        let id = ap.anime.anilist_id;
        let series = self.app.library.find(id);
        let (episodes, extras) = self.app.episode_rows(ap);
        let coverage = self.app.coverage(ap);
        let have = episodes.iter().filter(|e| e.file.is_some()).count();
        let bytes: u64 = episodes
            .iter()
            .filter_map(|e| e.file.as_ref().map(|f| f.1))
            .sum();
        let downloading = episodes.iter().filter(|e| e.downloading.is_some()).count();
        let mut summary = format!(
            "{have} of {} {}",
            episodes.len(),
            if self.app.servers_mode() {
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
        let mut tools = Row::new().spacing(8).align_y(Alignment::Center);
        if let Some(s) = series.filter(|s| s.remote.is_none()) {
            let files: Vec<PathBuf> = episodes
                .iter()
                .filter_map(|e| e.file.as_ref().map(|(rel, _)| s.folder.join(rel)))
                .collect();
            let new: Vec<PathBuf> = files
                .iter()
                .filter(|p| !self.app.playlist.items.iter().any(|i| &i.path == *p))
                .cloned()
                .collect();
            if !files.is_empty() {
                let label = match new.len() {
                    0 => "All in the playlist".to_owned(),
                    n => format!("+ Playlist ({n})"),
                };
                tools = tools.push(small(
                    label,
                    (!new.is_empty()).then(|| act(Action::AddToPlaylist { id, files: new })),
                ));
                tools = tools.push(small(
                    "Remux all…",
                    Some(act(Action::RemuxLibrary { id, files })),
                ));
            }
        }
        // A phone's: the tools under the title.
        let header: Element<'_, Message> = if compact() {
            column![
                row![
                    text("Episodes").size(18).font(SEMI).color(TEXT),
                    text(summary).size(13).color(MUTED).width(Fill),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                tools,
            ]
            .spacing(8)
            .into()
        } else {
            row![
                text("Episodes").size(18).font(SEMI).color(TEXT),
                text(summary).size(13).color(MUTED),
                space::horizontal(),
                tools,
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
        };
        let mut list = Column::new().spacing(6);
        if episodes.is_empty() {
            list = list.push(weak("AniList doesn't list its episodes yet."));
        }
        for e in &episodes {
            list = list.push(self.episode_row(id, e, &coverage));
            if let Some(details) = self.episode_details(e.n) {
                list = list.push(details);
            }
        }
        let mut c = column![header];
        if let Some(from) = self.release_runs(id, &episodes) {
            c = c.push(gap(4.0)).push(weak(from));
        }
        let mut c = c.push(gap(12.0)).push(list);
        if !extras.is_empty() {
            let mut files = Column::new().spacing(4);
            for (rel, len) in extras {
                files = files.push(
                    row![
                        text(fmt_bytes(len)).size(12).color(FAINT).width(80),
                        text(rel).size(12).color(MUTED)
                    ]
                    .spacing(10),
                );
            }
            c = c
                .push(gap(18.0))
                .push(label("Other files in the folder"))
                .push(gap(8.0))
                .push(files);
        }
        c.into()
    }

    fn episode_row<'a>(
        &'a self,
        id: u64,
        e: &Episode,
        coverage: &Coverage<'_>,
    ) -> Element<'a, Message> {
        let n = e.n;
        let series = self.app.library.find(id);
        let watch = series.and_then(|s| s.watched.get(&n));
        let done = watch.is_some_and(|w| w.done);
        let present = e.file.is_some();
        let number = container(text(format!("{n:02}")).size(13).font(SEMI).color(if done {
            GREEN
        } else {
            MUTED
        }))
        .center(36)
        .style(move |_| container::Style {
            background: Some(fill_color(if done {
                GREEN.scale_alpha(0.12)
            } else {
                SURFACE2
            })),
            border: rounded(9.0),
            ..Default::default()
        });
        let mal = self.app.episode_scores.episode(id, n);
        let mut title = row![text(clip(&e.title, 80)).size(14).font(MEDIUM).color(
            if present || e.downloading.is_some() {
                TEXT
            } else {
                FAINT
            }
        )]
        .spacing(8)
        .align_y(Alignment::Center);
        if mal.is_some_and(|m| m.filler) {
            title = title.push(badge("Filler", GOLD));
        }
        if mal.is_some_and(|m| m.recap) {
            title = title.push(badge("Recap", MUTED));
        }
        let mut middle = column![title].spacing(5).width(Fill);
        // Its file's release: group, BD or WEB, dual audio, censored.
        if present && let Some(r) = self.app.episode_release(id, n).filter(|r| !r.is_empty()) {
            middle = middle.push(release_tags(r));
        }
        if let Some(w) = watch.filter(|w| !w.done && w.position > 0.0 && w.duration > 0.0) {
            middle = middle.push(
                row![
                    bar((w.position / w.duration) as f32, ACCENT, 3.0).length(200),
                    text(format!("Watched to {}", clock(w.position)))
                        .size(11)
                        .color(FAINT)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        let airs = text(
            e.airs
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%-d %b %Y")
                        .to_string()
                })
                .unwrap_or_default(),
        )
        .size(12)
        .color(FAINT)
        .width(if compact() {
            iced::Length::Shrink
        } else {
            100.into()
        });
        // A phone's rows leave out sizes and remuxing (the list has
        // "Remux all…").
        let size = |len: u64| -> Element<'_, Message> {
            if compact() {
                space().width(0).into()
            } else {
                text(fmt_bytes(len)).size(12).color(FAINT).into()
            }
        };

        let right: Element<'_, Message> = if let Some(p) = e.downloading {
            row![
                bar(p, GREEN, 6.0).length(160),
                text(format!("{:.0}%", p * 100.0))
                    .size(12)
                    .font(SEMI)
                    .color(GREEN)
                    .width(40),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        } else if let (Some((_, len)), Some(r)) = (&e.file, series.and_then(|s| s.remote.as_ref()))
        {
            // On the servers: it plays from Jellyfin.
            let on_jellyfin = r.episodes.get(&n).is_some_and(|x| x.jellyfin.is_some());
            let copy = self.app.offline_copy(id, n).is_some();
            let playable = on_jellyfin || copy;
            let play: Element<'_, Message> = if done {
                text("✓ Watched").size(12).font(SEMI).color(GREEN).into()
            } else if !playable {
                with_tip(
                    text("Not in Jellyfin").size(12).color(FAINT).into(),
                    "Jellyfin hasn't found the file yet (or you're not signed in to it)".into(),
                )
            } else if watch.and_then(crate::library::Watch::resume_at).is_some() {
                text("▶ Resume").size(12).font(SEMI).color(ACCENT).into()
            } else {
                text("▶ Play").size(12).font(SEMI).color(ACCENT).into()
            };
            row![
                self.offline_controls(id, n, on_jellyfin),
                size(*len),
                container(play).width(110).align_x(Alignment::End),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        } else if let (Some((rel, len)), Some(s)) = (&e.file, series) {
            let path = s.folder.join(rel);
            let in_playlist = self.app.playlist.items.iter().any(|i| i.path == path);
            let playlist: Element<'_, Message> = if in_playlist {
                text("In playlist").size(12).color(FAINT).into()
            } else {
                small(
                    "+ Playlist",
                    Some(act(Action::AddToPlaylist {
                        id,
                        files: vec![path.clone()],
                    })),
                )
            };
            let play = if done {
                text("✓ Watched").size(12).font(SEMI).color(GREEN)
            } else if watch.and_then(crate::library::Watch::resume_at).is_some() {
                text("▶ Resume").size(12).font(SEMI).color(ACCENT)
            } else {
                text("▶ Play").size(12).font(SEMI).color(ACCENT)
            };
            row![
                size(*len),
                desktop_layout(small(
                    "Remux…",
                    Some(act(Action::RemuxLibrary {
                        id,
                        files: vec![path],
                    }))
                )),
                playlist,
                container(play).width(80).align_x(Alignment::End),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        } else if !e.out {
            text("Not out yet").size(12).color(FAINT).into()
        } else if let Some(r) = series.and_then(|s| s.remote.as_ref()).filter(|r| {
            // Sonarr can only look for episodes it knows of.
            matches!(r.arr, crate::library::ArrRef::Radarr { .. })
                || r.episodes.get(&n).is_some_and(|e| e.arr_id.is_some())
        }) {
            let to = match r.arr {
                crate::library::ArrRef::Radarr { .. } => "Radarr",
                _ => "Sonarr",
            };
            with_tip(
                small(
                    "⌕ Search",
                    (!self.app.servers.busy.contains(&id)).then(|| {
                        act(Action::Servers(
                            crate::app::servers_ui::ServersAction::Search {
                                id,
                                episodes: vec![n],
                            },
                        ))
                    }),
                ),
                format!("Have {to} look for it with its own indexers"),
            )
        } else if self.app.servers_mode() {
            space().width(0).into()
        } else if coverage.has(n) {
            small("↓ Download", Some(act(Action::DownloadEpisodes(vec![n]))))
        } else {
            with_tip(small("↓ Download", None), coverage_why(coverage))
        };
        // MyAnimeList's episode poll (out of 5), and the discussions.
        let score = with_tip(
            text(
                mal.and_then(|m| m.score)
                    .map(|v| format!("★ {v:.2}"))
                    .unwrap_or_default(),
            )
            .size(12)
            .font(SEMI)
            .color(GOLD)
            .width(56)
            .into(),
            "MyAnimeList's episode rating, out of 5".into(),
        );
        let discuss: Element<'_, Message> = if e.out {
            small(
                "Discuss",
                Some(act(Action::OpenDiscussion { id, episode: n })),
            )
        } else {
            space().width(0).into()
        };
        // On the servers: watched for or not, and its releases there.
        let (watch, releases) = match self.episode_manage(n) {
            Some((w, r)) => (w, r),
            None => (space().width(0).into(), space().width(0).into()),
        };
        // A phone's: two lines, the episode, then when and what to do.
        let line: Element<'_, Message> = if compact() {
            column![
                row![watch, number, middle, score]
                    .spacing(10)
                    .align_y(Alignment::Center),
                row![airs, discuss, releases, right]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .wrap()
                    .vertical_spacing(6),
            ]
            .spacing(6)
            .into()
        } else {
            row![watch, number, middle, score, airs, discuss, releases, right]
                .spacing(16)
                .align_y(Alignment::Center)
                .into()
        };
        let row = container(line)
            .padding([8, 12])
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(fill_color(if present {
                    SURFACE
                } else {
                    Color { a: 0.5, ..SURFACE }
                })),
                border: rounded(10.0),
                ..Default::default()
            });
        if present {
            mouse_area(row)
                .on_press(act(Action::Player(PlayerAction::Play { id, episode: n })))
                .interaction(iced::mouse::Interaction::Pointer)
                .into()
        } else {
            row.into()
        }
    }

    // ── The release ───────────────────────────────────────────────────

    /// Where downloads come from: SeaDex's releases (or, without one on
    /// Nyaa, Nyaa's ranked by release group), to choose from.
    fn release_section<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        let source = match &ap.nyaa {
            Some(search) => search
                .chosen
                .and_then(|i| search.releases.get(i))
                .map(nyaa_release_label),
            None => chosen_release(ap).map(release_label),
        };
        let id = ap.anime.anilist_id;
        let open = source.is_none() || self.release_open == Some(id);
        let title = match &source {
            Some(label) => format!("Downloads come from {label}"),
            None if ap.entry_loading => "Looking up its releases on SeaDex…".to_owned(),
            None => "The release to download from".to_owned(),
        };
        let toggle = if source.is_none() {
            space().into()
        } else if open {
            link("Done", Some(edit(|g| g.release_open = None)))
        } else {
            link("Change", Some(edit(move |g| g.release_open = Some(id))))
        };
        let mut c = column![
            row![
                text(title).size(16).font(SEMI).color(TEXT).width(Fill),
                toggle
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        ]
        .spacing(10);
        if !open {
            return card(c);
        }
        let entry = ap.anime.entry.as_ref();
        if let Some(entry) = entry {
            if entry.incomplete {
                c = c.push(
                    text("⚠ SeaDex marks this entry as incomplete.")
                        .size(13)
                        .color(GOLD),
                );
            }
            if !entry.theoretical_best.is_empty() {
                c = c.push(weak(format!(
                    "Theoretical best: {}",
                    entry.theoretical_best
                )));
            }
        }
        match (&ap.nyaa, entry) {
            (Some(search), _) => c = c.push(self.nyaa_section(entry.is_some(), search)),
            (None, Some(entry)) => c = c.push(self.seadex_section(ap, entry)),
            (None, None) => {}
        }
        if let Some(entry) = entry
            && !entry.notes.is_empty()
        {
            c = c
                .push(gap(4.0))
                .push(label("SeaDex notes"))
                .push(weak(entry.notes.clone()));
        }
        card(c)
    }

    fn seadex_section<'a>(&'a self, ap: &'a AnimePage, entry: &'a Entry) -> Element<'a, Message> {
        let dual = self.app.prefer_dual_audio(ap.anime.anilist_id);
        let usenet = self.app.usenet.active();
        let mut headers = vec![
            "Release", "SeaDex", "Audio", "Files", "Size", "Tags", "Source",
        ];
        if usenet {
            headers.push("Usenet");
        }
        let widths = [
            Length::FillPortion(3),
            Length::Fixed(80.0),
            Length::Fixed(100.0),
            Length::Fixed(50.0),
            Length::Fixed(90.0),
            Length::FillPortion(2),
            Length::Fixed(130.0),
            Length::Fixed(90.0),
        ];
        let mut rows = Column::new().spacing(2).push(header_row(&headers, &widths));
        for i in entry.ranked(dual) {
            let r = &entry.releases[i];
            let nyaa_id = r.nyaa_id();
            let chosen = ap.chosen == Some(i);
            let mut cells: Vec<Element<'_, Message>> = vec![
                radio_cell(&r.group, chosen, nyaa_id.is_some()),
                if r.is_best {
                    badge("★ Best", GOLD)
                } else {
                    text("Alt").size(12).color(FAINT).into()
                },
                cell(if r.dual_audio {
                    "Dual audio"
                } else {
                    "Japanese"
                }),
                cell(r.files.len().to_string()),
                cell(fmt_bytes(r.total_size())),
                cell(r.tags.join(", ")),
                match nyaa_id {
                    Some(id) => link("Nyaa ↗", Some(open_url(crate::nyaa::view_url(id)))),
                    None => with_tip(
                        text(format!("{} (private)", r.tracker))
                            .size(12)
                            .color(FAINT)
                            .into(),
                        "Private tracker: this app downloads from Nyaa only.".into(),
                    ),
                },
            ];
            if usenet {
                cells.push(match r.info_hash_hex().filter(|_| nyaa_id.is_some()) {
                    Some(hash) => self.usenet_cell(&[hash]),
                    None => cell("—"),
                });
            }
            rows = rows.push(release_row(
                cells,
                &widths,
                chosen,
                nyaa_id.map(|_| on_page(move |ap| ap.chosen = Some(i))),
            ));
        }
        column![rows, self.choose_button(ap.chosen.is_some())]
            .spacing(12)
            .into()
    }

    /// The fallback for anime with nothing on SeaDex to download.
    fn nyaa_section<'a>(&'a self, has_entry: bool, search: &'a NyaaSearch) -> Element<'a, Message> {
        let mut c = column![
            weak(if has_entry {
                "SeaDex has no Nyaa release for this, so Nyaa's releases are ranked by TRaSH \
                 Guides release-group tiers."
            } else {
                "Not on SeaDex, so Nyaa's releases are ranked by TRaSH Guides release-group tiers."
            }),
            row![
                field("Search Nyaa", &search.query)
                    .id("nyaa-search")
                    .on_input(|q| on_page(move |ap| {
                        if let Some(s) = &mut ap.nyaa {
                            s.query = q.clone();
                        }
                    }))
                    .on_submit(act(Action::SearchNyaa))
                    .width(360),
                secondary(
                    if search.loading {
                        "Searching…"
                    } else {
                        "Search Nyaa"
                    },
                    (!search.loading && !search.query.trim().is_empty())
                        .then(|| act(Action::SearchNyaa))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);
        if search.query.trim() == search.default_plan.display_query()
            && let Some(season) = search.default_plan.season
        {
            c = c.push(weak(format!(
                "Showing season {season} only. Edit the search to see everything."
            )));
        }
        if let Some(e) = &search.error {
            c = c.push(error_text(e.clone()));
        }
        if search.searched
            && !search.loading
            && search.releases.is_empty()
            && search.error.is_none()
        {
            c = c.push(body(
                "Nothing found on Nyaa. Try another name for the show, e.g. its English title.",
            ));
        }
        if search.releases.is_empty() {
            return c.into();
        }
        let usenet = self.app.usenet.active();
        let mut headers = vec![
            "Release", "Ranking", "Episodes", "Quality", "Audio", "Size", "Seeders", "Source",
        ];
        if usenet {
            headers.push("Usenet");
        }
        let widths = [
            Length::FillPortion(2),
            Length::FillPortion(2),
            Length::Fixed(110.0),
            Length::Fixed(110.0),
            Length::Fixed(80.0),
            Length::Fixed(80.0),
            Length::Fixed(70.0),
            Length::Fixed(90.0),
            Length::Fixed(90.0),
        ];
        c = c.push(self.choose_button(search.chosen.is_some()));
        let mut rows = Column::new().spacing(2).push(header_row(&headers, &widths));
        for (i, r) in search.releases.iter().enumerate().take(MAX_NYAA_ROWS) {
            rows = rows.push(self.nyaa_row(i, r, search.chosen == Some(i), &widths, usenet));
        }
        c = c.push(rows);
        if search.releases.len() > MAX_NYAA_ROWS {
            c = c.push(weak(format!(
                "{} lower-ranked releases not shown.",
                search.releases.len() - MAX_NYAA_ROWS
            )));
        }
        c.into()
    }

    fn nyaa_row<'a>(
        &'a self,
        i: usize,
        r: &'a NyaaRelease,
        chosen: bool,
        widths: &[Length],
        usenet: bool,
    ) -> Element<'a, Message> {
        let problem = r.problem();
        let rank: Element<'_, Message> = if r.rank.is_blocked() {
            text(r.rank.label.clone()).size(12).color(RED).into()
        } else if r.rank.score > 0 {
            text(format!("★ {}", r.rank.label))
                .size(12)
                .color(GOLD)
                .into()
        } else {
            cell(r.rank.label.clone())
        };
        let rank = match problem {
            Some(p) => with_tip(rank, p.to_string()),
            None => rank,
        };
        let seeders = if r.max_seeders == 0 {
            text("0").size(12).color(RED).into()
        } else {
            cell(r.seeders_label())
        };
        let mut cells: Vec<Element<'_, Message>> = vec![
            with_tip(
                radio_cell(&r.group, chosen, problem.is_none()),
                r.title().to_owned(),
            ),
            rank,
            if r.incomplete {
                with_tip(
                    text(format!("{} (incomplete)", r.episodes_label()))
                        .size(12)
                        .color(FAINT)
                        .into(),
                    "Has far fewer episodes than other releases, so it ranks lower.".into(),
                )
            } else {
                cell(r.episodes_label())
            },
            cell(r.quality_label()),
            cell(if r.dual_audio { "Dual audio" } else { "" }),
            cell(fmt_bytes(r.size)),
            seeders,
            match &r.kind {
                ReleaseKind::Torrent { item, .. } => {
                    link("Nyaa ↗", Some(open_url(crate::nyaa::view_url(item.id))))
                }
                ReleaseKind::Episodes(_) => text("Per episode").size(12).color(FAINT).into(),
            },
        ];
        if usenet {
            let hashes: Vec<&str> = match &r.kind {
                ReleaseKind::Torrent { item, .. } => vec![item.info_hash.as_str()],
                ReleaseKind::Episodes(items) => {
                    items.iter().map(|(_, i)| i.info_hash.as_str()).collect()
                }
            };
            cells.push(self.usenet_cell(&hashes));
        }
        release_row(
            cells,
            widths,
            chosen,
            problem.is_none().then(|| {
                on_page(move |ap| {
                    if let Some(s) = &mut ap.nyaa {
                        s.chosen = Some(i);
                    }
                })
            }),
        )
    }

    /// Whether a release is on Usenet.
    fn usenet_cell<'a>(&self, hashes: &[&str]) -> Element<'a, Message> {
        let found: Vec<_> = hashes.iter().map(|h| self.app.usenet.links(h)).collect();
        if found.iter().any(Option::is_none) {
            return text("checking…").size(12).color(FAINT).into();
        }
        match found.as_slice() {
            [Some(links)] if !links.is_empty() => {
                let mut names: Vec<&str> = links.iter().map(|l| l.indexer.name()).collect();
                names.dedup();
                with_tip(badge("NZB", GREEN), format!("On {}", names.join(", ")))
            }
            [_] => cell("—"),
            many => {
                let on = many
                    .iter()
                    .filter(|l| l.is_some_and(|l| !l.is_empty()))
                    .count();
                if on == 0 {
                    cell("—")
                } else {
                    with_tip(
                        badge(format!("NZB {on}/{}", many.len()), GREEN),
                        "Episodes on Usenet download from there; the rest as torrents.".into(),
                    )
                }
            }
        }
    }

    /// Pick files from the chosen release yourself.
    fn choose_button(&self, has_choice: bool) -> Element<'_, Message> {
        // Releases on Usenet don't need the torrent engine or a debrid key.
        let ready = if self.app.usenet.active() {
            Ok(())
        } else {
            self.app.download_ready()
        };
        let enabled = has_choice && ready.is_ok() && !self.app.loading_listing;
        let note = if self.app.loading_listing {
            "Fetching the torrent from Nyaa…".to_owned()
        } else if let Err(why) = &ready {
            why.clone()
        } else if !has_choice {
            "No release to download.".to_owned()
        } else {
            "Or pick which files to download yourself.".to_owned()
        };
        row![
            secondary("Choose files…", enabled.then(|| act(Action::ChooseFiles))),
            weak(note)
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    }

    // ── In the library ────────────────────────────────────────────────

    /// A series' languages: the library's, or its own.
    pub(super) fn series_languages<'a>(
        &'a self,
        series: &'a crate::library::Series,
    ) -> Column<'a, Message> {
        let id = series.anilist_id;
        let own = series.prefs.is_some();
        let languages = column![
            label("Languages"),
            check("Use different languages for this series", own, move |on| {
                // Starting from the library's.
                edit(move |g| {
                    let prefs = on.then(|| g.app.settings.prefs.clone());
                    g.app.perform(Action::SetSeriesPrefs { id, prefs });
                })
            }),
        ]
        .spacing(10);
        match &series.prefs {
            Some(prefs) => languages.push(lang_editor(prefs, move |p| {
                act(Action::SetSeriesPrefs { id, prefs: Some(p) })
            })),
            None => languages.push(weak(format!(
                "Everyone's: {} (in Settings)",
                self.app.settings.prefs.summary()
            ))),
        }
    }

    fn library_box<'a>(&'a self, ap: &'a AnimePage) -> Element<'a, Message> {
        let anime = &ap.anime;
        if self.app.servers_mode() {
            if let Some(m) = self
                .app
                .manage
                .as_ref()
                .filter(|m| m.anilist == anime.anilist_id)
            {
                return self.manage_panel(m);
            }
            if let Some(r) = self
                .app
                .library
                .find(anime.anilist_id)
                .and_then(|s| s.remote.as_ref())
            {
                return self.remote_box(anime.anilist_id, r);
            }
            let why = self.app.why_not_addable(anime.anilist_id);
            return card(
                column![
                    text("Not on your servers").size(16).font(SEMI).color(TEXT),
                    weak(why.unwrap_or_else(|| {
                        "Adding it puts it in Sonarr or Radarr, watched for (not searched \
                         for: send them the release above, or have them search)."
                            .into()
                    })),
                ]
                .spacing(4),
            );
        }
        let Some(series) = self.app.library.find(anime.anilist_id) else {
            let folder = self
                .app
                .library
                .folder_for(&crate::app::new_series(anime), &self.app.library_dir());
            return card(
                row![
                    column![
                        text("Not in your library").size(16).font(SEMI).color(TEXT),
                        weak(format!(
                            "It would go in {}. Downloading episodes adds it too.",
                            folder.display()
                        )),
                    ]
                    .spacing(4)
                    .width(Fill),
                    secondary("+ Add to library", Some(act(Action::AddToLibrary))),
                ]
                .spacing(16)
                .align_y(Alignment::Center),
            );
        };
        let id = series.anilist_id;
        let stats = self.app.pages.stats.get(&id).cloned().unwrap_or_default();
        let folder = series.folder.clone();
        let mut languages = self.series_languages(series);
        let count = stats.files.len();
        if count > 0 {
            let done = count.saturating_sub(stats.pending);
            languages = languages.push(weak(if stats.pending > 0 {
                format!("Default tracks set on {done} of {count} episodes; setting the rest…")
            } else {
                format!(
                    "Default tracks set on all {count} episodes. Every track is still there to \
                     switch to."
                )
            }));
        }
        let about = column![
            text("In your library").size(16).font(SEMI).color(GREEN),
            weak(folder.display().to_string()),
        ]
        .spacing(4);
        let buttons = row![
            desktop_only(secondary(
                "Open folder",
                Some(act(Action::OpenFolder(folder.clone())))
            )),
            secondary(
                "Remove",
                Some(act(Action::RemoveSeries {
                    id,
                    delete_files: false
                }))
            ),
            danger("Delete folder…", Some(act(Action::AskRemoveSeries(id)))),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        // A phone's: the buttons under it.
        let head: Element<'_, Message> = if compact() {
            column![about, buttons].spacing(12).into()
        } else {
            row![about, space::horizontal(), buttons]
                .spacing(10)
                .align_y(Alignment::Center)
                .into()
        };
        card(column![head, rule(), languages,].spacing(16))
    }
}

use crate::app::offline_ui::{OfflineAction, Saving};

impl Gui {
    /// An episode's offline copy: there (remove it), being saved (how far;
    /// stop), failed (retry), or to save.
    fn offline_controls(&self, id: u64, n: u32, on_jellyfin: bool) -> Element<'_, Message> {
        let offline = |a: OfflineAction| act(Action::Offline(a));
        if self.app.offline_copy(id, n).is_some() {
            return row![
                badge("Offline ✓", GREEN),
                small(
                    "Remove copy",
                    Some(offline(OfflineAction::Remove { id, episode: n }))
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into();
        }
        if let Some(j) = self.app.offline_job(id, n) {
            let (status, extra): (Element<'_, Message>, Element<'_, Message>) = match &j.state {
                Saving::Waiting => (
                    text("Waiting to save").size(12).color(FAINT).into(),
                    small("Stop", Some(offline(OfflineAction::Cancel(j.id)))),
                ),
                Saving::Getting => (
                    row![
                        bar(j.fraction(), ACCENT, 4.0).length(80),
                        text(format!("{:.0}%", j.fraction() * 100.0))
                            .size(12)
                            .font(SEMI)
                            .color(ACCENT),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .into(),
                    small("Stop", Some(offline(OfflineAction::Cancel(j.id)))),
                ),
                Saving::Failed(e) => (
                    with_tip(text("Couldn't save").size(12).color(RED).into(), e.clone()),
                    small("Retry", Some(offline(OfflineAction::Retry(j.id)))),
                ),
            };
            return row![status, extra]
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        if on_jellyfin {
            return small(
                "↓ Save offline",
                Some(offline(OfflineAction::Save {
                    id,
                    episodes: vec![n],
                })),
            );
        }
        space().width(0).into()
    }
}

/// A release, in words: "SubsPlease · WEB · Dual audio · Uncensored".
fn release_words(r: &crate::library::EpisodeRelease) -> String {
    let mut w: Vec<String> = Vec::new();
    w.extend(r.group.clone());
    w.extend(r.source.clone());
    if r.dual_audio == Some(true) {
        w.push("Dual audio".into());
    }
    match r.censored {
        Some(true) => w.push("Censored".into()),
        Some(false) => w.push("Uncensored".into()),
        None => {}
    }
    w.join(" · ")
}

/// An episode's release, as tags.
fn release_tags<'a>(r: &crate::library::EpisodeRelease) -> Element<'a, Message> {
    let mut tags = Row::new().spacing(6).align_y(Alignment::Center);
    if let Some(g) = &r.group {
        tags = tags.push(text(g.clone()).size(11).font(SEMI).color(MUTED));
    }
    if let Some(s) = &r.source {
        tags = tags.push(badge(s.clone(), if s == "BD" { ACCENT } else { MUTED }));
    }
    match r.dual_audio {
        Some(true) => tags = tags.push(badge("Dual audio", GREEN)),
        Some(false) => tags = tags.push(text("Japanese only").size(11).color(FAINT)),
        None => {}
    }
    match r.censored {
        Some(true) => tags = tags.push(badge("Censored", GOLD)),
        Some(false) => tags = tags.push(badge("Uncensored", GREEN)),
        None => {}
    }
    tags.into()
}

impl Gui {
    /// Which releases the episodes here are, run by run: "From
    /// SubsPlease · WEB (1–10), Group · BD (11–12)".
    fn release_runs(&self, id: u64, episodes: &[Episode]) -> Option<String> {
        let mut runs: Vec<(String, u32, u32)> = Vec::new();
        for e in episodes.iter().filter(|e| e.file.is_some()) {
            let words = self
                .app
                .episode_release(id, e.n)
                .map(release_words)
                .unwrap_or_default();
            match runs.last_mut() {
                Some((w, _, last)) if *w == words => *last = e.n,
                _ => runs.push((words, e.n, e.n)),
            }
        }
        runs.retain(|(w, _, _)| !w.is_empty());
        let span = |a: u32, b: u32| {
            if a == b {
                format!("{a}")
            } else {
                format!("{a}–{b}")
            }
        };
        match runs.as_slice() {
            [] => None,
            [(w, _, _)] => Some(format!("From {w}")),
            many => Some(format!(
                "From {}",
                many.iter()
                    .map(|(w, a, b)| format!("{w} ({})", span(*a, *b)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// Episodes out, not here or downloading, that the release has.
fn missing(episodes: &[Episode], coverage: &Coverage<'_>) -> Vec<u32> {
    episodes
        .iter()
        .filter(|e| e.file.is_none() && e.downloading.is_none() && e.out && coverage.has(e.n))
        .map(|e| e.n)
        .collect()
}

fn coverage_why(c: &Coverage<'_>) -> String {
    c.why_not()
}

fn facts(d: &Details) -> String {
    let mut facts = Vec::new();
    if let Some(score) = d.score {
        facts.push(format!("★ {score}%"));
    }
    if let (Some(season), Some(year)) = (&d.season, d.year) {
        facts.push(format!("{} {year}", title_case(season)));
    } else if let Some(year) = d.year {
        facts.push(year.to_string());
    }
    if let Some(format) = &d.format {
        facts.push(format.replace('_', " "));
    }
    if !d.studios.is_empty() {
        facts.push(d.studios.join(", "));
    }
    if let Some(source) = &d.source {
        facts.push(format!("from {}", title_case(&source.replace('_', " "))));
    }
    if let Some(minutes) = d.duration {
        facts.push(format!("{minutes} min episodes"));
    }
    if let Some(line) = crate::app::library_ui::airing_line(d) {
        facts.push(line);
    }
    facts.join("   ·   ")
}

fn title_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

pub(super) fn open_url(url: String) -> Message {
    edit(move |g| g.app.ctx.open_url(&url))
}

pub(super) fn with_tip<'a>(content: Element<'a, Message>, tip: String) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(tip).size(12).color(TEXT))
            .padding([6, 10])
            .max_width(360)
            .style(|_| card_style(SURFACE2)),
        tooltip::Position::Top,
    )
    .into()
}

fn cell<'a>(s: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(s).size(12).color(MUTED).into()
}

fn radio_cell<'a>(group: &str, chosen: bool, enabled: bool) -> Element<'a, Message> {
    let mark = container(if chosen {
        container(space())
            .width(8)
            .height(8)
            .style(|_| pill(Color::WHITE))
    } else {
        container(space())
    })
    .center(16)
    .style(move |_| container::Style {
        background: Some(fill_color(if chosen { ACCENT } else { SURFACE2 })),
        border: iced::Border {
            color: if chosen { ACCENT } else { LINE },
            width: 1.0,
            radius: 999.0.into(),
        },
        ..Default::default()
    });
    row![
        mark,
        text(group.to_owned())
            .size(13)
            .font(SEMI)
            .color(if enabled { TEXT } else { FAINT })
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

pub(super) fn header_row<'a>(headers: &[&str], widths: &[Length]) -> Element<'a, Message> {
    let mut r = Row::new().spacing(12).padding([4, 12]);
    for (h, w) in headers.iter().zip(widths) {
        r = r.push(container(text(h.to_uppercase()).size(11).font(SEMI).color(FAINT)).width(*w));
    }
    r.into()
}

/// A row in a table of releases: chosen when clicked (if it can be).
fn release_row<'a>(
    cells: Vec<Element<'a, Message>>,
    widths: &[Length],
    chosen: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let mut r = Row::new().spacing(12).align_y(Alignment::Center);
    for (c, w) in cells.into_iter().zip(widths) {
        r = r.push(container(c).width(*w));
    }
    let boxed = container(r)
        .padding([9, 12])
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(fill_color(if chosen {
                ACCENT.scale_alpha(0.12)
            } else {
                SURFACE2.scale_alpha(0.5)
            })),
            border: iced::Border {
                color: if chosen {
                    ACCENT.scale_alpha(0.5)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
    match on_press {
        Some(m) => mouse_area(boxed)
            .on_press(m)
            .interaction(iced::mouse::Interaction::Pointer)
            .into(),
        None => boxed.into(),
    }
}

// ── Languages ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct Lang(String);

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&language_name(&self.0))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Subs(Option<String>);

impl std::fmt::Display for Subs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(code) => f.write_str(&language_name(code)),
            None => f.write_str("None"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Kind(SubType);

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.name())
    }
}

/// Audio language, subtitle language (or none) and subtitle kind.
pub(super) fn lang_editor<'a>(
    prefs: &LangPrefs,
    on_change: impl Fn(LangPrefs) -> Message + Clone + 'a,
) -> Element<'a, Message> {
    let langs: Vec<Lang> = LANGUAGES.iter().map(|c| Lang((*c).to_owned())).collect();
    let mut subs = vec![Subs(None)];
    subs.extend(LANGUAGES.iter().map(|c| Subs(Some((*c).to_owned()))));
    let kinds: Vec<Kind> = SubType::ALL.into_iter().map(Kind).collect();
    let (p1, p2, p3) = (prefs.clone(), prefs.clone(), prefs.clone());
    let (f1, f2, f3) = (on_change.clone(), on_change.clone(), on_change);
    let row_of = |name: &'static str, picker: Element<'a, Message>| {
        row![text(name).size(13).color(MUTED).width(90), picker]
            .spacing(10)
            .align_y(Alignment::Center)
    };
    let mut c = column![
        row_of(
            "Audio",
            dropdown(langs, Some(Lang(prefs.audio.clone())), move |l| {
                f1(LangPrefs {
                    audio: l.0,
                    ..p1.clone()
                })
            })
        ),
        row_of(
            "Subtitles",
            dropdown(subs, Some(Subs(prefs.subtitles.clone())), move |s| {
                f2(LangPrefs {
                    subtitles: s.0,
                    ..p2.clone()
                })
            })
        ),
    ]
    .spacing(8);
    if prefs.subtitles.is_some() {
        c = c.push(row_of(
            "Kind",
            dropdown(kinds, Some(Kind(prefs.sub_type)), move |k| {
                f3(LangPrefs {
                    sub_type: k.0,
                    ..p3.clone()
                })
            }),
        ));
        c = c.push(weak(prefs.sub_type.help()));
    }
    c.into()
}

/// The series' buttons, and your lists beside them (a phone's: under them).
fn actions_and_lists<'a>(
    buttons: Row<'a, Message>,
    lists: Element<'a, Message>,
) -> Element<'a, Message> {
    if compact() {
        column![buttons.wrap().vertical_spacing(8), lists]
            .spacing(14)
            .into()
    } else {
        row![buttons, space::horizontal(), lists]
            .align_y(Alignment::Center)
            .into()
    }
}
