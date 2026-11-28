//! Discover: AniList's trending, seasonal, popular and top-rated anime,
//! narrowed by genre and format.

use iced::widget::{Column, Row, column, container, mouse_area, space, stack, text};
use iced::{Color, Element};

use super::{Gui, Message, act, edit};
use crate::app::discover_ui::{LIST_CHOICES, Loaded, list_choice_name};
use crate::app::{Action, Origin};
use crate::browse::{self, FORMATS, Filter, GENRES, Media, Season, Shelf, Sort};
use crate::tracking::{Service, Status};

use super::style::*;

/// A choice in a dropdown: a value (or none) and what it's called.
#[derive(Debug, Clone, PartialEq)]
struct Pick {
    value: Option<String>,
    label: String,
}

impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// The earliest year Discover offers.
const FIRST_YEAR: i32 = 1960;

/// When an episode airs, here: "Fri 23:30".
pub(super) fn when(at: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
}

impl Gui {
    /// Your list's status for an anime (AniList's, else MyAnimeList's).
    pub(super) fn list_status(&self, m: &Media) -> Option<Status> {
        let t = &self.app.tracking;
        t.lists
            .get(&Service::AniList)
            .and_then(|l| l.get(&m.id))
            .or_else(|| m.id_mal.and_then(|k| t.lists.get(&Service::Mal)?.get(&k)))
            .map(|e| e.status)
    }

    pub(super) fn discover_page(&self) -> Element<'_, Message> {
        let d = &self.app.discover;
        let f = &d.filter;
        let today = chrono::Local::now().date_naive();
        let season = browse::season_of(today);
        let small = compact();

        // Ready-made views: a sort and a season.
        let mut shelves = Row::new().spacing(8);
        for shelf in Shelf::ALL {
            let label = match shelf {
                Shelf::ThisSeason => format!("This season · {}", browse::season_name(season)),
                Shelf::NextSeason => format!(
                    "Next season · {}",
                    browse::season_name(browse::next_season(season))
                ),
                other => other.name().to_owned(),
            };
            shelves = shelves.push(chip(
                label,
                shelf.is(f, today),
                Some(edit(move |g| g.app.discover.show(shelf))),
            ));
        }

        // Sort, season, year, format.
        let sorts: Vec<Pick> = Sort::ALL
            .iter()
            .map(|s| Pick {
                value: Some(format!("{s:?}")),
                label: s.name().to_owned(),
            })
            .collect();
        let sort_now = sorts
            .iter()
            .find(|p| p.value.as_deref() == Some(&format!("{:?}", f.sort)))
            .cloned();
        let mut seasons = vec![Pick {
            value: None,
            label: "Any season".into(),
        }];
        seasons.extend(Season::ALL.iter().map(|s| Pick {
            value: Some(s.anilist().to_owned()),
            label: s.name().to_owned(),
        }));
        let season_now = seasons
            .iter()
            .find(|p| p.value.as_deref() == f.season.map(Season::anilist))
            .cloned();
        let mut years = vec![Pick {
            value: None,
            label: "Any year".into(),
        }];
        years.extend((FIRST_YEAR..=season.1 + 1).rev().map(|y| Pick {
            value: Some(y.to_string()),
            label: y.to_string(),
        }));
        let year_now = years
            .iter()
            .find(|p| p.value == f.year.map(|y| y.to_string()))
            .cloned();
        let mut formats = vec![Pick {
            value: None,
            label: "Any format".into(),
        }];
        formats.extend(FORMATS.iter().map(|(label, value)| Pick {
            value: Some((*value).to_owned()),
            label: (*label).to_owned(),
        }));
        let format_now = formats.iter().find(|p| p.value == f.format).cloned();
        let mut controls = Row::new()
            .spacing(10)
            .push(dropdown(sorts, sort_now, |p: Pick| {
                edit(move |g| {
                    if let Some(sort) = Sort::ALL
                        .into_iter()
                        .find(|s| p.value.as_deref() == Some(&format!("{s:?}")))
                    {
                        g.app.discover.set_sort(sort);
                    }
                })
            }))
            .push(dropdown(seasons, season_now, |p: Pick| {
                edit(move |g| {
                    g.app.discover.filter.season = p.value.as_deref().map(Season::from_anilist)
                })
            }))
            .push(dropdown(years, year_now, |p: Pick| {
                edit(move |g| {
                    g.app.discover.filter.year = p.value.as_deref().and_then(|y| y.parse().ok())
                })
            }))
            .push(dropdown(formats, format_now, |p: Pick| {
                edit(move |g| g.app.discover.filter.format = p.value.clone())
            }));
        if f.sort == Sort::Random {
            controls = controls.push(secondary(
                "Shuffle again",
                Some(edit(|g| g.app.discover.reshuffle())),
            ));
        }

