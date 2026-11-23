//! The watch party page: host one or join one; who's there, whether they
//! have the episode, and starting it.

use iced::widget::{Column, column, row, space, text};
use iced::{Alignment, Element, Fill};

use super::style::*;
use super::{Gui, Message, act, edit};
use crate::app::Action;
use crate::app::party_ui::{PartyAction, Role, have_text};
use crate::party::Have;

fn party(a: PartyAction) -> Message {
    act(Action::Party(a))
}

impl Gui {
    pub(super) fn party_page(&self) -> Element<'_, Message> {
        let p = &self.app.party;
        let mut c = column![page_title(
            "Watch party",
            "Watch an episode together, each from your own copy. The host's player leads; \
             it starts once everyone has the episode."
                .into()
        )]
        .spacing(18)
        .max_width(820);
        match &p.role {
            None => {
                let name = column![
                    label("Your name"),
                    field(&self.app.party_name(), &self.app.settings.party_name)
                        .id("party-name")
                        .on_input(|s| party(PartyAction::SetName(s))),
                ]
                .spacing(6);
                let host = column![
                    text("Host a party").size(17).font(SEMI).color(TEXT),
                    weak(
                        "You get a code to send your friends. Then play an episode: it starts for \
                         everyone once they all have it, and your player controls theirs."
                    ),
                    primary(
                        if p.busy {
                            "Starting…"
                        } else {
                            "Start a party"
                        },
                        (!p.busy).then(|| party(PartyAction::Host))
                    ),
                ]
                .spacing(10);
                let code = field("Paste the code your friend sent", &p.code)
                    .id("party-code")
                    .on_input(|s| party(PartyAction::SetCode(s)))
                    .on_submit(party(PartyAction::Join));
                let join = column![
                    text("Join a party").size(17).font(SEMI).color(TEXT),
                    pasteable(code, |s| party(PartyAction::SetCode(s))),
                    primary(
                        if p.busy { "Joining…" } else { "Join" },
                        (!p.busy && !p.code.trim().is_empty()).then(|| party(PartyAction::Join))
                    ),
                ]
                .spacing(10);
                c = c.push(card(name)).push(card(host)).push(card(join));
                if let Some(e) = &p.error {
                    c = c.push(error_text(e.clone()));
                }
            }
            Some(Role::Host(h)) => {
                let code = h.code.clone();
                c = c.push(card(
                    column![
                        text("Your party's code").size(17).font(SEMI).color(TEXT),
                        weak(
                            "Send it to your friends (anyone with it can join): they paste it \
                             on their Watch party page."
                        ),
                        text(clip(&code, 90)).size(12).color(MUTED),
                        row![
                            primary(
                                "Copy the code",
                                Some(edit(move |g| g.copy = Some(code.clone())))
                            ),
                            danger("End the party", Some(party(PartyAction::Leave))),
                        ]
                        .spacing(12),
                    ]
                    .spacing(10),
                ));
                c = c.push(self.party_episode_card());
                c = c.push(self.party_people_card());
            }
            Some(Role::Member(m)) => {
                let host = self.app.party_host_name().unwrap_or("the host".into());
                let mut top = column![
                    text(format!("You're in {host}'s party"))
                        .size(17)
                        .font(SEMI)
                        .color(TEXT),
                    weak(format!(
                        "{host}'s player leads: yours opens, pauses and moves with it. Pausing \
                         asks {host} to pause."
                    )),
                ]
                .spacing(10);
                if !m.following && m.host_playing() {
                    top = top.push(primary("Watch along", Some(party(PartyAction::Follow))));
                }
                top = top.push(secondary(
                    "Leave the party",
                    Some(party(PartyAction::Leave)),
                ));
                c = c.push(card(top));
                c = c.push(self.party_episode_card());
                c = c.push(self.party_people_card());
            }
        }
        page(c)
    }

    /// The episode chosen, and (the host) starting it.
    fn party_episode_card(&self) -> Element<'_, Message> {
        let app = &self.app;
        let host = matches!(app.party.role, Some(Role::Host(_)));
        let mut c = column![text("Watching").size(17).font(SEMI).color(TEXT)].spacing(10);
        let Some(e) = app.party_episode() else {
            c = c.push(weak(if host {
                "Nothing yet: play an episode from your library, and it starts for everyone \
                 once they all have it."
            } else {
                "Nothing yet: the host chooses."
            }));
            return card(c);
        };
        c = c.push(body(app.episode_label(e)));
        match &app.party.role {
            Some(Role::Host(h)) => {
                let waiting = app.party_waiting_for();
                if h.starting {
                    c = c.push(
                        row![
                            weak(format!(
                                "Starts once {} {} it",
                                waiting.join(", "),
                                if waiting.len() == 1 { "has" } else { "have" }
                            )),
                            space::horizontal(),
                            small("Don't start", Some(party(PartyAction::Cancel))),
                        ]
                        .align_y(Alignment::Center),
                    );
                } else if self.player.is_none() {
                    let note = if waiting.is_empty() {
                        "Everyone has it.".to_owned()
                    } else {
                        format!("Waiting for {}.", waiting.join(", "))
                    };
                    c = c.push(
                        row![
                            weak(note),
                            space::horizontal(),
                            primary("Start for everyone", Some(party(PartyAction::Start))),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    );
                }
            }
            Some(Role::Member(m)) => match m.me {
                Have::Ready => {
                    c = c.push(weak(if m.host_playing() {
                        "You have it: it's playing."
                    } else {
                        "You have it: it starts when the host starts it."
                    }))
                }
                Have::Downloading(_) => {
                    c = c.push(weak(format!(
                        "You're getting it: {}",
                        have_text(m.me).to_lowercase()
                    )))
                }
                Have::Missing => {
                    let in_library = app.library.find(e.series).is_some();
                    let id = e.series;
                    c = c.push(
                        row![
                            weak("You don't have it yet: download it from its page."),
                            space::horizontal(),
                            small(
                                "Open its page",
                                in_library.then(|| act(Action::OpenSeries(id)))
                            ),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    );
                }
            },
            None => {}
        }
        for line in app.share_lines() {
            c = c.push(weak(line));
        }
        card(c)
    }

    /// Who's there, and whether they have the episode.
    fn party_people_card(&self) -> Element<'_, Message> {
        let people = self.app.party_people();
        let chosen = self.app.party_episode().is_some();
        let rows = people.into_iter().map(|(id, name, have, you)| {
            let color = match have {
                Have::Ready => GREEN,
                Have::Downloading(_) => ACCENT,
                Have::Missing => MUTED,
            };
            let who = match (you, id == crate::party::HOST) {
                (true, true) => format!("{name} (you, hosting)"),
                (true, false) => format!("{name} (you)"),
                (false, true) => format!("{name} (hosting)"),
                (false, false) => name,
            };
            let mut r = row![dot(color), body(who), space::horizontal()]
                .spacing(10)
                .align_y(Alignment::Center);
            if chosen {
                r = r.push(weak(have_text(have)));
            }
            r.width(Fill).into()
        });
        card(
            column![
                text(format!("People ({})", self.app.party_size().unwrap_or(1)))
                    .size(17)
                    .font(SEMI)
                    .color(TEXT),
                Column::with_children(rows).spacing(10),
            ]
            .spacing(12),
        )
    }
}
