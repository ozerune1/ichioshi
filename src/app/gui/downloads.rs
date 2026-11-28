//! What's downloading: from Usenet, through a debrid service, or as
//! torrents here. Finished downloads go into the library.

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Color, Element, Fill};
use librqbit::TorrentStatsState;

use super::{Gui, Message, act, edit};
use crate::app::servers_ui::ServersAction;
use crate::app::{Action, EngineState, Picked, fmt_bytes, fmt_duration, fmt_speed};
use crate::engine::TorrentHandle;
use crate::servers::arr::{Kind, Queued};

use super::style::*;

/// One download, however it's getting here.
struct Card<'a> {
    key: String,
    picked: Picked,
    source: &'static str,
    color: Color,
    name: String,
    hash: String,
    fraction: f32,
    /// What it's doing: "Downloading · 12 MB/s · 3 min left".
    detail: String,
    error: Option<String>,
    note: Option<String>,
    buttons: Vec<Element<'a, Message>>,
    /// Each file it's getting: how far along, and its path.
    files: Vec<(f32, String)>,
}

impl Gui {
    pub(super) fn downloads_page(&self) -> Element<'_, Message> {
        if self.app.servers_mode() {
            return self.server_queues();
        }
        self.own_downloads(true)
    }

    /// The servers' queues (what Sonarr's and Radarr's Queue tabs show),
    /// then whatever this computer is downloading itself.
    fn server_queues(&self) -> Element<'_, Message> {
        let s = &self.app.servers;
        let c = &s.config;
        let read = match s.queue_read {
            Some(t) => format!("read {}", super::servers::ago(t)),
            None => "reading…".into(),
        };
        let mut page_c = column![
            page_title(
                "Downloads",
                format!("{} in your servers' queues · {read}", s.queue.len())
            ),
            gap(18.0)
        ];
        for kind in [Kind::Sonarr, Kind::Radarr] {
            let set = match kind {
                Kind::Sonarr => c.sonarr.is_set(),
                Kind::Radarr => c.radarr.is_set(),
            };
            if !set {
                continue;
            }
            let items: Vec<&Queued> = s
                .queue
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, q)| q)
                .collect();
            let mut list = Column::new().spacing(10);
            if items.is_empty() {
                list = list.push(weak(format!("Nothing in {}'s queue.", kind.name())));
            } else {
                let all = items.iter().map(|q| Picked::Queued(kind, q.id)).collect();
                list = list.push(self.pick_bar(all, true));
            }
            for q in items.iter().copied() {
                list = list.push(self.queued_card(kind, q));
            }
            page_c = page_c.push(
                column![
                    row![
                        text(kind.name()).size(17).font(SEMI).color(TEXT),
                        weak(match items.len() {
                            1 => "1 in its queue".to_owned(),
                            n => format!("{n} in its queue"),
                        }),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    list
                ]
                .spacing(12),
            );
            page_c = page_c.push(gap(24.0));
        }
        if !c.any() {
            page_c = page_c.push(weak(
                "Set up Sonarr or Radarr in Settings to see their queues.",
            ));
        }
        // Episodes from Jellyfin being saved here, to watch offline.
        let saving = &self.app.offline.jobs;
        if !saving.is_empty() {
            let mut list = Column::new().spacing(10);
            for j in saving {
                list = list.push(self.offline_card(j));
            }
            page_c = page_c
                .push(
                    row![
                        text("Saving offline").size(17).font(SEMI).color(TEXT),
                        weak(format!(
                            "{} from Jellyfin · into {}",
                            saving.len(),
                            self.app.library_dir().join("Offline").display()
                        )),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .push(gap(12.0))
                .push(list)
                .push(gap(24.0));
        }
        let own = self.app.debrid.manager.views().len()
            + self.app.usenet.manager.views().len()
            + self.app.engine().map_or(0, |e| e.torrents().len());
        if own > 0 {
            page_c = page_c
                .push(
                    text(if DESKTOP {
                        "On this computer"
                    } else {
                        "On this phone"
                    })
                    .size(17)
                    .font(SEMI)
                    .color(TEXT),
                )
                .push(gap(12.0))
                .push(self.own_downloads(false));
        }
        page(page_c)
    }

    /// Something in Sonarr's or Radarr's queue: the anime and episode, how
    /// far along, and what's wrong if something is.
    fn queued_card<'a>(&'a self, kind: Kind, q: &'a Queued) -> Element<'a, Message> {
        let found = crate::servers::build::queued_episode(&self.app.library.series, kind, q);
        let series = found.map(|(s, _)| s);
        let cover = self.images.get(series.and_then(|s| {
            s.cover
                .as_deref()
                .or(s.details.as_ref().and_then(|d| d.cover.as_deref()))
        }));
        let title = series
            .map(|s| s.title().to_owned())
            .or_else(|| q.show.clone())
            .unwrap_or_else(|| q.title.clone());
        let which = match (found, kind) {
            (_, Kind::Radarr) => "Film".to_owned(),
            (Some((_, n)), _) => format!("Episode {n}"),
            (None, _) => match (q.season, q.number) {
                (Some(s), Some(e)) => format!("S{s:02}E{e:02}"),
                _ => "Episode".to_owned(),
            },
        };
        let which = match &q.episode_title {
            Some(t) if kind == Kind::Sonarr => format!("{which} · {t}"),
            _ => which,
        };
        let (status, color) = queue_status(q);
        let mut detail = vec![format!("{:.0}%", q.fraction() * 100.0), status.clone()];
        if let Some(t) = q.time_left.as_deref().and_then(time_left)
            && q.left > 0.0
        {
            detail.push(t);
        }
        if q.size > 0.0 {
            detail.push(fmt_bytes(q.size as u64));
        }
        if let Some(quality) = &q.quality {
            detail.push(quality.clone());
        }
        if let Some(client) = &q.client {
            detail.push(match q.protocol.as_str() {
                "" => client.clone(),
                p => format!("{client} ({p})"),
            });
        }
        let head = row![
            badge(kind.name(), color),
            text(clip(&title, 90)).size(15).font(SEMI).color(TEXT),
            desktop_layout(text(which.clone()).size(13).color(MUTED)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        // A phone's: the episode on a line of its own.
        let head: Element<'_, Message> = if compact() {
            column![head, text(which).size(13).color(MUTED)]
                .spacing(4)
                .into()
        } else {
            head.into()
        };
        let mut info = column![
            head,
            bar(q.fraction(), color, 8.0),
            text(detail.join(" · ")).size(12).color(MUTED),
            text(clip(&q.title, 140)).size(12).color(FAINT),
        ]
        .spacing(8)
        .width(Fill);
        if let Some(p) = &q.problem {
            info = info.push(
                text(p.clone())
                    .size(13)
                    .color(if color == RED { RED } else { GOLD }),
            );
        }
        let mut buttons = Row::new().spacing(6).align_y(Alignment::Center);
        if let Some(s) = series {
            buttons = buttons.push(small(
                "Open series",
                Some(act(Action::OpenSeries(s.anilist_id))),
            ));
        }
        // Finished but not imported (or stuck): import it by hand.
        if let Some(d) = q
            .download_id
            .clone()
            .filter(|_| q.state.starts_with("import") || q.problem.is_some())
        {
            buttons = buttons.push(small(
                "Import…",
                Some(act(Action::Import(
                    crate::app::import_ui::ImportAction::Open {
                        kind,
                        owner: q.owner,
                        anilist: series.map(|s| s.anilist_id),
                        download: Some(d),
                    },
                ))),
            ));
        }
        buttons = buttons.push(small(
            "Remove…",
            Some(act(Action::Servers(ServersAction::AskRemoveQueued {
                kind,
                id: q.id,
                title: q.title.clone(),
            }))),
        ));
        info = info.push(buttons);
        container(
            row![
                self.pick_box(Picked::Queued(kind, q.id)),
                picture(cover, 54.0, 76.0, 8.0),
                info
            ]
            .spacing(16)
            .align_y(Alignment::Start),
        )
        .padding(16)
        .width(Fill)
        .style(|_| card_style(SURFACE))
        .into()
    }

    /// An episode being saved offline: how far, and stopping it.
    fn offline_card<'a>(
        &'a self,
        j: &'a crate::app::offline_ui::OfflineJob,
    ) -> Element<'a, Message> {
        use crate::app::offline_ui::{OfflineAction, Saving};
        let series = self.app.library.find(j.anilist);
        let cover = self.images.get(series.and_then(|s| {
            s.cover
                .as_deref()
                .or(s.details.as_ref().and_then(|d| d.cover.as_deref()))
        }));
        let len = j.len.load(std::sync::atomic::Ordering::Relaxed);
        let have = j.have.load(std::sync::atomic::Ordering::Relaxed);
        let speed = *j.speed.lock().unwrap();
        let (status, color) = match &j.state {
            Saving::Waiting => ("Waiting its turn".to_owned(), MUTED),
            Saving::Getting if len == 0 => ("Asking Jellyfin for it…".to_owned(), ACCENT),
            Saving::Getting => {
                let mut s = format!(
                    "{:.0}% · {} of {}",
                    j.fraction() * 100.0,
                    fmt_bytes(have),
                    fmt_bytes(len)
                );
                if speed > 0.0 {
                    s += &format!(
                        " · {} · {} left",
                        fmt_speed(speed / 1_048_576.0),
                        fmt_duration((len - have.min(len)) as f64 / speed)
                    );
                }
                (s, ACCENT)
            }
            Saving::Failed(e) => (format!("Couldn't save it: {e}"), RED),
        };
        let mut buttons = Row::new().spacing(6);
        if matches!(j.state, Saving::Failed(_)) {
            buttons = buttons.push(small(
                "Retry",
                Some(act(Action::Offline(OfflineAction::Retry(j.id)))),
            ));
        }
        buttons = buttons.push(small(
            "Stop",
            Some(act(Action::Offline(OfflineAction::Cancel(j.id)))),
        ));
        container(
            row![
                picture(cover, 40.0, 56.0, 6.0),
                column![
                    row![
                        badge("Jellyfin", ACCENT),
                        text(clip(&j.title, 90)).size(15).font(SEMI).color(TEXT),
                        text(format!("Episode {}", j.episode)).size(13).color(MUTED),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    bar(j.fraction(), color, 6.0),
                    text(status)
                        .size(12)
                        .color(if color == RED { RED } else { MUTED }),
                    buttons,
                ]
                .spacing(8)
                .width(Fill),
            ]
            .spacing(16),
        )
        .padding(16)
        .width(Fill)
        .style(|_| card_style(SURFACE))
        .into()
    }

    /// What this computer is downloading itself (with the page's title,
    /// unless it's under the servers' queues).
    fn own_downloads(&self, titled: bool) -> Element<'_, Message> {
        let a = &self.app;
        let jobs = a.debrid.manager.views();
        let usenet_jobs = a.usenet.manager.views();
        let streams = a.streams.views();
        let torrents = a.engine().map(|e| e.torrents()).unwrap_or_default();
        let active = a.active_downloads();
        let order = match (a.usenet.active(), a.debrid.service()) {
            (true, Some(s)) => format!("from Usenet first, then {s}"),
            (true, None) => "from Usenet first, then BitTorrent".to_owned(),
            (false, Some(s)) => format!("through {s}"),
            (false, None) => "as torrents · never seeded, except to a watch party".to_owned(),
        };
        let mut c = if titled {
            column![
                page_title("Downloads", format!("{active} active · {order}")),
                gap(18.0)
            ]
        } else {
            column![]
        }
        .spacing(0);

        match &a.engine {
            EngineState::Starting => c = c.push(weak("Restoring downloads…")),
            EngineState::Failed(e) => {
                c = c.push(error_text(format!("The torrent engine failed: {e}")))
            }
            EngineState::WaitingForVpn(reason) => c = c.push(weak(reason.clone())),
            _ => {}
        }
        if let Some(reason) = &a.vpn.waiting {
            if !a.debrid.config.enabled {
                c = c.push(self.vpn_banner(reason)).push(gap(12.0));
            } else if torrents.iter().any(|(_, h)| !h.stats().finished) {
                c = c.push(weak(
                    "Unfinished torrents from before carry on once your VPN connects.",
                ));
            }
        }
        if jobs.is_empty() && usenet_jobs.is_empty() && torrents.is_empty() && streams.is_empty() {
            if !matches!(
                a.engine,
                EngineState::Starting | EngineState::WaitingForVpn(_)
            ) {
                c = c.push(
                    container(
                        column![
                            text("Nothing downloading").size(20).font(BOLD).color(TEXT),
                            weak("Finished downloads go into your library."),
                        ]
                        .spacing(6)
                        .align_x(Alignment::Center),
                    )
                    .center_x(Fill)
                    .padding([100, 0]),
                );
            }
            return if titled { page(c) } else { c.into() };
        }
        let mut list = Column::new().spacing(12);
        let mut all = Vec::new();
        all.extend(usenet_jobs.iter().map(|v| Picked::Usenet(v.job.id)));
        all.extend(jobs.iter().map(|v| Picked::Debrid(v.job.id)));
        all.extend(streams.iter().map(|v| Picked::Stream(v.job.id)));
        all.extend(torrents.iter().rev().map(|(id, _)| Picked::Torrent(*id)));
        list = list.push(self.pick_bar(all, false));
        for v in &usenet_jobs {
            list = list.push(self.download_card(usenet_card(v)));
        }
        for v in &jobs {
            list = list.push(self.download_card(debrid_card(v)));
        }
        for v in &streams {
            list = list.push(self.download_card(stream_card(v)));
        }
        for (id, handle) in torrents.iter().rev() {
            list = list.push(self.download_card(torrent_card(*id, handle)));
        }
        let c = c.push(list);
        if titled { page(c) } else { c.into() }
    }

    fn download_card<'a>(&'a self, d: Card<'a>) -> Element<'a, Message> {
        let series = self
            .app
            .library
            .series
            .iter()
            .find(|s| s.downloads.contains(&d.hash));
        let cover = self.images.get(series.and_then(|s| {
            s.cover
                .as_deref()
                .or(s.details.as_ref().and_then(|d| d.cover.as_deref()))
        }));
        let color = d.color;
        let mut info = column![
            row![
                badge(d.source, color),
                text(clip(&d.name, 110)).size(15).font(SEMI).color(TEXT),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            bar(d.fraction, color, 8.0),
            row![
                text(format!("{:.0}%", d.fraction * 100.0))
                    .size(12)
                    .font(SEMI)
                    .color(TEXT),
                text(d.detail).size(12).color(MUTED),
            ]
            .spacing(10),
        ]
        .spacing(9)
        .width(Fill);
        if let Some(e) = d.error {
            info = info.push(error_text(e));
        }
        if let Some(n) = d.note {
            info = info.push(text(n).size(13).color(GOLD));
        }
        let open = self.expanded.contains(&d.key);
        let key = d.key.clone();
        let mut buttons = Row::with_children(d.buttons)
            .spacing(6)
            .align_y(Alignment::Center);
        if !d.files.is_empty() {
            buttons = buttons.push(space::horizontal()).push(link(
                if open {
                    "Hide files".to_owned()
                } else {
                    format!("Files ({})", d.files.len())
                },
                Some(edit(move |g| {
                    if !g.expanded.remove(&key) {
                        g.expanded.insert(key.clone());
                    }
                })),
            ));
        }
        info = info.push(buttons);
        if open {
            let mut files = Column::new().spacing(4);
            for (fraction, path) in d.files {
                files = files.push(
                    row![
                        bar(fraction, color, 4.0).length(80),
                        text(format!("{:.0}%", fraction * 100.0))
                            .size(11)
                            .color(FAINT)
                            .width(36),
                        text(path).size(12).color(MUTED),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            info = info.push(files);
        }
        container(
            row![
                self.pick_box(d.picked),
                picture(cover, 54.0, 76.0, 8.0),
                info
            ]
            .spacing(16)
            .align_y(Alignment::Start),
        )
        .padding(16)
        .width(Fill)
        .style(|_| card_style(SURFACE))
        .into()
    }

    /// Torrents waiting for the VPN, and how to connect it.
    pub(super) fn vpn_banner<'a>(&'a self, reason: &'a str) -> Element<'a, Message> {
        let mut c = column![
            text("Torrents are waiting for the VPN")
                .size(15)
                .font(SEMI)
                .color(GOLD),
        ]
        .spacing(6);
        if !reason.is_empty() {
            c = c.push(body(reason));
        }
        c = c.push(weak(
            "They continue as soon as it connects. Nothing goes out without it.",
        ));
        if self.app.vpn.monitor.status().fix() == Some(crate::vpn::Fix::PiaConnect) {
            c = c.push(secondary(
                "Connect PIA",
                (!self.app.vpn.busy).then(|| act(Action::VpnFix(crate::vpn::Fix::PiaConnect))),
            ));
        }
        container(c)
            .padding(16)
            .width(Fill)
            .style(|_| container::Style {
                border: iced::Border {
                    color: GOLD.scale_alpha(0.4),
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..card_style(GOLD.scale_alpha(0.06))
            })
            .into()
    }
}

impl Gui {
    /// A download's checkbox (to remove several together).
    fn pick_box(&self, p: Picked) -> Element<'_, Message> {
        let on = self.picked.contains(&p);
        check("", on, move |v| {
            let p = p.clone();
            edit(move |g| {
                if v {
                    g.picked.insert(p.clone());
                } else {
                    g.picked.remove(&p);
                }
            })
        })
    }

    /// Above a list of downloads: pick them all, and remove the picked
    /// ones (queue items: from the queue; downloads here: keeping or
    /// deleting their files).
    fn pick_bar(&self, all: Vec<Picked>, queue: bool) -> Element<'_, Message> {
        let picked: Vec<Picked> = all
            .iter()
            .filter(|p| self.picked.contains(*p))
            .cloned()
            .collect();
        let every = !all.is_empty() && picked.len() == all.len();
        let toggle = all.clone();
        let mut bar = row![check("Select all", every, move |on| {
            let toggle = toggle.clone();
            edit(move |g| {
                for p in &toggle {
                    if on {
                        g.picked.insert(p.clone());
                    } else {
                        g.picked.remove(p);
                    }
                }
            })
        })]
        .spacing(10)
        .align_y(Alignment::Center);
        if picked.is_empty() {
            return bar.into();
        }
        bar = bar.push(weak(format!("{} selected", picked.len())));
        bar = bar.push(space::horizontal());
        if queue {
            bar = bar.push(outlined_red(
                "Remove selected…",
                act(Action::AskRemoveMany(picked.clone())),
            ));
        } else {
            bar = bar
                .push(small(
                    "Remove selected (keep files)",
                    Some(act(Action::RemoveMany {
                        items: picked.clone(),
                        delete_files: false,
                        blocklist: false,
                    })),
                ))
                .push(outlined_red(
                    "Delete selected…",
                    act(Action::AskRemoveMany(picked.clone())),
                ));
        }
        bar.into()
    }
}

/// What a queue item is doing, as Sonarr's Queue tab says it, and its
/// color.
fn queue_status(q: &Queued) -> (String, Color) {
    let failed = q.state.starts_with("failed") || q.status == "failed";
    let s = match (q.state.as_str(), q.status.as_str()) {
        _ if failed => "Failed",
        ("importPending", _) => "Waiting to import",
        ("importing", _) => "Importing",
        ("imported", _) => "Imported",
        (_, "queued") => "Queued",
        (_, "paused") => "Paused",
        (_, "delay") => "Delayed (by a delay profile)",
        (_, "completed") => "Downloaded",
        (_, "warning") => "Downloading, with a warning",
        _ => "Downloading",
    };
    let color = if failed {
        RED
    } else if q.problem.is_some() || q.state == "importPending" {
        GOLD
    } else {
        GREEN
    };
    (s.to_owned(), color)
}

/// "00:12:34" (or "1.02:03:04", with days) as "12 min left".
fn time_left(t: &str) -> Option<String> {
    let (days, hms) = match t.split_once('.') {
        Some((d, rest)) if !d.contains(':') => (d.parse::<u64>().ok()?, rest),
        _ => (0, t),
    };
    let mut p = hms.split(':').map(|x| x.parse::<f64>().ok());
    let (h, m, s) = (p.next()??, p.next()??, p.next()??);
    let secs = days as f64 * 86_400.0 + h * 3600.0 + m * 60.0 + s;
    Some(format!("{} left", fmt_duration(secs)))
}

/// Pause or resume, files, remux, folder, remove (keeping or deleting).
#[allow(clippy::too_many_arguments)]
fn buttons<'a>(
    pause: Option<Message>,
    resume: Option<(&'static str, Message)>,
    episodes: Message,
    remux: Message,
    folder: std::path::PathBuf,
    remove: Message,
    delete: Message,
) -> Vec<Element<'a, Message>> {
    let mut b = Vec::new();
    if let Some(m) = pause {
        b.push(small("Pause", Some(m)));
    }
    if let Some((label, m)) = resume {
        b.push(small(label, Some(m)));
    }
    b.push(small("Episodes…", Some(episodes)));
    b.push(small("Remux…", Some(remux)));
    if DESKTOP {
        b.push(small("Open folder", Some(act(Action::OpenFolder(folder)))));
    }
    b.push(small("Remove", Some(remove)));
    b.push(outlined_red("Delete…", delete));
    b
}

fn outlined_red<'a>(label: &'a str, m: Message) -> Element<'a, Message> {
    iced::widget::button(text(label).size(12).font(MEDIUM).color(RED))
        .padding([5, 10])
        .on_press(m)
        .style(|_, status| iced::widget::button::Style {
            background: Some(fill_color(
                if matches!(status, iced::widget::button::Status::Hovered) {
                    RED.scale_alpha(0.12)
                } else {
                    SURFACE
                },
            )),
            text_color: RED,
            border: iced::Border {
                color: LINE,
                width: 1.0,
                radius: 9.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn torrent_card<'a>(id: crate::engine::TorrentId, handle: &TorrentHandle) -> Card<'a> {
    let stats = handle.stats();
    let name = handle.name().unwrap_or_else(|| format!("Torrent #{id}"));
    // Finished torrents are stopped by the engine right away (no seeding).
    let state = match &stats.state {
        TorrentStatsState::Initializing { .. } => "Checking files",
        _ if stats.finished => "Done · not seeding",
        TorrentStatsState::Live => "Downloading",
        TorrentStatsState::Paused => "Paused",
        TorrentStatsState::Error => "Error",
    };
    let fraction = if stats.total_bytes == 0 {
        0.0
    } else {
        stats.progress_bytes as f32 / stats.total_bytes as f32
    };
    let mut detail = format!(
        "{} of {}  ·  {state}",
        fmt_bytes(stats.progress_bytes),
        fmt_bytes(stats.total_bytes)
    );
    if let Some(live) = &stats.live
        && !stats.finished
    {
        detail.push_str(&format!("  ·  {}", fmt_speed(live.download_speed.mbps)));
        if let Some(eta) = &live.time_remaining {
            detail.push_str(&format!("  ·  {eta} left"));
        }
        detail.push_str(&format!("  ·  {} peers", live.snapshot.peer_stats.live));
    }
    let (pause, resume) = match stats.state {
        _ if stats.finished => (None, None),
        TorrentStatsState::Live => (Some(act(Action::Pause(handle.clone()))), None),
        TorrentStatsState::Paused | TorrentStatsState::Error => {
            (None, Some(("Resume", act(Action::Resume(handle.clone())))))
        }
        TorrentStatsState::Initializing { .. } => (None, None),
    };
    let only = handle.only_files();
    let mut files = Vec::new();
    let _ = handle.with_metadata(|m| {
        for (i, file) in m.file_infos.iter().enumerate() {
            if file.attrs.padding || only.as_ref().is_some_and(|o| !o.contains(&i)) {
                continue;
            }
            let have = stats.file_progress.get(i).copied().unwrap_or(0);
            let fraction = if file.len == 0 {
                1.0
            } else {
                have as f32 / file.len as f32
            };
            files.push((fraction, file.relative_filename.display().to_string()));
        }
    });
    Card {
        key: format!("t{id}"),
        picked: Picked::Torrent(id),
        source: "BitTorrent",
        color: BLUE,
        hash: handle.info_hash().as_string().to_ascii_lowercase(),
        fraction,
        detail,
        error: stats.error.clone(),
        note: None,
        buttons: buttons(
            pause,
            resume,
            act(Action::EditFiles(handle.clone())),
            act(Action::OpenRemux(handle.clone())),
            handle.output_folder().to_owned(),
            act(Action::Remove {
                id,
                delete_files: false,
            }),
            act(Action::AskDelete {
                id,
                name: name.clone(),
            }),
        ),
        name,
        files,
    }
}

/// An 18+ episode from a streaming site.
fn stream_card<'a>(v: &crate::streams::View) -> Card<'a> {
    use crate::streams::Phase;
    let job = &v.job;
    let id = job.id;
    let site = job.episode.site.name();
    let have = v.live.done;
    let sizes = match v.live.total {
        Some(t) => format!("{} of {}", fmt_bytes(have), fmt_bytes(t)),
        None => fmt_bytes(have),
    };
    let detail = match &v.live.phase {
        Phase::Queued => "Waiting for the episodes before it".to_owned(),
        Phase::Starting => format!("Reading its page on {site}…"),
        Phase::Downloading => {
            let mut d = format!("{sizes}  ·  Downloading from {site}");
            if v.live.speed > 0.0 {
                d.push_str(&format!("  ·  {}", fmt_speed(v.live.speed / 1_048_576.0)));
                if let Some(t) = v.live.total {
                    let left = t.saturating_sub(have) as f64 / v.live.speed;
                    d.push_str(&format!("  ·  {} left", fmt_duration(left)));
                }
            }
            d
        }
        Phase::Done => format!("{sizes}  ·  Done · from {site}"),
        Phase::Paused => format!("{sizes}  ·  Paused"),
        Phase::Failed(_) => sizes,
    };
    let mut b = Vec::new();
    match &v.live.phase {
        Phase::Queued | Phase::Starting | Phase::Downloading => {
            b.push(small("Pause", Some(act(Action::StreamPause(id)))))
        }
        Phase::Paused => b.push(small("Resume", Some(act(Action::StreamResume(id))))),
        Phase::Failed(_) => b.push(small("Retry", Some(act(Action::StreamResume(id))))),
        Phase::Done => {}
    }
    if DESKTOP {
        b.push(small(
            "Open folder",
            Some(act(Action::OpenFolder(job.folder.clone()))),
        ));
    }
    b.push(small(
        "Remove",
        Some(act(Action::StreamRemove {
            id,
            delete_files: false,
        })),
    ));
    let fraction = v.live.fraction().unwrap_or(0.0);
    Card {
        key: format!("s{id}"),
        picked: Picked::Stream(id),
        source: site,
        color: ACCENT,
        name: job.stem(),
        hash: String::new(),
        fraction,
        detail,
        error: match &v.live.phase {
            Phase::Failed(e) => Some(e.clone()),
            _ => None,
        },
        note: None,
        buttons: b,
        files: vec![(fraction, job.episode.title.clone())],
    }
}

fn debrid_card<'a>(v: &crate::debrid::jobs::View) -> Card<'a> {
    use crate::debrid::jobs::Phase;
    let job = &v.job;
    let service = job.provider.name();
    let total = job.wanted_bytes();
    let have = v.have_bytes();
    let local = if total == 0 {
        0.0
    } else {
        have as f32 / total as f32
    };
    let sizes = format!("{} of {}", fmt_bytes(have), fmt_bytes(total));
    let (fraction, detail) = match &v.live.phase {
        Phase::Adding => (0.0, format!("Sending it to {service}…")),
        Phase::Remote {
            status,
            progress,
            speed,
            seeders,
        } => {
            let mut d = format!("{service} is getting it");
            if !status.is_empty() {
                d.push_str(&format!(" · {status}"));
            }
            if let Some(s) = speed.filter(|s| *s > 0) {
                d.push_str(&format!("  ·  {}", fmt_speed(s as f64 / 1_048_576.0)));
            }
            if let Some(n) = seeders {
                d.push_str(&format!("  ·  {n} seeders"));
            }
            (progress.unwrap_or(0.0), d)
        }
        Phase::Downloading => {
            let mut d = format!("{sizes}  ·  Downloading from {service}");
            if v.live.speed > 0.0 {
                d.push_str(&format!("  ·  {}", fmt_speed(v.live.speed / 1_048_576.0)));
                let left = total.saturating_sub(have) as f64 / v.live.speed;
                d.push_str(&format!("  ·  {} left", fmt_duration(left)));
            }
            (local, d)
        }
        Phase::Done => (local, format!("{sizes}  ·  Done · via {service}")),
        Phase::Paused => (local, format!("{sizes}  ·  Paused")),
        Phase::Failed(_) => (local, sizes),
    };
    let (pause, resume) = match &v.live.phase {
        Phase::Adding | Phase::Remote { .. } | Phase::Downloading => {
            (Some(act(Action::DebridPause(job.id))), None)
        }
        Phase::Paused => (None, Some(("Resume", act(Action::DebridResume(job.id))))),
        Phase::Failed(_) => (None, Some(("Retry", act(Action::DebridResume(job.id))))),
        Phase::Done => (None, None),
    };
    let files = job
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.wanted)
        .map(|(i, f)| {
            let fraction = if f.len == 0 {
                1.0
            } else {
                v.live.have[i] as f32 / f.len as f32
            };
            (fraction, f.path.clone())
        })
        .collect();
    Card {
        key: format!("d{}", job.id),
        picked: Picked::Debrid(job.id),
        source: service,
        color: ACCENT,
        name: job.name.clone(),
        hash: job.info_hash.clone(),
        fraction,
        detail,
        error: match &v.live.phase {
            Phase::Failed(e) => Some(e.clone()),
            _ => None,
        },
        note: v.live.note.clone(),
        buttons: buttons(
            pause,
            resume,
            act(Action::DebridEditFiles(job.id)),
            act(Action::DebridOpenRemux(job.id)),
            job.output_folder.clone(),
            act(Action::DebridRemove {
                id: job.id,
                delete_files: false,
            }),
            act(Action::DebridAskDelete {
                id: job.id,
                name: job.name.clone(),
            }),
        ),
        files,
    }
}

fn usenet_card<'a>(v: &crate::usenet::jobs::View) -> Card<'a> {
    use crate::usenet::jobs::Phase;
    let job = &v.job;
    let total = job.wanted_bytes();
    let have = v.have_bytes().min(total);
    let source = v.source().unwrap_or("Usenet");
    let fraction = if total == 0 {
        0.0
    } else {
        have as f32 / total as f32
    };
    let sizes = format!("{} of {}", fmt_bytes(have), fmt_bytes(total));
    let detail = match &v.live.phase {
        Phase::Queued => format!("{sizes}  ·  Waiting for the download before it"),
        Phase::Fetching => format!("Getting the NZB from {source}"),
        Phase::Downloading => {
            let mut d = format!("{sizes}  ·  Downloading from Usenet ({source})");
            if v.live.speed > 0.0 {
                d.push_str(&format!("  ·  {}", fmt_speed(v.live.speed / 1_048_576.0)));
                d.push_str(&format!(
                    "  ·  {} left",
                    fmt_duration(total.saturating_sub(have) as f64 / v.live.speed)
                ));
            }
            d
        }
        Phase::Repairing(what) => format!("Repairing with PAR2: {what}"),
        Phase::Done => format!("{sizes}  ·  Done · from Usenet ({source})"),
        Phase::Paused => format!("{sizes}  ·  Paused"),
        Phase::Failed(_) => sizes,
    };
    let mut note = v.live.note.clone();
    if v.live.missing > 0 && !matches!(v.live.phase, Phase::Done) {
        note = Some(format!(
            "{} articles aren't on any of your providers; they'll be repaired with PAR2",
            v.live.missing
        ));
    }
    let (pause, resume) = match &v.live.phase {
        Phase::Queued | Phase::Fetching | Phase::Downloading | Phase::Repairing(_) => {
            (Some(act(Action::UsenetPause(job.id))), None)
        }
        Phase::Paused => (None, Some(("Resume", act(Action::UsenetResume(job.id))))),
        Phase::Failed(_) => (None, Some(("Retry", act(Action::UsenetResume(job.id))))),
        Phase::Done => (None, None),
    };
    let files = job
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.wanted)
        .map(|(i, f)| {
            let fraction = if f.len == 0 {
                1.0
            } else {
                (v.live.have[i] as f32 / f.len as f32).min(1.0)
            };
            (fraction, f.path.clone())
        })
        .collect();
    Card {
        key: format!("u{}", job.id),
        picked: Picked::Usenet(job.id),
        source: "Usenet",
        color: GREEN,
        name: job.name.clone(),
        hash: job.info_hash.clone(),
        fraction,
        detail,
        error: match &v.live.phase {
            Phase::Failed(e) => Some(e.clone()),
            _ => None,
        },
        note,
        buttons: buttons(
            pause,
            resume,
            act(Action::UsenetEditFiles(job.id)),
            act(Action::UsenetOpenRemux(job.id)),
            job.output_folder.clone(),
            act(Action::UsenetRemove {
                id: job.id,
                delete_files: false,
            }),
            act(Action::UsenetAskDelete {
                id: job.id,
                name: job.name.clone(),
            }),
        ),
        files,
    }
}
