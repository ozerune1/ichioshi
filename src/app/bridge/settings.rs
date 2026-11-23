//! More: how things are, the playlist, and the settings' groups (each a
//! page of its own on the phone).

use serde_json::{Value, json};

use super::form::{boxed, buttons, check, chips, copy, field, heading, link, said, select, text};
use super::ui::Ui;
use crate::app::servers_ui::{ServersAction, ago};
use crate::app::tracking_ui::TrackingAction;
use crate::app::{Action, App, EngineState};
use crate::debrid::Provider as DebridProvider;
use crate::prefs::{LANGUAGES, LangPrefs, SubType};
use crate::remux::language_name;
use crate::servers::LibraryMode;
use crate::servers::arr::Kind;
use crate::tracking::Service;

pub(super) fn page(ui: &mut Ui) -> Value {
    let app = ui.app;
    let lists = signed_in_as(app);
    let sections = vec![
        section(
            "library",
            "▦",
            "Library",
            if app.servers_mode() {
                "Jellyfin, Sonarr and Radarr".to_owned()
            } else {
                app.library_dir().display().to_string()
            },
            library(ui),
        ),
        section(
            "languages",
            "A",
            "Languages",
            app.settings.prefs.summary(),
            languages(ui),
        ),
        section(
            "downloading",
            "↓",
            "Downloading",
            match (app.debrid.service(), app.usenet.active()) {
                (Some(s), true) => format!("Usenet first, then {s}"),
                (Some(s), false) => format!("Through {s}"),
                (None, true) => "Usenet first, then BitTorrent".to_owned(),
                (None, false) => "BitTorrent; nothing is ever seeded".to_owned(),
            },
            downloading(ui),
        ),
        section(
            "usenet",
            "≡",
            "Usenet",
            if app.usenet.active() {
                "On".to_owned()
            } else {
                "Providers".to_owned()
            },
            usenet(ui),
        ),
        section(
            "lists",
            "✓",
            "Lists",
            if lists.is_empty() {
                "AniList and MyAnimeList".to_owned()
            } else {
                lists.join(", ")
            },
            list_settings(ui),
        ),
        section(
            "player",
            "▶",
            "Player",
            "Skipping, autoplay".to_owned(),
            player(ui),
        ),
    ];
    let playlist = app.playlist.items.len();
    json!({
        "kind": "more",
        "status": status(app),
        "playlist": {
            "note": match playlist {
                0 => "Episodes to watch in order, or export".to_owned(),
                1 => "1 episode".to_owned(),
                n => format!("{n} episodes"),
            },
            "id": ui.act(Action::OpenPlaylist),
        },
        "sections": sections,
    })
}

fn section(key: &str, icon: &str, name: &str, note: String, items: Vec<Value>) -> Value {
    json!({"key": key, "icon": icon, "name": name, "note": note, "items": items})
}

/// How downloading is (the engine, a debrid service), as the desktop's
/// sidebar says.
fn status(app: &App) -> Vec<Value> {
    let (tone, line) = if let Some(service) = app.debrid.service() {
        ("good", format!("Downloads through {service}"))
    } else {
        match &app.engine {
            EngineState::Off => ("muted", "Ready · nothing seeded".into()),
            EngineState::WaitingForVpn(_) => ("warn", "Connecting to the VPN…".into()),
            EngineState::Starting => ("warn", "Starting the torrent engine…".into()),
            EngineState::Ready(_) => ("good", "Ready · nothing seeded".into()),
            EngineState::Failed(_) => ("error", "The torrent engine failed".into()),
        }
    };
    let episodes: usize = app.pages.stats.values().map(|s| s.files.len()).sum();
    vec![
        json!({"text": line, "tone": tone}),
        json!({"text": format!("{episodes} episodes in the library"), "tone": "faint"}),
    ]
}

fn signed_in_as(app: &App) -> Vec<String> {
    let c = &app.tracking.config;
    let mut names = Vec::new();
    if let Some(u) = &c.anilist.user {
        names.push(format!("AniList · {}", u.name));
    }
    if let Some(u) = &c.mal.user {
        names.push(format!("MyAnimeList · {u}"));
    }
    names
}

