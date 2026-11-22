//! The "Studio" pages: sidebar, library, series, downloads; and the look
//! (colors, fonts, styles) the player shares.

use std::path::PathBuf;

use iced::widget::{
    Column, button, column, container, image, mouse_area, pick_list, progress_bar, row, scrollable,
    space, stack, text, text_input,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Fill, Font, Length, Shadow, Theme,
    Vector, font, gradient,
};

use super::{App, Filter, Message, Page, ScoreChoice, StatusChoice};
use crate::tracking::Status;
use crate::view::{Card, EpisodeState, EpisodeView, SeriesView};

pub(super) const BG: Color = Color::from_rgb8(0x0d, 0x0f, 0x14);
pub(super) const SIDEBAR: Color = Color::from_rgb8(0x11, 0x14, 0x1b);
pub(super) const SURFACE: Color = Color::from_rgb8(0x17, 0x1b, 0x24);
pub(super) const SURFACE2: Color = Color::from_rgb8(0x1f, 0x24, 0x30);
pub(super) const LINE: Color = Color::from_rgb8(0x27, 0x2d, 0x3b);
pub(super) const TEXT: Color = Color::from_rgb8(0xe9, 0xec, 0xf3);
pub(super) const MUTED: Color = Color::from_rgb8(0x8b, 0x94, 0xa8);
pub(super) const FAINT: Color = Color::from_rgb8(0x5a, 0x62, 0x74);
pub(super) const ACCENT: Color = Color::from_rgb8(0x8b, 0x7c, 0xff);
pub(super) const GREEN: Color = Color::from_rgb8(0x34, 0xd3, 0x99);

pub(super) const INTER: Font = Font::with_name("Inter");
pub(super) const MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..INTER
};
pub(super) const SEMI: Font = Font {
    weight: font::Weight::Semibold,
    ..INTER
};
pub(super) const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..INTER
};

pub(super) fn status_color(s: Status) -> Color {
    match s {
        Status::Watching => Color::from_rgb8(0x60, 0xa5, 0xfa),
        Status::Completed => GREEN,
        Status::Planning => Color::from_rgb8(0xc0, 0x84, 0xfc),
        Status::Rewatching => Color::from_rgb8(0x22, 0xd3, 0xee),
        Status::Paused => Color::from_rgb8(0xfb, 0xbf, 0x24),
        Status::Dropped => Color::from_rgb8(0xf8, 0x71, 0x71),
    }
}

impl App {
    // ── Sidebar ───────────────────────────────────────────────────────

