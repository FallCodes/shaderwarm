//! Canvas-drawn shapes: slanted buttons and tags, cover decoration, the diagonal wipe,
//! stat cards, bars and the static backdrop. Every program is stateless; animation
//! values come in as fields.

use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{mouse, Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme};

use crate::anim::{ease_out, lerp};
use crate::style::{alpha, mix, BLACK, GRAPHITE, GREY, INK, PAPER, RED, RED_HI};

pub const DISPLAY: Font = Font::with_name("Anton");
/// Room left on the right and bottom of slanted buttons for their offset shadow.
const SHADOW: f32 = 5.0;

pub fn slant(x: f32, y: f32, w: f32, h: f32, skew: f32) -> Path {
    Path::new(|b| {
        b.move_to(Point::new(x + skew, y));
        b.line_to(Point::new(x + w, y));
        b.line_to(Point::new(x + w - skew, y + h));
        b.line_to(Point::new(x, y + h));
        b.close();
    })
}

fn text(frame: &mut Frame, content: &str, at: Point, size: f32, color: Color, font: Font, h: Horizontal) {
    frame.fill_text(Text {
        content: content.to_string(),
        position: at,
        color,
        size: Pixels(size),
        font,
        horizontal_alignment: h,
        vertical_alignment: Vertical::Center,
        ..Text::default()
    });
}

/// Width a slanted button needs for `label` at `size` (Anton is condensed, ~0.5 em).
pub fn button_width(label: &str, size: f32) -> f32 {
    label.chars().count() as f32 * size * 0.5 + size * 1.9 + SHADOW
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Red, inverts to paper on hover.
    Primary,
    /// Graphite, inverts to paper on hover.
    Secondary,
    /// Unselected tab or chip.
    Ghost,
    /// Selected tab or chip.
    Active,
    /// A static label: fill and text colour.
    Tag(Color, Color),
}

/// Slanted button or tag. `hover` is the 0..1 hover fade.
pub struct Slant {
    pub label: String,
    pub kind: Kind,
    pub hover: f32,
    pub enabled: bool,
    pub size: f32,
}

impl<M> canvas::Program<M> for Slant {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width - SHADOW, bounds.height - SHADOW);
        let skew = (h * 0.3).min(14.0);
        let p = if self.enabled { self.hover } else { 0.0 };
        let (fill, ink, shadow) = match (self.kind, self.enabled) {
            (Kind::Tag(fill, ink), _) => (fill, ink, None),
            (_, false) => (alpha(GRAPHITE, 0.55), alpha(GREY, 0.6), None),
            (Kind::Primary, _) => (mix(RED, PAPER, p), mix(PAPER, BLACK, p), Some(mix(BLACK, RED_HI, p))),
            (Kind::Secondary, _) => (mix(GRAPHITE, PAPER, p), mix(PAPER, BLACK, p), Some(alpha(RED_HI, p))),
            (Kind::Ghost, _) => (mix(INK, GRAPHITE, p), mix(GREY, PAPER, p), Some(alpha(RED_HI, p))),
            (Kind::Active, _) => (PAPER, BLACK, Some(RED_HI)),
        };
        if let Some(shadow) = shadow {
            let off = 2.0 + 3.0 * p.max(if self.kind == Kind::Active { 1.0 } else { 0.0 });
            f.fill(&slant(off, off, w, h, skew), shadow);
        }
        f.fill(&slant(0.0, 0.0, w, h, skew), fill);
        text(&mut f, &self.label.to_uppercase(), Point::new(w / 2.0, h / 2.0), self.size, ink, DISPLAY, Horizontal::Center);
        vec![f.into_geometry()]
    }
}

/// Everything around a cover: the red slab that slides out on hover/selection, the
/// selection outline and the name strip below.
pub struct TileDeco {
    pub name: String,
    pub hover: f32,
    pub selected: f32,
    pub inset: f32,
    pub cover: Size,
}

impl<M> canvas::Program<M> for TileDeco {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (i, cw, ch) = (self.inset, self.cover.width, self.cover.height);
        let k = self.hover.max(self.selected);
        if k > 0.0 {
            let off = 3.0 + 7.0 * k;
            f.fill(&slant(i + off * 0.5, i + off, cw, ch, 16.0), alpha(if self.selected > 0.0 { RED_HI } else { RED }, k));
        }
        if self.selected > 0.0 {
            f.stroke(
                &slant(i - 5.0, i - 5.0, cw + 10.0, ch + 10.0, 12.0),
                Stroke::default().with_color(alpha(PAPER, self.selected)).with_width(3.0),
            );
        }
        let strip_y = i + ch + 10.0;
        let strip_h = 26.0;
        let fill = mix(mix(GRAPHITE, PAPER, self.hover), RED, self.selected);
        let ink = mix(mix(PAPER, BLACK, self.hover), PAPER, self.selected);
        f.fill(&slant(i, strip_y, cw, strip_h, 8.0), fill);
        let max_chars = ((cw - 18.0) / (14.0 * 0.5)) as usize;
        text(&mut f, &truncate(&self.name.to_uppercase(), max_chars), Point::new(i + cw / 2.0, strip_y + strip_h / 2.0), 14.0, ink, DISPLAY, Horizontal::Center);
        vec![f.into_geometry()]
    }
}

/// Diagonal red band that sweeps across the hero when the selection changes.
pub struct Wipe {
    pub t: f32,
}