        // Genres (all of them), tags (chosen, then found by typing), your
        // lists' statuses: each tapped to want it, again to not, again off.
        let adult = self.app.adult_on();
        let mut genres = Row::new().spacing(8);
        for g in GENRES
            .iter()
            .copied()
            .chain(adult.then_some(browse::ADULT_GENRE))
        {
            genres = genres.push(tri_chip(
                g,
                f.tags.get(g).copied(),
                edit(move |gui| gui.app.discover.cycle_tag(g)),
            ));
        }
        let mut tags = Row::new().spacing(8).align_y(iced::Alignment::Center);
        for (name, choice) in f.tags.iter().filter(|(n, _)| !Filter::is_genre(n)) {
            let n = name.clone();
            tags = tags.push(tri_chip(
                name,
                Some(*choice),
                edit(move |g| g.app.discover.cycle_tag(&n)),
            ));
        }
        tags = tags.push(
            field("Find a tag (e.g. Time Skip)…", &d.tag_query)
                .id("discover-tags")
                .on_input(|q| edit(move |g| g.app.discover.tag_query = q.clone()))
                .width(if small { 200 } else { 260 }),
        );
        let mut found = Row::new().spacing(8);
        let matches = d.tag_matches(adult);
        for t in &matches {
            let n = t.name.clone();
            found = found.push(tri_chip(
                &t.name,
                None,
                edit(move |g| {
                    g.app.discover.cycle_tag(&n);
                    g.app.discover.tag_query.clear();
                }),
            ));
        }
        let tag_note: Option<Element<'_, Message>> = match &d.tags {
            Some((Loaded::Failed(e), _)) => {
                Some(error_text(format!("Couldn't read AniList's tags: {e}")).into())
            }
            _ if !d.tag_query.trim().is_empty() && matches.is_empty() => {
                Some(weak("No tag by that name.").into())
            }
            _ => None,
        };
        let mut lists = Row::new().spacing(8);
        for status in LIST_CHOICES {
            lists = lists.push(tri_chip(
                list_choice_name(status),
                d.list_choice(status),
                edit(move |g| g.app.discover.cycle_list(status)),
            ));
        }
        let lists_note = (!self.app.tracking.any())
            .then(|| weak("Sign in to AniList or MyAnimeList (Settings) to choose by your lists."));

        let mut choices = column![
            label("Genres"),
            genres.wrap().vertical_spacing(8),
            gap(6.0),
            label("Tags"),
            tags.wrap().vertical_spacing(8),
        ]
        .spacing(8);
        if !matches.is_empty() {
            choices = choices.push(found.wrap().vertical_spacing(8));
        }
        if let Some(note) = tag_note {
            choices = choices.push(note);
        }
        choices = choices
            .push(gap(6.0))
            .push(label("Your lists"))
            .push(lists.wrap().vertical_spacing(8));
        if let Some(note) = lists_note {
            choices = choices.push(note);
        }
        choices = choices.push(weak(
            "Tap a choice to want it, again for anime without it, again to let it go.",
        ));
        if d.narrowed() {
            choices = choices.push(secondary(
                "Clear choices",
                Some(edit(|g| g.app.discover.clear_choices())),
            ));
        }

        let body: Element<'_, Message> = match d.shelves.get(f).map(|(l, _)| l) {
            None | Some(Loaded::Loading) => weak("Asking AniList…").into(),
            Some(Loaded::Failed(e)) => column![
                error_text(format!("Couldn't read AniList's list: {e}")),
                secondary(
                    "Try again",
                    Some(edit(|g| {
                        let filter = g.app.discover.filter.clone();
                        g.app.discover.shelves.remove(&filter);
                    }))
                ),
            ]
            .spacing(10)
            .into(),
            Some(Loaded::Ready(shelf)) if shelf.items.is_empty() => {
                weak("Nothing on AniList for that.").into()
            }
            Some(Loaded::Ready(shelf)) => {
                let grid = Row::with_children(
                    shelf
                        .items
                        .iter()
                        .map(|m| self.media_card(m, Origin::Discover)),
                )
                .spacing(if small { POSTER_GAP } else { 24.0 })
                .wrap()
                .vertical_spacing(if small { 18 } else { 28 });
                let end = list_end(&shelf.more, shelf.next, act(Action::MoreDiscover));
                column![grid].push(end).into()
            }
        };
        // A phone's: Browse or Calendar on top, the shelves a strip to
        // scroll sideways.
        if small {
            return page(column![
                self.discover_switch(false),
                gap(14.0),
                iced::widget::scrollable(shelves).direction(
                    iced::widget::scrollable::Direction::Horizontal(
                        iced::widget::scrollable::Scrollbar::new()
                            .width(0)
                            .scroller_width(0)
                    )
                ),
                gap(10.0),
                controls.wrap().vertical_spacing(8),
                gap(12.0),
                choices,
                gap(16.0),
                body,
            ]);
        }
        page(column![
            page_title(
                "Discover",
                "Anime on AniList: open one to see its releases, add it or download it.".into()
            ),
            gap(18.0),
            shelves.wrap().vertical_spacing(8),
            gap(12.0),
            controls.wrap().vertical_spacing(8),
            gap(16.0),
            choices,
            gap(24.0),
            body,
        ])
    }

    /// A poster: AniList's score, format, genres, what's airing; whether
    /// it's on your lists or in the library.
    pub(super) fn media_card<'a>(&'a self, m: &'a Media, origin: Origin) -> Element<'a, Message> {
        self.poster_card(m, act(Action::OpenMedia(Box::new(m.clone()), origin)))
    }

    /// [`Gui::media_card`], doing `on_press` when clicked.
    pub(super) fn poster_card<'a>(
        &'a self,
        m: &'a Media,
        on_press: Message,
    ) -> Element<'a, Message> {
        let (w, h) = poster_size((176.0, 250.0));
        let small = compact();
        let note = if small { 11 } else { 12 };
        let mut badges = Column::new()
            .spacing(6)
            .padding(if small { 6 } else { 10 })
            .width(w)
            .height(h);
        if let Some(status) = self.list_status(m) {
            let color = status_color(status);
            badges = badges.push(
                container(text(status.name()).size(11).font(SEMI).color(Color::WHITE))
                    .padding([3, 9])
                    .style(move |_| pill(color.scale_alpha(0.92))),
            );
        }
        if self.app.library.find(m.id).is_some() {
            badges = badges.push(
                container(text("In library").size(11).font(SEMI).color(Color::WHITE))
                    .padding([3, 9])
                    .style(|_| pill(GREEN.scale_alpha(0.92))),
            );
        }
        if let Some(score) = m.score {
            badges = badges.push(space::vertical()).push(
                container(text(format!("★ {score}%")).size(11).font(SEMI).color(GOLD))
                    .padding([3, 8])
                    .style(|_| pill(Color { a: 0.85, ..BG })),
            );
        }
        let poster = stack![
            picture(self.images.get(m.cover.as_deref()), w, h, 12.0),
            badges
        ];
        let mut facts = Vec::new();
        if let Some(format) = &m.format {
            facts.push(format.replace('_', " "));
        }
        if let Some(n) = m.episodes {
            facts.push(format!("{n} eps"));
        }
        if let (Some(season), Some(year)) = (&m.season, m.year) {
            facts.push(browse::season_name((season.as_str(), year)));
        }
        let mut c = column![
            poster,
            space().height(if small { 6 } else { 10 }),
            text(clip(m.title(), if small { 28 } else { 34 }))
                .size(if small { 13 } else { 14 })
                .font(SEMI)
                .color(TEXT)
                .height(if small { 34 } else { 38 }),
            text(facts.join(" · ")).size(note).color(MUTED),
        ]
        .width(w)
        .spacing(2);
        if let Some((ep, at)) = m.next_airing {
            c = c.push(
                text(if small {
                    format!("Ep {ep} · {}", when(at, "%a %-d %b"))
                } else {
                    format!("Episode {ep} airs {}", when(at, "%a %-d %b, %H:%M"))
                })
                .size(note)
                .color(ACCENT),
            );
        } else if !m.genres.is_empty() {
            c = c.push(
                text(
                    m.genres
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · "),
                )
                .size(note)
                .color(FAINT),
            );
        }
        mouse_area(c)
            .on_press(on_press)
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    }
}
