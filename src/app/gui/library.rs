//! The sidebar and the library page: your series as posters, with what's
//! next to watch.

use std::collections::BTreeSet;

use iced::widget::{
    Column, Row, button, column, container, mouse_area, row, scrollable, space, stack, text,
};
use iced::{Alignment, Background, Border, Color, Element, Fill, Shadow, Vector, gradient};

use super::{Filter, Gui, Message, act, edit};
use crate::app::watching::{PlayerAction, clock, up_next};
use crate::app::{Action, EngineState, Origin, Page, fmt_bytes};
use crate::library::Series;
use crate::tracking::{Change, Status};

use super::style::*;

/// A list status to pick (or "Add to list").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StatusChoice(pub Option<Status>);

impl std::fmt::Display for StatusChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.map_or("Add to list", Status::name))
    }
}

/// A score to pick (0: none); `short` leaves out its word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScoreChoice {
    pub n: u8,
    pub short: bool,
}

impl std::fmt::Display for ScoreChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.n, self.short) {
            (0, true) => f.write_str("★ –"),
            (0, false) => f.write_str("No score"),
            (n, true) => write!(f, "★ {n}"),
            (n, false) => write!(f, "★ {n} · {}", crate::tracking::score_word(u32::from(n))),
        }
    }
}

pub(super) fn change(id: u64, change: Change) -> Message {
    act(Action::Tracking(
        crate::app::tracking_ui::TrackingAction::Change { id, change },
    ))
}

/// The series' episode numbers on disk (as the stats last counted them).
pub(super) fn on_disk(gui: &Gui, id: u64) -> BTreeSet<u32> {
    gui.app
        .pages
        .stats
        .get(&id)
        .map(|s| s.episodes.keys().copied().collect())
        .unwrap_or_default()
}

impl Gui {
    // ── Sidebar ───────────────────────────────────────────────────────

