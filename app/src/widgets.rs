//! The few widgets the dashboard needs that Denise does not ship.
//!
//! All of them are display-only: nothing here accepts a pointer, so a click
//! falls through to whatever sits underneath, and every one of them paints from
//! theme roles so a theme switch reaches them like any other widget.

use std::collections::VecDeque;

use denise::theme::{AA_LARGE, contrast_x100};
use denise::{Color, Pen, Point, Rect, Role};
use denise_ui::widgets::Align;
use denise_ui::{PaintCtx, TextStyle, Widget};

use crate::format;

/// How a [`Text`] is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// The content colour of the surface it sits on.
    Content,
    /// Quieter: labels, units, captions. Falls back to `Content` if muting
    /// would drop below the large-text contrast floor.
    Muted,
    /// A role's own colour, for status words ("Accepted" in success green).
    Role(Role),
}

/// A line or paragraph of text.
///
/// Denise's `Label` draws one line in one colour. The dashboard wants muted
/// captions, paragraphs that wrap, and hashes that shorten with an ellipsis
/// when the window is narrow, so it gets this instead.
pub struct Text {
    text: String,
    style: TextStyle,
    tone: Tone,
    surface: Role,
    align: Align,
    wrap: bool,
}

impl Text {
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            tone: Tone::Content,
            surface: Role::Base100,
            align: Align::Start,
            wrap: false,
        }
    }

    pub fn muted(mut self) -> Self {
        self.tone = Tone::Muted;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn wrapped(mut self) -> Self {
        self.wrap = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn current_tone(&self) -> Tone {
        self.tone
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    pub fn set_tone(&mut self, tone: Tone) {
        self.tone = tone;
    }

    fn color(&self, ctx: &PaintCtx<'_>) -> Color {
        let theme = ctx.theme;
        let surface = theme.color(self.surface);
        let content = theme.content_of(self.surface);
        match self.tone {
            Tone::Content => content,
            Tone::Muted => {
                let muted = content.mix(surface, 90);
                if contrast_x100(surface, muted) >= AA_LARGE { muted } else { content }
            }
            Tone::Role(role) => theme.color(role),
        }
    }
}

impl<M: 'static> Widget<M> for Text {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let color = self.color(ctx);
        let bounds = ctx.bounds;
        let engine = &mut *ctx.text;
        if self.wrap {
            let line_height = engine.line_height(self.style);
            let lines: Vec<String> = engine
                .wrap(self.style, &self.text, bounds.width)
                .into_iter()
                .map(str::to_owned)
                .collect();
            for (i, line) in lines.iter().enumerate() {
                let y = bounds.y + i as i32 * line_height;
                if y > bounds.bottom() {
                    break;
                }
                engine.draw(pen, self.style, Point::new(bounds.x, y), line, color);
            }
            return;
        }
        let fitted = fit(engine, self.style, &self.text, bounds.width);
        let size = engine.measure(self.style, &fitted);
        let at = Point::new(
            bounds.x + self.align.offset(bounds.width, size.width as i32),
            bounds.y + (bounds.height - size.height as i32) / 2,
        );
        engine.draw(pen, self.style, at, &fitted, color);
    }
}

