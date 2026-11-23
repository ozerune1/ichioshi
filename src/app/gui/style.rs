//! The look ("Studio"): colors, fonts and the pieces every page is made
//! of: buttons, fields, cards, badges, progress bars, dialogs.

use iced::widget::{
    Space, button, checkbox, column, container, image, mouse_area, opaque, pick_list, progress_bar,
    row, scrollable, space, stack, text, text_input,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Fill, Font, Length, Shadow, Theme,
    Vector, font, gradient,
};

use super::Message;
use crate::tracking::Status;

pub(super) const BG: Color = Color::from_rgb8(0x0d, 0x0f, 0x14);
pub(super) const SIDEBAR: Color = Color::from_rgb8(0x11, 0x14, 0x1b);
pub(super) const SURFACE: Color = Color::from_rgb8(0x17, 0x1b, 0x24);
pub(super) const SURFACE2: Color = Color::from_rgb8(0x1f, 0x24, 0x30);
pub(super) const LINE: Color = Color::from_rgb8(0x27, 0x2d, 0x3b);
pub(super) const TEXT: Color = Color::from_rgb8(0xe9, 0xec, 0xf3);
pub(super) const MUTED: Color = Color::from_rgb8(0x8b, 0x94, 0xa8);
pub(super) const FAINT: Color = Color::from_rgb8(0x5a, 0x62, 0x74);
pub(super) const ACCENT: Color = Color::from_rgb8(0x8b, 0x7c, 0xff);
pub(super) const GREEN: Color = Color::from_rgb8(0x34, 0xd3, 0x99);
pub(super) const BLUE: Color = Color::from_rgb8(0x60, 0xa5, 0xfa);
pub(super) const GOLD: Color = Color::from_rgb8(0xfb, 0xbf, 0x24);
pub(super) const RED: Color = Color::from_rgb8(0xf8, 0x71, 0x71);

pub(super) const INTER: Font = Font::with_name("Inter");
pub(super) const MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..INTER
};
pub(super) const SEMI: Font = Font {
    weight: font::Weight::Semibold,
    ..INTER
};
pub(super) const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..INTER
};
pub(super) const MONO: Font = Font::MONOSPACE;

pub(super) fn status_color(s: Status) -> Color {
    match s {
        Status::Watching => BLUE,
        Status::Completed => GREEN,
        Status::Planning => Color::from_rgb8(0xc0, 0x84, 0xfc),
        Status::Rewatching => Color::from_rgb8(0x22, 0xd3, 0xee),
        Status::Paused => GOLD,
        Status::Dropped => RED,
    }
}

// ── Surfaces ──────────────────────────────────────────────────────────

pub(super) fn fill_color(c: Color) -> Background {
    Background::Color(c)
}

pub(super) fn fill(c: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(c)),
        ..Default::default()
    }
}

pub(super) fn rounded(r: f32) -> Border {
    Border {
        radius: r.into(),
        ..Border::default()
    }
}

pub(super) fn pill(bg: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(bg)),
        border: rounded(999.0),
        ..Default::default()
    }
}

pub(super) fn card_style(bg: Color) -> container::Style {
    container::Style {
        background: Some(fill_color(bg)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

/// A bordered box around a group of things.
pub(super) fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(|_| card_style(SURFACE))
        .into()
}

pub(super) fn accent_gradient() -> Background {
    Background::Gradient(
        gradient::Linear::new(std::f32::consts::FRAC_PI_2 * 1.5)
            .add_stop(0.0, ACCENT)
            .add_stop(1.0, Color::from_rgb8(0x5b, 0x8c, 0xff))
            .into(),
    )
}

pub(super) fn dot<'a>(color: Color) -> Element<'a, Message> {
    container(space())
        .width(8)
        .height(8)
        .style(move |_| pill(color))
        .into()
}

// ── Text ──────────────────────────────────────────────────────────────

/// A page's title and what it's about.
pub(super) fn page_title<'a>(title: &'a str, note: String) -> Element<'a, Message> {
    column![
        text(title).size(30).font(BOLD).color(TEXT),
        text(note).size(13).color(MUTED),
    ]
    .spacing(2)
    .into()
}

pub(super) fn section<'a>(title: &'a str, note: Option<String>) -> Element<'a, Message> {
    let mut r = row![text(title).size(18).font(SEMI).color(TEXT)]
        .spacing(12)
        .align_y(Alignment::End);
    if let Some(n) = note {
        r = r.push(text(n).size(13).color(MUTED));
    }
    container(r)
        .padding(iced::Padding {
            bottom: 12.0,
            ..Default::default()
        })
        .into()
}

