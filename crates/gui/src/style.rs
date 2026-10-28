//! Palette and widget styles: a dark slate base with the icon's warm gradient as accent.

use iced::widget::{button, container};
use iced::{gradient, Background, Border, Color, Radians, Shadow, Theme, Vector};

const fn rgb(hex: u32) -> Color {
    Color::from_rgb(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

pub const BG: Color = rgb(0x0e1118);
pub const SURFACE: Color = rgb(0x161a24);
pub const SURFACE_HI: Color = rgb(0x212634);
pub const TEXT: Color = rgb(0xe8eaf0);
pub const MUTED: Color = rgb(0x8b93a7);
pub const ACCENT: Color = rgb(0xff8a3d);
pub const GOLD: Color = rgb(0xffd166);
pub const ROSE: Color = rgb(0xff4d6d);
pub const GOOD: Color = rgb(0x4ade80);
pub const WARN: Color = rgb(0xffb547);
pub const VIOLET: Color = rgb(0xa78bfa);
pub const SKY: Color = rgb(0x60a5fa);

pub fn theme() -> Theme {
    Theme::custom(
        "shaderwarm".to_string(),
        iced::theme::Palette { background: BG, text: TEXT, primary: ACCENT, success: GOOD, danger: ROSE },
    )
}

fn radius(r: f32) -> Border {
    Border { radius: r.into(), ..Border::default() }
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// The icon's rose → amber → gold sweep.
pub fn warm_gradient(alpha: f32) -> Background {
    gradient::Linear::new(Radians(std::f32::consts::FRAC_PI_2))
        .add_stop(0.0, with_alpha(ROSE, alpha))
        .add_stop(0.55, with_alpha(ACCENT, alpha))
        .add_stop(1.0, with_alpha(GOLD, alpha))
        .into()
}

pub fn card(_: &Theme) -> container::Style {
    container::Style { background: Some(SURFACE.into()), border: radius(14.0), ..container::Style::default() }
}

pub fn card_hi(_: &Theme) -> container::Style {
    container::Style { background: Some(SURFACE_HI.into()), border: radius(12.0), ..container::Style::default() }
}

/// Stat card with a faint warm wash.
pub fn stat_card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: Border { color: with_alpha(ACCENT, 0.18), width: 1.0, radius: 14.0.into() },
        ..container::Style::default()
    }
}

pub fn dialog(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE_HI.into()),
        border: Border { color: with_alpha(ACCENT, 0.6), width: 1.5, radius: 14.0.into() },
        shadow: Shadow { color: with_alpha(Color::BLACK, 0.5), offset: Vector::new(0.0, 8.0), blur_radius: 24.0 },
        ..container::Style::default()
    }
}

/// Fades the bottom of the hero image into the panel.
pub fn hero_fade(_: &Theme) -> container::Style {
    let fade = gradient::Linear::new(Radians(std::f32::consts::PI))
        .add_stop(0.0, with_alpha(SURFACE, 0.0))
        .add_stop(0.55, with_alpha(SURFACE, 0.15))
        .add_stop(1.0, SURFACE);
    container::Style { background: Some(fade.into()), ..container::Style::default() }
}

pub fn pill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        text_color: Some(BG),
        border: radius(999.0),
        ..container::Style::default()
    }
}

pub fn chip(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE_HI.into()),
        text_color: Some(TEXT),
        border: radius(999.0),
        ..container::Style::default()
    }
}

pub fn swatch(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style { background: Some(color.into()), border: radius(3.0), ..container::Style::default() }
}

/// Placeholder cover for games without artwork.
pub fn cover_placeholder(_: &Theme) -> container::Style {
    container::Style { background: Some(warm_gradient(0.35)), border: radius(10.0), ..container::Style::default() }
}

fn button_base(bg: Color, fg: Color, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color { r: (bg.r + 0.06).min(1.0), g: (bg.g + 0.06).min(1.0), b: (bg.b + 0.06).min(1.0), ..bg },
        button::Status::Pressed => Color { r: bg.r * 0.9, g: bg.g * 0.9, b: bg.b * 0.9, ..bg },
        button::Status::Disabled => with_alpha(bg, 0.35),
        button::Status::Active => bg,
    };
    let fg = if status == button::Status::Disabled { with_alpha(fg, 0.5) } else { fg };
    button::Style { background: Some(bg.into()), text_color: fg, border: radius(10.0), shadow: Shadow::default() }
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let mut s = button_base(ACCENT, BG, status);
    if status != button::Status::Disabled {
        s.background = Some(warm_gradient(if status == button::Status::Hovered { 1.0 } else { 0.92 }));
    }
    s
}

pub fn secondary(_: &Theme, status: button::Status) -> button::Style {
    button_base(SURFACE_HI, TEXT, status)
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    button_base(ROSE, BG, status)
}

/// Header tabs and filter chips: highlighted when `on`.
pub fn toggle(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        if on {
            let mut s = button_base(with_alpha(ACCENT, 0.18), ACCENT, status);
            s.border = Border { color: with_alpha(ACCENT, 0.7), width: 1.0, radius: 999.0.into() };
            s
        } else {
            let mut s = secondary(theme, status);
            s.border.radius = 999.0.into();
            s.background = Some(if status == button::Status::Hovered { SURFACE_HI } else { SURFACE }.into());
            s
        }
    }
}

/// Cover tiles: no fill, a warm frame when selected or hovered.
pub fn tile(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let border = match (selected, status) {
            (true, _) => Border { color: ACCENT, width: 2.5, radius: 14.0.into() },
            (false, button::Status::Hovered) => Border { color: with_alpha(GOLD, 0.6), width: 2.0, radius: 14.0.into() },
            _ => Border { color: Color::TRANSPARENT, width: 2.5, radius: 14.0.into() },
        };
        button::Style {
            background: Some(if selected { with_alpha(ACCENT, 0.08) } else { Color::TRANSPARENT }.into()),
            text_color: TEXT,
            border,
            shadow: Shadow::default(),
        }
    }
}