/// `text`, shortened in the middle with an ellipsis until it fits `width`.
///
/// The middle rather than the end, because the text that overflows here is
/// mostly hashes and URLs, where both ends carry the meaning.
fn fit(engine: &mut denise_text::TextEngine, style: TextStyle, text: &str, width: i32) -> String {
    if width <= 0 || engine.measure_line(style, text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 2 {
        keep -= 1;
        let head: String = chars[..keep.div_ceil(2)].iter().collect();
        let tail: String = chars[chars.len() - keep / 2..].iter().collect();
        let candidate = format!("{head}…{tail}");
        if engine.measure_line(style, &candidate) <= width {
            return candidate;
        }
    }
    "…".into()
}

/// Hashrate over time: a filled area under a line, with a light grid.
pub struct Chart {
    samples: Vec<f64>,
    style: TextStyle,
    /// The most samples the x axis ever spans; fewer are stretched from a
    /// minimum window so the first minute does not look like an empty chart.
    capacity: usize,
}

impl Chart {
    pub fn new(style: TextStyle, capacity: usize) -> Self {
        Self {
            samples: Vec::new(),
            style,
            capacity,
        }
    }

    pub fn same_as(&self, samples: &VecDeque<f64>) -> bool {
        self.samples.len() == samples.len() && self.samples.last() == samples.back()
    }

    pub fn set_samples(&mut self, samples: &VecDeque<f64>) {
        self.samples.clear();
        self.samples.extend(samples.iter().copied());
    }
}

impl<M: 'static> Widget<M> for Chart {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let theme = ctx.theme;
        let b = ctx.bounds;
        let label_h = ctx.text.line_height(self.style) + 4;
        let plot = Rect::new(b.x, b.y + label_h / 2, b.width, b.height - label_h - label_h / 2);
        if plot.width < 8 || plot.height < 8 {
            return;
        }
        let grid = theme.color(Role::Base300);
        let muted = theme.content_of(Role::Base100).mix(theme.color(Role::Base100), 110);
        let line = theme.color(Role::Primary);
        let fill = line.with_alpha(44);
        let surface = theme.color(Role::Base100);

        let peak = self.samples.iter().copied().fold(0.0f64, f64::max);
        let top = nice_ceiling(peak * 1.1);

        // Grid lines first so the area sits over them; their values last, so
        // the area does not wash them out.
        for i in 0..=4 {
            let y = plot.bottom() - plot.height * i / 4;
            pen.fill_rect(Rect::new(plot.x, y, plot.width, 1), grid);
        }
        let grid_labels = |pen: &mut Pen<'_>, engine: &mut denise_text::TextEngine| {
            for i in 1..=4 {
                let y = plot.bottom() - plot.height * i / 4;
                let text = format::hashrate(top * i as f64 / 4.0);
                let size = engine.measure(self.style, &text);
                let chip = Rect::new(plot.x + 2, y + 2, size.width as i32 + 8, size.height as i32 + 2);
                pen.fill_rounded_rect(chip, 4, surface.with_alpha(200));
                engine.draw(pen, self.style, Point::new(plot.x + 6, y + 3), &text, muted);
            }
        };

        let window = self.samples.len().clamp(60, self.capacity.max(60));
        let axis = [
            (0.0, format!("{} min ago", window.div_ceil(60))),
            (1.0, "now".to_string()),
        ];
        for (at, text) in axis {
            let w = ctx.text.measure_line(self.style, &text);
            let x = plot.x + ((plot.width - w) as f64 * at) as i32;
            ctx.text.draw(pen, self.style, Point::new(x, plot.bottom() + 4), &text, muted);
        }

        if self.samples.len() < 2 || top <= 0.0 {
            let text = "Waiting for hashes…";
            let w = ctx.text.measure_line(self.style, text);
            let at = Point::new(plot.x + (plot.width - w) / 2, plot.y + plot.height / 2 - 8);
            ctx.text.draw(pen, self.style, at, text, muted);
            return;
        }

        // Samples occupy the right-hand end of a `window`-wide axis.
        let n = self.samples.len();
        let first_x = plot.x + plot.width - ((n - 1) as i64 * plot.width as i64 / (window - 1) as i64) as i32;
        let value_at = |x: i32| -> f64 {
            let pos = (x - first_x) as f64 / (plot.right() - first_x).max(1) as f64 * (n - 1) as f64;
            let i = pos.floor().clamp(0.0, (n - 1) as f64) as usize;
            let j = (i + 1).min(n - 1);
            let t = pos - i as f64;
            self.samples[i] * (1.0 - t) + self.samples[j] * t
        };
        let y_of = |v: f64| plot.bottom() - ((v / top) * plot.height as f64).round() as i32;

        let mut previous: Option<Point> = None;
        for x in first_x.max(plot.x)..plot.right() {
            let y = y_of(value_at(x));
            pen.fill_rect(Rect::new(x, y, 1, plot.bottom() - y), fill);
            let here = Point::new(x, y);
            if let Some(prev) = previous {
                pen.draw_line(prev, here, line);
                pen.draw_line(Point::new(prev.x, prev.y - 1), Point::new(here.x, here.y - 1), line);
            }
            previous = Some(here);
        }
        if let Some(last) = previous {
            pen.fill_circle(last, 4, line);
        }
        grid_labels(pen, ctx.text);
    }
}