    pub(super) fn sidebar(&self) -> Element<'_, Message> {
        let on_library = matches!(self.page, Page::Library | Page::Series(_));
        let nav = |icon: &'static str,
                   label: &'static str,
                   badge: Option<String>,
                   active: bool,
                   to: Option<Page>| {
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
                .on_press_maybe(to.map(Message::Go))
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
                        border: Border {
                            radius: 8.0.into(),
                            ..Border::default()
                        },
                        ..button::Style::default()
                    }
                })
        };
        let active_downloads = self.model.downloads.len();
        let episodes: usize = self.model.cards.iter().map(|c| c.have).sum();
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
            ]
            .spacing(0),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let mut accounts = column![text("LISTS").size(10).font(SEMI).color(FAINT)].spacing(6);
        for a in &self.model.accounts {
            accounts = accounts.push(
                row![dot(GREEN), text(a.clone()).size(12).color(MUTED)]
                    .spacing(8)
                    .align_y(Alignment::Center),
            );
        }
        let content = column![
            logo,
            space().height(22),
            text("BROWSE").size(10).font(SEMI).color(FAINT),
            space().height(4),
            nav(
                "▦",
                "Library",
                Some(self.model.cards.len().to_string()),
                on_library,
                Some(Page::Library)
            ),
            nav(
                "↓",
                "Downloads",
                (active_downloads > 0).then(|| active_downloads.to_string()),
                self.page == Page::Downloads,
                Some(Page::Downloads)
            ),
            nav("⌕", "Search", None, false, None),
            nav("≡", "Playlist", None, false, None),
            nav("⚙", "Settings", None, false, None),
            space::vertical(),
            accounts,
            space().height(14),
            container(
                column![
                    row![
                        dot(GREEN),
                        text("Ready · nothing seeded").size(12).color(MUTED)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text(format!("{episodes} episodes in the library"))
                        .size(11)
                        .color(FAINT),
                ]
                .spacing(4)
            )
            .padding(12)
            .width(Fill)
            .style(|_| card_style(SURFACE)),
        ]
        .spacing(2)
        .padding([20, 16]);
        container(content)
            .width(236)
            .height(Fill)
            .style(|_| container::Style {
                background: Some(fill_color(SIDEBAR)),
                border: Border {
                    color: LINE,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    // ── Library ───────────────────────────────────────────────────────

    pub(super) fn library(&self) -> Element<'_, Message> {
        let total_bytes: u64 = 0;
        let _ = total_bytes;
        let header = row![
            column![
                text("Library").size(30).font(BOLD).color(TEXT),
                text(format!(
                    "{} series · {} episodes",
                    self.model.cards.len(),
                    self.model.cards.iter().map(|c| c.have).sum::<usize>()
                ))
                .size(13)
                .color(MUTED),
            ]
            .spacing(2),
            space::horizontal(),
            text_input("Search your library", &self.search)
                .on_input(Message::Search)
                .padding([10, 14])
                .width(300)
                .style(|_, _| text_input::Style {
                    background: fill_color(SURFACE),
                    border: Border {
                        color: LINE,
                        width: 1.0,
                        radius: 10.0.into()
                    },
                    icon: MUTED,
                    placeholder: FAINT,
                    value: TEXT,
                    selection: ACCENT.scale_alpha(0.4),
                }),
        ]
        .align_y(Alignment::Center);

        let count = |s: Status| {
            self.model
                .cards
                .iter()
                .filter(|c| c.list.as_ref().is_some_and(|l| l.status == s))
                .count()
        };
        let chip = |label: String, f: Filter| {
            let on = self.filter == f;
            button(
                text(label)
                    .size(13)
                    .font(MEDIUM)
                    .color(if on { Color::WHITE } else { MUTED }),
            )
            .padding([7, 14])
            .on_press(Message::Filter(f))
            .style(move |_, status| button::Style {
                background: Some(if on {
                    accent_gradient()
                } else if status == button::Status::Hovered {
                    fill_color(SURFACE2)
                } else {
                    fill_color(SURFACE)
                }),
                border: Border {
                    color: if on { Color::TRANSPARENT } else { LINE },
                    width: 1.0,
                    radius: 999.0.into(),
                },
                text_color: TEXT,
                ..button::Style::default()
            })
            .into()
        };
        let mut chips: Vec<Element<'_, Message>> = vec![chip(
            format!("All  {}", self.model.cards.len()),
            Filter::All,
        )];
        for s in [
            Status::Watching,
            Status::Completed,
            Status::Planning,
            Status::Paused,
        ] {
            chips.push(chip(format!("{}  {}", s.name(), count(s)), Filter::Only(s)));
        }

        // Continue watching: series being watched, with what's next.
        let mut shelf = row![].spacing(16);
        for s in self.model.series.iter().filter(|s| {
            s.list
                .as_ref()
                .is_some_and(|l| l.status == Status::Watching)
        }) {
            shelf = shelf.push(continue_card(s));
        }

        let search = self.search.to_lowercase();
        let cards: Vec<&Card> = self
            .model
            .cards
            .iter()
            .filter(|c| match self.filter {
                Filter::All => true,
                Filter::Only(s) => c.list.as_ref().is_some_and(|l| l.status == s),
            })
            .filter(|c| search.is_empty() || c.title.to_lowercase().contains(&search))
            .collect();
        let grid = iced::widget::Row::with_children(cards.into_iter().map(cover_card))
            .spacing(24)
            .wrap()
            .vertical_spacing(28);

        let body = column![
            header,
            space().height(22),
            section("Continue watching", None),
            scrollable(shelf).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4)
            )),
            space().height(26),
            row(chips).spacing(8),
            space().height(18),
            grid,
        ]
        .padding([28, 36]);
        scrollable(body).height(Fill).style(scroll_style).into()
    }

    // ── Series ────────────────────────────────────────────────────────

    pub(super) fn series(&self, id: u64) -> Element<'_, Message> {
        let Some(s) = self.model.series(id) else {
            return text("Not in the library").into();
        };
        const HERO: f32 = 400.0;
        let banner: Element<'_, Message> = match s.banner.as_ref().and_then(|b| b.path.clone()) {
            Some(path) => image(image::Handle::from_path(path))
                .width(Fill)
                .height(HERO)
                .content_fit(ContentFit::Cover)
                .into(),
            None => container(space())
                .width(Fill)
                .height(HERO)
                .style(|_| fill(SURFACE))
                .into(),
        };
        let shade = container(space())
            .width(Fill)
            .height(HERO)
            .style(|_| container::Style {
                background: Some(Background::Gradient(
                    gradient::Linear::new(std::f32::consts::PI)
                        .add_stop(0.0, Color { a: 0.35, ..BG })
                        .add_stop(0.5, Color { a: 0.7, ..BG })
                        .add_stop(1.0, BG)
                        .into(),
                )),
                ..Default::default()
            });
        let back =
            button(row![text("←").size(14), text("Library").size(13).font(MEDIUM)].spacing(6))
                .padding([7, 12])
                .on_press(Message::Go(Page::Library))
                .style(|_, _| button::Style {
                    background: Some(fill_color(Color { a: 0.55, ..BG })),
                    text_color: TEXT,
                    border: rounded(8.0),
                    ..button::Style::default()
                });
        let cover = cover_image(
            s.cover.as_ref().and_then(|c| c.path.clone()),
            170.0,
            242.0,
            14.0,
        );
        let mut genres = row![].spacing(6);
        for g in s.genres.iter().take(5) {
            genres = genres.push(
                container(text(g.clone()).size(12).font(MEDIUM).color(TEXT))
                    .padding([4, 10])
                    .style(|_| pill(Color::WHITE.scale_alpha(0.1))),
            );
        }
        let titles = column![
            text(s.title.clone())
                .size(34)
                .font(BOLD)
                .color(Color::WHITE),
            text(s.native.clone().unwrap_or_default())
                .size(14)
                .color(MUTED),
            space().height(8),
            text(s.facts.join("   ·   "))
                .size(13)
                .font(MEDIUM)
                .color(Color::from_rgb8(0xc9, 0xce, 0xda)),
            space().height(10),
            genres,
        ]
        .spacing(2);
        let hero = stack![
            banner,
            shade,
            column![
                back,
                space::vertical(),
                row![cover, titles].spacing(26).align_y(Alignment::End)
            ]
            .padding([22, 36])
            .height(HERO),
        ];

        let resume = s.up_next.map(|(n, part)| {
            if part {
                format!("▶  Resume episode {n}")
            } else {
                format!("▶  Play episode {n}")
            }
        });
        let mut actions = row![].spacing(10).align_y(Alignment::Center);
        if let (Some(label), Some((n, _))) = (resume, s.up_next) {
            actions = actions.push(primary_button(label, Message::Play(s.id, n)));
        }
        if s.missing > 0 {
            actions = actions.push(secondary_button(format!(
                "↓  Download {} missing",
                s.missing
            )));
        }
        actions = actions.push(secondary_button("Remux…".into()));
        let list: Element<'_, Message> = match &s.list {
            Some(l) => {
                let id = s.id;
                let statuses: Vec<StatusChoice> =
                    Status::ALL.into_iter().map(StatusChoice).collect();
                let scores: Vec<ScoreChoice> = (0..=10).rev().map(ScoreChoice).collect();
                let score_now = l.score.parse::<f32>().map_or(0, |v| v.round() as u8);
                container(
                    row![
                        text("Your lists").size(12).font(SEMI).color(FAINT),
                        dot(status_color(l.status)),
                        pick_list(statuses, Some(StatusChoice(l.status)), move |c| {
                            Message::Status(id, c.0)
                        })
                        .text_size(13)
                        .padding([7, 10])
                        .style(pick_style),
                        pick_list(scores, Some(ScoreChoice(score_now)), move |c| {
                            Message::Score(id, c)
                        })
                        .text_size(13)
                        .padding([7, 10])
                        .style(pick_style),
                        text(format!("{} / {} watched", l.progress, s.episodes.len()))
                            .size(13)
                            .color(MUTED),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding([8, 14])
                .style(|_| card_style(SURFACE))
                .into()
            }
            None => secondary_button("+  Add to list".into()),
        };
        let top = row![actions, space::horizontal(), list].align_y(Alignment::Center);

        let description = s.description.clone().unwrap_or_default();
        let description = description
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(420)
            .collect::<String>();

        let mut list_rows = Column::new().spacing(6);
        for e in &s.episodes {
            list_rows = list_rows.push(episode_row(s.id, e));
        }
        let downloading = s
            .episodes
            .iter()
            .filter(|e| matches!(e.state, EpisodeState::Downloading { .. }))
            .count();
        let summary = format!(
            "{} of {} in your library · {}{}",
            s.have,
            s.episodes.len(),
            s.size,
            if downloading > 0 {
                format!(" · {downloading} downloading")
            } else {
                String::new()
            }
        );
        let body = column![
            hero,
            container(
                column![
                    top,
                    space().height(24),
                    text(description)
                        .size(14)
                        .line_height(1.55)
                        .color(MUTED)
                        .width(Length::Fixed(820.0)),
                    space().height(28),
                    section("Episodes", Some(summary)),
                    list_rows,
                ]
                .spacing(0)
            )
            .padding([8, 36]),
        ];
        scrollable(body).height(Fill).style(scroll_style).into()
    }

    // ── Downloads ─────────────────────────────────────────────────────

    pub(super) fn downloads(&self) -> Element<'_, Message> {
        let mut list = Column::new().spacing(12);
        for d in &self.model.downloads {
            let color = match d.source.as_str() {
                "Usenet" => GREEN,
                "TorBox" => ACCENT,
                _ => status_color(Status::Watching),
            };
            let cover = d
                .series
                .and_then(|id| self.model.cards.iter().find(|c| c.id == id))
                .and_then(|c| c.cover.as_ref().and_then(|i| i.path.clone()));
            let card = row![
                cover_image(cover, 54.0, 76.0, 8.0),
                column![
                    row![
                        container(text(d.source.clone()).size(11).font(SEMI).color(color))
                            .padding([2, 8])
                            .style(move |_| pill(color.scale_alpha(0.15))),
                        text(d.name.clone()).size(15).font(SEMI).color(TEXT),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    progress_bar(0.0..=1.0, d.progress)
                        .girth(8)
                        .style(move |_| progress_bar::Style {
                            background: fill_color(SURFACE2),
                            bar: fill_color(color),
                            border: rounded(999.0),
                        }),
                    row![
                        text(format!("{:.0}%", d.progress * 100.0))
                            .size(12)
                            .font(SEMI)
                            .color(TEXT),
                        text(format!("·  {}  ·  {}", d.size, d.detail))
                            .size(12)
                            .color(MUTED),
                    ]
                    .spacing(6),
                ]
                .spacing(9)
                .width(Fill),
                secondary_button("Pause".into()),
            ]
            .spacing(16)
            .align_y(Alignment::Center);
            list = list.push(
                container(card)
                    .padding(16)
                    .width(Fill)
                    .style(|_| card_style(SURFACE)),
            );
        }
        let body = column![
            text("Downloads").size(30).font(BOLD).color(TEXT),
            text(format!(
                "{} active · from Usenet first, then TorBox, then BitTorrent",
                self.model.downloads.len()
            ))
            .size(13)
            .color(MUTED),
            space().height(22),
            list,
        ]
        .spacing(2)
        .padding([28, 36]);
        scrollable(body).height(Fill).style(scroll_style).into()
    }
}

