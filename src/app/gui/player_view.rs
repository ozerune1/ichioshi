//! The player's look, after the players people like:
//! - IINA's floating controls: a translucent panel with volume on the left,
//!   big transport buttons in the middle, tools on the right, and the
//!   timeline under them with the time either side;
//! - uosc's timeline: chapters as segments, openings and endings marked;
//! - Netflix's audio & subtitles panel and outlined "Skip intro" button.
//!
//! Icons: Lucide (ISC, assets/icons/LICENSE).

use std::sync::LazyLock;

use iced::widget::{
    Column, button, column, container, mouse_area, row, scrollable, slider, space, stack, svg,
    text, toggler,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Fill, Length, Shadow, Vector,
};
use iced_video_player::VideoPlayer;

use super::Message;
use super::player::{Msg, Panel, Player, SKIP, Setting, TrackChoice, clock};
use super::style::{ACCENT, BOLD, MEDIUM, SEMI, fill_color, rounded};
use crate::player::{Part, TrackKind};

macro_rules! icons {
    ($($name:ident = $file:literal;)*) => {
        $(static $name: LazyLock<svg::Handle> =
            LazyLock::new(|| svg::Handle::from_memory(include_bytes!(concat!("../../../assets/icons/", $file, ".svg")).as_slice()));)*
    };
}

icons! {
    PLAY = "play";
    PAUSE = "pause";
    PREV = "skip-back";
    NEXT = "skip-forward";
    BACK10 = "rotate-ccw";
    FWD10 = "rotate-cw";
    VOLUME = "volume-2";
    VOLUME_LOW = "volume-1";
    MUTED = "volume-x";
    CAPTIONS = "captions";
    SETTINGS = "settings-2";
    FULLSCREEN = "maximize";
    WINDOWED = "minimize";
    ARROW_LEFT = "arrow-left";
    CHECK = "check";
    MINUS = "minus";
    PLUS = "plus";
}

const WHITE: Color = Color::WHITE;
const DIM: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.62,
};
const FAINT: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.38,
};
const AMBER: Color = Color::from_rgb8(0xff, 0xc4, 0x5c);

fn m(msg: Msg) -> Message {
    Message::Player(msg)
}

fn icon<'a>(handle: &svg::Handle, size: f32, color: Color) -> Element<'a, Message> {
    svg(handle.clone())
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}

/// The frosted panel the controls sit on.
/// Menus over the picture: nearly solid, so a busy frame doesn't show
/// through the text.
fn menu() -> container::Style {
    container::Style {
        background: Some(fill_color(Color::from_rgba8(18, 18, 24, 0.96))),
        ..glass()
    }
}

fn glass() -> container::Style {
    container::Style {
        background: Some(fill_color(Color::from_rgba8(18, 18, 24, 0.82))),
        border: Border {
            color: Color::WHITE.scale_alpha(0.09),
            width: 1.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(0.5),
            offset: Vector::new(0.0, 12.0),
            blur_radius: 40.0,
        },
        ..Default::default()
    }
}

/// A round icon button (disabled without a message).
fn tool<'a>(handle: &svg::Handle, size: f32, msg: Option<Msg>, on: bool) -> Element<'a, Message> {
    let enabled = msg.is_some();
    let color = if !enabled {
        FAINT
    } else if on {
        ACCENT
    } else {
        WHITE
    };
    button(container(icon(handle, size, color)).center(size + 18.0))
        .padding(0)
        .on_press_maybe(msg.map(m))
        .style(move |_, status| button::Style {
            background: match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Some(fill_color(Color::WHITE.scale_alpha(0.12)))
                }
                _ if on => Some(fill_color(ACCENT.scale_alpha(0.16))),
                _ => None,
            },
            border: rounded(999.0),
            ..button::Style::default()
        })
        .into()
}

fn scrim(top: bool, height: f32) -> Element<'static, Message> {
    let (a, b) = if top { (0.7, 0.0) } else { (0.0, 0.55) };
    container(space())
        .width(Fill)
        .height(height)
        .style(move |_| container::Style {
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
        })
        .into()
}

