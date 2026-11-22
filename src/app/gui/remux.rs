//! Remuxing: copies of a series' episodes that keep only the audio and
//! subtitles you pick, named after the anime.

use std::path::PathBuf;

use iced::widget::{Column, column, container, mouse_area, row, space, text};
use iced::{Alignment, Element, Fill, Length};

use super::series::{header_row, with_tip};
use super::{Gui, Message, act, edit};
use crate::app::remux_page::{FileState, RemuxPage, default_out_dir};
use crate::app::{Action, Page};
use crate::remux::{Analysis, Kind, PlannedFile, language_name};

use super::style::*;

/// Change the remux page's state, if it's still showing.
fn on_remux(f: impl Fn(&mut RemuxPage) + Send + Sync + 'static) -> Message {
    edit(move |g| {
        if let Page::Remux(rp) = &mut g.app.page {
            f(rp);
        }
    })
}

/// A language to pick (or none), labeled with how many episodes have it.
#[derive(Debug, Clone, PartialEq)]
struct LangPick {
    code: Option<String>,
    label: String,
}

impl std::fmt::Display for LangPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl Gui {
    pub(super) fn remux_page<'a>(&'a self, rp: &'a RemuxPage) -> Element<'a, Message> {
        let locked = rp.running();
        let ready = rp.series.files.len();
        let mut summary = format!("{ready} finished episode{}", plural(ready));
        if rp.series.not_ready > 0 {
            summary.push_str(&format!(
                " · {} still downloading (not included)",
                rp.series.not_ready
            ));
        }
        let mut c = column![
            link("← Back", (!locked).then(|| act(Action::Back))),
            page_title("Remux", format!("{} · {summary}", rp.series.name)),
            weak(
                "Copies keep only the audio and subtitles you pick; video and chapters are kept. \
                 The downloads stay as they are."
            ),
        ]
        .spacing(10);
        if ready == 0 {
            return page(c.push(body(
                "Nothing to remux yet: wait for an episode to finish downloading.",
            )));
        }
        let note = rp.title_note.trim_end_matches('.').to_owned();
        let naming = card(
            column![
                row![
                    text("Title").size(14).color(MUTED).width(80),
                    field("English title", &rp.title)
                        .on_input_maybe((!locked).then_some(|t: String| edit(move |g| {
                            let dir = g.app.library_dir();
                            if let Page::Remux(rp) = &mut g.app.page {
                                rp.title = t.clone();
                                if !rp.out_dir_edited {
                                    rp.out_dir = default_out_dir(&dir, &rp.title);
                                }
                            }
                        })))
                        .width(Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                row![
                    space().width(80),
                    weak(format!(
                        "{note}. Files are named “<title> - 01.mkv” and titled the same."
                    ))
                ]
                .spacing(10),
                row![
                    text("Save to").size(14).color(MUTED).width(80),
                    field("Folder", &rp.out_dir)
                        .on_input_maybe((!locked).then_some(|t: String| on_remux(move |rp| {
                            rp.out_dir = t.clone();
                            rp.out_dir_edited = true;
                        })))
                        .width(Fill),
                    secondary("Browse…", (!locked).then(|| act(Action::PickRemuxFolder))),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            ]
            .spacing(10),
        );
        c = c.push(gap(8.0)).push(naming);
        if let Some(e) = &rp.error {
            return page(c.push(error_text(format!(
                "Couldn't read the episodes' tracks: {e}"
            ))));
        }
        let Some(analysis) = &rp.analysis else {
            return page(c.push(weak("Reading the episodes' tracks…")));
        };
        if !analysis.unreadable.is_empty() {
            let names: Vec<String> = analysis
                .unreadable
                .iter()
                .map(|(p, _)| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            let why = analysis
                .unreadable
                .iter()
                .map(|(_, e)| e.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            c = c.push(with_tip(
                text(format!(
                    "Can't remux {} (only MKV files are supported): {}",
                    names.len(),
                    names.join(", ")
                ))
                .size(13)
                .color(GOLD)
                .into(),
                why,
            ));
        }
        let tracks = card(
            column![
                text("Audio").size(16).font(SEMI).color(TEXT),
                language_pick(analysis, Kind::Audio, rp.audio_lang.clone(), locked),
                track_choice(rp, analysis, Kind::Audio, locked),
                gap(6.0),
                text("Subtitles").size(16).font(SEMI).color(TEXT),
                language_pick(analysis, Kind::Subtitles, rp.sub_lang.clone(), locked),
                track_choice(rp, analysis, Kind::Subtitles, locked),
            ]
            .spacing(10),
        );
        let plan = rp.plan();
        c = c
            .push(tracks)
            .push(remux_controls(rp, &plan))
            .push(episode_table(rp, &plan));
        page(c)
    }
}

fn language_pick<'a>(
    analysis: &Analysis,
    kind: Kind,
    selected: Option<String>,
    locked: bool,
) -> Element<'a, Message> {
    let total = analysis.files.len();
    let langs = analysis.languages(kind);
    let label = |code: &str| {
        let files = langs.iter().find(|l| l.code == code).map_or(0, |l| l.files);
        if files == total {
            language_name(code)
        } else {
            format!("{} ({files} of {total} episodes)", language_name(code))
        }
    };
    let mut options = Vec::new();
    if kind == Kind::Subtitles {
        options.push(LangPick {
            code: None,
            label: "None (no subtitles)".into(),
        });
    }
    options.extend(langs.iter().map(|l| LangPick {
        code: Some(l.code.clone()),
        label: label(&l.code),
    }));
    let current = options.iter().find(|o| o.code == selected).cloned();
    row![
        text("Language").size(13).color(MUTED).width(80),
        dropdown(options, current, move |o: LangPick| {
            if locked {
                return Message::Wake;
            }
            let code = o.code;
            on_remux(move |rp| match kind {
                Kind::Audio => {
                    rp.audio_lang = code.clone();
                    rp.refresh_audio_track();
                }
                _ => {
                    rp.sub_lang = code.clone();
                    rp.refresh_subtitle_track();
                }
            })
        }),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// When a language has several tracks, which to keep. Subtitle tracks show
/// how many lines they have and how they start, which tells "signs only"
/// from full dialogue better than their names do.
fn track_choice<'a>(
    rp: &'a RemuxPage,
    analysis: &Analysis,
    kind: Kind,
    locked: bool,
) -> Element<'a, Message> {
    let (lang, chosen) = match kind {
        Kind::Audio => (rp.audio_lang.as_deref(), rp.audio.as_ref()),
        _ => (rp.sub_lang.as_deref(), rp.subs.as_ref()),
    };
    let Some(lang) = lang else {
        return space().into();
    };
    let candidates = analysis.candidates(kind, lang);
    if candidates.len() < 2 {
        return space().into();
    }
    let total = analysis.files.len();
    let mut c = Column::new().spacing(6).push(weak(format!(
        "{} has {} tracks; keep:",
        language_name(lang),
        candidates.len()
    )));
    for cand in candidates {
        let mut label = cand.label();
        let sample = rp.samples.get(&cand.sig);
        if let Some(s) = sample {
            label.push_str(&format!(" · {} line{}", s.events, plural(s.events)));
        }
        if cand.files < total {
            label.push_str(&format!(" · in {} of {total} episodes", cand.files));
        }
        let on = chosen == Some(&cand.sig);
        let mut item = column![
            row![
                radio_dot(on),
                text(label).size(13).color(if on { TEXT } else { MUTED })
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        ]
        .spacing(4);
        if let Some(s) = sample.filter(|s| !s.lines.is_empty()) {
            let preview = s
                .lines
                .iter()
                .map(|l| format!("“{l}”"))
                .collect::<Vec<_>>()
                .join("  ");
            item = item.push(
                container(text(clip(&preview, 160)).size(12).color(FAINT)).padding(iced::Padding {
                    left: 26.0,
                    ..Default::default()
                }),
            );
        }
        let sig = cand.sig.clone();
        c = c.push(
            mouse_area(
                container(item)
                    .padding([6, 10])
                    .width(Fill)
                    .style(move |_| {
                        fill(if on {
                            ACCENT.scale_alpha(0.1)
                        } else {
                            iced::Color::TRANSPARENT
                        })
                    }),
            )
            .on_press(if locked {
                Message::Wake
            } else {
                on_remux(move |rp| match kind {
                    Kind::Audio => rp.audio = Some(sig.clone()),
                    _ => rp.subs = Some(sig.clone()),
                })
            })
            .interaction(iced::mouse::Interaction::Pointer),
        );
    }
    if kind == Kind::Subtitles && rp.samples.is_empty() {
        c = c.push(weak("Reading what each track contains…"));
    }
    c.into()
}

fn radio_dot<'a>(on: bool) -> Element<'a, Message> {
    container(if on {
        container(space())
            .width(8)
            .height(8)
            .style(|_| pill(iced::Color::WHITE))
    } else {
        container(space())
    })
    .center(16)
    .style(move |_| container::Style {
        background: Some(fill_color(if on { ACCENT } else { SURFACE2 })),
        border: iced::Border {
            color: if on { ACCENT } else { LINE },
            width: 1.0,
            radius: 999.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn remux_controls<'a>(rp: &'a RemuxPage, plan: &[PlannedFile]) -> Element<'a, Message> {
    let runnable = plan.iter().filter(|p| p.problem().is_none()).count();
    match &rp.run {
        Some(run) if !run.finished => {
            let progress: f32 = run
                .states
                .iter()
                .map(|s| match s {
                    FileState::Done => 1.0,
                    FileState::Running(f) => *f,
                    _ => 0.0,
                })
                .sum::<f32>()
                / runnable.max(1) as f32;
            row![
                bar(progress, ACCENT, 8.0).length(320),
                text(format!("{:.0}%", progress * 100.0))
                    .size(13)
                    .font(SEMI)
                    .color(TEXT),
                secondary("Cancel", Some(act(Action::CancelRemux))),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
        }
        _ => {
            let mut r = row![with_tip(
                primary(
                    format!("Remux {runnable} episode{}", plural(runnable)),
                    (runnable > 0).then(|| act(Action::StartRemux))
                ),
                "Writes new files; the downloads are left as they are. Earlier remuxes with the \
                 same names are replaced."
                    .into()
            )]
            .spacing(10)
            .align_y(Alignment::Center);
            if let Some(run) = rp.run.as_ref().filter(|r| r.finished) {
                r = r.push(secondary(
                    "Open folder",
                    Some(act(Action::OpenFolder(PathBuf::from(rp.out_dir.trim())))),
                ));
                let done = run
                    .states
                    .iter()
                    .filter(|s| matches!(s, FileState::Done))
                    .count();
                if done > 0 {
                    r = r.push(secondary(
                        format!("Add {done} to playlist"),
                        Some(act(Action::AddRemuxesToPlaylist)),
                    ));
                }
            }
            if rp.title.trim().is_empty() {
                r = r.push(weak("Enter a title first."));
            }
            r.into()
        }
    }
}

fn episode_table<'a>(rp: &'a RemuxPage, plan: &[PlannedFile]) -> Element<'a, Message> {
    let widths = [
        Length::FillPortion(3),
        Length::FillPortion(2),
        Length::FillPortion(2),
        Length::Fixed(220.0),
    ];
    let mut rows = Column::new()
        .spacing(2)
        .push(header_row(&["New file", "Audio", "Subtitles", ""], &widths));
    for (i, item) in plan.iter().enumerate() {
        let name = item
            .output
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let state: Element<'_, Message> = match rp.run.as_ref().and_then(|r| r.states.get(i)) {
            Some(FileState::Waiting) => weak("Waiting").into(),
            Some(FileState::Running(f)) => row![
                bar(*f, ACCENT, 5.0).length(120),
                text(format!("{:.0}%", f * 100.0)).size(12).color(MUTED)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
            Some(FileState::Done) => text("Done").size(12).font(SEMI).color(GREEN).into(),
            Some(FileState::Failed(e)) => {
                with_tip(text("Failed").size(12).color(RED).into(), e.clone())
            }
            Some(FileState::Skipped(why)) => {
                text(format!("Skipped: {why}")).size(12).color(GOLD).into()
            }
            Some(FileState::Cancelled) => weak("Cancelled").into(),
            None => match item.problem() {
                Some(p) => text(format!("Will skip: {p}")).size(12).color(GOLD).into(),
                None if !item.notes.is_empty() => {
                    text(item.notes.join("; ")).size(12).color(GOLD).into()
                }
                None => space().into(),
            },
        };
        let cells = row![
            container(with_tip(
                text(name).size(13).color(TEXT).into(),
                format!("From {}", item.input.display())
            ))
            .width(widths[0]),
            container(
                text(
                    item.audio
                        .as_ref()
                        .map_or("—".to_owned(), |t| t.short_label())
                )
                .size(12)
                .color(MUTED)
            )
            .width(widths[1]),
            container(
                text(
                    item.subtitles
                        .as_ref()
                        .map_or("None".to_owned(), |t| t.short_label())
                )
                .size(12)
                .color(MUTED)
            )
            .width(widths[2]),
            container(state).width(widths[3]),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        rows = rows.push(
            container(cells)
                .padding([8, 12])
                .width(Fill)
                .style(|_| fill(SURFACE.scale_alpha(0.6))),
        );
    }
    column![section("Episodes", None), rows].into()
}
