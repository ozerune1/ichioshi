//! A phone's layout (windows narrower than `style::COMPACT_BELOW`): a bar on
//! top with the page's name and a way back, four tabs along the bottom
//! (Library, Discover, Downloads, More), the settings as a list to tap
//! into, and Discover with its calendar.

use iced::widget::{Column, Row, button, column, container, row, rule, space, text};
use iced::{Alignment, Color, Element, Fill};

use super::style::*;
use super::{Gui, Message, act, edit};
use crate::app::{Action, Origin, Page};

/// The settings' groups: each a page of its own on a phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Section {
    Library,
    Languages,
    Downloading,
    Usenet,
    Vpn,
    Lists,
    Drive,
    Player,
}

impl Section {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Languages => "Languages",
            Self::Downloading => "Downloading",
            Self::Usenet => "Usenet",
            Self::Vpn => "VPN",
            Self::Lists => "Lists",
            Self::Drive => "Google Drive",
            Self::Player => "Player",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Library => "▦",
            Self::Languages => "A",
            Self::Downloading => "↓",
            Self::Usenet => "≡",
            Self::Vpn => "◷",
            Self::Lists => "✓",
            Self::Drive => "↑",
            Self::Player => "▶",
        }
    }
}

/// The tabs along the bottom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Discover,
    Downloads,
    More,
}

impl Gui {
    /// The page in a phone's frame: the bar on top, the tabs below.
    pub(super) fn phone_frame<'a>(&'a self, page: Element<'a, Message>) -> Element<'a, Message> {
        // The keyboard up (more than a gesture bar covered): no tabs, for
        // room to see what's typed.
        let typing = self.bottom_clear > 120.0;
        let mut c = column![
            self.top_bar(),
            container(page).width(Fill).height(Fill).style(|_| fill(BG)),
        ];
        if !typing {
            c = c.push(self.tab_bar());
        }
        // What Android's gesture bar or keyboard covers.
        c.push(
            container(space().height(self.bottom_clear))
                .width(Fill)
                .style(|_| fill(SIDEBAR)),
        )
        .into()
    }

    fn tab(&self) -> Tab {
        let in_library = |id: u64| self.app.library.find(id).is_some();
        match &self.app.page {
            Page::Home | Page::Remux(_) => Tab::Library,
            Page::Discover | Page::Calendar | Page::Results | Page::Files(_) => Tab::Discover,
            Page::Anime(ap) => {
                let found = matches!(
                    ap.origin,
                    Origin::Discover | Origin::Calendar | Origin::Results
                );
                if found && !in_library(ap.anime.anilist_id) {
                    Tab::Discover
                } else {
                    Tab::Library
                }
            }
            Page::Downloads => Tab::Downloads,
            Page::Playlist(_) | Page::Settings => Tab::More,
        }
    }