    pub(super) fn sidebar(&self) -> Element<'_, Message> {
        let page = &self.app.page;
        let in_library = |id: u64| self.app.library.find(id).is_some();
        let came_from = |o: Origin| matches!(page, Page::Anime(ap) if ap.origin == o);
        let on_library = match page {
            Page::Home => true,
            Page::Anime(ap) => {
                ap.opened_from_library()
                    || (ap.origin == Origin::Results && in_library(ap.anime.anilist_id))
            }
            _ => false,
        };
        let on_discover = matches!(page, Page::Discover) || came_from(Origin::Discover);
        let on_calendar = matches!(page, Page::Calendar) || came_from(Origin::Calendar);
        let on_search = match page {
            Page::Results | Page::Files(_) => true,
            Page::Anime(ap) => ap.origin == Origin::Results && !in_library(ap.anime.anilist_id),
            _ => false,
        };
        let nav = |icon: &'static str,
                   label: &'static str,
                   badge: Option<String>,
                   active: bool,
                   to: Message| {
            let mut r = row![
                text(icon)
                    .size(15)
                    .width(20)
                    .color(if active { ACCENT } else { MUTED }),
                text(label)
                    .size(14)
                    .font(if active { SEMI } else { MEDIUM })
                    .color(if active { TEXT } else { MUTED }),
                space::horizontal(),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            if let Some(b) = badge {
                r = r.push(
                    container(text(b).size(11).font(SEMI).color(TEXT))
                        .padding([1, 7])
                        .style(|_| pill(SURFACE2)),
                );
            }
            button(r)
                .width(Fill)
                .padding([9, 12])
                .on_press(to)
                .style(move |_, status| {
                    let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: Some(fill_color(if active {
                            ACCENT.scale_alpha(0.14)
                        } else if hover {
                            SURFACE
                        } else {
                            Color::TRANSPARENT
                        })),
                        text_color: TEXT,
                        border: rounded(8.0),
                        ..button::Style::default()
                    }
                })
        };
        let active = self.app.active_downloads();
        let playlist = self.app.playlist.items.len();
        let logo = row![
            container(text("S").size(16).font(BOLD).color(Color::WHITE))
                .center(28)
                .style(|_| container::Style {
                    background: Some(accent_gradient()),
                    border: rounded(8.0),
                    ..Default::default()
                }),
            column![
                text("SeaDex").size(16).font(BOLD).color(TEXT),
                text("anime library").size(11).color(FAINT)
            ],
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let mut accounts = Column::new().spacing(6);
        let names = signed_in_as(&self.app.tracking.config);
        if !names.is_empty() {
            accounts = accounts.push(label("Lists"));
            for a in names {
                accounts = accounts.push(
                    row![dot(GREEN), text(a).size(12).color(MUTED)]
                        .spacing(8)
                        .align_y(Alignment::Center),
                );
            }
        }
        let episodes: usize = self.app.pages.stats.values().map(|s| s.files.len()).sum();
        let (color, state) = self.engine_line();
        let mut lines = column![
            row![dot(color), text(state).size(12).color(MUTED)]
                .spacing(8)
                .align_y(Alignment::Center),
        ]
        .spacing(4);
        // Torrents here, over the VPN: how it is.
        let a = &self.app;
        if !a.debrid.config.enabled && a.vpn.config.enabled && !matches!(a.engine, EngineState::Off)
        {
            use crate::vpn::{Port, Status};
            let (color, line) = match a.vpn.monitor.status() {
                Status::Off => (MUTED, String::new()),
                Status::Checking => (MUTED, "VPN: checking…".to_owned()),
                Status::Down { .. } => (GOLD, "VPN: not connected".to_owned()),
                Status::Up {
                    interface,
                    port: Port::Open(p),
                    ..
                } => (GREEN, format!("VPN: {interface} · port {p}")),
                Status::Up { interface, .. } => (GREEN, format!("VPN: {interface}")),
            };
            if !line.is_empty() {
                lines = lines.push(
                    row![dot(color), text(line).size(12).color(MUTED)]
                        .spacing(8)
                        .align_y(Alignment::Center),
                );
            }
        }
        // An upload to Google Drive keeps going on every page.
        if let Some(p) = self.app.drive.upload_progress() {
            let left = self.app.drive.upload_time_left().unwrap_or_default();
            lines = lines.push(
                column![
                    text(format!("Uploading to Drive · {:.0}%", p * 100.0))
                        .size(12)
                        .color(TEXT),
                    bar(p, ACCENT, 4.0),
                    text(left).size(11).color(FAINT),
                ]
                .spacing(4),
            );
        }
        lines = lines.push(
            text(format!("{episodes} episodes in the library"))
                .size(11)
                .color(FAINT),
        );
        let status = container(lines)
            .padding(12)
            .width(Fill)
            .style(|_| card_style(SURFACE));

        let content = column![
            logo,
            space().height(22),
            label("Browse"),
            space().height(4),
            nav(
                "▦",
                "Library",
                Some(self.app.library.series.len().to_string()),
                on_library,
                act(Action::GoLibrary)
            ),
            nav("✦", "Discover", None, on_discover, act(Action::GoDiscover)),
            nav("◷", "Calendar", None, on_calendar, act(Action::GoCalendar)),
            nav(
                "↓",
                "Downloads",
                (active > 0).then(|| active.to_string()),
                matches!(page, Page::Downloads),
                act(Action::GoDownloads)
            ),
            nav(
                "⌕",
                "Search",
                None,
                on_search,
                edit(|g| {
                    g.app.cancel_listing();
                    g.app.page = Page::Results;
                })
            ),
            nav(
                "≡",
                "Playlist",
                (playlist > 0).then(|| playlist.to_string()),
                matches!(page, Page::Playlist(_)),
                act(Action::OpenPlaylist)
            ),
            nav(
                "⚙",
                "Settings",
                None,
                matches!(page, Page::Settings),
                edit(|g| {
                    g.app.cancel_listing();
                    g.app.page = Page::Settings;
                })
            ),
            space::vertical(),
            accounts,
            space().height(14),
            status,
        ]
        .spacing(2)
        .padding([20, 16]);
        container(content)
            .width(236)
            .height(Fill)
            .style(|_| fill(SIDEBAR))
            .into()
    }

    /// How downloading is: the torrent engine, the VPN, a debrid service.
    fn engine_line(&self) -> (Color, String) {
        let a = &self.app;
        if let Some(service) = a.debrid.service() {
            return (ACCENT, format!("Downloads through {service}"));
        }
        let local = !a.debrid.config.enabled;
        match &a.engine {
            EngineState::Off => (MUTED, "Ready · nothing seeded".into()),
            EngineState::WaitingForVpn(_) => (GOLD, "Connecting to the VPN…".into()),
            EngineState::Ready(_) if local && a.vpn.waiting.is_some() => {
                (GOLD, "Torrents wait for the VPN".into())
            }
            EngineState::Starting => (GOLD, "Starting the torrent engine…".into()),
            EngineState::Ready(_) if a.vpn.config.enabled => {
                (GREEN, "Ready · through the VPN".into())
            }
            EngineState::Ready(_) => (GREEN, "Ready · nothing seeded".into()),
            EngineState::Failed(_) => (RED, "The torrent engine failed".into()),
        }
    }