fn library(ui: &mut Ui) -> Vec<Value> {
    let app = ui.app;
    let on_servers = app.servers_mode();
    let mode = chips(
        ui,
        vec![
            (
                "On this phone".into(),
                !on_servers,
                Box::new(|app: &mut App| {
                    app.perform(Action::Servers(ServersAction::SetMode(LibraryMode::Local)))
                }),
            ),
            (
                "Jellyfin, Sonarr and Radarr".into(),
                on_servers,
                Box::new(|app: &mut App| {
                    app.perform(Action::Servers(ServersAction::SetMode(
                        LibraryMode::Servers,
                    )))
                }),
            ),
        ],
    );
    if !on_servers {
        return vec![
            said(
                "Each series gets its own folder here; downloads go straight into it.",
                "muted",
            ),
            mode,
            field(
                ui,
                "Library folder",
                "",
                &app.settings.library_dir.display().to_string(),
                false,
                |app, t| app.settings.library_dir = t.into(),
            ),
            check(
                ui,
                "Prefer dual-audio releases",
                app.settings.prefer_dual_audio,
                |app, on| app.settings.prefer_dual_audio = on,
            ),
        ];
    }
    let s = &app.servers;
    let mut items = vec![
        said(
            "Sonarr and Radarr keep and download your anime; Jellyfin streams it and remembers \
             what you've watched. Your library on this phone is kept as it was, for when you \
             switch back.",
            "muted",
        ),
        mode,
    ];
    // Jellyfin.
    let j = &s.config.jellyfin;
    let mut jellyfin = vec![heading(
        "Jellyfin",
        Some("Plays your library, and keeps where you are in each episode.".into()),
    )];
    if j.signed_in() {
        jellyfin.push(said(
            format!(
                "Signed in as {} on {} ({})",
                j.username,
                j.server_name.as_deref().unwrap_or("Jellyfin"),
                crate::servers::base(&j.url)
            ),
            "good",
        ));
        let out = ui.button("Sign out", Some(Action::Servers(ServersAction::SignOut)));
        jellyfin.push(buttons(vec![out], false));
    } else {
        jellyfin.push(field(
            ui,
            "Address",
            "https://jellyfin.example.com",
            &j.url,
            false,
            |app, v| app.servers.config.jellyfin.url = v,
        ));
        jellyfin.push(field(ui, "Username", "", &j.username, false, |app, v| {
            app.servers.config.jellyfin.username = v
        }));
        jellyfin.push(field(ui, "Password", "", &s.password, true, |app, v| {
            app.servers.password = v
        }));
        let sign_in = ui.button(
            if s.signing_in {
                "Signing in…"
            } else {
                "Sign in"
            },
            (!s.signing_in).then_some(Action::Servers(ServersAction::SignIn)),
        );
        jellyfin.push(buttons(vec![sign_in], true));
        jellyfin.push(said(
            "The password isn't kept: Jellyfin gives the app a sign-in of its own.",
            "faint",
        ));
    }
    if let Some(e) = &s.jellyfin_error {
        jellyfin.push(said(e.clone(), "error"));
    }
    jellyfin.push(said(
        format!(
            "Episodes saved from Jellyfin to watch offline go in {}",
            app.library_dir().join("Offline").display()
        ),
        "faint",
    ));
    items.push(boxed(jellyfin));
    for kind in [Kind::Sonarr, Kind::Radarr] {
        items.push(boxed(arr(ui, kind)));
    }
    let when = if s.refreshing {
        "Reading your servers…".to_owned()
    } else if let Some(t) = s.refreshed {
        format!("Read {}", ago(t))
    } else if s.config.any() {
        "Not read yet".to_owned()
    } else {
        "Set up Sonarr or Radarr to see your library".to_owned()
    };
    items.push(said(when, "muted"));
    let again = ui.button(
        "Read again",
        (s.config.any() && !s.refreshing).then_some(Action::Servers(ServersAction::Refresh)),
    );
    items.push(buttons(vec![again], false));
    for p in &s.problems {
        items.push(said(p.clone(), "error"));
    }
    items
}