// ── Pieces ────────────────────────────────────────────────────────────

pub(super) fn fill_color(c: Color) -> Background {
    Background::Color(c)
}

pub(super) fn fill(c: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(c)),
        ..Default::default()
    }
}

pub(super) fn rounded(r: f32) -> Border {
    Border {
        radius: r.into(),
        ..Border::default()
    }
}

pub(super) fn pill(bg: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(bg)),
        border: rounded(999.0),
        ..Default::default()
    }
}

pub(super) fn card_style(bg: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(bg)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

pub(super) fn accent_gradient() -> Background {
    Background::Gradient(
        gradient::Linear::new(std::f32::consts::FRAC_PI_2 * 1.5)
            .add_stop(0.0, ACCENT)
            .add_stop(1.0, Color::from_rgb8(0x5b, 0x8c, 0xff))
            .into(),
    )
}

pub(super) fn dot<'a>(color: Color) -> Element<'a, Message> {
    container(space())
        .width(8)
        .height(8)
        .style(move |_| pill(color))
        .into()
}

pub(super) fn section<'a>(title: &'a str, note: Option<String>) -> Element<'a, Message> {
    let mut r = row![text(title).size(18).font(SEMI).color(TEXT)]
        .spacing(12)
        .align_y(Alignment::End);
    if let Some(n) = note {
        r = r.push(text(n).size(13).color(MUTED));
    }
    container(r)
        .padding(iced::Padding {
            bottom: 12.0,
            ..Default::default()
        })
        .into()
}

