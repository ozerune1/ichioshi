//! An episode's discussion, read here: its AniList thread and its
//! MyAnimeList topic, with spoilers hidden until asked for.

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

use super::discover::when;
use super::series::open_url;
use super::{Gui, Message, act, edit};
use crate::app::Action;
use crate::app::discover_ui::Loaded;
use crate::app::discuss_ui::Discussion;
use crate::discuss::{Piece, Post, Site, Thread};

use super::style::*;

impl Gui {
    /// The discussion open, if any, over `base`.
    pub(super) fn discussion<'a>(
        &'a self,
        base: Element<'a, Message>,
        d: &'a Discussion,
    ) -> Element<'a, Message> {
        let close = edit(|g| g.app.discussion = None);
        let mut tabs = Row::new().spacing(8);
        for site in [Site::AniList, Site::Mal] {
            let label = match d.threads.get(&site) {
                Some(Loaded::Ready(Some(t))) => format!("{} · {} replies", site.name(), t.replies),
                Some(Loaded::Ready(None)) => format!("{} · none", site.name()),
                _ => site.name().to_owned(),
            };
            tabs = tabs.push(chip(
                label,
                d.site == site,
                Some(edit(move |g| {
                    if let Some(d) = &mut g.app.discussion {
                        d.site = site;
                    }
                })),
            ));
        }
        let head = column![
            row![
                column![
                    text(format!("Episode {} discussion", d.episode))
                        .size(20)
                        .font(BOLD)
                        .color(TEXT),
                    weak(d.title.clone()),
                ]
                .spacing(4)
                .width(Fill),
                secondary("Close", Some(close.clone())),
            ]
            .align_y(Alignment::Center),
            tabs,
        ]
        .spacing(14);
        let body: Element<'_, Message> = match d.threads.get(&d.site) {
            None | Some(Loaded::Loading) => weak(format!(
                "Finding episode {}'s thread on {}…",
                d.episode,
                d.site.name()
            ))
            .into(),
            Some(Loaded::Failed(e)) => error_text(format!("Couldn't read it: {e}")).into(),
            Some(Loaded::Ready(None)) => column![
                body(format!(
                    "{} has no thread for episode {} (none titled with it).",
                    d.site.name(),
                    d.episode
                )),
                link(
                    format!("Look on {} ↗", d.site.name()),
                    Some(open_url(match d.site {
                        Site::AniList => format!("https://anilist.co/anime/{}", d.anilist_id),
                        Site::Mal => match self.app.mal_id(d.anilist_id) {
                            Some(m) =>
                                format!("https://myanimelist.net/forum/?animeid={m}&topic=episode"),
                            None => "https://myanimelist.net/forum/".into(),
                        },
                    }))
                ),
            ]
            .spacing(10)
            .into(),
            Some(Loaded::Ready(Some(thread))) => self.thread(d, thread),
        };
        dialog(base, column![head, gap(16.0), body].into(), 980.0, close)
    }

    fn thread<'a>(&'a self, d: &'a Discussion, t: &'a Thread) -> Element<'a, Message> {
        let mut c = Column::new().spacing(12).push(
            row![
                text(t.title.clone())
                    .size(15)
                    .font(SEMI)
                    .color(TEXT)
                    .width(Fill),
                link(
                    format!("Reply on {} ↗", t.site.name()),
                    Some(open_url(t.url.clone()))
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        for p in &t.posts {
            c = c.push(post(d, t.site, p, 0));
        }
        if t.next.is_some() {
            c = c.push(secondary(
                if d.more.contains(&t.site) {
                    "Loading…"
                } else {
                    "Load more"
                },
                (!d.more.contains(&t.site)).then(|| act(Action::DiscussionMore)),
            ));
        }
        c.into()
    }
}

/// A post: who and when, its text (spoilers hidden until shown), and the
/// replies to it, indented.
fn post<'a>(d: &'a Discussion, site: Site, p: &'a Post, depth: u16) -> Element<'a, Message> {
    let shown = d.shown.contains(&(site, p.id));
    let mut meta = row![
        text(p.author.clone()).size(13).font(SEMI).color(TEXT),
        text(when(p.at, "%-d %b %Y, %H:%M")).size(12).color(FAINT),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if let Some(likes) = p.likes.filter(|n| *n > 0) {
        meta = meta.push(text(format!("♥ {likes}")).size(12).color(MUTED));
    }
    let mut c = column![meta].spacing(8);
    let id = p.id;
    for piece in &p.body {
        c = c.push(match piece {
            Piece::Text(t) => text(t.clone()).size(14).line_height(1.5).color(TEXT).into(),
            Piece::Quote(who, t) => {
                let mut q = column![].spacing(4);
                if let Some(who) = who {
                    q = q.push(
                        text(format!("{who} said:"))
                            .size(12)
                            .font(SEMI)
                            .color(MUTED),
                    );
                }
                container(q.push(text(t.clone()).size(13).line_height(1.45).color(MUTED)))
                    .padding([8, 12])
                    .width(Fill)
                    .style(|_| container::Style {
                        background: Some(fill_color(SURFACE2.scale_alpha(0.6))),
                        border: iced::Border {
                            color: LINE,
                            width: 1.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    })
                    .into()
            }
            Piece::Spoiler(t) if shown => {
                container(text(t.clone()).size(14).line_height(1.5).color(GOLD))
                    .padding([6, 10])
                    .style(|_| card_style(GOLD.scale_alpha(0.06)))
                    .into()
            }
            Piece::Spoiler(_) => small(
                "Show spoiler",
                Some(edit(move |g| {
                    if let Some(d) = &mut g.app.discussion {
                        d.shown.insert((site, id));
                    }
                })),
            ),
        });
    }
    let boxed = container(c)
        .padding([12, 14])
        .width(Fill)
        .style(|_| container::Style {
            background: Some(fill_color(SURFACE)),
            border: iced::Border {
                color: LINE,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        });
    let mut all = column![boxed].spacing(8);
    // Deeper replies stop moving in (the panel isn't wide enough for more).
    let indent = if depth < 3 { 24 } else { 0 };
    for r in &p.replies {
        all = all.push(row![space().width(indent), post(d, site, r, depth + 1)]);
    }
    all.into()
}