/// Sonarr's or Radarr's address and key, and what anime are added with.
fn arr(ui: &mut Ui, kind: Kind) -> Vec<Value> {
    use crate::app::discover_ui::Loaded;
    let s = &ui.app.servers;
    let account = match kind {
        Kind::Sonarr => &s.config.sonarr,
        Kind::Radarr => &s.config.radarr,
    };
    let (what, port) = match kind {
        Kind::Sonarr => ("Series: where they're kept, and downloading them.", 8989),
        Kind::Radarr => ("Films: where they're kept, and downloading them.", 7878),
    };
    fn account_of(app: &mut App, kind: Kind) -> &mut crate::servers::ArrAccount {
        match kind {
            Kind::Sonarr => &mut app.servers.config.sonarr,
            Kind::Radarr => &mut app.servers.config.radarr,
        }
    }
    let about = s.about.get(&kind);
    let mut items = vec![
        heading(kind.name(), Some(what.into())),
        field(
            ui,
            "Address",
            &format!("http://nas:{port}"),
            &account.url,
            false,
            move |app, v| account_of(app, kind).url = v,
        ),
        field(
            ui,
            "API key",
            "Its Settings → General",
            &account.api_key,
            true,
            move |app, v| account_of(app, kind).api_key = v,
        ),
    ];
    let checking = matches!(about, Some(Loaded::Loading));
    let check_it = ui.button(
        if checking { "Checking…" } else { "Check" },
        (!checking).then_some(Action::Servers(ServersAction::Check(kind))),
    );
    items.push(buttons(vec![check_it], false));
    match about {
        Some(Loaded::Failed(e)) => items.push(said(e.clone(), "error")),
        Some(Loaded::Ready(about)) => {
            items.push(said(format!("{} {}", kind.name(), about.version), "good"));
            let profiles = about
                .profiles
                .iter()
                .map(|p| {
                    let id = p.id;
                    (
                        p.name.clone(),
                        account.quality_profile == Some(id),
                        Box::new(move |app: &mut App| {
                            account_of(app, kind).quality_profile = Some(id);
                        }) as Box<dyn Fn(&mut App)>,
                    )
                })
                .collect();
            items.push(select(ui, "Anime are added with", profiles));
            let folders = about
                .folders
                .iter()
                .map(|f| {
                    let path = f.path.clone();
                    (
                        f.to_string(),
                        account.root_folder.as_ref() == Some(&f.path),
                        Box::new(move |app: &mut App| {
                            account_of(app, kind).root_folder = Some(path.clone());
                        }) as Box<dyn Fn(&mut App)>,
                    )
                })
                .collect();
            items.push(select(ui, "Into", folders));
        }
        _ if account.is_set() => items.push(said(
            "Check it to choose what anime are added with.",
            "muted",
        )),
        _ => {}
    }
    items
}

fn languages(ui: &mut Ui) -> Vec<Value> {
    let prefs = ui.app.settings.prefs.clone();
    let set = |prefs: LangPrefs| -> Box<dyn Fn(&mut App)> {
        Box::new(move |app: &mut App| {
            app.settings.prefs = prefs.clone();
            app.recheck_defaults();
            app.pages.last_stats = None;
        })
    };
    let audio = LANGUAGES
        .iter()
        .map(|code| {
            (
                language_name(code),
                prefs.audio == *code,
                set(LangPrefs {
                    audio: (*code).into(),
                    ..prefs.clone()
                }),
            )
        })
        .collect();
    let mut subs = vec![(
        "None".to_owned(),
        prefs.subtitles.is_none(),
        set(LangPrefs {
            subtitles: None,
            ..prefs.clone()
        }),
    )];
    subs.extend(LANGUAGES.iter().map(|code| {
        (
            language_name(code),
            prefs.subtitles.as_deref() == Some(*code),
            set(LangPrefs {
                subtitles: Some((*code).into()),
                ..prefs.clone()
            }),
        )
    }));
    let mut items = vec![
        said(
            "What the player plays, and what the library marks default in its files (every \
             track is kept). A series can have its own, on its page.",
            "muted",
        ),
        select(ui, "Audio", audio),
        select(ui, "Subtitles", subs),
    ];
    if prefs.subtitles.is_some() {
        let kinds = SubType::ALL
            .into_iter()
            .map(|k| {
                (
                    k.name().to_owned(),
                    prefs.sub_type == k,
                    set(LangPrefs {
                        sub_type: k,
                        ..prefs.clone()
                    }),
                )
            })
            .collect();
        items.push(select(ui, "Kind", kinds));
        items.push(said(prefs.sub_type.help(), "faint"));
    }
    items
}