pub(super) fn primary_button<'a>(label: String, on_press: Message) -> Element<'a, Message> {
    button(text(label).size(14).font(SEMI).color(Color::WHITE))
        .padding([10, 18])
        .on_press(on_press)
        .style(|_, _| button::Style {
            background: Some(accent_gradient()),
            text_color: Color::WHITE,
            border: rounded(10.0),
            shadow: Shadow {
                color: ACCENT.scale_alpha(0.35),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
            ..button::Style::default()
        })
        .into()
}

pub(super) fn secondary_button<'a>(label: String) -> Element<'a, Message> {
    button(text(label).size(14).font(MEDIUM).color(TEXT))
        .padding([10, 16])
        .on_press(Message::Go(Page::Library))
        .style(|_, status| button::Style {
            background: Some(fill_color(if status == button::Status::Hovered {
                SURFACE2
            } else {
                SURFACE
            })),
            text_color: TEXT,
            border: Border {
                color: LINE,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..button::Style::default()
        })
        .into()
}

pub(super) fn pick_style(_: &Theme, _: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: FAINT,
        handle_color: MUTED,
        background: fill_color(SURFACE2),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 8.0.into(),
        },
    }
}

pub(super) fn scroll_style(_: &Theme, _: scrollable::Status) -> scrollable::Style {
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: fill_color(LINE),
            border: rounded(999.0),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: fill_color(SURFACE),
            border: rounded(999.0),
            shadow: Shadow::default(),
            icon: TEXT,
        },
    }
}

