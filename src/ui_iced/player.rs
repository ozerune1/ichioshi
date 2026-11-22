//! The player page (iced, GStreamer): the picture, with controls made for
//! anime: skip the opening or ending, next episode, audio and subtitle
//! tracks by name, chapters on the seek bar.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use iced::widget::{
    button, column, container, mouse_area, pick_list, row, slider, space, stack, text,
};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Fill, Length};
use iced_video_player::{Video, VideoPlayer};

use super::Message;
use super::pages::{
    ACCENT, BOLD, LINE, MEDIUM, MUTED, SEMI, TEXT, accent_gradient, fill_color, rounded,
};
use crate::gst_video::{self, Frames};
use crate::player::{Chapter, Part, Track, TrackKind};

/// A track to pick (or none, for subtitles).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackChoice {
    pub index: Option<usize>,
    pub label: String,
}

impl std::fmt::Display for TrackChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    TogglePause,
    Seek(f64),
    SeekDone,
    SeekBy(f64),
    Volume(f64),
    Mute,
    Audio(TrackChoice),
    Subs(TrackChoice),
    SkipPart,
    Next,
    Prev,
    Fullscreen,
    Ended,
    Moved,
    Back,
}

pub struct Player {
    pub video: Video,
    pub series: u64,
    pub episode: u32,
    pub path: PathBuf,
    /// "Frieren - 05".
    pub title: String,
    /// The episode's own title.
    pub name: Option<String>,
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub audio: Option<usize>,
    pub subs: Option<usize>,
    /// 0–150.
    pub volume: f64,
    /// Where the seek bar is being dragged to.
    dragging: Option<f64>,
    /// Paused by you. (GStreamer's own state passes through "paused" while
    /// it settles after a seek, so it can't say.)
    want_paused: bool,
    pub ended: bool,
    pub last_saved: Instant,
    /// When the mouse last moved: the controls show for a while after.
    pub active: Instant,
    /// Chapters skipped automatically in this episode.
    pub skipped: HashSet<usize>,
}