fn downloading(ui: &mut Ui) -> Vec<Value> {
    let d = &ui.app.debrid;
    let enabled = d.config.enabled;
    let mut items = vec![chips(
        ui,
        vec![
            (
                "BitTorrent, on this phone".into(),
                !enabled,
                Box::new(|app: &mut App| app.perform(Action::DebridSetEnabled(false))),
            ),
            (
                "A debrid service".into(),
                enabled,
                Box::new(|app: &mut App| app.perform(Action::DebridSetEnabled(true))),
            ),
        ],
    )];
    if !enabled {
        items.push(said(
            "Uploading is turned off: nothing is seeded, and each torrent stops as soon as the \
             files you picked are complete.",
            "muted",
        ));
        return items;
    }
    let provider = d.config.provider;
    let providers = DebridProvider::ALL
        .into_iter()
        .map(|p| {
            (
                p.name().to_owned(),
                p == provider,
                Box::new(move |app: &mut App| {
                    app.debrid.config.provider = p;
                    app.debrid.save();
                    app.debrid.account = None;
                    app.perform(Action::DebridCheck);
                }) as Box<dyn Fn(&mut App)>,
            )
        })
        .collect();
    items.push(chips(ui, providers));
    let key = d.config.keys.get(&provider).cloned().unwrap_or_default();
    items.push(field(ui, "API key", "", &key, true, move |app, k| {
        app.debrid.config.keys.insert(provider, k);
        app.debrid.save();
        app.debrid.account = None;
    }));
    let check_it = ui.button(
        "Check",
        d.config
            .key(provider)
            .is_some()
            .then_some(Action::DebridCheck),
    );
    items.push(buttons(vec![check_it], false));
    items.push(link(
        ui,
        &format!("Get your {} API key ↗", provider.name()),
        provider.key_page().to_owned(),
    ));
    match &d.account {
        _ if d.checking => items.push(said("Checking…", "muted")),
        Some((p, Ok(a))) if *p == provider => {
            let until = a
                .until
                .as_deref()
                .map(|u| format!(" until {u}"))
                .unwrap_or_default();
            items.push(if a.premium {
                said(format!("✓ {} · premium{until}", a.name), "good")
            } else {
                said(
                    format!("{} · not premium: downloads may not work", a.name),
                    "warn",
                )
            });
        }
        Some((p, Err(e))) if *p == provider => items.push(said(e.clone(), "error")),
        _ if d.config.key(provider).is_none() => {
            items.push(said("Add your API key to download.", "warn"))
        }
        _ => {}
    }
    items.push(said(
        format!(
            "{} fetches each torrent on its servers; this phone then downloads just the files \
             you pick, over HTTPS. No BitTorrent traffic comes from here.",
            provider.name()
        ),
        "faint",
    ));
    items
}

fn usenet(ui: &mut Ui) -> Vec<Value> {
    let app = ui.app;
    let mut items = vec![said(
        "Releases on Usenet (AnimeTosho, TsukiHime) download from there; the rest as torrents.",
        "muted",
    )];
    let providers: Vec<_> = app
        .usenet
        .config
        .providers
        .iter()
        .filter(|p| p.usable())
        .collect();
    if providers.is_empty() {
        items.push(text("No providers yet."));
    }
    for p in providers {
        items.push(said(
            format!(
                "{} · {} connections{}",
                p.label(),
                p.connections,
                if p.backup { " · backup" } else { "" }
            ),
            "good",
        ));
    }
    let open = ui.button("Providers…", Some(Action::UsenetOpenProviders));
    items.push(buttons(vec![open], false));
    items
}

