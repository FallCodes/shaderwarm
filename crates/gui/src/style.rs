//! Palette and widget styles. Ink black, paper white and one hard red, with yellow kept
//! for a single meaning (games that compile their own shaders). Corners are sharp;
//! slanted shapes come from `shapes`.

use iced::widget::{container, text_input};
use iced::{Border, Color, Theme};

const fn rgb(hex: u32) -> Color {
    Color::from_rgb(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

pub const BLACK: Color = rgb(0x0b0b0c);
pub const INK: Color = rgb(0x151517);
pub const GRAPHITE: Color = rgb(0x26262a);
pub const SMOKE: Color = rgb(0x3a3a40);
/// Secondary text: 7:1 on BLACK.
pub const GREY: Color = rgb(0x9a9a9e);
pub const PAPER: Color = rgb(0xf2efe6);
/// Text-safe red: PAPER on RED and RED on BLACK at large sizes both pass contrast.
pub const RED: Color = rgb(0xd10a24);
/// Brighter red for shapes only, never for text.
pub const RED_HI: Color = rgb(0xff2a3d);
pub const YELLOW: Color = rgb(0xffd60a);

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgba(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, a.a + (b.a - a.a) * t)
}

pub fn theme() -> Theme {
    Theme::custom(
        "shaderwarm".to_string(),
        iced::theme::Palette { background: BLACK, text: PAPER, primary: RED, success: PAPER, danger: RED },
    )
}

fn sharp(color: Color, width: f32) -> Border {
    Border { color, width, radius: 0.0.into() }
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style { background: Some(INK.into()), border: sharp(GRAPHITE, 1.0), ..container::Style::default() }
}

pub fn well(_: &Theme) -> container::Style {
    container::Style { background: Some(BLACK.into()), border: sharp(GRAPHITE, 1.0), ..container::Style::default() }
}

/// Dialogs and warnings: black with a hard red frame.
pub fn alert(_: &Theme) -> container::Style {
    container::Style { background: Some(BLACK.into()), border: sharp(RED, 3.0), ..container::Style::default() }
}

/// A plain fill, used for reveal curtains and fades.
pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style { background: Some(color.into()), ..container::Style::default() }
}

/// Fades the bottom of the hero image into the panel.
pub fn hero_fade(_: &Theme) -> container::Style {
    let fade = iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI))
        .add_stop(0.0, alpha(INK, 0.0))
        .add_stop(0.6, alpha(INK, 0.25))
        .add_stop(1.0, INK);
    container::Style { background: Some(fade.into()), ..container::Style::default() }
}

pub fn search(_: &Theme, status: text_input::Status) -> text_input::Style {
    let edge = match status {
        text_input::Status::Focused => PAPER,
        text_input::Status::Hovered => GREY,
        _ => SMOKE,
    };
    text_input::Style {
        background: BLACK.into(),
        border: sharp(edge, 2.0),
        icon: GREY,
        placeholder: GREY,
        value: PAPER,
        selection: alpha(RED, 0.6),
    }
}