    /// The page's name, a way back from pages deeper in, and search.
    fn top_bar(&self) -> Element<'_, Message> {
        let section = self.settings_section;
        let (title, back): (String, Option<Message>) = match &self.app.page {
            Page::Home => ("Library".into(), None),
            Page::Discover | Page::Calendar => ("Discover".into(), None),
            Page::Results => ("Search".into(), None),
            Page::Downloads => ("Downloads".into(), None),
            Page::Settings => match section {
                None => ("More".into(), None),
                Some(s) => (s.name().into(), Some(edit(|g| g.settings_section = None))),
            },
            Page::Playlist(_) => (
                "Playlist".into(),
                Some(edit(|g| g.app.page = Page::Settings)),
            ),
            Page::Anime(ap) => (ap.anime.title.clone(), Some(act(Action::Back))),
            Page::Files(_) => ("Choose files".into(), Some(act(Action::Back))),
            Page::Remux(_) => ("Remux".into(), Some(act(Action::Back))),
        };
        let searchable = matches!(self.app.page, Page::Home | Page::Discover | Page::Calendar);
        let mut bar = Row::new().spacing(4).align_y(Alignment::Center);
        match back {
            Some(to) => bar = bar.push(icon_button("←", to)),
            None => bar = bar.push(space().width(12)),
        }
        // One line: as much of the name as fits.
        let room = ((window_width() - 110.0) / 11.0).max(12.0) as usize;
        bar = bar.push(
            text(clip(&title, room))
                .size(20)
                .font(BOLD)
                .color(TEXT)
                .width(Fill),
        );
        if searchable {
            bar = bar.push(icon_button(
                "⌕",
                edit(|g| {
                    g.app.cancel_listing();
                    g.app.page = Page::Results;
                }),
            ));
        }
        container(bar.padding([0, 4]))
            .height(56)
            .width(Fill)
            .align_y(Alignment::Center)
            .style(|_| fill(BG))
            .into()
    }

    /// Library, Discover, Downloads and More, along the bottom.
    pub(super) fn tab_bar(&self) -> Element<'_, Message> {
        let now = self.tab();
        let active = self.app.active_downloads();
        let tabs = [
            (Tab::Library, "▦", "Library", act(Action::GoLibrary)),
            (Tab::Discover, "✦", "Discover", act(Action::GoDiscover)),
            (Tab::Downloads, "↓", "Downloads", act(Action::GoDownloads)),
            (
                Tab::More,
                "☰",
                "More",
                edit(|g| {
                    g.app.cancel_listing();
                    g.app.page = Page::Settings;
                    g.settings_section = None;
                }),
            ),
        ];
        let tabs = tabs.into_iter().map(|(tab, icon, name, to)| {
            let on = tab == now;
            let color = if on { TEXT } else { MUTED };
            let mut top = row![text(icon).size(18).color(if on { ACCENT } else { MUTED })]
                .spacing(4)
                .align_y(Alignment::Center);
            if tab == Tab::Downloads && active > 0 {
                top = top.push(
                    container(text(active.to_string()).size(10).font(SEMI).color(TEXT))
                        .padding([0, 5])
                        .style(|_| pill(ACCENT.scale_alpha(0.6))),
                );
            }
            let pillbox = container(top).padding([4, 18]).style(move |_| {
                if on {
                    pill(ACCENT.scale_alpha(0.18))
                } else {
                    container::Style::default()
                }
            });
            button(
                column![
                    pillbox,
                    text(name)
                        .size(12)
                        .font(if on { SEMI } else { MEDIUM })
                        .color(color),
                ]
                .spacing(4)
                .align_x(Alignment::Center),
            )
            .width(Fill)
            .padding([8, 0])
            .on_press(to)
            .style(|_, _| button::Style {
                background: None,
                text_color: TEXT,
                ..button::Style::default()
            })
            .into()
        });
        container(column![
            rule::horizontal(1).style(|_| rule::Style {
                color: LINE,
                radius: 0.0.into(),
                fill_mode: rule::FillMode::Full,
                snap: true,
            }),
            Row::with_children(tabs)
                .align_y(Alignment::Center)
                .padding([2, 4]),
        ])
        .width(Fill)
        .style(|_| fill(SIDEBAR))
        .into()
    }

    /// Discover's two halves: AniList's shelves, and the calendar.
    pub(super) fn discover_switch(&self, calendar: bool) -> Element<'_, Message> {
        let half = |name: &'static str, on: bool, to: Message| {
            button(
                container(
                    text(name)
                        .size(14)
                        .font(SEMI)
                        .color(if on { TEXT } else { MUTED }),
                )
                .center_x(Fill),
            )
            .width(Fill)
            .padding([8, 0])
            .on_press(to)
            .style(move |_, _| button::Style {
                background: on.then(|| fill_color(ACCENT.scale_alpha(0.22))),
                text_color: TEXT,
                border: rounded(9.0),
                ..button::Style::default()
            })
        };
        container(
            row![
                half("Browse", !calendar, act(Action::GoDiscover)),
                half("Calendar", calendar, act(Action::GoCalendar)),
            ]
            .spacing(4),
        )
        .padding(4)
        .width(Fill)
        .style(|_| container::Style {
            background: Some(fill_color(SURFACE)),
            border: rounded(12.0),
            ..Default::default()
        })
        .into()
    }

    /// More: how things are, the playlist, and the settings' groups.
    pub(super) fn more_page(&self) -> Element<'_, Message> {
        let a = &self.app;
        let (_, status) = self.status_box();
        let playlist = a.playlist.items.len();
        let places = list_box(vec![list_row(
            "≡",
            "Playlist",
            match playlist {
                0 => "Episodes to watch in order, or export".to_owned(),
                1 => "1 episode".to_owned(),
                n => format!("{n} episodes"),
            },
            act(Action::OpenPlaylist),
        )]);
        let lists = super::library::signed_in_as(&a.tracking.config);
        let mut sections = vec![
            (
                Section::Library,
                if a.servers_mode() {
                    "Jellyfin, Sonarr and Radarr".to_owned()
                } else {
                    a.library_dir().display().to_string()
                },
            ),
            (Section::Languages, a.settings.prefs.summary()),
            (
                Section::Downloading,
                match (a.debrid.service(), a.usenet.active()) {
                    (Some(s), true) => format!("Usenet first, then {s}"),
                    (Some(s), false) => format!("Through {s}"),
                    (None, true) => "Usenet first, then BitTorrent".to_owned(),
                    (None, false) => "BitTorrent; nothing is ever seeded".to_owned(),
                },
            ),
            (
                Section::Usenet,
                if a.usenet.active() {
                    "On".to_owned()
                } else {
                    "Providers and indexers".to_owned()
                },
            ),
        ];
        if !a.debrid.config.enabled {
            sections.push((
                Section::Vpn,
                if a.vpn.config.enabled { "On" } else { "Off" }.to_owned(),
            ));
        }
        sections.push((
            Section::Lists,
            if lists.is_empty() {
                "AniList and MyAnimeList".to_owned()
            } else {
                lists.join(", ")
            },
        ));
        if DESKTOP {
            sections.push((Section::Drive, "Upload playlist exports".to_owned()));
            sections.push((Section::Player, "Skipping, autoplay".to_owned()));
        }
        let settings = list_box(
            sections
                .into_iter()
                .map(|(s, note)| {
                    list_row(
                        s.icon(),
                        s.name(),
                        note,
                        edit(move |g| g.settings_section = Some(s)),
                    )
                })
                .collect(),
        );
        page(
            column![
                status,
                gap(18.0),
                places,
                gap(22.0),
                label("Settings"),
                gap(8.0),
                settings,
            ]
            .width(Fill),
        )
    }

    /// Back (Android's back button): out of a dialog, a settings group, a
    /// page; from the library, the app goes to the background.
    pub(super) fn go_back(&mut self) -> iced::Task<Message> {
        let a = &self.app;
        let dialog = a.confirm_delete.is_some()
            || a.pages.languages.is_some()
            || a.usenet.dialog.is_some()
            || a.drive.browser.is_some()
            || a.discussion.is_some()
            || a.import.is_some();
        if dialog {
            self.app.close_dialogs();
            return iced::Task::none();
        }
        if self.settings_section.take().is_some() {
            return iced::Task::none();
        }
        match &self.app.page {
            Page::Anime(_) | Page::Files(_) | Page::Remux(_) => self.act(Action::Back),
            Page::Playlist(_) => {
                self.app.page = Page::Settings;
                iced::Task::none()
            }
            Page::Calendar => self.act(Action::GoDiscover),
            Page::Home => {
                #[cfg(target_os = "android")]
                crate::android::to_background();
                iced::Task::none()
            }
            _ => self.act(Action::GoLibrary),
        }
    }
}

