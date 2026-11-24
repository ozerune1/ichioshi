//! Ichioshi's look (the app's src/app/gui/style.rs: its colours, Inter, and
//! the buttons, checkboxes, cards and bars made with them), for the
//! installer's few pieces.

use iced::widget::{button, checkbox, container, mouse_area, progress_bar, row, text};
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Font, Shadow, Vector, font, gradient,
};

use crate::ui::Message;

pub const BG: Color = Color::from_rgb8(0x0d, 0x0f, 0x14);
pub const SURFACE: Color = Color::from_rgb8(0x17, 0x1b, 0x24);
pub const SURFACE2: Color = Color::from_rgb8(0x1f, 0x24, 0x30);
pub const LINE: Color = Color::from_rgb8(0x27, 0x2d, 0x3b);
pub const TEXT: Color = Color::from_rgb8(0xe9, 0xec, 0xf3);
pub const MUTED: Color = Color::from_rgb8(0x8b, 0x94, 0xa8);
pub const FAINT: Color = Color::from_rgb8(0x5a, 0x62, 0x74);
pub const ACCENT: Color = Color::from_rgb8(0x8b, 0x7c, 0xff);
pub const GREEN: Color = Color::from_rgb8(0x34, 0xd3, 0x99);
pub const RED: Color = Color::from_rgb8(0xf8, 0x71, 0x71);

pub const INTER: Font = Font::with_name("Inter");
pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
];
pub const MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..INTER
};
pub const SEMI: Font = Font {
    weight: font::Weight::Semibold,
    ..INTER
};
pub const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..INTER
};

fn rounded(r: f32) -> Border {
    Border {
        radius: r.into(),
        ..Border::default()
    }
}

fn accent_gradient() -> Background {
    Background::Gradient(
        gradient::Linear::new(std::f32::consts::FRAC_PI_2 * 1.5)
            .add_stop(0.0, ACCENT)
            .add_stop(1.0, Color::from_rgb8(0x5b, 0x8c, 0xff))
            .into(),
    )
}

/// A bordered box around a group of things.
pub fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(|_| container::Style {
            background: Some(SURFACE.into()),
            border: Border {
                color: LINE,
                width: 1.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// The main thing to do: filled with the accent (red, for removing).
pub fn primary<'a>(label: &'a str, on_press: Message, red: bool) -> Element<'a, Message> {
    button(text(label).size(14).font(SEMI).color(Color::WHITE))
        .padding([10, 20])
        .on_press(on_press)
        .style(move |_, status| {
            let glow = if red { RED } else { ACCENT };
            button::Style {
                background: Some(if red {
                    Background::Color(Color::from_rgb8(0xdc, 0x4c, 0x4c))
                } else {
                    accent_gradient()
                }),
                text_color: Color::WHITE,
                border: rounded(10.0),
                shadow: if status == button::Status::Pressed {
                    Shadow::default()
                } else {
                    Shadow {
                        color: glow.scale_alpha(0.35),
                        offset: Vector::new(0.0, 6.0),
                        blur_radius: 18.0,
                    }
                },
                ..button::Style::default()
            }
        })
        .into()
}

/// Other things to do: outlined (`small` for one inside a card).
pub fn secondary<'a>(label: &'a str, on_press: Message, small: bool) -> Element<'a, Message> {
    button(
        text(label)
            .size(if small { 12 } else { 14 })
            .font(MEDIUM)
            .color(TEXT),
    )
    .padding(if small { [5, 10] } else { [10, 16] })
    .on_press(on_press)
    .style(|_, status| button::Style {
        background: Some(
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                SURFACE2
            } else {
                SURFACE
            }
            .into(),
        ),
        text_color: TEXT,
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 9.0.into(),
        },
        ..button::Style::default()
    })
    .into()
}

/// A checkbox whose label can be clicked too.
pub fn check<'a>(
    label: &'a str,
    on: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    let flip = on_toggle(!on);
    let boxed = checkbox(on)
        .on_toggle(on_toggle)
        .size(16)
        .style(|_, status| {
            let (checked, hovered) = match status {
                checkbox::Status::Active { is_checked } => (is_checked, false),
                checkbox::Status::Hovered { is_checked } => (is_checked, true),
                checkbox::Status::Disabled { is_checked } => (is_checked, false),
            };
            checkbox::Style {
                background: if checked {
                    ACCENT
                } else if hovered {
                    SURFACE2
                } else {
                    SURFACE
                }
                .into(),
                icon_color: Color::WHITE,
                border: Border {
                    color: if checked { ACCENT } else { LINE },
                    width: 1.0,
                    radius: 5.0.into(),
                },
                text_color: Some(TEXT),
            }
        });
    mouse_area(
        row![boxed, text(label).size(14).color(TEXT)]
            .spacing(10)
            .align_y(Alignment::Center),
    )
    .on_press(flip)
    .interaction(iced::mouse::Interaction::Pointer)
    .into()
}

/// A progress bar (0 to 1).
pub fn bar<'a>(value: f32) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, value.clamp(0.0, 1.0))
        .girth(8)
        .style(|_| progress_bar::Style {
            background: SURFACE2.into(),
            bar: accent_gradient(),
            border: rounded(999.0),
        })
        .into()
}