/// A small heading in capitals, over a group of settings or a list.
pub(super) fn label<'a>(s: &'a str) -> Element<'a, Message> {
    text(s.to_uppercase())
        .size(11)
        .font(SEMI)
        .color(FAINT)
        .into()
}

pub(super) fn body<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).size(14).color(TEXT)
}

pub(super) fn weak<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).size(13).color(MUTED)
}

pub(super) fn error_text<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).size(13).color(RED)
}

/// A colored tag: "★ Best", "Usenet", "Dual audio".
pub(super) fn badge<'a>(s: impl text::IntoFragment<'a>, color: Color) -> Element<'a, Message> {
    container(text(s).size(11).font(SEMI).color(color))
        .padding([2, 8])
        .style(move |_| pill(color.scale_alpha(0.15)))
        .into()
}

pub(super) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>().trim_end())
    }
}

// ── Buttons ───────────────────────────────────────────────────────────

/// The main thing to do: filled with the accent. `None` grays it out.
pub(super) fn primary<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    button(text(label).size(14).font(SEMI).color(Color::WHITE))
        .padding([10, 18])
        .on_press_maybe(on_press)
        .style(move |_, status| button::Style {
            background: Some(if enabled {
                accent_gradient()
            } else {
                fill_color(SURFACE2)
            }),
            text_color: Color::WHITE,
            border: rounded(10.0),
            shadow: if enabled && status != button::Status::Pressed {
                Shadow {
                    color: ACCENT.scale_alpha(0.35),
                    offset: Vector::new(0.0, 6.0),
                    blur_radius: 18.0,
                }
            } else {
                Shadow::default()
            },
            ..button::Style::default()
        })
        .into()
}

/// Other things to do: outlined.
pub(super) fn secondary<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    outlined(label, on_press, [10, 16], 14.0, TEXT)
}

/// A small outlined button, for rows in lists.
pub(super) fn small<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    outlined(label, on_press, [5, 10], 12.0, TEXT)
}

/// A small button for things that delete.
pub(super) fn danger<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    outlined(label, on_press, [10, 16], 14.0, RED)
}

fn outlined<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
    padding: [u16; 2],
    size: f32,
    color: Color,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    button(
        text(label)
            .size(size)
            .font(MEDIUM)
            .color(if enabled { color } else { FAINT }),
    )
    .padding(padding)
    .on_press_maybe(on_press)
    .style(move |_, status| button::Style {
        background: Some(fill_color(
            if enabled && matches!(status, button::Status::Hovered | button::Status::Pressed) {
                SURFACE2
            } else {
                SURFACE
            },
        )),
        text_color: color,
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 9.0.into(),
        },
        ..button::Style::default()
    })
    .into()
}

/// A text-only button (links, "Back").
pub(super) fn link<'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    button(text(label).size(13).font(MEDIUM))
        .padding([4, 6])
        .on_press_maybe(on_press)
        .style(move |_, status| button::Style {
            background: None,
            text_color: if !enabled {
                FAINT
            } else if status == button::Status::Hovered {
                TEXT
            } else {
                ACCENT
            },
            ..button::Style::default()
        })
        .into()
}

/// A selectable chip (filters, choices).
pub(super) fn chip<'a>(
    label: impl text::IntoFragment<'a>,
    on: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(
        text(label)
            .size(13)
            .font(MEDIUM)
            .color(if on { Color::WHITE } else { MUTED }),
    )
    .padding([7, 14])
    .on_press_maybe(on_press)
    .style(move |_, status| button::Style {
        background: Some(if on {
            accent_gradient()
        } else if status == button::Status::Hovered {
            fill_color(SURFACE2)
        } else {
            fill_color(SURFACE)
        }),
        border: Border {
            color: if on { Color::TRANSPARENT } else { LINE },
            width: 1.0,
            radius: 999.0.into(),
        },
        text_color: TEXT,
        ..button::Style::default()
    })
    .into()
}

// ── Fields ────────────────────────────────────────────────────────────

pub(super) fn field<'a>(placeholder: &str, value: &str) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value)
        .padding([10, 14])
        .size(14)
        .style(field_style)
}

pub(super) fn field_style(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: fill_color(SURFACE),
        border: Border {
            color: if focused { ACCENT } else { LINE },
            width: 1.0,
            radius: 10.0.into(),
        },
        icon: MUTED,
        placeholder: FAINT,
        value: TEXT,
        selection: ACCENT.scale_alpha(0.4),
    }
}