/// Rounds up to 1, 2 or 5 times a power of ten, so grid labels are round.
fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 0.0;
    }
    let magnitude = 10f64.powf(v.log10().floor());
    for step in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if step * magnitude >= v {
            return step * magnitude;
        }
    }
    10.0 * magnitude
}

/// Horizontal bars with a label and a value: the benchmark results.
pub struct Bars {
    rows: Vec<BarRow>,
    style: TextStyle,
}

#[derive(Clone, PartialEq)]
pub struct BarRow {
    pub label: String,
    pub value: f64,
    pub value_text: String,
    pub highlight: bool,
}

impl Bars {
    pub fn new(style: TextStyle) -> Self {
        Self {
            rows: Vec::new(),
            style,
        }
    }

    pub fn rows(&self) -> &[BarRow] {
        &self.rows
    }

    pub fn set_rows(&mut self, rows: Vec<BarRow>) {
        self.rows = rows;
    }
}

impl<M: 'static> Widget<M> for Bars {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let theme = ctx.theme;
        let b = ctx.bounds;
        let lh = ctx.text.line_height(self.style);
        let content = theme.content_of(Role::Base100);
        let muted = content.mix(theme.color(Role::Base100), 90);
        if self.rows.is_empty() {
            ctx.text.draw(pen, self.style, Point::new(b.x, b.y), "No benchmark yet — start mining to measure.", muted);
            return;
        }
        let max = self.rows.iter().map(|r| r.value).fold(0.0, f64::max).max(1.0);
        let radius = theme.radius(denise::Radius::Selector).min(6);
        for (i, row) in self.rows.iter().enumerate() {
            let y = b.y + i as i32 * (lh * 2 + 10);
            ctx.text.draw(pen, self.style, Point::new(b.x, y), &row.label, if row.highlight { content } else { muted });
            let vw = ctx.text.measure_line(self.style, &row.value_text);
            ctx.text.draw(pen, self.style, Point::new(b.right() - vw, y), &row.value_text, content);
            let track = Rect::new(b.x, y + lh + 3, b.width, lh.clamp(6, 10));
            pen.fill_rounded_rect(track, radius, theme.color(Role::Base300));
            let w = ((row.value / max) * track.width as f64).round() as i32;
            if w > 0 {
                let role = if row.highlight { Role::Primary } else { Role::Neutral };
                let color = if row.highlight { theme.color(role) } else { content.mix(theme.color(Role::Base300), 150) };
                pen.fill_rounded_rect(Rect::new(track.x, track.y, w.max(radius * 2), track.height), radius, color);
            }
        }
    }
}

/// The 80-byte block header, drawn to scale, with the nonce picked out.
pub struct HeaderMap {
    style: TextStyle,
    small: TextStyle,
}

impl HeaderMap {
    pub fn new(style: TextStyle, small: TextStyle) -> Self {
        Self { style, small }
    }
}

const SEGMENTS: [(&str, i32, Role); 6] = [
    ("Version", 4, Role::Info),
    ("Previous block hash", 32, Role::Secondary),
    ("Merkle root", 32, Role::Accent),
    ("Time", 4, Role::Warning),
    ("Bits", 4, Role::Error),
    ("Nonce", 4, Role::Primary),
];

impl<M: 'static> Widget<M> for HeaderMap {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let theme = ctx.theme;
        let b = ctx.bounds;
        let bar_h = (b.height / 2).clamp(20, 40);
        let gap = 3;
        let usable = b.width - gap * (SEGMENTS.len() as i32 - 1);
        let radius = theme.radius(denise::Radius::Field).min(bar_h / 2);
        let muted = theme.content_of(Role::Base100).mix(theme.color(Role::Base100), 90);
        let mut x = b.x;
        for (i, (name, bytes, role)) in SEGMENTS.iter().enumerate() {
            let w = if i == SEGMENTS.len() - 1 { b.right() - x } else { usable * bytes / 80 };
            let (fill, content) = theme.pair(*role);
            pen.fill_rounded_rect(Rect::new(x, b.y, w, bar_h), radius, fill);
            let label = format!("{bytes}");
            let lw = ctx.text.measure_line(self.small, &label);
            let lh = ctx.text.line_height(self.small);
            if lw + 6 < w {
                ctx.text.draw(pen, self.small, Point::new(x + (w - lw) / 2, b.y + (bar_h - lh) / 2), &label, content);
            }
            // Names under the bar, only where there is room for them; the
            // narrow fields are named in the legend the page draws beside it.
            let nw = ctx.text.measure_line(self.style, name);
            if nw + 8 < w {
                ctx.text.draw(pen, self.style, Point::new(x + (w - nw) / 2, b.y + bar_h + 6), name, muted);
            }
            x += w + gap;
        }
    }
}

