//! Settings: the library, how downloads happen (BitTorrent, a debrid
//! service, Usenet, the VPN), your lists, Google Drive and the player.

use std::path::PathBuf;
use std::time::Instant;

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

use super::library::rule;
use super::series::{open_url, with_tip};
use super::{Gui, Message, act, edit};
use crate::app::Action;
use crate::app::tracking_ui::TrackingAction;
use crate::debrid::Provider as DebridProvider;
use crate::tracking::Service;
use crate::vpn::{self, Fix, Forwarding, Port, Status};

use super::style::*;

/// A titled card of settings.
fn group<'a>(
    title: &'a str,
    note: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    // A phone's group is a page of its own, named in the bar on top.
    if compact() {
        return column![weak(note), content.into()].spacing(16).into();
    }
    card(
        column![
            column![text(title).size(17).font(SEMI).color(TEXT), weak(note)].spacing(4),
            rule(),
            content.into(),
        ]
        .spacing(14),
    )
}

impl Gui {
    pub(super) fn settings_page(&self) -> Element<'_, Message> {
        // A phone's: More, or one group.
        if compact() {
            use super::phone::Section;
            let Some(section) = self.settings_section else {
                return self.more_page();
            };
            return page(match section {
                Section::Library => self.library_settings(),
                Section::Languages => self.language_settings(),
                Section::Downloading => self.download_settings(),
                Section::Usenet => self.usenet_settings(),
                Section::Vpn => self.vpn_settings(),
                Section::Lists => self.list_settings(),
                Section::Drive => group(
                    "Google Drive",
                    "Upload playlist exports to a folder in your Drive.",
                    self.drive_section(None),
                ),
                Section::Player => self.player_settings(),
            });
        }
        let mut c = column![
            page_title(
                "Settings",
                "Nothing is ever seeded: each download stops once your files are complete.".into()
            ),
            gap(10.0),
            self.library_settings(),
            self.language_settings(),
            self.download_settings(),
            self.usenet_settings(),
        ]
        .spacing(18)
        .max_width(980);
        if !self.app.debrid.config.enabled {
            c = c.push(self.vpn_settings());
        }
        c = c.push(self.list_settings());
        if DESKTOP {
            c = c
                .push(group(
                    "Google Drive",
                    "Upload playlist exports to a folder in your Drive.",
                    self.drive_section(None),
                ))
                .push(self.player_settings());
        }
        page(c)
    }

    fn library_settings(&self) -> Element<'_, Message> {
        use crate::app::servers_ui::ServersAction;
        use crate::servers::LibraryMode;
        let s = &self.app.settings;
        let on_servers = self.app.servers_mode();
        let mode = row![
            chip(
                if DESKTOP {
                    "On this computer"
                } else {
                    "On this phone"
                },
                !on_servers,
                Some(act(Action::Servers(ServersAction::SetMode(
                    LibraryMode::Local
                ))))
            ),
            chip(
                "Jellyfin, Sonarr and Radarr",
                on_servers,
                Some(act(Action::Servers(ServersAction::SetMode(
                    LibraryMode::Servers
                ))))
            ),
        ]
        .spacing(8);
        if on_servers {
            return group(
                "Library",
                if DESKTOP {
                    "Your servers keep the library; the one on this computer is kept as it \
                     was, for when you switch back."
                } else {
                    "Your servers keep the library; the one on this phone is kept as it was, \
                     for when you switch back."
                },
                column![mode, self.servers_settings()].spacing(14),
            );
        }
        group(
            "Library",
            "Each series gets its own folder here; downloads go straight into it.",
            column![
                mode,
                row![
                    field("Library folder", &s.library_dir.display().to_string())
                        .on_input(|t| edit(move |g| g.app.settings.library_dir = PathBuf::from(&t)))
                        .width(Fill),
                    desktop_only(secondary("Browse…", Some(act(Action::PickLibraryFolder)))),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                with_tip(
                    check(
                        "Prefer dual-audio releases",
                        s.prefer_dual_audio,
                        |on| edit(move |g| g.app.settings.prefer_dual_audio = on)
                    ),
                    "Between equally good releases (two SeaDex \"best\" picks, or two Nyaa \
                     releases from the same tier), pick the dual-audio one."
                        .into()
                ),
            ]
            .spacing(14),
        )
    }

    fn download_settings(&self) -> Element<'_, Message> {
        let d = &self.app.debrid;
        let enabled = d.config.enabled;
        let mut c = column![
            row![
                chip(
                    if DESKTOP {
                        "BitTorrent, on this computer"
                    } else {
                        "BitTorrent, on this phone"
                    },
                    !enabled,
                    Some(act(Action::DebridSetEnabled(false)))
                ),
                chip(
                    "A debrid service",
                    enabled,
                    Some(act(Action::DebridSetEnabled(true)))
                ),
            ]
            .spacing(8)
        ]
        .spacing(14);
        if !enabled {
            c = c.push(weak(
                "Uploading is turned off: nothing is seeded, and each torrent stops as soon as \
                 the files you picked are complete.",
            ));
            return group("Downloading", "Where releases come from.", c);
        }
        let provider = d.config.provider;
        let mut providers = Row::new().spacing(8);
        for p in DebridProvider::ALL {
            providers = providers.push(chip(
                p.name(),
                p == provider,
                Some(edit(move |g| {
                    g.app.debrid.config.provider = p;
                    g.app.debrid.save();
                    g.app.debrid.account = None;
                    g.app.perform(Action::DebridCheck);
                })),
            ));
        }
        let key = d.config.keys.get(&provider).cloned().unwrap_or_default();
        let account: Element<'_, Message> = match &d.account {
            _ if d.checking => weak("Checking…").into(),
            Some((p, Ok(a))) if *p == provider => {
                let until = a
                    .until
                    .as_deref()
                    .map(|u| format!(" until {u}"))
                    .unwrap_or_default();
                if a.premium {
                    text(format!("✓ {} · premium{until}", a.name))
                        .size(13)
                        .color(GREEN)
                        .into()
                } else {
                    text(format!("{} · not premium: downloads may not work", a.name))
                        .size(13)
                        .color(GOLD)
                        .into()
                }
            }
            Some((p, Err(e))) if *p == provider => error_text(e.clone()).into(),
            _ if d.config.key(provider).is_none() => text("Add your API key to download.")
                .size(13)
                .color(GOLD)
                .into(),
            _ => space().into(),
        };
        c = c
            .push(providers)
            .push(
                row![
                    text("API key").size(14).color(MUTED).width(80),
                    field("Paste it here", &key)
                        .id("debrid-key")
                        .secure(true)
                        .on_input(move |k| edit(move |g| {
                            g.app.debrid.config.keys.insert(provider, k.clone());
                            g.app.debrid.save();
                            g.app.debrid.account = None;
                        }))
                        .on_submit(act(Action::DebridCheck))
                        .width(360),
                    secondary(
                        "Check",
                        d.config
                            .key(provider)
                            .is_some()
                            .then(|| act(Action::DebridCheck))
                    ),
                    link(
                        format!("Get your {} API key ↗", provider.name()),
                        Some(open_url(provider.key_page().to_owned()))
                    ),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .push(account)
            .push(weak(format!(
                "{} fetches each torrent on its servers; {HERE} then downloads just the files \
                 you pick, over HTTPS. No BitTorrent traffic comes from here, and it's told not \
                 to seed.",
                provider.name()
            )));
        group("Downloading", "Where releases come from.", c)
    }

    fn usenet_settings(&self) -> Element<'_, Message> {
        let providers: Vec<_> = self
            .app
            .usenet
            .config
            .providers
            .iter()
            .filter(|p| p.usable())
            .collect();
        let mut list = Column::new().spacing(6);
        if providers.is_empty() {
            list = list.push(weak(
                "Add a Usenet provider to download releases that are on Usenet from there \
                 instead.",
            ));
        } else {
            for p in providers {
                list = list.push(
                    row![
                        dot(GREEN),
                        body(p.label().to_owned()),
                        weak(format!(
                            "{} connections{}",
                            p.connections,
                            if p.backup { " · backup" } else { "" }
                        )),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
        }
        group(
            "Usenet",
            "Releases on Usenet (AnimeTosho, TsukiHime) download from there; the rest as torrents.",
            row![
                list.width(Fill),
                secondary("Providers…", Some(act(Action::UsenetOpenProviders)))
            ]
            .align_y(Alignment::Center),
        )
    }

    fn vpn_settings(&self) -> Element<'_, Message> {
        let v = &self.app.vpn;
        const NOTE: &str = "Every torrent connection (peers, DHT, trackers) is tied to the VPN, so \
                            nothing goes out over your normal connection.";
        if !vpn::SUPPORTED {
            return group(
                "VPN",
                NOTE,
                weak("Downloading over a VPN isn't supported on this system yet."),
            );
        }
        let mut c = column![check(
            "Only download torrents over a VPN",
            v.config.enabled,
            |on| edit(move |g| {
                g.app.vpn.config.enabled = on;
                g.app.vpn.save();
            })
        )]
        .spacing(14);
        if !v.config.enabled {
            c = c.push(weak(
                "Torrents use your normal connection, or whatever VPN the whole computer is on.",
            ));
            return group("VPN", NOTE, c);
        }
        let mut services = Row::new().spacing(8);
        for s in vpn::Service::ALL {
            services = services.push(chip(
                s.name(),
                s == v.config.service,
                Some(edit(move |g| {
                    g.app.vpn.config.service = s;
                    g.app.vpn.port_text = g
                        .app
                        .vpn
                        .config
                        .manual_port()
                        .map(|p| p.to_string())
                        .unwrap_or_default();
                    g.app.vpn.save();
                })),
            ));
        }
        c = c.push(services);
        let status = v.monitor.status();
        c = c.push(vpn_status(&status));
        if let Some(fix) = status.fix() {
            let label = match fix {
                Fix::PiaConnect => "Connect PIA",
                Fix::PiaForwardPorts => "Turn on port forwarding in PIA (reconnects)",
            };
            c = c.push(secondary(
                label,
                (!v.busy).then(|| act(Action::VpnFix(fix))),
            ));
        }
        let service = v.config.service;
        let mut forwarding = column![
            text("Port forwarding").size(14).font(MEDIUM).color(TEXT),
            weak(service.forwarding_help())
        ]
        .spacing(4);
        if service.forwarding() == Forwarding::Manual {
            let mut r = row![
                text("Forwarded port").size(13).color(MUTED),
                field("none", &v.port_text)
                    .on_input(move |t| edit(move |g| {
                        let vpn = &mut g.app.vpn;
                        vpn.port_text = t.clone();
                        match t.trim().parse::<u16>().ok().filter(|p| *p > 0) {
                            Some(p) => vpn.config.ports.insert(service, p),
                            None => vpn.config.ports.remove(&service),
                        };
                        vpn.save();
                    }))
                    .width(100),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            if !v.port_text.trim().is_empty() && v.config.manual_port().is_none() {
                r = r.push(error_text("not a port number"));
            }
            forwarding = forwarding.push(r);
        }
        c = c.push(forwarding);

        // The network interface torrents are bound to.
        let mut list = vpn::interfaces();
        list.retain(|i| !i.loopback && i.up && i.ipv4.is_some());
        list.sort_by_key(|i| (!i.tunnel, i.name.clone()));
        let chosen = v.config.interface.clone();
        let pick = |label: String, value: Option<String>| {
            let on = chosen == value;
            chip(
                label,
                on,
                Some(edit(move |g| {
                    g.app.vpn.config.interface = value.clone();
                    g.app.vpn.save();
                })),
            )
        };
        let mut interfaces = vec![pick("Automatic".into(), None)];
        if let Some(name) = chosen.clone()
            && !list.iter().any(|i| i.name == name)
        {
            interfaces.push(pick(format!("{name} (not connected)"), Some(name)));
        }
        for i in &list {
            let ip = i.ipv4.map(|ip| ip.to_string()).unwrap_or_default();
            let kind = if i.mesh() {
                " · mesh network"
            } else if i.tunnel {
                " · tunnel"
            } else {
                ""
            };
            interfaces.push(pick(
                format!("{} · {ip}{kind}", i.name),
                Some(i.name.clone()),
            ));
        }
        let interfaces = Row::with_children(interfaces)
            .spacing(8)
            .wrap()
            .vertical_spacing(8);
        c = c.push(
            column![
                text("Network interface").size(14).font(MEDIUM).color(TEXT),
                weak("Torrents are bound to this connection. Automatic finds your VPN's."),
                interfaces,
            ]
            .spacing(6),
        );
        c = c.push(weak("If the VPN drops, torrents stop until it's back."));
        group("VPN", NOTE, c)
    }

    fn list_settings(&self) -> Element<'_, Message> {
        let t = &self.app.tracking;
        group(
            "AniList & MyAnimeList",
            "Sign in to change your lists from the library. Changes go to every list you're \
             signed in to.",
            column![
                self.account_box(Service::AniList),
                rule(),
                self.account_box(Service::Mal),
                rule(),
                with_tip(
                    check(
                        "Update episodes watched when I finish one in the player",
                        t.config.sync_watched,
                        |on| edit(move |g| {
                            g.app.tracking.config.sync_watched = on;
                            g.app.tracking.save();
                        })
                    ),
                    "Plan to watch, on hold or dropped become watching; the last episode \
                     completes it."
                        .into()
                ),
            ]
            .spacing(16),
        )
    }

    /// Signed in, signing in, or how to.
    fn account_box(&self, service: Service) -> Element<'_, Message> {
        let t = &self.app.tracking;
        let mut c = column![text(service.name()).size(15).font(SEMI).color(TEXT)].spacing(8);
        if let Some(e) = t.errors.get(&service) {
            c = c.push(error_text(e.clone()));
        }
        if t.signed_in(service) {
            let who = match service {
                Service::AniList => t.config.anilist.user.as_ref().map(|u| u.name.clone()),
                Service::Mal => t.config.mal.user.clone(),
            }
            .unwrap_or_default();
            return c
                .push(
                    row![
                        dot(GREEN),
                        body(format!("Signed in as {who}")),
                        space::horizontal(),
                        secondary(
                            "Sign out",
                            Some(act(Action::Tracking(TrackingAction::SignOut(service))))
                        ),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .into();
        }
        if let Some(job) = t.signing_in.as_ref().filter(|j| j.service == service) {
            let mut r = row![
                weak("Finish signing in in your browser…"),
                secondary(
                    "Cancel",
                    Some(act(Action::Tracking(TrackingAction::CancelSignIn)))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            if let Some(url) = &job.url {
                r = r.push(link(
                    "Open the sign-in page again ↗",
                    Some(open_url(url.clone())),
                ));
            }
            return c.push(r).into();
        }
        let redirect = crate::tracking::redirect_url(service);
        let (site, site_url, kind) = match service {
            Service::AniList => (
                "anilist.co/settings/developer",
                "https://anilist.co/settings/developer",
                "any name",
            ),
            Service::Mal => (
                "myanimelist.net/apiconfig",
                "https://myanimelist.net/apiconfig",
                "App Type \"other\", any name and description",
            ),
        };
        let id = match service {
            Service::AniList => t.config.anilist.client_id.clone(),
            Service::Mal => t.config.mal.client_id.clone(),
        };
        c = c
            .push(
                row![
                    weak("1. Make an API client at"),
                    link(format!("{site} ↗"), Some(open_url(site_url.to_owned()))),
                    weak(format!("({kind}).")),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(4),
            )
            .push(
                row![
                    weak("2. Give it the redirect URL"),
                    container(text(redirect.clone()).size(12).font(MONO).color(TEXT))
                        .padding([3, 8])
                        .style(|_| pill(SURFACE2)),
                    link("Copy", Some(copy(redirect))),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(4),
            )
            .push(line(vec![
                weak("3. Its client ID").into(),
                field("Client ID", &id)
                    .on_input(move |v| {
                        edit(move |g| {
                            let c = &mut g.app.tracking.config;
                            match service {
                                Service::AniList => c.anilist.client_id = v.clone(),
                                Service::Mal => c.mal.client_id = v.clone(),
                            }
                        })
                    })
                    .width(w(320.0))
                    .into(),
            ]));
        // AniList's code grant needs the secret; MyAnimeList shows one for
        // some kinds of client only.
        let (secret, note) = match service {
            Service::AniList => (&t.config.anilist.client_secret, "    and its client secret"),
            Service::Mal => (
                &t.config.mal.client_secret,
                "    and its client secret, if it shows one",
            ),
        };
        let note = if compact() { note.trim_start() } else { note };
        c = c.push(line(vec![
            weak(note).into(),
            field("Client secret", secret)
                .id(match service {
                    Service::AniList => "anilist-secret",
                    Service::Mal => "mal-secret",
                })
                .secure(true)
                .on_input(move |v| {
                    edit(move |g| {
                        let c = &mut g.app.tracking.config;
                        match service {
                            Service::AniList => c.anilist.client_secret = v.clone(),
                            Service::Mal => c.mal.client_secret = v.clone(),
                        }
                    })
                })
                .width(w(320.0))
                .into(),
        ]));
        c.push(primary(
            format!("Sign in with {}", service.name()),
            t.signing_in
                .is_none()
                .then(|| act(Action::Tracking(TrackingAction::SignIn(service)))),
        ))
        .into()
    }

    /// Google Drive: the one-time setup, signing in, the folder; and
    /// uploading `export`, if there's one.
    pub(super) fn drive_section(&self, export: Option<&PathBuf>) -> Element<'_, Message> {
        use crate::app::drive_ui::{SETUP_STEPS, file_name, short_client_id};
        let d = &self.app.drive;
        let upload = d.upload.as_ref();
        let Some(client) = &d.account.client else {
            let mut steps = Column::new().spacing(4);
            for step in SETUP_STEPS {
                steps = steps.push(weak(step));
            }
            return column![
                body(
                    "To upload, the app needs an OAuth client from your own Google Cloud project \
                     (a one-time setup)."
                ),
                row![
                    secondary("Load client file…", Some(act(Action::DriveLoadClient))),
                    link(
                        "Google Cloud Console ↗",
                        Some(open_url(
                            "https://console.cloud.google.com/apis/credentials".into()
                        ))
                    ),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                steps,
            ]
            .spacing(12)
            .into();
        };
        if let Some(job) = &d.signing_in {
            let mut r = row![
                weak("Finish signing in in your browser…"),
                secondary("Cancel", Some(act(Action::DriveCancelSignIn))),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            if let Some(url) = &job.url {
                r = r.push(link(
                    "Open the sign-in page again ↗",
                    Some(open_url(url.clone())),
                ));
            }
            return r.into();
        }
        let Some(email) = d.account.email.as_ref().filter(|_| d.drive.is_some()) else {
            return row![
                primary("Sign in with Google", Some(act(Action::DriveSignIn))),
                weak(format!("Client {}", short_client_id(&client.id))),
                link("Change client file…", Some(act(Action::DriveLoadClient))),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into();
        };
        let uploading = upload.is_some_and(|r| r.running());
        let folder: Element<'_, Message> = match &d.account.folder {
            Some(f) => text(f.path.clone()).size(14).font(SEMI).color(TEXT).into(),
            None => text("no folder chosen").size(14).color(GOLD).into(),
        };
        let mut c = column![
            row![
                dot(GREEN),
                body(format!("Signed in as {email}")),
                link("Sign out", (!uploading).then(|| act(Action::DriveSignOut))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                weak("Upload to"),
                folder,
                small(
                    "Choose folder…",
                    (!uploading).then(|| act(Action::DriveChooseFolder))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            check(
                "Upload each export to this folder",
                d.account.upload_after_export,
                |on| act(Action::DriveSetAutoUpload(on))
            ),
        ]
        .spacing(12);
        match upload {
            Some(run) if run.running() => {
                c = c.push(
                    row![
                        body(format!("Uploading {}", file_name(&run.file))),
                        bar(run.progress, ACCENT, 6.0).length(220),
                        weak(format!(
                            "{:.0}% · {}",
                            run.progress * 100.0,
                            run.time_left(Instant::now())
                        )),
                        small("Cancel", Some(act(Action::DriveCancelUpload))),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            _ => {
                let mut r = Row::new().spacing(10).align_y(Alignment::Center);
                if let Some(path) = export {
                    let already =
                        upload.is_some_and(|r| r.file == *path && matches!(r.result, Some(Ok(_))));
                    if !already {
                        r = r.push(secondary(
                            format!("Upload {}", file_name(path)),
                            d.account
                                .folder
                                .is_some()
                                .then(|| act(Action::DriveUpload(path.clone()))),
                        ));
                    }
                }
                match upload.and_then(|r| r.result.as_ref()) {
                    Some(Ok(file)) => {
                        r = r.push(
                            text(format!("Uploaded {}", file.name))
                                .size(13)
                                .color(GREEN),
                        );
                        if let Some(l) = &file.link {
                            r = r.push(link("Open in Drive ↗", Some(open_url(l.clone()))));
                        }
                    }
                    Some(Err(e)) if e != "cancelled" => {
                        r = r.push(with_tip(error_text("Upload failed").into(), e.clone()));
                    }
                    _ => {}
                }
                c = c.push(r);
            }
        }
        c.into()
    }

    /// Which audio and subtitles play: in the player (files here and
    /// Jellyfin's streams), as the library's default tracks, and for
    /// remuxes. Series can have their own (on their pages).
    fn language_settings(&self) -> Element<'_, Message> {
        let note = if self.app.servers_mode() {
            "What the player plays, from Jellyfin or from files here, and what remuxes keep. A \
             series can have its own, on its page; any track can still be picked while watching."
        } else {
            "What the player plays, what the library marks default in its files (every track is \
             kept), and what remuxes keep. A series can have its own, on its page."
        };
        group(
            "Languages",
            note,
            super::series::lang_editor(&self.app.settings.prefs, |p| {
                edit(move |g| {
                    g.app.settings.prefs = p.clone();
                    g.app.recheck_defaults();
                    g.app.pages.last_stats = None;
                    if let Some(player) = &mut g.player {
                        player.use_languages(&p);
                    }
                })
            }),
        )
    }

    fn player_settings(&self) -> Element<'_, Message> {
        let p = &self.app.settings.player;
        group(
            "Player",
            "Also in the player's own settings.",
            column![
                check("Skip openings automatically", p.skip_openings, |on| edit(
                    move |g| g.app.settings.player.skip_openings = on
                )),
                check("Skip endings automatically", p.skip_endings, |on| edit(
                    move |g| g.app.settings.player.skip_endings = on
                )),
                check(
                    "Play the next episode automatically",
                    p.autoplay,
                    |on| edit(move |g| g.app.settings.player.autoplay = on)
                ),
            ]
            .spacing(12),
        )
    }
}

/// Where the VPN is at.
fn vpn_status<'a>(status: &Status) -> Element<'a, Message> {
    match status {
        Status::Off => space().into(),
        Status::Checking => weak("Looking for the VPN…").into(),
        Status::Down { reason, .. } => text(format!("Not connected: {reason}"))
            .size(13)
            .color(GOLD)
            .into(),
        Status::Up {
            interface,
            ip,
            port,
            ..
        } => {
            let (color, line) = match port {
                Port::Open(p) => (GREEN, format!("✓ Port {p} forwarded")),
                Port::Requesting => (MUTED, "Getting a forwarded port…".to_owned()),
                Port::Unavailable(why) | Port::None(why) => {
                    (FAINT, format!("No forwarded port: {why}"))
                }
            };
            column![
                text(format!("✓ Connected through {interface} ({ip})"))
                    .size(13)
                    .color(GREEN),
                text(line).size(13).color(color),
            ]
            .spacing(4)
            .into()
        }
    }
}

/// Put `s` on the clipboard.
fn copy(s: String) -> Message {
    edit(move |g| g.copy = Some(s.clone()))
}
