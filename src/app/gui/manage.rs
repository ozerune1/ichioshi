//! A series' (or film's) panel on its page, on the servers: managing it as
//! in Sonarr (or Radarr).

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

use super::series::{open_url, with_tip};
use super::{Gui, Message, act, edit};
use crate::app::discover_ui::Loaded;
use crate::app::manage_ui::{Edit, Looking, Manage, ManageAction, Tab};
use crate::app::{Action, fmt_bytes};
use crate::library::Remote;
use crate::servers::arr::{Kind, Profile, RootFolder};

use super::style::*;

fn manage(a: ManageAction) -> Message {
    act(Action::Manage(a))
}

/// A choice of words for a setting's values (dropdowns show them).
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    value: &'static str,
    label: &'static str,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

const SERIES_TYPES: [Choice; 3] = [
    Choice {
        value: "anime",
        label: "Anime (absolute numbers)",
    },
    Choice {
        value: "standard",
        label: "Standard (season and episode)",
    },
    Choice {
        value: "daily",
        label: "Daily (dates)",
    },
];

const NEW_SEASONS: [Choice; 2] = [
    Choice {
        value: "all",
        label: "Watch for new seasons",
    },
    Choice {
        value: "none",
        label: "Don't watch for new seasons",
    },
];

const AVAILABILITY: [Choice; 3] = [
    Choice {
        value: "announced",
        label: "Announced",
    },
    Choice {
        value: "inCinemas",
        label: "In cinemas",
    },
    Choice {
        value: "released",
        label: "Released",
    },
];

/// "3 h", "2 d", "5 mo".
fn age(hours: f64) -> String {
    if hours < 48.0 {
        format!("{:.0} h", hours.max(0.0))
    } else if hours < 24.0 * 60.0 {
        format!("{:.0} d", hours / 24.0)
    } else {
        format!("{:.0} mo", hours / 24.0 / 30.0)
    }
}