/// Instruction-set flags as pills: filled when present, hollow when not.
///
/// Drawn rather than written as ✓ and ✗, which half the system fonts on the
/// machines this runs on do not have.
pub struct Pills {
    items: Vec<(String, bool)>,
    style: TextStyle,
}

impl Pills {
    pub fn new(style: TextStyle) -> Self {
        Self {
            items: Vec::new(),
            style,
        }
    }

    pub fn items(&self) -> &[(String, bool)] {
        &self.items
    }

    pub fn set_items(&mut self, items: Vec<(String, bool)>) {
        self.items = items;
    }
}

impl<M: 'static> Widget<M> for Pills {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let theme = ctx.theme;
        let b = ctx.bounds;
        let lh = ctx.text.line_height(self.style);
        let h = lh + lh / 2;
        let pad = lh * 2 / 3;
        let gap = lh / 2;
        let (mut x, mut y) = (b.x, b.y);
        let base = theme.color(Role::Base100);
        let content = theme.content_of(Role::Base100);
        for (name, on) in &self.items {
            let w = ctx.text.measure_line(self.style, name) + 2 * pad + if *on { h / 2 } else { 0 };
            if x + w > b.right() && x > b.x {
                x = b.x;
                y += h + gap;
            }
            let rect = Rect::new(x, y, w, h);
            let mut tx = x + pad;
            let color = if *on {
                let (fill, fg) = theme.pair(Role::Success);
                pen.fill_rounded_rect(rect, h / 2, fill);
                pen.fill_circle(Point::new(x + pad + h / 6, y + h / 2), (h / 7).max(2), fg);
                tx += h / 2;
                fg
            } else {
                pen.stroke_rounded_rect(rect, h / 2, 1, theme.color(Role::Base300).mix(content, 40));
                content.mix(base, 110)
            };
            ctx.text.draw(pen, self.style, Point::new(tx, y + (h - lh) / 2), name, color);
            x += w + gap;
        }
    }
}

/// The mark in the corner: a ring with an H in it.
pub struct Logo {
    style: TextStyle,
}

impl Logo {
    pub fn new(style: TextStyle) -> Self {
        Self { style }
    }
}

impl<M: 'static> Widget<M> for Logo {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let theme = ctx.theme;
        let b = ctx.bounds;
        let r = b.width.min(b.height) / 2;
        let c = Point::new(b.x + b.width / 2, b.y + b.height / 2);
        let (fill, content) = theme.pair(Role::Primary);
        pen.fill_circle(c, r, fill);
        pen.stroke_circle(c, r - r / 5, (r / 10).max(1), content.with_alpha(90));
        let size = ctx.text.measure(self.style, "H");
        ctx.text.draw(
            pen,
            self.style,
            Point::new(c.x - size.width as i32 / 2, c.y - size.height as i32 / 2),
            "H",
            content,
        );
    }
}

/// A small filled circle in a role colour: the connection light.
pub struct Dot {
    role: Role,
}

impl Dot {
    pub fn new(role: Role) -> Self {
        Self { role }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn set_role(&mut self, role: Role) {
        self.role = role;
    }
}

impl<M: 'static> Widget<M> for Dot {
    fn paint(&self, ctx: &mut PaintCtx<'_>, pen: &mut Pen<'_>) {
        let b = ctx.bounds;
        let r = b.width.min(b.height) / 2;
        let c = Point::new(b.x + b.width / 2, b.y + b.height / 2);
        let color = ctx.theme.color(self.role);
        pen.fill_circle(c, r, color.with_alpha(70));
        pen.fill_circle(c, (r * 3 / 5).max(1), color);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nice_ceilings() {
        assert_eq!(super::nice_ceiling(0.0), 0.0);
        assert_eq!(super::nice_ceiling(3.2e8), 5e8);
        assert_eq!(super::nice_ceiling(1.0e6), 1e6);
        assert_eq!(super::nice_ceiling(1.9e6), 2e6);
    }
}