/// A square button with a symbol (the top bar's).
fn icon_button<'a>(icon: &'a str, to: Message) -> Element<'a, Message> {
    button(
        container(text(icon).size(20).color(TEXT))
            .center_x(40)
            .center_y(40),
    )
    .padding(0)
    .on_press(to)
    .style(|_, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then(|| fill_color(SURFACE2)),
        text_color: TEXT,
        border: rounded(20.0),
        ..button::Style::default()
    })
    .into()
}

/// Rows in one box, with lines between.
fn list_box(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    let mut c = Column::new();
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        c = c.push(r);
        if i + 1 < n {
            c = c.push(container(rule::horizontal(1)).padding([0, 14]));
        }
    }
    container(c)
        .width(Fill)
        .style(|_| card_style(SURFACE))
        .into()
}

/// A row to tap: what it is, a word on how it is, and where it goes.
fn list_row<'a>(icon: &'a str, name: &'a str, note: String, to: Message) -> Element<'a, Message> {
    button(
        row![
            text(icon).size(16).color(ACCENT).width(26),
            column![
                text(name).size(15).font(SEMI).color(TEXT),
                text(note).size(12).color(MUTED),
            ]
            .spacing(2)
            .width(Fill),
            text("›").size(22).color(FAINT),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([12, 14])
    .on_press(to)
    .style(|_, status| button::Style {
        background: matches!(status, button::Status::Pressed)
            .then(|| fill_color(Color::WHITE.scale_alpha(0.05))),
        text_color: TEXT,
        ..button::Style::default()
    })
    .into()
}