    // ── Library ───────────────────────────────────────────────────────

    pub(super) fn library_page(&self) -> Element<'_, Message> {
        let a = &self.app;
        let episodes: usize = a.pages.stats.values().map(|s| s.files.len()).sum();
        let bytes: u64 = a.pages.stats.values().map(|s| s.bytes).sum();
        let header = row![
            page_title(
                "Library",
                format!(
                    "{} series · {episodes} episodes · {}",
                    a.library.series.len(),
                    fmt_bytes(bytes)
                ),
            ),
            space::horizontal(),
            column![
                row![
                    weak("Languages"),
                    text(a.settings.prefs.summary())
                        .size(13)
                        .font(SEMI)
                        .color(TEXT),
                    link("Change…", Some(act(Action::OpenLanguages))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                field("Search your library", &self.lib_search)
                    .on_input(|s| edit(move |g| g.lib_search = s.clone()))
                    .width(320),
            ]
            .spacing(8)
            .align_x(Alignment::End),
        ]
        .align_y(Alignment::End);

        if a.library.series.is_empty() {
            let empty = column![
                text("Your library is empty")
                    .size(22)
                    .font(BOLD)
                    .color(TEXT),
                weak("Search for an anime, then add it or download episodes of it."),
                weak(
                    "Releases come from what SeaDex (releases.moe) rates best, or, for anime \
                     SeaDex doesn't cover, from the best-ranked release group on Nyaa."
                ),
                gap(8.0),
                primary(
                    "⌕  Search for an anime",
                    Some(edit(|g| g.app.page = Page::Results))
                ),
            ]
            .spacing(8)
            .align_x(Alignment::Center);
            return page(column![
                header,
                container(empty).center_x(Fill).padding([120, 0])
            ]);
        }

        let mut order: Vec<&Series> = a.library.series.iter().collect();
        order.sort_by(|x, y| y.added.cmp(&x.added).then(x.title().cmp(y.title())));

        // Filters by list status, when there are lists.
        let mut chips = Row::new().spacing(8);
        if a.tracking.any() {
            let status_of = |s: &Series| a.tracking.shown(s.anilist_id).map(|e| e.status);
            let filter = self.filter;
            chips = chips.push(chip(
                format!("All  {}", order.len()),
                filter == Filter::All,
                Some(edit(|g| g.filter = Filter::All)),
            ));
            for st in [
                Status::Watching,
                Status::Completed,
                Status::Planning,
                Status::Paused,
                Status::Dropped,
            ] {
                let n = order.iter().filter(|s| status_of(s) == Some(st)).count();
                if n == 0 && st == Status::Dropped {
                    continue;
                }
                chips = chips.push(chip(
                    format!("{}  {n}", st.name()),
                    filter == Filter::Only(st),
                    Some(edit(move |g| g.filter = Filter::Only(st))),
                ));
            }
        }

        // Continue watching: series being watched, with what's next.
        let mut shelf = Row::new().spacing(16);
        let mut shelved = 0;
        for s in &order {
            let watching = a
                .tracking
                .shown(s.anilist_id)
                .is_some_and(|e| matches!(e.status, Status::Watching | Status::Rewatching))
                || (!a.tracking.any() && s.watched.values().any(|w| !w.done));
            if !watching {
                continue;
            }
            let disk = on_disk(self, s.anilist_id);
            if let Some(next) = up_next(s, disk.iter().copied()) {
                shelf = shelf.push(self.continue_card(s, next));
                shelved += 1;
            }
        }

        let search = self.lib_search.to_lowercase();
        let cards = order
            .into_iter()
            .filter(|s| match self.filter {
                Filter::All => true,
                Filter::Only(st) => a.tracking.shown(s.anilist_id).map(|e| e.status) == Some(st),
            })
            .filter(|s| search.is_empty() || s.title().to_lowercase().contains(&search))
            .map(|s| self.cover_card(s));
        let grid = Row::with_children(cards)
            .spacing(24)
            .wrap()
            .vertical_spacing(28);

        let mut body = column![header, gap(24.0)];
        if shelved > 0 {
            body = body
                .push(section("Continue watching", None))
                .push(
                    scrollable(shelf).direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::new().width(4).scroller_width(4),
                    )),
                );
            body = body.push(gap(26.0));
        }
        if a.tracking.any() {
            body = body.push(chips).push(gap(18.0));
        }
        page(body.push(grid))
    }

    /// A poster: list status, how much is watched, downloading; with the
    /// status and score to change under it.
    fn cover_card<'a>(&'a self, s: &'a Series) -> Element<'a, Message> {
        const W: f32 = 176.0;
        const H: f32 = 250.0;
        let a = &self.app;
        let id = s.anilist_id;
        let details = s.details.as_ref();
        let stats = a.pages.stats.get(&id);
        let cover = self.images.get(
            s.cover
                .as_deref()
                .or(details.and_then(|d| d.cover.as_deref())),
        );
        let entry = a.tracking.shown(id);
        let have = stats.map_or(0, |s| s.files.len());
        let total = s.episodes.or(details.and_then(|d| d.episodes));
        let done = s.watched.values().filter(|w| w.done).count();
        let watched = match total {
            Some(t) if t > 0 => done as f32 / t as f32,
            _ if have > 0 => done as f32 / have as f32,
            _ => 0.0,
        };

        let mut over = column![].padding(10).width(W).height(H);
        if let Some(e) = entry {
            let color = status_color(e.status);
            over = over.push(
                container(
                    text(e.status.name())
                        .size(11)
                        .font(SEMI)
                        .color(Color::WHITE),
                )
                .padding([3, 9])
                .style(move |_| container::Style {
                    background: Some(fill_color(color.scale_alpha(0.92))),
                    border: rounded(999.0),
                    ..Default::default()
                }),
            );
        }
        if stats.is_some_and(|s| s.downloading) {
            let progress = stats
                .map(|s| {
                    let n = s.progress.len().max(1) as f32;
                    s.progress.values().sum::<f32>() / n
                })
                .unwrap_or(0.0);
            over = over.push(space::vertical()).push(
                container(
                    text(format!("↓ {:.0}%", progress * 100.0))
                        .size(11)
                        .font(SEMI)
                        .color(TEXT),
                )
                .padding([3, 8])
                .style(|_| pill(Color { a: 0.85, ..BG })),
            );
        }
        let watched_bar =
            container(
                bar(watched, GREEN, 4.0).style(|_| iced::widget::progress_bar::Style {
                    background: fill_color(Color::WHITE.scale_alpha(0.15)),
                    bar: fill_color(GREEN),
                    border: rounded(999.0),
                }),
            )
            .padding(iced::Padding {
                top: H - 10.0,
                right: 12.0,
                bottom: 6.0,
                left: 12.0,
            })
            .width(W);
        let poster: Element<'_, Message> = if watched > 0.0 {
            stack![picture(cover, W, H, 12.0), over, watched_bar].into()
        } else {
            stack![picture(cover, W, H, 12.0), over].into()
        };
        let meta = match (have, total) {
            (0, _) => "No episodes yet".to_owned(),
            (n, Some(t)) => format!("{n} of {t} episodes"),
            (n, None) => format!("{n} episodes"),
        };
        let state = if stats.is_some_and(|s| s.downloading) {
            text("Downloading").size(12).color(GREEN)
        } else {
            text(
                details
                    .and_then(crate::app::library_ui::airing_line)
                    .unwrap_or_default(),
            )
            .size(12)
            .color(FAINT)
        };
        let mut card = column![
            mouse_area(column![
                poster,
                space().height(10),
                text(clip(s.title(), 34))
                    .size(14)
                    .font(SEMI)
                    .color(TEXT)
                    .height(38),
                text(meta).size(12).color(MUTED),
                state,
            ])
            .on_press(act(Action::OpenSeries(id)))
            .interaction(iced::mouse::Interaction::Pointer),
        ]
        .width(W)
        .spacing(6);
        if a.tracking.any() {
            card = card.push(self.list_pickers(id, true));
        }
        card.into()
    }

    /// Status and score on your lists, to change (grayed while the lists
    /// are being read).
    pub(super) fn list_pickers(&self, id: u64, compact: bool) -> Element<'_, Message> {
        let t = &self.app.tracking;
        let shown = t.shown(id);
        let ready = !t.pending(id);
        let statuses: Vec<StatusChoice> = Status::ALL
            .into_iter()
            .map(|s| StatusChoice(Some(s)))
            .collect();
        let status = iced::widget::pick_list(
            statuses,
            Some(StatusChoice(shown.map(|e| e.status))),
            move |c: StatusChoice| match c.0 {
                Some(s) if ready => change(id, Change::Status(s)),
                _ => Message::Wake,
            },
        )
        .text_size(12)
        .padding([5, 8])
        .style(pick_style)
        .menu_style(menu_style);
        let mut r = Row::new().spacing(6).align_y(Alignment::Center);
        r = r.push(if compact {
            status.width(iced::Length::FillPortion(3))
        } else {
            status
        });
        if let Some(e) = shown {
            let scores: Vec<ScoreChoice> = (0..=10)
                .rev()
                .map(|n| ScoreChoice { n, short: compact })
                .collect();
            let now = ScoreChoice {
                n: e.score.round().clamp(0.0, 10.0) as u8,
                short: compact,
            };
            let score = iced::widget::pick_list(scores, Some(now), move |c: ScoreChoice| {
                change(id, Change::Score(f32::from(c.n)))
            })
            .text_size(12)
            .padding([5, 8])
            .style(pick_style)
            .menu_style(menu_style);
            r = r.push(if compact {
                score.width(iced::Length::FillPortion(2))
            } else {
                score
            });
        }
        r.into()
    }

    /// A wide card on the "continue watching" shelf.
    fn continue_card<'a>(&'a self, s: &'a Series, next: u32) -> Element<'a, Message> {
        let id = s.anilist_id;
        let details = s.details.as_ref();
        let art = self.images.get(
            details
                .and_then(|d| d.banner.as_deref())
                .or(s.cover.as_deref())
                .or(details.and_then(|d| d.cover.as_deref())),
        );
        let picture = picture(art, 360.0, 170.0, 14.0);
        let shade = container(space())
            .width(360)
            .height(170)
            .style(|_| container::Style {
                background: Some(Background::Gradient(
                    gradient::Linear::new(std::f32::consts::PI)
                        .add_stop(0.2, Color::TRANSPARENT)
                        .add_stop(1.0, Color { a: 0.92, ..BG })
                        .into(),
                )),
                border: rounded(14.0),
                ..Default::default()
            });
        let watch = s.watched.get(&next);
        let resume = watch.and_then(crate::library::Watch::resume_at);
        let name = details
            .and_then(|d| d.episode(next))
            .and_then(|e| e.title.clone())
            .filter(|t| !t.starts_with("Episode "));
        let label = match resume {
            Some(at) => format!("Resume episode {next} from {}", clock(at)),
            None => format!("Up next · Episode {next}"),
        };
        let progress = watch.map_or(0.0, |w| (w.position / w.duration.max(1.0)) as f32);
        let mut caption = column![
            space::vertical(),
            text(clip(s.title(), 34))
                .size(16)
                .font(BOLD)
                .color(Color::WHITE),
            text(match name {
                Some(n) => format!("{label} · {}", clip(&n, 26)),
                None => label,
            })
            .size(12)
            .color(Color::from_rgb8(0xd0, 0xd5, 0xe0)),
        ]
        .spacing(2)
        .padding(16)
        .width(360)
        .height(170);
        if progress > 0.0 {
            caption = caption
                .push(space().height(6))
                .push(
                    bar(progress, ACCENT, 4.0).style(|_| iced::widget::progress_bar::Style {
                        background: fill_color(Color::WHITE.scale_alpha(0.2)),
                        bar: fill_color(ACCENT),
                        border: rounded(999.0),
                    }),
                );
        }
        let play = container(
            button(container(text("▶").size(16).color(Color::WHITE)).center(44))
                .padding(0)
                .on_press(act(Action::Player(PlayerAction::Play {
                    id,
                    episode: next,
                })))
                .style(|_, _| button::Style {
                    text_color: Color::WHITE,
                    background: Some(accent_gradient()),
                    border: rounded(999.0),
                    shadow: Shadow {
                        color: Color::BLACK.scale_alpha(0.4),
                        offset: Vector::new(0.0, 4.0),
                        blur_radius: 12.0,
                    },
                    ..Default::default()
                }),
        )
        .padding(iced::Padding {
            top: 14.0,
            left: 300.0,
            ..Default::default()
        });
        mouse_area(stack![picture, shade, caption, play])
            .on_press(act(Action::OpenSeries(id)))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    }
}

/// "AniList · jake", "MyAnimeList · jake": the lists signed in to.
fn signed_in_as(tracking: &crate::tracking::Config) -> Vec<String> {
    let mut accounts = Vec::new();
    if let Some(u) = &tracking.anilist.user {
        accounts.push(format!("AniList · {}", u.name));
    }
    if let Some(u) = &tracking.mal.user {
        accounts.push(format!("MyAnimeList · {u}"));
    }
    accounts
}

/// A thin line between things.
pub(super) fn rule<'a>() -> Element<'a, Message> {
    container(space())
        .width(Fill)
        .height(1)
        .style(|_| container::Style {
            background: Some(fill_color(LINE)),
            border: Border::default(),
            ..Default::default()
        })
        .into()
}