/// "1:23", "1:02:03".
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Player {
    /// Start playing `path` (from `start` seconds in).
    pub fn open(
        path: &Path,
        start: Option<f64>,
        (series, episode): (u64, u32),
        title: String,
        name: Option<String>,
        volume: f64,
    ) -> anyhow::Result<Self> {
        let (pipeline, sink) = gst_video::pipeline(path, Frames::Nv12)?;
        let (tracks, chapters, audio, subs) = gst_video::describe(path);
        let mut video =
            Video::from_gst_pipeline(pipeline, sink, None).map_err(|e| anyhow!("{e}"))?;
        if let Some(a) = audio {
            gst_video::set_audio(&video.pipeline(), a);
        }
        gst_video::set_subtitles(&video.pipeline(), subs);
        video.set_volume((volume / 100.0).clamp(0.0, 1.5));
        if let Some(at) = start {
            let _ = video.seek(Duration::from_secs_f64(at), true);
        }
        Ok(Self {
            video,
            series,
            episode,
            path: path.to_owned(),
            title,
            name,
            tracks,
            chapters,
            audio,
            subs,
            volume,
            dragging: None,
            want_paused: false,
            ended: false,
            last_saved: Instant::now(),
            active: Instant::now(),
            skipped: HashSet::new(),
        })
    }

    pub fn position(&self) -> f64 {
        self.dragging
            .unwrap_or_else(|| self.video.position().as_secs_f64())
    }

    pub fn duration(&self) -> f64 {
        self.video.duration().as_secs_f64()
    }

    pub fn paused(&self) -> bool {
        self.want_paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.want_paused = paused;
        self.video.set_paused(paused);
    }

    /// The chapter playing, its index, and when it ends.
    pub fn current_part(&self) -> Option<(usize, &Chapter, f64)> {
        let pos = self.position();
        let i = self.chapters.iter().rposition(|c| c.start <= pos + 0.05)?;
        let end = self
            .chapters
            .get(i + 1)
            .map_or(self.duration(), |n| n.start);
        Some((i, &self.chapters[i], end))
    }

    /// Watched: to the end, into the ending (past halfway), or 90% through.
    pub fn watched_enough(&self) -> bool {
        let (pos, dur) = (self.position(), self.duration());
        dur > 0.0
            && (self.ended
                || pos >= dur * 0.9
                || self
                    .chapters
                    .iter()
                    .any(|c| c.part == Part::Ending && c.start > dur * 0.5 && pos >= c.start))
    }

    /// Whether the video has reached its end (the bus says so; the widget
    /// only notices while it's drawn).
    pub fn reached_end(&self) -> bool {
        use gstreamer::prelude::*;
        !self.ended
            && (self.video.eos()
                || self
                    .video
                    .pipeline()
                    .bus()
                    .is_some_and(|bus| bus.pop_filtered(&[gstreamer::MessageType::Eos]).is_some()))
    }

    pub fn seek_to(&mut self, secs: f64, accurate: bool) {
        let secs = secs.clamp(0.0, self.duration().max(0.0));
        self.ended = false;
        let _ = self.video.seek(Duration::from_secs_f64(secs), accurate);
    }

    pub fn skip_part(&mut self) {
        if let Some((_, _, end)) = self.current_part() {
            self.seek_to(end, true);
        }
    }

    /// The controls' own changes (the app handles going places).
    pub fn update(&mut self, msg: Msg) {
        self.active = Instant::now();
        match msg {
            Msg::TogglePause => {
                if self.ended {
                    self.seek_to(0.0, true);
                    self.set_paused(false);
                } else {
                    self.set_paused(!self.want_paused);
                }
            }
            Msg::Seek(to) => {
                self.dragging = Some(to);
                let _ = self.video.seek(Duration::from_secs_f64(to), false);
            }
            Msg::SeekDone => {
                if let Some(to) = self.dragging.take() {
                    self.seek_to(to, true);
                }
            }
            Msg::SeekBy(d) => {
                let to = self.position() + d;
                self.seek_to(to, true);
            }
            Msg::Volume(v) => {
                self.volume = v.clamp(0.0, 150.0);
                self.video.set_volume(self.volume / 100.0);
                self.video.set_muted(false);
            }
            Msg::Mute => {
                let muted = self.video.muted();
                self.video.set_muted(!muted);
            }
            Msg::Audio(c) => {
                if let Some(i) = c.index {
                    gst_video::set_audio(&self.video.pipeline(), i);
                    self.audio = Some(i);
                }
            }
            Msg::Subs(c) => {
                gst_video::set_subtitles(&self.video.pipeline(), c.index);
                self.subs = c.index;
            }
            Msg::SkipPart => self.skip_part(),
            Msg::Ended => self.ended = true,
            Msg::Moved | Msg::Next | Msg::Prev | Msg::Fullscreen | Msg::Back => {}
        }
    }

    fn choices(&self, kind: TrackKind) -> Vec<TrackChoice> {
        let mut out: Vec<TrackChoice> = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| TrackChoice {
                index: Some(t.id as usize),
                label: t.label(),
            })
            .collect();
        if kind == TrackKind::Sub {
            out.insert(
                0,
                TrackChoice {
                    index: None,
                    label: "No subtitles".into(),
                },
            );
        }
        out
    }

    fn chosen(&self, kind: TrackKind) -> Option<TrackChoice> {
        let index = if kind == TrackKind::Audio {
            self.audio
        } else {
            self.subs
        };
        self.choices(kind).into_iter().find(|c| c.index == index)
    }

    /// The page: the picture, and the controls while the mouse moves (or
    /// when paused).
    pub fn view<'a>(
        &'a self,
        next: Option<u32>,
        prev: Option<u32>,
        fullscreen: bool,
    ) -> Element<'a, Message> {
        let picture = VideoPlayer::new(&self.video)
            .width(Fill)
            .height(Fill)
            .content_fit(ContentFit::Contain)
            .on_end_of_stream(Message::Player(Msg::Ended));
        let base = mouse_area(
            container(picture)
                .width(Fill)
                .height(Fill)
                .style(|_| black()),
        )
        .on_press(Message::Player(Msg::TogglePause))
        .on_double_click(Message::Player(Msg::Fullscreen))
        .on_move(|_| Message::Player(Msg::Moved));
        let show = self.paused()
            || self.ended
            || self.dragging.is_some()
            || self.active.elapsed() < Duration::from_millis(2500);
        let mut layers = vec![Element::from(base)];

        // Skip the opening or ending, whenever it's on.
        if let Some((_, chapter, end)) = self.current_part() {
            let last = (end - self.duration()).abs() < 0.5;
            let label = match chapter.part {
                Part::Opening => Some(("Skip opening  ⏵", Msg::SkipPart)),
                Part::Ending if last && next.is_some() => Some(("Next episode  ⏵", Msg::Next)),
                Part::Ending => Some(("Skip ending  ⏵", Msg::SkipPart)),
                _ => None,
            };
            if let Some((label, msg)) = label {
                let b = button(text(label).size(15).font(SEMI).color(Color::WHITE))
                    .padding([11, 20])
                    .on_press(Message::Player(msg))
                    .style(|_, _| button::Style {
                        background: Some(fill_color(Color {
                            a: 0.75,
                            ..Color::BLACK
                        })),
                        text_color: Color::WHITE,
                        border: Border {
                            color: Color::WHITE.scale_alpha(0.35),
                            width: 1.0,
                            radius: 10.0.into(),
                        },
                        ..button::Style::default()
                    });
                let gap = if show { 120.0 } else { 40.0 };
                layers.push(
                    container(b)
                        .width(Fill)
                        .height(Fill)
                        .align_x(Alignment::End)
                        .align_y(Alignment::End)
                        .padding(iced::Padding {
                            bottom: gap,
                            right: 32.0,
                            ..Default::default()
                        })
                        .into(),
                );
            }
        }

        // At the end, with nothing playing next: what's next.
        if self.ended
            && let Some(n) = next
        {
            let b = button(
                text(format!("▶  Episode {n}"))
                    .size(18)
                    .font(BOLD)
                    .color(Color::WHITE),
            )
            .padding([14, 26])
            .on_press(Message::Player(Msg::Next))
            .style(|_, _| button::Style {
                background: Some(accent_gradient()),
                text_color: Color::WHITE,
                border: rounded(12.0),
                ..button::Style::default()
            });
            layers.push(
                container(
                    column![text("Up next").size(13).color(MUTED), b]
                        .spacing(10)
                        .align_x(Alignment::Center),
                )
                .center(Fill)
                .into(),
            );
        }

        if show {
            layers.push(self.controls(next, prev, fullscreen));
        }
        stack(layers).width(Fill).height(Fill).into()
    }

    fn controls<'a>(
        &'a self,
        next: Option<u32>,
        prev: Option<u32>,
        fullscreen: bool,
    ) -> Element<'a, Message> {
        let flat = |label: &'static str, size: u32| text(label).size(size).color(TEXT);
        let icon = |label: &'static str, msg: Option<Msg>| {
            button(flat(label, 18))
                .padding([6, 10])
                .on_press_maybe(msg.map(p))
                .style(|_, status| button::Style {
                    background: (status == button::Status::Hovered)
                        .then(|| fill_color(Color::WHITE.scale_alpha(0.1))),
                    text_color: if status == button::Status::Disabled {
                        MUTED
                    } else {
                        TEXT
                    },
                    border: rounded(8.0),
                    ..button::Style::default()
                })
        };

        let top = container(
            row![
                button(row![text("←").size(15), text("Back").size(14).font(MEDIUM)].spacing(6))
                    .padding([8, 14])
                    .on_press(p(Msg::Back))
                    .style(|_, _| button::Style {
                        background: Some(fill_color(Color::WHITE.scale_alpha(0.1))),
                        text_color: TEXT,
                        border: rounded(9.0),
                        ..button::Style::default()
                    }),
                column![
                    iced::widget::tooltip(
                        text(self.title.clone())
                            .size(17)
                            .font(BOLD)
                            .color(Color::WHITE),
                        container(text(self.path.display().to_string()).size(12).color(TEXT))
                            .padding(8)
                            .style(|_| container::Style {
                                background: Some(fill_color(Color::BLACK.scale_alpha(0.85))),
                                border: rounded(6.0),
                                ..Default::default()
                            }),
                        iced::widget::tooltip::Position::Bottom,
                    ),
                    text(self.name.clone().unwrap_or_default())
                        .size(13)
                        .color(MUTED),
                ]
                .spacing(1),
            ]
            .spacing(16)
            .align_y(Alignment::Center),
        )
        .padding([16, 24])
        .width(Fill)
        .style(|_| gradient_style(true));

        let (pos, dur) = (self.position(), self.duration().max(0.001));
        let seek = stack![
            container(self.chapter_strip(dur))
                .height(18)
                .align_y(Alignment::Center),
            slider(0.0..=dur, pos.min(dur), |v| p(Msg::Seek(v)))
                .on_release(p(Msg::SeekDone))
                .step(0.1)
                .height(18)
                .style(|_, _| slider::Style {
                    rail: slider::Rail {
                        backgrounds: (
                            fill_color(ACCENT.scale_alpha(0.85)),
                            Background::Color(Color::TRANSPARENT)
                        ),
                        width: 5.0,
                        border: rounded(3.0),
                    },
                    handle: slider::Handle {
                        shape: slider::HandleShape::Circle { radius: 7.0 },
                        background: fill_color(Color::WHITE),
                        border_width: 0.0,
                        border_color: Color::TRANSPARENT,
                    },
                }),
        ];

        let play = if self.paused() || self.ended {
            "▶"
        } else {
            "⏸"
        };
        let audio = pick_list(
            self.choices(TrackKind::Audio),
            self.chosen(TrackKind::Audio),
            |c| p(Msg::Audio(c)),
        )
        .text_size(13)
        .padding([6, 10])
        .width(230)
        .style(pick_style);
        let subs = pick_list(
            self.choices(TrackKind::Sub),
            self.chosen(TrackKind::Sub),
            |c| p(Msg::Subs(c)),
        )
        .text_size(13)
        .padding([6, 10])
        .width(230)
        .style(pick_style);
        let speaker = if self.video.muted() || self.volume <= 0.0 {
            "🔇"
        } else {
            "🔊"
        };
        let buttons = row![
            icon("⏮", prev.map(|_| Msg::Prev)),
            icon(play, Some(Msg::TogglePause)),
            icon("⏭", next.map(|_| Msg::Next)),
            text(format!("{} / {}", clock(pos), clock(self.duration())))
                .size(13)
                .font(MEDIUM)
                .color(TEXT),
            space().width(10),
            icon(speaker, Some(Msg::Mute)),
            slider(0.0..=150.0, self.volume, |v| p(Msg::Volume(v)))
                .width(110)
                .height(14),
            space::horizontal(),
            audio,
            subs,
            icon(if fullscreen { "⤡" } else { "⤢" }, Some(Msg::Fullscreen)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let bottom = container(column![seek, buttons].spacing(8))
            .padding([14, 24])
            .width(Fill)
            .style(|_| gradient_style(false));
        column![top, space::vertical(), bottom]
            .width(Fill)
            .height(Fill)
            .into()
    }

    /// The chapters as a strip under the seek bar: openings and endings
    /// tinted.
    fn chapter_strip<'a>(&self, dur: f64) -> Element<'a, Message> {
        if self.chapters.is_empty() {
            return container(space())
                .width(Fill)
                .height(5)
                .style(|_| container::Style {
                    background: Some(fill_color(Color::WHITE.scale_alpha(0.2))),
                    border: rounded(3.0),
                    ..Default::default()
                })
                .into();
        }
        let mut strip = row![].spacing(2).height(5);
        for (i, c) in self.chapters.iter().enumerate() {
            let end = self.chapters.get(i + 1).map_or(dur, |n| n.start);
            let share = (((end - c.start) / dur) * 1000.0).round().max(1.0) as u16;
            let color = match c.part {
                Part::Opening | Part::Ending => Color::from_rgba8(0xff, 0xc8, 0x50, 0.55),
                _ => Color::WHITE.scale_alpha(0.22),
            };
            strip = strip.push(
                container(space())
                    .width(Length::FillPortion(share))
                    .height(5)
                    .style(move |_| container::Style {
                        background: Some(fill_color(color)),
                        border: rounded(2.0),
                        ..Default::default()
                    }),
            );
        }
        strip.into()
    }
}

fn p(m: Msg) -> Message {
    Message::Player(m)
}

fn black() -> container::Style {
    container::Style {
        background: Some(fill_color(Color::BLACK)),
        ..Default::default()
    }
}

fn gradient_style(top: bool) -> container::Style {
    let (a, b) = if top { (0.75, 0.0) } else { (0.0, 0.85) };
    container::Style {
        background: Some(Background::Gradient(
            iced::gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, Color { a, ..Color::BLACK })
                .add_stop(
                    1.0,
                    Color {
                        a: b,
                        ..Color::BLACK
                    },
                )
                .into(),
        )),
        ..Default::default()
    }
}

fn pick_style(_: &iced::Theme, _: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: MUTED,
        handle_color: MUTED,
        background: fill_color(Color::WHITE.scale_alpha(0.08)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 8.0.into(),
        },
    }
}