fn list_settings(ui: &mut Ui) -> Vec<Value> {
    let mut items = vec![said(
        "Sign in to change your lists from the library. Changes go to every list you're \
         signed in to.",
        "muted",
    )];
    items.push(boxed(account(ui, Service::AniList)));
    items.push(boxed(account(ui, Service::Mal)));
    let on = ui.app.tracking.config.sync_watched;
    items.push(check(
        ui,
        "Update episodes watched when I finish one in the player",
        on,
        |app, on| {
            app.tracking.config.sync_watched = on;
            app.tracking.save();
        },
    ));
    items
}

/// Signed in, signing in, or how to.
fn account(ui: &mut Ui, service: Service) -> Vec<Value> {
    let t = &ui.app.tracking;
    let mut items = vec![heading(service.name(), None)];
    if let Some(e) = t.errors.get(&service) {
        items.push(said(e.clone(), "error"));
    }
    if t.signed_in(service) {
        let who = match service {
            Service::AniList => t.config.anilist.user.as_ref().map(|u| u.name.clone()),
            Service::Mal => t.config.mal.user.clone(),
        }
        .unwrap_or_default();
        items.push(said(format!("Signed in as {who}"), "good"));
        let out = ui.button(
            "Sign out",
            Some(Action::Tracking(TrackingAction::SignOut(service))),
        );
        items.push(buttons(vec![out], false));
        return items;
    }
    if let Some(job) = t.signing_in.as_ref().filter(|j| j.service == service) {
        items.push(said("Finish signing in in your browser…", "muted"));
        let cancel = ui.button(
            "Cancel",
            Some(Action::Tracking(TrackingAction::CancelSignIn)),
        );
        items.push(buttons(vec![cancel], false));
        if let Some(url) = job.url.clone() {
            items.push(link(ui, "Open the sign-in page again ↗", url));
        }
        return items;
    }
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
    items.push(text(format!("1. Make an API client ({kind}):")));
    items.push(link(ui, &format!("{site} ↗"), site_url.to_owned()));
    items.push(text("2. Give it the redirect URL"));
    items.push(copy("Redirect URL", crate::tracking::redirect_url(service)));
    let (id, secret) = match service {
        Service::AniList => (&t.config.anilist.client_id, &t.config.anilist.client_secret),
        Service::Mal => (&t.config.mal.client_id, &t.config.mal.client_secret),
    };
    items.push(field(
        ui,
        "3. Its client ID",
        "Client ID",
        id,
        false,
        move |app, v| {
            let c = &mut app.tracking.config;
            match service {
                Service::AniList => c.anilist.client_id = v,
                Service::Mal => c.mal.client_id = v,
            }
        },
    ));
    items.push(field(
        ui,
        match service {
            Service::AniList => "and its client secret",
            Service::Mal => "and its client secret, if it shows one",
        },
        "Client secret",
        secret,
        true,
        move |app, v| {
            let c = &mut app.tracking.config;
            match service {
                Service::AniList => c.anilist.client_secret = v,
                Service::Mal => c.mal.client_secret = v,
            }
        },
    ));
    let sign_in = ui.button(
        format!("Sign in with {}", service.name()),
        t.signing_in
            .is_none()
            .then_some(Action::Tracking(TrackingAction::SignIn(service))),
    );
    items.push(buttons(vec![sign_in], true));
    items
}

fn player(ui: &mut Ui) -> Vec<Value> {
    let p = &ui.app.settings.player;
    let (openings, endings, autoplay) = (p.skip_openings, p.skip_endings, p.autoplay);
    vec![
        check(ui, "Skip openings automatically", openings, |app, on| {
            app.settings.player.skip_openings = on
        }),
        check(ui, "Skip endings automatically", endings, |app, on| {
            app.settings.player.skip_endings = on
        }),
        check(
            ui,
            "Play the next episode automatically",
            autoplay,
            |app, on| app.settings.player.autoplay = on,
        ),
    ]
}
