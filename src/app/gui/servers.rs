//! The library on your servers, in the GUI: setting up Jellyfin, Sonarr
//! and Radarr; the library page's line about them; and a series' place on
//! them.

use iced::widget::{Column, column, row, space, text};
use iced::{Alignment, Element, Fill};

use super::series::{open_url, with_tip};
use super::{Gui, Message, act, edit};
use crate::app::Action;
use crate::app::discover_ui::Loaded;
use crate::app::servers_ui::ServersAction;
use crate::library::{ArrRef, Remote};
use crate::servers::arr::{Kind, Profile, RootFolder};

use super::style::*;

fn servers(a: ServersAction) -> Message {
    act(Action::Servers(a))
}

/// "4 min ago".
pub(super) fn ago(t: std::time::Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

impl Gui {
    /// Settings: Jellyfin, Sonarr and Radarr.
    pub(super) fn servers_settings(&self) -> Element<'_, Message> {
        let s = &self.app.servers;
        let mut c = column![
            weak(
                "Sonarr (series) and Radarr (films) keep your anime and download them with \
                 their own download client: choosing a release here hands it to them. Jellyfin \
                 streams what's there and remembers what you've watched. Anime are matched to \
                 AniList through the community anime-lists mapping, so each season (or part of \
                 a split-cour season) is its own anime with its own episode numbers."
            ),
            self.jellyfin_settings(),
            self.arr_settings(Kind::Sonarr),
            self.arr_settings(Kind::Radarr),
        ]
        .spacing(18);
        let mut status = row![].spacing(10).align_y(Alignment::Center);
        status = status.push(if s.refreshing {
            weak("Reading your servers…")
        } else if let Some(t) = s.refreshed {
            weak(format!("Read {}", ago(t)))
        } else if s.config.any() {
            weak("Not read yet")
        } else {
            weak("Set up Sonarr or Radarr to see your library")
        });
        status = status.push(small(
            "Read again",
            (s.config.any() && !s.refreshing).then(|| servers(ServersAction::Refresh)),
        ));
        c = c.push(status);
        for p in &s.problems {
            c = c.push(error_text(p.clone()));
        }
        c.into()
    }

    fn jellyfin_settings(&self) -> Element<'_, Message> {
        let s = &self.app.servers;
        let j = &s.config.jellyfin;
        let head = column![
            text("Jellyfin").size(15).font(SEMI).color(TEXT),
            weak("Plays your library here, and keeps where you are in each episode."),
        ]
        .spacing(2);
        if j.signed_in() {
            return column![
                head,
                row![
                    dot(GREEN),
                    text(format!(
                        "Signed in as {} on {}",
                        j.username,
                        j.server_name.as_deref().unwrap_or("Jellyfin")
                    ))
                    .size(13)
                    .color(TEXT),
                    weak(crate::servers::base(&j.url)),
                    space::horizontal(),
                    small("Sign out", Some(servers(ServersAction::SignOut))),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            ]
            .spacing(10)
            .into();
        }
        let mut c = column![
            head,
            row![
                field("Address, like http://nas:8096", &j.url)
                    .id("jellyfin-url")
                    .on_input(|v| edit(move |g| g.app.servers.config.jellyfin.url = v.clone()))
                    .width(Fill),
                field("Username", &j.username)
                    .id("jellyfin-user")
                    .on_input(|v| edit(move |g| g.app.servers.config.jellyfin.username = v.clone()))
                    .width(200),
                field("Password", &s.password)
                    .id("jellyfin-password")
                    .secure(true)
                    .on_input(|v| edit(move |g| g.app.servers.password = v.clone()))
                    .on_submit(servers(ServersAction::SignIn))
                    .width(200),
                primary(
                    if s.signing_in {
                        "Signing in…"
                    } else {
                        "Sign in"
                    },
                    (!s.signing_in).then(|| servers(ServersAction::SignIn)),
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            weak("The password isn't kept: Jellyfin gives the app a sign-in of its own."),
        ]
        .spacing(10);
        if let Some(e) = &s.jellyfin_error {
            c = c.push(error_text(e.clone()));
        }
        c.into()
    }

    fn arr_settings(&self, kind: Kind) -> Element<'_, Message> {
        let s = &self.app.servers;
        let account = match kind {
            Kind::Sonarr => &s.config.sonarr,
            Kind::Radarr => &s.config.radarr,
        };
        let (what, port) = match kind {
            Kind::Sonarr => ("Series: where they're kept, and downloading them.", 8989),
            Kind::Radarr => ("Films: where they're kept, and downloading them.", 7878),
        };
        let id = |f: &str| format!("{}-{f}", kind.name().to_lowercase());
        let about = s.about.get(&kind);
        let mut c = column![
            column![
                text(kind.name()).size(15).font(SEMI).color(TEXT),
                weak(what)
            ]
            .spacing(2),
            row![
                field(&format!("Address, like http://nas:{port}"), &account.url)
                    .id(id("url"))
                    .on_input(move |v| edit(move |g| account_mut(g, kind).url = v.clone()))
                    .width(Fill),
                field("API key (its Settings → General)", &account.api_key)
                    .id(id("key"))
                    .secure(true)
                    .on_input(move |v| edit(move |g| account_mut(g, kind).api_key = v.clone()))
                    .on_submit(servers(ServersAction::Check(kind)))
                    .width(300),
                secondary(
                    if matches!(about, Some(Loaded::Loading)) {
                        "Checking…"
                    } else {
                        "Check"
                    },
                    (!matches!(about, Some(Loaded::Loading)))
                        .then(|| servers(ServersAction::Check(kind))),
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);
        match about {
            Some(Loaded::Failed(e)) => c = c.push(error_text(e.clone())),
            Some(Loaded::Ready(about)) => {
                let profile = about
                    .profiles
                    .iter()
                    .find(|p| Some(p.id) == account.quality_profile)
                    .cloned();
                let folder = about
                    .folders
                    .iter()
                    .find(|f| Some(&f.path) == account.root_folder.as_ref())
                    .cloned();
                c = c.push(
                    row![
                        dot(GREEN),
                        text(format!("{} {}", kind.name(), about.version))
                            .size(13)
                            .color(TEXT),
                        space::horizontal(),
                        weak("Anime are added with"),
                        dropdown(about.profiles.clone(), profile, move |p: Profile| {
                            edit(move |g| set_profile(g, kind, p.id))
                        }),
                        weak("into"),
                        dropdown(about.folders.clone(), folder, move |f: RootFolder| {
                            edit(move |g| set_folder(g, kind, f.path.clone()))
                        }),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            _ if account.is_set() => {
                let chosen = account.quality_profile.is_some() && account.root_folder.is_some();
                c = c.push(weak(if chosen {
                    "Check it to change what anime are added with."
                } else {
                    "Check it to choose what anime are added with."
                }));
            }
            _ => {}
        }
        c.into()
    }

    /// The library page's line about the servers.
    pub(super) fn servers_line(&self) -> Element<'_, Message> {
        let s = &self.app.servers;
        let c = &s.config;
        let mut names = Vec::new();
        if c.sonarr.is_set() {
            names.push("Sonarr");
        }
        if c.radarr.is_set() {
            names.push("Radarr");
        }
        if c.jellyfin.signed_in() {
            names.push("Jellyfin");
        }
        let when = if s.refreshing {
            "reading…".to_owned()
        } else {
            s.refreshed.map(ago).unwrap_or_default()
        };
        let mut line = row![
            weak("On"),
            text(if names.is_empty() {
                "no servers yet".to_owned()
            } else {
                names.join(" · ")
            })
            .size(13)
            .font(SEMI)
            .color(TEXT),
            weak(when),
            link(
                "Read again",
                (c.any() && !s.refreshing).then(|| servers(ServersAction::Refresh))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if !s.problems.is_empty() {
            line = line.push(with_tip(
                text("⚠").size(14).color(GOLD).into(),
                s.problems.join("\n"),
            ));
        }
        line.into()
    }

    /// The library page when the servers have nothing (yet).
    pub(super) fn servers_empty(&self) -> Element<'_, Message> {
        let s = &self.app.servers;
        let (title, note) = if !s.config.any() {
            (
                "Your servers aren't set up yet",
                "Add Sonarr and Radarr (and Jellyfin, to watch) in Settings.",
            )
        } else if s.refreshing || s.refreshed.is_none() {
            ("Reading your servers…", "")
        } else {
            (
                "No anime on your servers yet",
                "Anime you add go to Sonarr or Radarr; only anime AniList knows of show here.",
            )
        };
        let mut c = column![text(title).size(22).font(BOLD).color(TEXT), weak(note)]
            .spacing(8)
            .align_x(Alignment::Center);
        for p in &s.problems {
            c = c.push(error_text(p.clone()));
        }
        if !s.config.any() {
            c = c.push(gap(8.0)).push(primary(
                "Settings",
                Some(edit(|g| g.app.page = crate::app::Page::Settings)),
            ));
        }
        c.into()
    }

    /// A series' place on the servers (its page's last box).
    pub(super) fn remote_box<'a>(&'a self, id: u64, r: &'a Remote) -> Element<'a, Message> {
        let s = &self.app.servers;
        let (kind, what) = match r.arr {
            ArrRef::Sonarr { season, offset, .. } => (
                Kind::Sonarr,
                if offset > 0 {
                    format!("Season {season}, from its episode {}", offset + 1)
                } else {
                    format!("Season {season}")
                },
            ),
            ArrRef::Radarr { .. } => (Kind::Radarr, "Film".to_owned()),
        };
        let account = match kind {
            Kind::Sonarr => &s.config.sonarr,
            Kind::Radarr => &s.config.radarr,
        };
        let arr = crate::servers::arr::Arr::new(kind, account, &reqwest::Client::new());
        let mut links = row![link(
            format!("Open in {} ↗", kind.name()),
            (!r.slug.is_empty()).then(|| open_url(arr.page(&r.slug)))
        )]
        .spacing(14);
        if let (Some(item), Some(j)) = (&r.jellyfin, self.app.jellyfin()) {
            links = links.push(link("Open in Jellyfin ↗", Some(open_url(j.page(item)))));
        }
        let have = r.episodes.values().filter(|e| e.file.is_some()).count();
        let mut c: Column<'_, Message> = column![
            row![
                text(format!("On {}", kind.name()))
                    .size(15)
                    .font(SEMI)
                    .color(GREEN),
                weak(format!(
                    "{what} · {} · {have} of {} there",
                    if r.monitored {
                        "watched for new episodes"
                    } else {
                        "not watched for"
                    },
                    r.episodes.len()
                )),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(&r.path).size(12).color(FAINT),
            links,
        ]
        .spacing(8);
        if r.jellyfin.is_none() && self.app.servers.config.jellyfin.signed_in() {
            c = c.push(weak(
                "Jellyfin doesn't have it yet (it shows up once Jellyfin has scanned it).",
            ));
        }
        let missing = r.episodes.values().filter(|e| e.file.is_none()).count();
        if missing > 0 && kind == Kind::Sonarr || have == 0 && kind == Kind::Radarr {
            c = c.push(small(
                match kind {
                    Kind::Sonarr => format!("Have Sonarr search for the {missing} missing"),
                    Kind::Radarr => "Have Radarr search for it".into(),
                },
                (!s.busy.contains(&id)).then(|| {
                    servers(ServersAction::Search {
                        id,
                        episodes: Vec::new(),
                    })
                }),
            ));
        }
        if let Some(series) = self.app.library.find(id) {
            c = c
                .push(super::library::rule())
                .push(self.series_languages(series));
        }
        card(c)
    }
}

/// Sonarr's or Radarr's account, being changed (what was read of it goes).
fn account_mut(g: &mut Gui, kind: Kind) -> &mut crate::servers::ArrAccount {
    g.app.servers.about.remove(&kind);
    match kind {
        Kind::Sonarr => &mut g.app.servers.config.sonarr,
        Kind::Radarr => &mut g.app.servers.config.radarr,
    }
}

fn set_profile(g: &mut Gui, kind: Kind, id: u64) {
    match kind {
        Kind::Sonarr => g.app.servers.config.sonarr.quality_profile = Some(id),
        Kind::Radarr => g.app.servers.config.radarr.quality_profile = Some(id),
    }
}

fn set_folder(g: &mut Gui, kind: Kind, path: String) {
    match kind {
        Kind::Sonarr => g.app.servers.config.sonarr.root_folder = Some(path),
        Kind::Radarr => g.app.servers.config.radarr.root_folder = Some(path),
    }
}