pub(super) fn cover_image<'a>(
    path: Option<PathBuf>,
    w: f32,
    h: f32,
    radius: f32,
) -> Element<'a, Message> {
    match path {
        Some(p) => container(
            image(image::Handle::from_path(p))
                .width(w)
                .height(h)
                .content_fit(ContentFit::Cover)
                .border_radius(radius),
        )
        .style(move |_| container::Style {
            shadow: Shadow {
                color: Color::BLACK.scale_alpha(0.5),
                offset: Vector::new(0.0, 10.0),
                blur_radius: 24.0,
            },
            border: rounded(radius),
            ..Default::default()
        })
        .into(),
        None => container(space())
            .width(w)
            .height(h)
            .style(move |_| container::Style {
                background: Some(fill_color(SURFACE2)),
                border: rounded(radius),
                ..Default::default()
            })
            .into(),
    }
}

pub(super) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>().trim_end())
    }
}

/// A poster in the library grid: status badge, episodes watched.
pub(super) fn cover_card(c: &Card) -> Element<'_, Message> {
    let poster = cover_image(
        c.cover.as_ref().and_then(|i| i.path.clone()),
        184.0,
        262.0,
        12.0,
    );
    let mut over = column![].padding(10).width(184).height(262);
    if let Some(l) = &c.list {
        let color = status_color(l.status);
        over = over.push(
            container(
                text(l.status_name.clone())
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
    if let Some(p) = c.downloading {
        over = over.push(space::vertical()).push(
            container(
                text(format!("↓ {:.0}%", p * 100.0))
                    .size(11)
                    .font(SEMI)
                    .color(TEXT),
            )
            .padding([3, 8])
            .style(|_| pill(Color { a: 0.8, ..BG })),
        );
    }
    let watched =
        container(
            progress_bar(0.0..=1.0, c.watched)
                .girth(4)
                .style(|_| progress_bar::Style {
                    background: fill_color(Color::WHITE.scale_alpha(0.15)),
                    bar: fill_color(GREEN),
                    border: rounded(999.0),
                }),
        )
        .padding(iced::Padding {
            top: 252.0,
            right: 12.0,
            bottom: 6.0,
            left: 12.0,
        })
        .width(184);
    let score = c
        .list
        .as_ref()
        .map(|l| format!("★ {}", l.score))
        .unwrap_or_default();
    let meta = match c.total {
        Some(t) => format!("{}/{t} episodes", c.have),
        None => format!("{} episodes", c.have),
    };
    let body = column![
        stack![poster, over, watched],
        space().height(10),
        text(clip(&c.title, 40))
            .size(14)
            .font(SEMI)
            .color(TEXT)
            .height(38),
        row![
            text(meta).size(12).color(MUTED),
            space::horizontal(),
            text(score)
                .size(12)
                .font(SEMI)
                .color(Color::from_rgb8(0xfb, 0xbf, 0x24))
        ],
    ]
    .width(184);
    mouse_area(body)
        .on_press(Message::Go(Page::Series(c.id)))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

/// A wide banner card on the "continue watching" shelf.
pub(super) fn continue_card(s: &SeriesView) -> Element<'_, Message> {
    let art = s
        .banner
        .as_ref()
        .or(s.cover.as_ref())
        .and_then(|i| i.path.clone());
    let picture: Element<'_, Message> = match art {
        Some(p) => image(image::Handle::from_path(p))
            .width(360)
            .height(170)
            .content_fit(ContentFit::Cover)
            .border_radius(14.0)
            .into(),
        None => container(space())
            .width(360)
            .height(170)
            .style(|_| card_style(SURFACE))
            .into(),
    };
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
    let (next, part) = s.up_next.unwrap_or((1, false));
    let ep = s.episodes.iter().find(|e| e.number == next);
    let ep_title = ep.map(|e| e.title.clone()).unwrap_or_default();
    let label = if part {
        format!("Resume · Episode {next}")
    } else {
        format!("Up next · Episode {next}")
    };
    let progress = ep.map_or(0.0, |e| e.watched);
    let caption = column![
        space::vertical(),
        text(clip(&s.title, 34))
            .size(16)
            .font(BOLD)
            .color(Color::WHITE),
        text(format!(
            "{label}{}",
            if ep_title.starts_with("Episode") {
                String::new()
            } else {
                format!(" · {}", clip(&ep_title, 24))
            }
        ))
        .size(12)
        .color(Color::from_rgb8(0xd0, 0xd5, 0xe0)),
        space().height(6),
        progress_bar(0.0..=1.0, progress)
            .girth(4)
            .style(|_| progress_bar::Style {
                background: fill_color(Color::WHITE.scale_alpha(0.2)),
                bar: fill_color(ACCENT),
                border: rounded(999.0),
            }),
    ]
    .spacing(2)
    .padding(16)
    .width(360)
    .height(170);
    let next_up = s.up_next.map_or(1, |(n, _)| n);
    let play = container(
        button(container(text("▶").size(16).color(Color::WHITE)).center(44))
            .padding(0)
            .on_press(Message::Play(s.id, next_up))
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
        .on_press(Message::Go(Page::Series(s.id)))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

pub(super) fn episode_row(id: u64, e: &EpisodeView) -> Element<'_, Message> {
    let present = matches!(e.state, EpisodeState::OnDisk { .. });
    let title_color = if present || matches!(e.state, EpisodeState::Downloading { .. }) {
        TEXT
    } else {
        FAINT
    };
    let number = container(
        text(format!("{:02}", e.number))
            .size(13)
            .font(SEMI)
            .color(if e.done { GREEN } else { MUTED }),
    )
    .center(36)
    .style(move |_| container::Style {
        background: Some(fill_color(if e.done {
            GREEN.scale_alpha(0.12)
        } else {
            SURFACE2
        })),
        border: rounded(9.0),
        ..Default::default()
    });
    let mut middle = column![
        text(clip(&e.title, 70))
            .size(14)
            .font(MEDIUM)
            .color(title_color)
    ]
    .spacing(5)
    .width(Fill);
    if e.watched > 0.0 && !e.done {
        middle = middle.push(
            progress_bar(0.0..=1.0, e.watched)
                .girth(3)
                .length(220)
                .style(|_| progress_bar::Style {
                    background: fill_color(SURFACE2),
                    bar: fill_color(ACCENT),
                    border: rounded(999.0),
                }),
        );
    }
    let right: Element<'_, Message> = match &e.state {
        EpisodeState::OnDisk { size } => row![
            text(size.clone()).size(12).color(FAINT),
            if e.done {
                Element::from(text("✓ Watched").size(12).font(SEMI).color(GREEN))
            } else {
                Element::from(
                    text(if e.watched > 0.0 {
                        "▶ Resume"
                    } else {
                        "▶ Play"
                    })
                    .size(12)
                    .font(SEMI)
                    .color(ACCENT),
                )
            },
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into(),
        EpisodeState::Downloading { progress } => row![
            progress_bar(0.0..=1.0, *progress)
                .girth(6)
                .length(160)
                .style(|_| progress_bar::Style {
                    background: fill_color(SURFACE2),
                    bar: fill_color(GREEN),
                    border: rounded(999.0),
                }),
            text(format!("{:.0}%", progress * 100.0))
                .size(12)
                .font(SEMI)
                .color(GREEN),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into(),
        EpisodeState::Missing => container(text("↓ Download").size(12).font(MEDIUM).color(MUTED))
            .padding([5, 10])
            .style(|_| container::Style {
                border: Border {
                    color: LINE,
                    width: 1.0,
                    radius: 7.0.into(),
                },
                ..Default::default()
            })
            .into(),
        EpisodeState::NotAired => text("Not out yet").size(12).color(FAINT).into(),
    };
    let airs = text(e.airs.clone().unwrap_or_default())
        .size(12)
        .color(FAINT)
        .width(110);
    let row = container(
        row![number, middle, airs, right]
            .spacing(16)
            .align_y(Alignment::Center),
    )
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
    // An episode on disk plays when clicked.
    if present {
        mouse_area(row)
            .on_press(Message::Play(id, e.number))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    } else {
        row.into()
    }
}