/// A checkbox whose label can be clicked too.
pub(super) fn check<'a>(
    label: impl text::IntoFragment<'a>,
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
                background: fill_color(if checked {
                    ACCENT
                } else if hovered {
                    SURFACE2
                } else {
                    SURFACE
                }),
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

pub(super) fn pick_style(_: &Theme, _: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: FAINT,
        handle_color: MUTED,
        background: fill_color(SURFACE2),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 8.0.into(),
        },
    }
}

pub(super) fn menu_style(_: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: fill_color(SURFACE2),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 8.0.into(),
        },
        text_color: TEXT,
        selected_text_color: Color::WHITE,
        selected_background: fill_color(ACCENT.scale_alpha(0.6)),
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(0.4),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 20.0,
        },
    }
}

/// A dropdown in the app's colors.
pub(super) fn dropdown<'a, T>(
    options: Vec<T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: ToString + PartialEq + Clone + 'a,
{
    pick_list(options, selected, on_select)
        .text_size(13)
        .padding([7, 10])
        .style(pick_style)
        .menu_style(menu_style)
        .into()
}

pub(super) fn scroll_style(_: &Theme, _: scrollable::Status) -> scrollable::Style {
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: fill_color(LINE),
            border: rounded(999.0),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: fill_color(SURFACE),
            border: rounded(999.0),
            shadow: Shadow::default(),
            icon: TEXT,
        },
    }
}

/// A page's content, scrolling, with the usual margins.
pub(super) fn page<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    scrollable(container(content).padding([28, 36]).width(Fill))
        .height(Fill)
        .style(scroll_style)
        .into()
}

// ── Progress ──────────────────────────────────────────────────────────

pub(super) fn bar<'a>(value: f32, color: Color, girth: f32) -> progress_bar::ProgressBar<'a> {
    progress_bar(0.0..=1.0, value.clamp(0.0, 1.0))
        .girth(girth)
        .style(move |_| progress_bar::Style {
            background: fill_color(SURFACE2),
            bar: fill_color(color),
            border: rounded(999.0),
        })
}

// ── Pictures ──────────────────────────────────────────────────────────

/// A cover (or banner), rounded with a shadow; a plain box until it's here.
pub(super) fn picture<'a>(
    handle: Option<image::Handle>,
    w: impl Into<Length>,
    h: impl Into<Length>,
    radius: f32,
) -> Element<'a, Message> {
    let (w, h) = (w.into(), h.into());
    match handle {
        Some(handle) => container(
            image(handle)
                .width(w)
                .height(h)
                .content_fit(ContentFit::Cover)
                .border_radius(radius),
        )
        .style(move |_| container::Style {
            shadow: Shadow {
                color: Color::BLACK.scale_alpha(0.45),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 20.0,
            },
            border: rounded(radius),
            ..Default::default()
        })
        .into(),
        None => container(space())
            .width(w)
            .height(h)
            .style(move |_| container::Style {
                background: Some(fill_color(SURFACE2)),
                border: rounded(radius),
                ..Default::default()
            })
            .into(),
    }
}

// ── Dialogs ───────────────────────────────────────────────────────────

/// `content` in a box over the dimmed page; clicking outside sends
/// `dismiss`.
pub(super) fn dialog<'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    width: f32,
    dismiss: Message,
) -> Element<'a, Message> {
    let panel = container(scrollable(content).style(scroll_style))
        .padding(24)
        .max_width(width)
        .max_height(760)
        .style(|_| container::Style {
            background: Some(fill_color(SURFACE)),
            border: Border {
                color: LINE,
                width: 1.0,
                radius: 16.0.into(),
            },
            shadow: Shadow {
                color: Color::BLACK.scale_alpha(0.6),
                offset: Vector::new(0.0, 16.0),
                blur_radius: 48.0,
            },
            ..Default::default()
        });
    stack![
        base,
        opaque(
            mouse_area(
                container(opaque(panel))
                    .center(Fill)
                    .style(|_| fill(Color::BLACK.scale_alpha(0.55)))
            )
            .on_press(dismiss)
        )
    ]
    .into()
}

/// A dialog's heading and what it's about.
pub(super) fn dialog_title<'a>(title: String, note: Option<String>) -> Element<'a, Message> {
    let mut c = column![text(title).size(20).font(BOLD).color(TEXT)].spacing(6);
    if let Some(n) = note {
        c = c.push(weak(n));
    }
    c.into()
}

pub(super) fn gap<'a>(h: f32) -> Element<'a, Message> {
    Space::new().height(h).into()
}