impl Player {
    pub fn view<'a>(
        &'a self,
        next: Option<u32>,
        prev: Option<u32>,
        fullscreen: bool,
        settings: &crate::player::Settings,
    ) -> Element<'a, Message> {
        let picture = VideoPlayer::new(&self.video)
            .width(Fill)
            .height(Fill)
            .content_fit(ContentFit::Contain)
            .on_end_of_stream(m(Msg::Ended))
            .on_new_frame(m(Msg::Frame));
        let base =
            mouse_area(
                container(picture)
                    .width(Fill)
                    .height(Fill)
                    .style(|_| container::Style {
                        background: Some(fill_color(Color::BLACK)),
                        ..Default::default()
                    }),
            )
            .on_press(m(Msg::Surface))
            .on_double_click(m(Msg::Fullscreen))
            .on_move(|_| m(Msg::Moved));
        let show = self.controls_shown();
        let mut layers: Vec<Element<'a, Message>> = vec![base.into()];
        if show {
            layers.push(column![scrim(true, 140.0), space::vertical(), scrim(false, 220.0)].into());
            layers.push(self.top_bar());
        }
        // Subtitles (libass), drawn at the picture's size so they sit on it;
        // above the shading, under the controls.
        if let Some(subs) = &self.overlay {
            layers.push(
                iced::widget::image(subs.clone())
                    .width(Fill)
                    .height(Fill)
                    .content_fit(ContentFit::Contain)
                    .into(),
            );
        }
        if self.paused() && !self.ended {
            layers.push(
                container(container(icon(&PLAY, 34.0, WHITE)).center(84).style(|_| {
                    container::Style {
                        background: Some(fill_color(Color::BLACK.scale_alpha(0.45))),
                        border: Border {
                            color: Color::WHITE.scale_alpha(0.25),
                            width: 1.0,
                            radius: 999.0.into(),
                        },
                        ..Default::default()
                    }
                }))
                .center(Fill)
                .into(),
            );
        }
        // Out of the way while a panel is open (as Netflix does).
        if let Some(skip) = self
            .skip_button(next, show)
            .filter(|_| self.panel.is_none())
        {
            layers.push(skip);
        }
        if self.ended
            && let Some(n) = next
        {
            layers.push(up_next(n));
        }
        if show {
            if let Some(panel) = self.panel {
                let content = match panel {
                    Panel::Tracks => self.tracks_panel(),
                    Panel::Settings => settings_panel(settings),
                };
                layers.push(
                    container(content)
                        .width(Fill)
                        .height(Fill)
                        .align_x(Alignment::End)
                        .align_y(Alignment::End)
                        .padding(iced::Padding {
                            bottom: 150.0,
                            right: 40.0,
                            ..Default::default()
                        })
                        .into(),
                );
            }
            layers.push(self.controls(next, prev, fullscreen));
        }
        iced::widget::stack(layers).width(Fill).height(Fill).into()
    }

    fn top_bar<'a>(&'a self) -> Element<'a, Message> {
        let back = button(container(icon(&ARROW_LEFT, 20.0, WHITE)).center(40))
            .padding(0)
            .on_press(m(Msg::Back))
            .style(|_, status| button::Style {
                background: Some(fill_color(Color::WHITE.scale_alpha(
                    if status == button::Status::Hovered {
                        0.2
                    } else {
                        0.1
                    },
                ))),
                border: rounded(999.0),
                ..button::Style::default()
            });
        let episode = match &self.name {
            Some(n) => format!("Episode {} · {n}", self.episode),
            None => format!("Episode {}", self.episode),
        };
        let titles = column![
            iced::widget::tooltip(
                text(self.series_title.clone())
                    .size(18)
                    .font(BOLD)
                    .color(WHITE),
                container(text(self.path.display().to_string()).size(12).color(WHITE))
                    .padding(8)
                    .style(|_| glass()),
                iced::widget::tooltip::Position::Bottom,
            ),
            text(episode).size(13).font(MEDIUM).color(DIM),
        ]
        .spacing(2);
        container(row![back, titles].spacing(14).align_y(Alignment::Center))
            .padding([22, 28])
            .into()
    }

    /// The floating controls: volume, transport, tools; the timeline.
    fn controls<'a>(
        &'a self,
        next: Option<u32>,
        prev: Option<u32>,
        fullscreen: bool,
    ) -> Element<'a, Message> {
        let muted = self.video.muted() || self.volume <= 0.0;
        let speaker = if muted {
            &*MUTED
        } else if self.volume < 50.0 {
            &*VOLUME_LOW
        } else {
            &*VOLUME
        };
        let volume = row![
            tool(speaker, 18.0, Some(Msg::Mute), false),
            slider(0.0..=150.0, if muted { 0.0 } else { self.volume }, |v| m(
                Msg::Volume(v)
            ))
            .width(92)
            .height(14)
            .style(|_, _| thin_slider(WHITE)),
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        let playing = !(self.paused() || self.ended);
        let play = button(
            container(icon(
                if playing { &PAUSE } else { &PLAY },
                22.0,
                Color::BLACK,
            ))
            .center(52),
        )
        .padding(0)
        .on_press(m(Msg::TogglePause))
        .style(|_, status| button::Style {
            background: Some(fill_color(if status == button::Status::Hovered {
                Color::from_rgb8(0xe6, 0xe6, 0xee)
            } else {
                WHITE
            })),
            border: rounded(999.0),
            shadow: Shadow {
                color: Color::BLACK.scale_alpha(0.35),
                offset: Vector::new(0.0, 4.0),
                blur_radius: 14.0,
            },
            ..button::Style::default()
        });
        let transport = row![
            tool(&PREV, 18.0, prev.map(|_| Msg::Prev), false),
            tool(&BACK10, 19.0, Some(Msg::SeekBy(-10.0)), false),
            play,
            tool(&FWD10, 19.0, Some(Msg::SeekBy(10.0)), false),
            tool(&NEXT, 18.0, next.map(|_| Msg::Next), false),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let tools = row![
            tool(
                &CAPTIONS,
                19.0,
                Some(Msg::Panel(Panel::Tracks)),
                self.panel == Some(Panel::Tracks)
            ),
            tool(
                &SETTINGS,
                18.0,
                Some(Msg::Panel(Panel::Settings)),
                self.panel == Some(Panel::Settings)
            ),
            tool(
                if fullscreen { &WINDOWED } else { &FULLSCREEN },
                18.0,
                Some(Msg::Fullscreen),
                false
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center);

        let (pos, dur) = (self.position(), self.duration().max(0.001));
        let chapter = self
            .current_part()
            .map(|(_, c, _)| c.title.clone())
            .filter(|t| !t.is_empty());
        let left_time = column![text(clock(pos)).size(12).font(SEMI).color(WHITE)].width(64);
        let right_time = column![
            text(format!("-{}", clock(dur - pos)))
                .size(12)
                .font(SEMI)
                .color(DIM)
        ]
        .width(64)
        .align_x(Alignment::End);
        let mut timeline = column![
            row![left_time, self.timeline(pos, dur), right_time]
                .spacing(12)
                .align_y(Alignment::Center)
        ];
        if let Some(name) = chapter {
            timeline = timeline.push(
                container(text(name).size(11).font(MEDIUM).color(DIM))
                    .width(Fill)
                    .align_x(Alignment::Center),
            );
        }

        let panel = container(
            column![
                row![
                    container(volume).width(Fill),
                    transport,
                    container(tools).width(Fill).align_x(Alignment::End),
                ]
                .align_y(Alignment::Center),
                timeline.spacing(4),
            ]
            .spacing(10),
        )
        .padding([14, 20])
        .max_width(880)
        .style(|_| glass());
        container(panel)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::End)
            .padding(iced::Padding {
                bottom: 28.0,
                left: 24.0,
                right: 24.0,
                top: 0.0,
            })
            .into()
    }

    /// The timeline: a segment per chapter (openings and endings in amber),
    /// filled as it plays, under a seek slider with only its knob showing.
    fn timeline<'a>(&'a self, pos: f64, dur: f64) -> Element<'a, Message> {
        let mut spans: Vec<(f64, f64, bool)> = self
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let end = self.chapters.get(i + 1).map_or(dur, |n| n.start);
                (c.start, end, matches!(c.part, Part::Opening | Part::Ending))
            })
            .filter(|(s, e, _)| e > s)
            .collect();
        if spans.is_empty() {
            spans.push((0.0, dur, false));
        }
        let mut bar = row![].spacing(3).height(5);
        for (start, end, marked) in spans {
            let share = (((end - start) / dur) * 10_000.0).round().max(1.0) as u16;
            let done = (((pos - start) / (end - start)).clamp(0.0, 1.0) * 1000.0).round() as u16;
            let rest = if marked {
                AMBER.scale_alpha(0.45)
            } else {
                Color::WHITE.scale_alpha(0.22)
            };
            let played = if marked { AMBER } else { ACCENT };
            let mut seg = row![].height(5);
            if done > 0 {
                seg = seg.push(
                    container(space())
                        .width(Length::FillPortion(done))
                        .height(5)
                        .style(move |_| bar_style(played)),
                );
            }
            if done < 1000 {
                seg = seg.push(
                    container(space())
                        .width(Length::FillPortion(1000 - done))
                        .height(5)
                        .style(move |_| bar_style(rest)),
                );
            }
            bar = bar.push(
                container(seg)
                    .width(Length::FillPortion(share))
                    .clip(true)
                    .style(|_| container::Style {
                        border: rounded(3.0),
                        ..Default::default()
                    }),
            );
        }
        let knob = slider(0.0..=dur, pos.min(dur), |v| m(Msg::Seek(v)))
            .on_release(m(Msg::SeekDone))
            .step(0.1)
            .height(18)
            .style(|_, status| slider::Style {
                rail: slider::Rail {
                    backgrounds: (
                        Background::Color(Color::TRANSPARENT),
                        Background::Color(Color::TRANSPARENT),
                    ),
                    width: 5.0,
                    border: rounded(3.0),
                },
                handle: slider::Handle {
                    shape: slider::HandleShape::Circle {
                        radius: if matches!(
                            status,
                            slider::Status::Hovered | slider::Status::Dragged
                        ) {
                            8.0
                        } else {
                            6.0
                        },
                    },
                    background: fill_color(WHITE),
                    border_width: 0.0,
                    border_color: Color::TRANSPARENT,
                },
            });
        stack![container(bar).height(18).align_y(Alignment::Center), knob]
            .width(Fill)
            .into()
    }

    /// "Skip opening" (and so on) when chapters say where it is; else
    /// 85 seconds, an opening's usual length.
    fn skip_button<'a>(&'a self, next: Option<u32>, show: bool) -> Option<Element<'a, Message>> {
        let named = self
            .chapters
            .iter()
            .any(|c| matches!(c.part, Part::Opening | Part::Ending));
        let (label, msg) = match self.current_part() {
            Some((_, chapter, end)) if named => {
                let last = (end - self.duration()).abs() < 0.5;
                match chapter.part {
                    Part::Opening => ("Skip opening", Msg::SkipPart),
                    Part::Ending if last && next.is_some() => ("Next episode", Msg::Next),
                    Part::Ending => ("Skip ending", Msg::SkipPart),
                    _ => return None,
                }
            }
            _ if !named && show && self.duration() - self.position() > SKIP => {
                ("Skip 85s", Msg::SeekBy(SKIP))
            }
            _ => return None,
        };
        let b = button(
            row![
                text(label).size(15).font(SEMI).color(WHITE),
                icon(&NEXT, 15.0, WHITE)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([12, 20])
        .on_press(m(msg))
        .style(|_, status| button::Style {
            background: Some(fill_color(Color::BLACK.scale_alpha(
                if status == button::Status::Hovered {
                    0.75
                } else {
                    0.55
                },
            ))),
            text_color: WHITE,
            border: Border {
                color: Color::WHITE.scale_alpha(0.85),
                width: 2.0,
                radius: 8.0.into(),
            },
            ..button::Style::default()
        });
        let b: Element<'a, Message> = if label == "Skip 85s" {
            iced::widget::tooltip(
                b,
                container(
                    text("No chapters say where the opening is: 85 seconds on (L)")
                        .size(12)
                        .color(WHITE),
                )
                .padding(8)
                .style(|_| glass()),
                iced::widget::tooltip::Position::Top,
            )
            .into()
        } else {
            b.into()
        };
        let gap = if show { 170.0 } else { 48.0 };
        Some(
            container(b)
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::End)
                .padding(iced::Padding {
                    bottom: gap,
                    right: 40.0,
                    ..Default::default()
                })
                .into(),
        )
    }

    /// Audio and subtitles side by side (Netflix-style), long lists scroll.
    fn tracks_panel<'a>(&'a self) -> Element<'a, Message> {
        let list = |title: &'static str, kind: TrackKind| {
            let chosen = self.chosen(kind);
            let items: Vec<Element<'a, Message>> = self
                .choices(kind)
                .into_iter()
                .map(|c: TrackChoice| {
                    let on = c.index == chosen;
                    let mark: Element<'a, Message> = if on {
                        icon(&CHECK, 16.0, ACCENT)
                    } else {
                        space().width(16).into()
                    };
                    let label = text(c.label.clone())
                        .size(14)
                        .font(if on { SEMI } else { MEDIUM })
                        .color(if on { WHITE } else { DIM });
                    let msg = if kind == TrackKind::Audio {
                        Msg::Audio(c)
                    } else {
                        Msg::Subs(c)
                    };
                    button(row![mark, label].spacing(10).align_y(Alignment::Center))
                        .width(Fill)
                        .padding([7, 10])
                        .on_press(m(msg))
                        .style(|_, status| button::Style {
                            background: matches!(status, button::Status::Hovered)
                                .then(|| fill_color(Color::WHITE.scale_alpha(0.08))),
                            border: rounded(8.0),
                            ..button::Style::default()
                        })
                        .into()
                })
                .collect();
            // Subtitles get a delay (as IINA's panel has): Z and X too.
            let header: Element<'a, Message> = if kind == TrackKind::Sub {
                let ms = self.sub_delay_ms;
                row![
                    text(title).size(12).font(BOLD).color(FAINT),
                    space::horizontal(),
                    text("Delay").size(12).color(FAINT),
                    tool(&MINUS, 13.0, Some(Msg::SubDelay(-100)), false),
                    text(format!(
                        "{}{:.1} s",
                        if ms > 0 { "+" } else { "" },
                        ms as f64 / 1000.0
                    ))
                    .size(12)
                    .font(SEMI)
                    .color(if ms == 0 { DIM } else { WHITE }),
                    tool(&PLUS, 13.0, Some(Msg::SubDelay(100)), false),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into()
            } else {
                text(title).size(12).font(BOLD).color(FAINT).into()
            };
            let header = container(header).height(34).align_y(Alignment::Center);
            column![
                header,
                scrollable(
                    Column::with_children(items)
                        .spacing(1)
                        .padding(iced::Padding {
                            right: 10.0,
                            ..Default::default()
                        })
                )
                .height(300)
                .style(thin_scroll),
            ]
            .spacing(10)
            .width(Fill)
        };
        container(
            row![
                list("AUDIO", TrackKind::Audio),
                list("SUBTITLES", TrackKind::Sub)
            ]
            .spacing(18),
        )
        .padding(18)
        .width(760)
        .style(|_| menu())
        .into()
    }
}

