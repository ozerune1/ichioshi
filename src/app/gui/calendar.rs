//! The calendar: a week of episodes airing (AniList's schedule), for the
//! anime on your lists except dropped ones, or everything.

use chrono::{Days, Local};
use iced::widget::{Column, Row, column, container, mouse_area, row, text};
use iced::{Alignment, Color, Element, Fill, Length};

use super::discover::when;
use super::{Gui, Message, act, edit};
use crate::app::discover_ui::{Loaded, midnight, monday_of};
use crate::app::{Action, Origin};
use crate::browse::Airing;

use super::style::*;

impl Gui {
    pub(super) fn calendar_page(&self) -> Element<'_, Message> {
        let c = &self.app.calendar;
        let today = Local::now().date_naive();
        let week = c.week;
        let last = week + Days::new(6);
        let range = if week.format("%b").to_string() == last.format("%b").to_string() {
            format!("{} – {}", week.format("%-d"), last.format("%-d %B %Y"))
        } else {
            format!("{} – {}", week.format("%-d %b"), last.format("%-d %b %Y"))
        };
        let whose = if c.full {
            "everything airing"
        } else if self.app.tracking.any() {
            "the anime on your lists (but not dropped)"
        } else {
            "your library's anime (sign in to AniList or MyAnimeList for your lists)"
        };
        let controls = row![
            small(
                "← Previous week",
                Some(edit(
                    |g| g.app.calendar.week = g.app.calendar.week - Days::new(7)
                ))
            ),
            small(
                "This week",
                (week != monday_of(today)).then(|| edit(move |g| {
                    g.app.calendar.week = monday_of(Local::now().date_naive())
                }))
            ),
            small(
                "Next week →",
                Some(edit(
                    |g| g.app.calendar.week = g.app.calendar.week + Days::new(7)
                ))
            ),
            desktop_only(iced::widget::space::horizontal()),
            check("Show full schedule", c.full, |on| edit(move |g| g
                .app
                .calendar
                .full =
                on)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(8);
        let head = column![
            page_title("Calendar", format!("{range} · {whose}")),
            controls
        ]
        .spacing(16);

        let body: Element<'_, Message> = match c.weeks.get(&week).map(|(l, _)| l) {
            None | Some(Loaded::Loading) => weak("Reading AniList's schedule…").into(),
            Some(Loaded::Failed(e)) => column![
                error_text(format!("Couldn't read the schedule: {e}")),
                secondary(
                    "Try again",
                    Some(edit(move |g| {
                        g.app.calendar.weeks.remove(&week);
                    }))
                ),
            ]
            .spacing(10)
            .into(),
            Some(Loaded::Ready(airing)) => {
                let shown: Vec<&Airing> = airing
                    .iter()
                    .filter(|a| c.full || self.app.on_your_lists(&a.media))
                    .collect();
                let mut days = Vec::new();
                for i in 0..7 {
                    let day = week + Days::new(i);
                    let (from, to) = (midnight(day), midnight(day + Days::new(1)));
                    let on_day: Vec<&Airing> = shown
                        .iter()
                        .copied()
                        .filter(|a| a.at >= from && a.at < to)
                        .collect();
                    days.push(self.day_column(day, day == today, on_day));
                }
                let days: Element<'_, Message> = if compact() {
                    Column::with_children(days).spacing(10).into()
                } else {
                    Row::with_children(days).spacing(10).into()
                };
                let mut c = column![days].spacing(16);
                if shown.is_empty() {
                    c = c.push(weak(if self.app.calendar.full {
                        "Nothing airing this week."
                    } else {
                        "Nothing of yours airs this week. Show the full schedule to see \
                         everything."
                    }));
                }
                c.into()
            }
        };
        page(column![head, gap(20.0), body])
    }

    fn day_column<'a>(
        &'a self,
        day: chrono::NaiveDate,
        today: bool,
        airing: Vec<&'a Airing>,
    ) -> Element<'a, Message> {
        let mut head = row![
            text(day.format("%a").to_string())
                .size(13)
                .font(SEMI)
                .color(if today { ACCENT } else { TEXT }),
            text(day.format("%-d").to_string())
                .size(13)
                .color(if today { ACCENT } else { MUTED }),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if today {
            head = head.push(badge("Today", ACCENT));
        }
        let now = chrono::Utc::now().timestamp();
        let mut list = Column::new().spacing(8);
        for a in airing {
            list = list.push(self.airing_card(a, a.at < now));
        }
        container(column![head, list].spacing(10))
            .padding(10)
            .width(Length::FillPortion(1))
            .height(Length::Shrink)
            .style(move |_| container::Style {
                background: Some(fill_color(if today {
                    ACCENT.scale_alpha(0.06)
                } else {
                    SURFACE.scale_alpha(0.5)
                })),
                border: iced::Border {
                    color: if today { ACCENT.scale_alpha(0.4) } else { LINE },
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// An episode airing: when, which, the anime (aired ones dimmed).
    fn airing_card<'a>(&'a self, a: &'a Airing, aired: bool) -> Element<'a, Message> {
        let m = &a.media;
        let status = self.list_status(m);
        let mut info = column![
            text(when(a.at, "%H:%M"))
                .size(11)
                .font(SEMI)
                .color(if aired { FAINT } else { ACCENT }),
            text(clip(m.title(), 40))
                .size(12)
                .font(MEDIUM)
                .color(if aired { MUTED } else { TEXT }),
        ]
        .spacing(2)
        .width(Fill);
        let mut line = row![
            text(match m.episodes {
                Some(n) => format!("Ep {} of {n}", a.episode),
                None => format!("Ep {}", a.episode),
            })
            .size(11)
            .color(FAINT)
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(s) = status {
            line = line.push(dot(status_color(s)));
        }
        info = info.push(line);
        let card = container(
            row![
                picture(self.images.get(m.cover.as_deref()), 36.0, 52.0, 6.0),
                info
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(6)
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(fill_color(if aired { Color::TRANSPARENT } else { SURFACE })),
            border: rounded(8.0),
            ..Default::default()
        });
        mouse_area(card)
            .on_press(act(Action::OpenMedia(
                Box::new(m.clone()),
                Origin::Calendar,
            )))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    }
}
