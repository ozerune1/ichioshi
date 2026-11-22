//! What's downloading: from Usenet, through a debrid service, or as
//! torrents here. Finished downloads go into the library.

use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Color, Element, Fill};
use librqbit::TorrentStatsState;

use super::{Gui, Message, act, edit};
use crate::app::{Action, EngineState, fmt_bytes, fmt_duration, fmt_speed};
use crate::engine::TorrentHandle;

use super::style::*;

/// One download, however it's getting here.
struct Card<'a> {
    key: String,
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
        let a = &self.app;
        let jobs = a.debrid.manager.views();
        let usenet_jobs = a.usenet.manager.views();
        let torrents = a.engine().map(|e| e.torrents()).unwrap_or_default();
        let active = a.active_downloads();
        let order = match (a.usenet.active(), a.debrid.service()) {
            (true, Some(s)) => format!("from Usenet first, then {s}"),
            (true, None) => "from Usenet first, then BitTorrent".to_owned(),
            (false, Some(s)) => format!("through {s}"),
            (false, None) => "as torrents · nothing is ever seeded".to_owned(),
        };
        let mut c = column![
            page_title("Downloads", format!("{active} active · {order}")),
            gap(18.0)
        ]
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
        if jobs.is_empty() && usenet_jobs.is_empty() && torrents.is_empty() {
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
            return page(c);
        }
        let mut list = Column::new().spacing(12);
        for v in &usenet_jobs {
            list = list.push(self.download_card(usenet_card(v)));
        }
        for v in &jobs {
            list = list.push(self.download_card(debrid_card(v)));
        }
        for (id, handle) in torrents.iter().rev() {
            list = list.push(self.download_card(torrent_card(*id, handle)));
        }
        page(c.push(list))
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
            row![picture(cover, 54.0, 76.0, 8.0), info]
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
    b.push(small("Open folder", Some(act(Action::OpenFolder(folder)))));
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