fn settings_panel<'a>(s: &crate::player::Settings) -> Element<'a, Message> {
    let switch = |label: &'static str, on: bool, which: Setting| {
        toggler(on)
            .label(label)
            .text_size(14)
            .size(20)
            .spacing(12)
            .on_toggle(move |v| m(Msg::Setting(which, v)))
            .style(|_, status| toggler::Style {
                background: fill_color(
                    if matches!(
                        status,
                        toggler::Status::Active { is_toggled: true }
                            | toggler::Status::Hovered { is_toggled: true }
                    ) {
                        ACCENT
                    } else {
                        Color::WHITE.scale_alpha(0.2)
                    },
                ),
                background_border_width: 0.0,
                background_border_color: Color::TRANSPARENT,
                foreground: fill_color(WHITE),
                foreground_border_width: 0.0,
                foreground_border_color: Color::TRANSPARENT,
                text_color: Some(WHITE),
                border_radius: None,
                padding_ratio: 0.12,
            })
    };
    container(
        column![
            text("PLAYBACK").size(12).font(BOLD).color(FAINT),
            switch(
                "Skip openings automatically",
                s.skip_openings,
                Setting::SkipOpenings
            ),
            switch(
                "Skip endings automatically",
                s.skip_endings,
                Setting::SkipEndings
            ),
            switch(
                "Play the next episode automatically",
                s.autoplay,
                Setting::Autoplay
            ),
        ]
        .spacing(14),
    )
    .padding(20)
    .width(380)
    .style(|_| menu())
    .into()
}