fn when(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%-d %b %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// What a history event was, in words.
fn event_name(kind: &str) -> &str {
    match kind {
        "grabbed" => "Grabbed",
        "downloadFolderImported" | "movieFolderImported" => "Imported",
        "downloadFailed" => "Download failed",
        "episodeFileDeleted" | "movieFileDeleted" => "File deleted",
        "episodeFileRenamed" | "movieFileRenamed" => "File renamed",
        "downloadIgnored" => "Ignored",
        "seriesFolderImported" => "Imported from the folder",
        other => other,
    }
}

impl Gui {
    /// The page's series on Sonarr (or film on Radarr): settings, search,
    /// files and history.
    pub(super) fn manage_panel<'a>(&'a self, m: &'a Manage) -> Element<'a, Message> {
        let remote = self
            .app
            .library
            .find(m.anilist)
            .and_then(|s| s.remote.as_ref());
        let name = m.kind.name();
        let record = match &m.record {
            Loaded::Ready(r) => Some(r),
            _ => None,
        };
        let title = record
            .and_then(|r| r["title"].as_str())
            .unwrap_or(name)
            .to_owned();
        let place = match (m.season, m.follows) {
            (_, Some(prev)) => format!(
                "after {} (its episodes show here once {name} lists them)",
                self.app
                    .library
                    .find(prev)
                    .map(|s| s.title().to_owned())
                    .unwrap_or_default()
            ),
            (Some(s), None) => match remote.map(|r| r.arr) {
                Some(crate::library::ArrRef::Sonarr { offset, .. }) if offset > 0 => {
                    format!("season {s}, from its episode {}", offset + 1)
                }
                _ => format!("season {s}"),
            },
            (None, None) => "film".into(),
        };
        let mut links = Row::new().spacing(14).align_y(Alignment::Center);
        if let Some(slug) = record.and_then(|r| r["titleSlug"].as_str()) {
            let account = match m.kind {
                Kind::Sonarr => &self.app.servers.config.sonarr,
                Kind::Radarr => &self.app.servers.config.radarr,
            };
            let arr = crate::servers::arr::Arr::new(m.kind, account, &reqwest::Client::new());
            links = links.push(link(
                format!("Open in {name} ↗"),
                Some(open_url(arr.page(slug))),
            ));
        }
        if let (Some(item), Some(j)) = (
            remote.and_then(|r| r.jellyfin.as_ref()),
            self.app.jellyfin(),
        ) {
            links = links.push(link("Open in Jellyfin ↗", Some(open_url(j.page(item)))));
        }
        let head = row![
            badge(name, GREEN),
            text(title).size(16).font(SEMI).color(TEXT),
            weak(place),
            space::horizontal(),
            links,
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let mut tabs = Row::new().spacing(8);
        for (tab, label) in [
            (Tab::Overview, "Overview"),
            (Tab::Search, "Releases"),
            (Tab::Files, "Files"),
            (Tab::History, "History"),
        ] {
            tabs = tabs.push(chip(
                label,
                m.tab == tab,
                Some(manage(ManageAction::Tab(tab))),
            ));
        }
        let body: Element<'a, Message> = match m.tab {
            Tab::Overview => self.manage_overview(m, remote),
            Tab::Search => self.manage_releases(m),
            Tab::Files => self.manage_files(m, remote),
            Tab::History => self.manage_history(m, remote),
        };
        card(column![head, tabs, super::library::rule(), body].spacing(14))
    }

    fn manage_overview<'a>(
        &'a self,
        m: &'a Manage,
        remote: Option<&'a Remote>,
    ) -> Element<'a, Message> {
        let name = m.kind.name();
        let record = match &m.record {
            Loaded::Loading => return weak(format!("Reading it from {name}…")).into(),
            Loaded::Failed(e) => {
                return error_text(format!("Couldn't read it from {name}: {e}")).into();
            }
            Loaded::Ready(r) => r,
        };
        let about = match self.app.servers.about.get(&m.kind) {
            Some(Loaded::Ready(a)) => Some(a),
            _ => None,
        };
        let profile = record["qualityProfileId"].as_u64();
        let profile_name = about
            .and_then(|a| a.profiles.iter().find(|p| Some(p.id) == profile))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("profile {}", profile.unwrap_or(0)));
        let monitored = record["monitored"] == true;
        let mut facts = vec![format!("Quality: {profile_name}")];
        let tag_ids: Vec<u64> = record["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_u64)
            .collect();
        if let (false, Some(Loaded::Ready(list))) = (tag_ids.is_empty(), &m.tags) {
            let names: Vec<&str> = list
                .iter()
                .filter(|t| tag_ids.contains(&t.id))
                .map(|t| t.label.as_str())
                .collect();
            if !names.is_empty() {
                facts.push(format!("Tags: {}", names.join(", ")));
            }
        }
        match m.kind {
            Kind::Sonarr => {
                facts.push(format!(
                    "Type: {}",
                    record["seriesType"].as_str().unwrap_or("?")
                ));
                if record["monitorNewItems"] == "none" {
                    facts.push("New seasons not watched for".into());
                }
            }
            Kind::Radarr => facts.push(format!(
                "Available when: {}",
                record["minimumAvailability"].as_str().unwrap_or("?")
            )),
        }
        let mut c = column![
            row![
                check(
                    match m.kind {
                        Kind::Sonarr => "Watch for this series",
                        Kind::Radarr => "Watch for this film",
                    },
                    monitored,
                    |on| manage(ManageAction::SetMonitored(on))
                ),
                weak(facts.join(" · ")),
            ]
            .spacing(16)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(6),
            text(record["path"].as_str().unwrap_or_default().to_owned())
                .size(12)
                .color(FAINT),
        ]
        .spacing(10);

        let idle = !m.busy;
        let mut buttons = Row::new().spacing(8).align_y(Alignment::Center);
        if let Some(r) = remote.filter(|_| m.kind == Kind::Sonarr) {
            let watched = r.episodes.values().filter(|e| e.monitored).count();
            let missing = r.episodes.values().filter(|e| e.file.is_none()).count();
            c = c.push(weak(format!(
                "{watched} of its {} episodes watched for · {missing} missing",
                r.episodes.len()
            )));
            buttons = buttons
                .push(small(
                    "Watch for all its episodes",
                    (idle && watched < r.episodes.len())
                        .then(|| manage(ManageAction::SetEpisodesMonitored(Vec::new(), true))),
                ))
                .push(small(
                    "Watch for none",
                    (idle && watched > 0)
                        .then(|| manage(ManageAction::SetEpisodesMonitored(Vec::new(), false))),
                ))
                .push(small(
                    format!("Search for the {missing} missing"),
                    (idle && missing > 0).then(|| manage(ManageAction::AutoSearch(Vec::new()))),
                ));
        }
        if m.kind == Kind::Radarr {
            buttons = buttons.push(small(
                "Search for it",
                idle.then(|| manage(ManageAction::AutoSearch(Vec::new()))),
            ));
        }
        buttons = buttons
            .push(small(
                "Refresh & scan",
                idle.then(|| manage(ManageAction::Refresh)),
            ))
            .push(small(
                "Rename files…",
                idle.then(|| manage(ManageAction::PreviewRenames)),
            ))
            .push(small(
                "Import by hand…",
                Some(act(Action::Import(
                    crate::app::import_ui::ImportAction::Open {
                        kind: m.kind,
                        owner: m.owner,
                        anilist: Some(m.anilist),
                        download: None,
                    },
                ))),
            ))
            .push(small(
                "Edit…",
                (m.edit.is_none()).then(|| manage(ManageAction::StartEdit)),
            ));
        // (Wrapped: a phone's are a few to a line.)
        if !compact() {
            buttons = buttons.push(space::horizontal());
        }
        buttons = buttons.push(danger("Delete…", Some(manage(ManageAction::AskDelete))));
        c = c.push(if compact() {
            Element::from(buttons.wrap().vertical_spacing(8))
        } else {
            buttons.into()
        });
        if let Some(e) = &m.edit {
            c = c.push(self.manage_edit(m, e));
        }
        if let Some(renames) = &m.renames {
            c = c.push(manage_renames(renames));
        }
        if let Some(series) = self.app.library.find(m.anilist) {
            c = c
                .push(super::library::rule())
                .push(self.series_languages(series));
        }
        c.into()
    }

    fn manage_edit<'a>(&'a self, m: &'a Manage, e: &'a Edit) -> Element<'a, Message> {
        let about = match self.app.servers.about.get(&m.kind) {
            Some(Loaded::Ready(a)) => a,
            Some(Loaded::Failed(err)) => {
                return error_text(format!("Couldn't read {}'s settings: {err}", m.kind.name()))
                    .into();
            }
            _ => return weak(format!("Reading {}'s profiles and folders…", m.kind.name())).into(),
        };
        let field_row = |label: &'static str, picker: Element<'a, Message>| {
            row![text(label).size(13).color(MUTED).width(150), picker]
                .spacing(10)
                .align_y(Alignment::Center)
        };
        let changed = |f: fn(&mut Edit, String), v: String| {
            let mut e = e.clone();
            f(&mut e, v);
            manage(ManageAction::ChangeEdit(e))
        };
        let profile = about.profiles.iter().find(|p| p.id == e.profile).cloned();
        let e2 = e.clone();
        let mut c = column![field_row(
            "Quality profile",
            dropdown(about.profiles.clone(), profile, move |p: Profile| {
                let mut e = e2.clone();
                e.profile = p.id;
                manage(ManageAction::ChangeEdit(e))
            })
        )]
        .spacing(10);
        match m.kind {
            Kind::Sonarr => {
                let pick =
                    |list: &[Choice; 3], v: &str| list.iter().find(|c| c.value == v).cloned();
                c = c.push(field_row(
                    "Series type",
                    dropdown(
                        SERIES_TYPES.to_vec(),
                        pick(&SERIES_TYPES, &e.series_type),
                        move |ch: Choice| changed(|e, v| e.series_type = v, ch.value.to_owned()),
                    ),
                ));
                c = c.push(field_row(
                    "New seasons",
                    dropdown(
                        NEW_SEASONS.to_vec(),
                        NEW_SEASONS
                            .iter()
                            .find(|c| c.value == e.monitor_new)
                            .cloned(),
                        move |ch: Choice| changed(|e, v| e.monitor_new = v, ch.value.to_owned()),
                    ),
                ));
                let e3 = e.clone();
                c = c.push(check("Season folders", e.season_folder, move |on| {
                    let mut e = e3.clone();
                    e.season_folder = on;
                    manage(ManageAction::ChangeEdit(e))
                }));
            }
            Kind::Radarr => {
                c = c.push(field_row(
                    "Available when",
                    dropdown(
                        AVAILABILITY.to_vec(),
                        AVAILABILITY
                            .iter()
                            .find(|c| c.value == e.availability)
                            .cloned(),
                        move |ch: Choice| changed(|e, v| e.availability = v, ch.value.to_owned()),
                    ),
                ));
            }
        }
        let root = about
            .folders
            .iter()
            .find(|f| f.path.trim_end_matches('/') == e.root)
            .cloned();
        let e4 = e.clone();
        c = c.push(field_row(
            "Root folder",
            dropdown(about.folders.clone(), root, move |f: RootFolder| {
                let mut e = e4.clone();
                e.root = f.path.trim_end_matches('/').to_owned();
                manage(ManageAction::ChangeEdit(e))
            }),
        ));
        // Its tags: the server's, picked; or a new one.
        let mut tags = Row::new().spacing(6).align_y(Alignment::Center);
        match &m.tags {
            Some(Loaded::Ready(list)) => {
                for t in list {
                    let on = e.tags.contains(&t.id);
                    let (e6, id) = (e.clone(), t.id);
                    tags = tags.push(chip(
                        t.label.clone(),
                        on,
                        Some(manage(ManageAction::ChangeEdit({
                            let mut e = e6;
                            if on {
                                e.tags.retain(|x| *x != id);
                            } else {
                                e.tags.push(id);
                            }
                            e
                        }))),
                    ));
                }
            }
            Some(Loaded::Failed(err)) => tags = tags.push(error_text(err.clone())),
            _ => tags = tags.push(weak("Reading its tags…")),
        }
        let e7 = e.clone();
        tags = tags.push(
            field("New tag", &e.new_tag)
                .id("manage-new-tag")
                .on_input(move |v| {
                    let mut e = e7.clone();
                    e.new_tag = v;
                    manage(ManageAction::ChangeEdit(e))
                })
                .on_submit(manage(ManageAction::AddTag))
                .width(160),
        );
        tags = tags.push(small(
            "Add",
            (!e.new_tag.trim().is_empty()).then(|| manage(ManageAction::AddTag)),
        ));
        c = c.push(field_row("Tags", tags.wrap().into()));
        let e5 = e.clone();
        c = c.push(check(
            "Move its files to a new root folder",
            e.move_files,
            move |on| {
                let mut e = e5.clone();
                e.move_files = on;
                manage(ManageAction::ChangeEdit(e))
            },
        ));
        c = c.push(
            row![
                secondary("Cancel", Some(manage(ManageAction::CancelEdit))),
                primary("Save", (!m.busy).then(|| manage(ManageAction::SaveEdit))),
            ]
            .spacing(8),
        );
        container(c)
            .padding(14)
            .width(Fill)
            .style(|_| card_style(SURFACE2))
            .into()
    }

    fn manage_releases<'a>(&'a self, m: &'a Manage) -> Element<'a, Message> {
        let what = match m.looking {
            Looking::Season => format!("season {}", m.season.unwrap_or(0)),
            Looking::Episode(n) => format!("episode {n}"),
            Looking::Film => "the film".into(),
        };
        let mut head = row![weak(format!(
            "What {}'s indexers have for {what}, best first (as it would choose). Grabbing \
             one sends it to its download client.",
            m.kind.name()
        ))]
        .spacing(10)
        .align_y(Alignment::Center);
        if matches!(m.looking, Looking::Episode(_)) {
            head = head.push(small(
                "The whole season",
                Some(manage(ManageAction::Look(Looking::Season))),
            ));
        }
        let mut c = column![head].spacing(8);
        match &m.releases {
            None | Some(Loaded::Loading) => {
                c = c.push(weak("Searching its indexers (this can take a while)…"));
            }
            Some(Loaded::Failed(e)) => c = c.push(error_text(format!("Couldn't search: {e}"))),
            Some(Loaded::Ready(list)) if list.is_empty() => {
                c = c.push(weak("Its indexers have nothing for it."));
            }
            Some(Loaded::Ready(list)) => {
                for (i, r) in list.iter().enumerate() {
                    let mut facts = vec![fmt_bytes(r.size)];
                    if let Some(h) = r.age_hours {
                        facts.push(age(h));
                    }
                    if let Some(s) = r.seeders {
                        facts.push(format!(
                            "{s} seeders · {} leechers",
                            r.leechers.unwrap_or(0)
                        ));
                    }
                    if let Some(i) = &r.indexer {
                        facts.push(i.clone());
                    }
                    if !r.languages.is_empty() {
                        facts.push(r.languages.join(", "));
                    }
                    if let Some(s) = r.score.filter(|s| *s != 0) {
                        facts.push(format!("score {s:+}"));
                    }
                    let grab: Element<'_, Message> = if m.grabbed.contains(&r.guid) {
                        text("Sent ✓").size(12).font(SEMI).color(GREEN).into()
                    } else if m.grabbing.contains(&r.guid) {
                        weak("Sending…").into()
                    } else {
                        small(
                            if r.approved { "Grab" } else { "Grab anyway" },
                            Some(manage(ManageAction::Grab(i))),
                        )
                    };
                    let mut info = column![
                        row![
                            badge(
                                r.quality.as_deref().unwrap_or("?"),
                                if r.approved { GREEN } else { GOLD }
                            ),
                            text(clip(&r.title, 110)).size(13).color(TEXT),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        text(facts.join(" · ")).size(12).color(MUTED),
                    ]
                    .spacing(4)
                    .width(Fill);
                    if !r.rejections.is_empty() {
                        info = info.push(text(r.rejections.join(" · ")).size(12).color(GOLD));
                    }
                    c = c.push(
                        container(row![info, grab].spacing(12).align_y(Alignment::Center))
                            .padding([8, 12])
                            .width(Fill)
                            .style(|_| card_style(SURFACE)),
                    );
                }
            }
        }
        c.into()
    }

    fn manage_files<'a>(
        &'a self,
        m: &'a Manage,
        remote: Option<&'a Remote>,
    ) -> Element<'a, Message> {
        let list = match &m.files {
            None | Some(Loaded::Loading) => return weak("Reading its files…").into(),
            Some(Loaded::Failed(e)) => {
                return error_text(format!("Couldn't read its files: {e}")).into();
            }
            Some(Loaded::Ready(list)) => list,
        };
        // This anime's files (a season can be several anime).
        let mine: Vec<(u32, &crate::servers::arr::FileInfo)> = match remote {
            Some(r) => r
                .episodes
                .iter()
                .filter_map(|(n, e)| {
                    let id = e.file_id?;
                    Some((*n, list.iter().find(|f| f.id == id)?))
                })
                .collect(),
            None => Vec::new(),
        };
        if mine.is_empty() {
            return weak("None of its episodes have files yet.").into();
        }
        let all: Vec<u64> = mine.iter().map(|(_, f)| f.id).collect();
        let picked: Vec<u64> = all
            .iter()
            .copied()
            .filter(|id| m.picked_files.contains(id))
            .collect();
        let total: u64 = mine.iter().map(|(_, f)| f.size).sum();
        let every = picked.len() == all.len();
        let all2 = all.clone();
        let mut bar = row![
            check("Select all", every, move |on| {
                let all = all2.clone();
                edit(move |g| {
                    for id in &all {
                        g.app
                            .perform(Action::Manage(ManageAction::PickFile(*id, on)));
                    }
                })
            }),
            weak(format!(
                "{} file{} · {}",
                all.len(),
                if all.len() == 1 { "" } else { "s" },
                fmt_bytes(total)
            )),
            space::horizontal(),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if !picked.is_empty() {
            bar = bar.push(danger(
                format!("Delete {} selected…", picked.len()),
                Some(manage(ManageAction::AskDeleteFiles(picked.clone()))),
            ));
        }
        let mut c = column![bar].spacing(8);
        for (n, f) in mine {
            let mut facts = vec![fmt_bytes(f.size)];
            facts.extend(f.quality.clone());
            facts.extend(f.group.clone());
            facts.extend(f.resolution.clone());
            facts.extend(f.video.clone());
            if let Some(a) = &f.audio {
                facts.push(match &f.audio_languages {
                    Some(l) => format!("{a} ({l})"),
                    None => a.clone(),
                });
            }
            if let Some(s) = &f.subtitles {
                facts.push(format!("subtitles: {s}"));
            }
            let id = f.id;
            c = c.push(
                container(
                    row![
                        check("", m.picked_files.contains(&id), move |on| {
                            manage(ManageAction::PickFile(id, on))
                        }),
                        column![
                            row![
                                text(format!("Episode {n}")).size(13).font(SEMI).color(TEXT),
                                text(clip(f.path.rsplit('/').next().unwrap_or(&f.path), 100))
                                    .size(12)
                                    .color(MUTED),
                            ]
                            .spacing(10),
                            text(facts.join(" · ")).size(12).color(FAINT),
                        ]
                        .spacing(4)
                        .width(Fill),
                        small(
                            "Delete…",
                            Some(manage(ManageAction::AskDeleteFiles(vec![id])))
                        ),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .padding([8, 12])
                .width(Fill)
                .style(|_| card_style(SURFACE)),
            );
        }
        c.into()
    }

    fn manage_history<'a>(
        &'a self,
        m: &'a Manage,
        remote: Option<&'a Remote>,
    ) -> Element<'a, Message> {
        let list = match &m.history {
            None | Some(Loaded::Loading) => return weak("Reading its history…").into(),
            Some(Loaded::Failed(e)) => {
                return error_text(format!("Couldn't read its history: {e}")).into();
            }
            Some(Loaded::Ready(list)) => list,
        };
        // This anime's episodes (by Sonarr's IDs), numbered as AniList does.
        let number = |id: u64| {
            remote.and_then(|r| {
                r.episodes
                    .iter()
                    .find(|(_, e)| e.arr_id == Some(id))
                    .map(|(n, _)| *n)
            })
        };
        let mine: Vec<_> = list
            .iter()
            .filter(|e| m.kind == Kind::Radarr || e.episode.is_none_or(|id| number(id).is_some()))
            .collect();
        if mine.is_empty() {
            return weak("Nothing's happened to it yet.").into();
        }
        let mut c = Column::new().spacing(6);
        for e in mine.into_iter().take(100) {
            let color = match e.kind.as_str() {
                "grabbed" => ACCENT,
                k if k.contains("Imported") => GREEN,
                k if k.contains("Failed") => RED,
                _ => MUTED,
            };
            let mut line = vec![when(e.at)];
            if let Some(n) = e.episode.and_then(number) {
                line.push(format!("Episode {n}"));
            }
            line.extend(e.quality.clone());
            line.extend(e.detail.clone());
            c = c.push(
                row![
                    badge(event_name(&e.kind), color),
                    column![
                        text(clip(&e.title, 120)).size(13).color(TEXT),
                        text(line.join(" · ")).size(12).color(FAINT),
                    ]
                    .spacing(2)
                    .width(Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
        c.into()
    }

    /// An episode's controls on the servers: watched for or not, and its
    /// indexers' releases.
    pub(super) fn episode_manage<'a>(
        &'a self,
        n: u32,
    ) -> Option<(Element<'a, Message>, Element<'a, Message>)> {
        let m = self.app.manage.as_ref()?;
        if m.kind != Kind::Sonarr {
            return None;
        }
        let r = self.app.library.find(m.anilist)?.remote.as_ref()?;
        let e = r.episodes.get(&n)?;
        e.arr_id?;
        let on = e.monitored;
        let watch = with_tip(
            check("", on, move |v| {
                manage(ManageAction::SetEpisodesMonitored(vec![n], v))
            }),
            if on {
                "Sonarr watches for it (untick to stop)".into()
            } else {
                "Sonarr doesn't watch for it (tick to)".into()
            },
        );
        let open = m.episode.as_ref().is_some_and(|(e, _)| *e == n);
        let releases = row![
            small(
                if open { "Hide details" } else { "Details" },
                Some(manage(ManageAction::Episode(n)))
            ),
            small(
                "Releases…",
                Some(manage(ManageAction::Look(Looking::Episode(n)))),
            ),
        ]
        .spacing(6)
        .into();
        Some((watch, releases))
    }

    /// An episode's details, under its row: what Sonarr says of it, its
    /// file, what happened to it, and what to do with it.
    pub(super) fn episode_details(&self, n: u32) -> Option<Element<'_, Message>> {
        let m = self.app.manage.as_ref()?;
        let (open, record) = m.episode.as_ref()?;
        if *open != n {
            return None;
        }
        let r = self.app.library.find(m.anilist)?.remote.as_ref()?;
        let e = r.episodes.get(&n)?;
        let mut c = Column::new().spacing(10);
        match record {
            Loaded::Loading => c = c.push(weak("Reading it from Sonarr…")),
            Loaded::Failed(err) => c = c.push(error_text(format!("Couldn't read it: {err}"))),
            Loaded::Ready(v) => {
                let mut head = vec![format!(
                    "S{:02}E{:02}",
                    v["seasonNumber"].as_u64().unwrap_or(0),
                    v["episodeNumber"].as_u64().unwrap_or(0)
                )];
                if let Some(t) = v["title"].as_str().filter(|t| !t.is_empty()) {
                    head.push(t.to_owned());
                }
                if let Some(at) = v["airDateUtc"]
                    .as_str()
                    .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                {
                    head.push(format!("aired {}", when(at.timestamp())));
                }
                if let Some(min) = v["runtime"].as_u64().filter(|m| *m > 0) {
                    head.push(format!("{min} min"));
                }
                c = c.push(text(head.join(" · ")).size(13).font(SEMI).color(TEXT));
                if let Some(o) = v["overview"].as_str().filter(|o| !o.is_empty()) {
                    c = c.push(
                        container(text(o.to_owned()).size(13).line_height(1.5).color(MUTED))
                            .max_width(900),
                    );
                }
            }
        }
        // Its file.
        let file = e.file_id.and_then(|id| match &m.files {
            Some(Loaded::Ready(list)) => list.iter().find(|f| f.id == id),
            _ => None,
        });
        match (e.file_id, file) {
            (Some(id), Some(f)) => {
                let mut facts = vec![fmt_bytes(f.size)];
                facts.extend(f.quality.clone());
                facts.extend(f.group.clone());
                facts.extend(f.resolution.clone());
                facts.extend(f.video.clone());
                facts.extend(f.audio.clone());
                facts.extend(f.audio_languages.as_ref().map(|l| format!("audio: {l}")));
                facts.extend(f.subtitles.as_ref().map(|s| format!("subtitles: {s}")));
                c = c.push(
                    row![
                        column![
                            text(f.path.clone()).size(12).color(TEXT),
                            text(facts.join(" · ")).size(12).color(FAINT),
                        ]
                        .spacing(2)
                        .width(Fill),
                        small(
                            "Delete file…",
                            Some(manage(ManageAction::AskDeleteFiles(vec![id])))
                        ),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            (Some(_), None) => c = c.push(weak("Reading its file…")),
            (None, _) => c = c.push(weak("No file yet.")),
        }
        // What happened to it.
        if let (Some(Loaded::Ready(list)), Some(arr_id)) = (&m.history, e.arr_id) {
            let mine: Vec<_> = list.iter().filter(|h| h.episode == Some(arr_id)).collect();
            for h in mine.iter().take(8) {
                let mut line = vec![when(h.at)];
                line.extend(h.quality.clone());
                line.extend(h.detail.clone());
                c = c.push(
                    row![
                        badge(event_name(&h.kind), MUTED),
                        text(clip(&h.title, 90)).size(12).color(TEXT),
                        text(line.join(" · ")).size(12).color(FAINT),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            if mine.is_empty() {
                c = c.push(weak("Nothing's happened to it yet."));
            }
        }
        c = c.push(
            row![
                check("Watch for it", e.monitored, move |v| manage(
                    ManageAction::SetEpisodesMonitored(vec![n], v)
                )),
                small(
                    "⌕ Search for it",
                    Some(manage(ManageAction::AutoSearch(vec![n])))
                ),
                small(
                    "Releases…",
                    Some(manage(ManageAction::Look(Looking::Episode(n))))
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        Some(
            container(c)
                .padding([12, 16])
                .width(Fill)
                .style(|_| card_style(SURFACE2))
                .into(),
        )
    }
}

fn manage_renames(renames: &Loaded<Vec<crate::servers::arr::Rename>>) -> Element<'_, Message> {
    let c = match renames {
        Loaded::Loading => column![weak("Working out the new names…")],
        Loaded::Failed(e) => column![error_text(format!("Couldn't work them out: {e}"))],
        Loaded::Ready(list) if list.is_empty() => column![
            weak("Its files are named as its settings say already."),
            small("Close", Some(manage(ManageAction::CancelRenames))),
        ]
        .spacing(8),
        Loaded::Ready(list) => {
            let mut c = Column::new().spacing(6);
            for r in list {
                c = c.push(
                    column![
                        text(r.from.clone()).size(12).color(FAINT),
                        text(format!("→ {}", r.to)).size(12).color(TEXT),
                    ]
                    .spacing(2),
                );
            }
            c.push(
                row![
                    secondary("Cancel", Some(manage(ManageAction::CancelRenames))),
                    primary(
                        format!(
                            "Rename {} file{}",
                            list.len(),
                            if list.len() == 1 { "" } else { "s" }
                        ),
                        Some(manage(ManageAction::Rename)),
                    ),
                ]
                .spacing(8),
            )
        }
    };
    container(c)
        .padding(14)
        .width(Fill)
        .style(|_| card_style(SURFACE2))
        .into()
}