impl<M> canvas::Program<M> for Wipe {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        if self.t >= 1.0 {
            return Vec::new();
        }
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let skew = h * 0.45;
        let span = w + 2.0 * skew;
        // The leading edge rushes in, the trailing edge follows: covered, then revealed.
        let lead = -skew + span * ease_out(self.t / 0.45);
        let trail = -skew + span * ease_out((self.t - 0.3) / 0.7);
        if lead > trail {
            f.fill(&slant(trail, 0.0, lead - trail + skew, h, skew), RED);
            let edge = 22.0;
            f.fill(&slant(lead - edge, 0.0, edge + skew, h, skew), BLACK);
        }
        vec![f.into_geometry()]
    }
}

pub struct StatCard {
    pub value: String,
    pub label: String,
    pub color: Color,
    pub hot: bool,
}

impl<M> canvas::Program<M> for StatCard {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let skew = 18.0;
        if self.hot {
            f.fill(&slant(4.0, 4.0, w - 4.0, h - 4.0, skew), RED);
        }
        f.fill(&slant(0.0, 0.0, w - 4.0, h - 4.0, skew), INK);
        f.fill(&slant(skew * 0.35, 0.0, 9.0, h - 4.0, skew * 0.65), self.color);
        text(&mut f, &self.value, Point::new(34.0, h * 0.40), 34.0, self.color, DISPLAY, Horizontal::Left);
        text(&mut f, &self.label.to_uppercase(), Point::new(30.0, h * 0.76), 12.0, GREY, Font::DEFAULT, Horizontal::Left);
        vec![f.into_geometry()]
    }
}

/// Segmented slanted bar: (fraction, colour) parts left to right.
pub struct SegBar {
    pub parts: Vec<(f32, Color)>,
}

impl<M> canvas::Program<M> for SegBar {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let skew = h * 0.6;
        f.fill(&slant(0.0, 0.0, w, h, skew), GRAPHITE);
        let gap = 3.0;
        let usable = w - skew - gap * self.parts.len().saturating_sub(1) as f32;
        let mut x = 0.0;
        for (frac, color) in &self.parts {
            let seg = (usable * frac).max(4.0);
            f.fill(&slant(x, 0.0, seg + skew, h, skew), *color);
            x += seg + gap;
        }
        vec![f.into_geometry()]
    }
}

/// Progress bar with stripes that march while a job runs.
pub struct Stripes {
    pub frac: f32,
    pub phase: f32,
}

impl<M> canvas::Program<M> for Stripes {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let skew = h * 0.7;
        f.fill(&slant(0.0, 0.0, w, h, skew), GRAPHITE);
        let filled = skew + (w - skew) * self.frac.clamp(0.0, 1.0);
        f.fill(&slant(0.0, 0.0, filled, h, skew), RED);
        f.with_clip(Rectangle::new(Point::ORIGIN, Size::new(filled - skew * 0.5, h)), |f| {
            let step = 16.0;
            let mut x = -2.0 * step + self.phase % step;
            while x < filled {
                f.fill(&slant(x, 0.0, 7.0 + skew, h, skew), alpha(PAPER, 0.22));
                x += step;
            }
        });
        vec![f.into_geometry()]
    }
}

/// The wordmark: paper letters on a black slab over an offset red slab.
pub struct Title {
    pub intro: f32,
}

impl<M> canvas::Program<M> for Title {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width - 10.0, bounds.height - 8.0);
        let skew = 16.0;
        let slide = lerp(-40.0, 0.0, ease_out(self.intro));
        f.fill(&slant(8.0 + slide * 0.5, 8.0, w, h, skew), RED);
        f.fill(&slant(slide, 0.0, w, h, skew), BLACK);
        f.stroke(&slant(slide, 0.0, w, h, skew), Stroke::default().with_color(PAPER).with_width(2.0));
        text(&mut f, "SHADERWARM", Point::new(slide + w / 2.0, h / 2.0 + 1.0), 34.0, alpha(PAPER, self.intro.max(0.2)), DISPLAY, Horizontal::Center);
        vec![f.into_geometry()]
    }
}

/// Static backdrop: diagonal red slashes and a halftone fade, drawn once per size.
pub struct Backdrop<'a> {
    pub cache: &'a canvas::Cache,
}

impl<M> canvas::Program<M> for Backdrop<'_> {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |f| {
            let (w, h) = (bounds.width, bounds.height);
            f.fill_rectangle(Point::ORIGIN, bounds.size(), BLACK);
            let band = |x0: f32, x1: f32, lean: f32| {
                Path::new(|b| {
                    b.move_to(Point::new(w * x0, 0.0));
                    b.line_to(Point::new(w * x1, 0.0));
                    b.line_to(Point::new(w * x1 - lean, h));
                    b.line_to(Point::new(w * x0 - lean, h));
                    b.close();
                })
            };
            let lean = h * 0.55;
            f.fill(&band(0.66, 0.80, lean), alpha(RED, 0.07));
            f.fill(&band(0.815, 0.822, lean), alpha(RED_HI, 0.30));
            f.fill(&band(0.86, 0.93, lean), alpha(RED, 0.04));
            // Halftone dots growing toward the bottom-left corner.
            let step = 18.0;
            let (cols, rows) = ((w * 0.45 / step) as i32, (h * 0.5 / step) as i32);
            for gy in 0..rows {
                for gx in 0..cols {
                    let (x, y) = (gx as f32 * step + step / 2.0, h - gy as f32 * step - step / 2.0);
                    let d = ((gx as f32 / cols as f32).powi(2) + (gy as f32 / rows as f32).powi(2)).sqrt();
                    let r = 3.6 * (1.0 - d);
                    if r > 0.4 {
                        f.fill(&Path::circle(Point::new(x, y), r), alpha(PAPER, 0.045));
                    }
                }
            }
        });
        vec![geometry]
    }
}