fn up_next<'a>(n: u32) -> Element<'a, Message> {
    let b = button(
        row![
            icon(&PLAY, 16.0, Color::BLACK),
            text(format!("Episode {n}"))
                .size(17)
                .font(BOLD)
                .color(Color::BLACK)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([14, 26])
    .on_press(m(Msg::Next))
    .style(|_, _| button::Style {
        background: Some(fill_color(WHITE)),
        border: rounded(999.0),
        ..button::Style::default()
    });
    container(
        column![text("Up next").size(13).font(SEMI).color(DIM), b]
            .spacing(12)
            .align_x(Alignment::Center),
    )
    .center(Fill)
    .into()
}

fn bar_style(color: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(color)),
        ..Default::default()
    }
}

fn thin_slider(color: Color) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                fill_color(color),
                fill_color(Color::WHITE.scale_alpha(0.22)),
            ),
            width: 4.0,
            border: rounded(2.0),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 6.0 },
            background: fill_color(WHITE),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

fn thin_scroll(_: &iced::Theme, _: scrollable::Status) -> scrollable::Style {
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: fill_color(Color::WHITE.scale_alpha(0.2)),
            border: rounded(999.0),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: fill_color(Color::BLACK),
            border: rounded(999.0),
            shadow: Shadow::default(),
            icon: WHITE,
        },
    }
}
