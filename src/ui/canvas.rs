//! Shared canvas-grid geometry for the ratatui `Canvas` panels — the world map
//! (`ui::map`) and the sky plot (`ui::skyplot`).
//!
//! A `Canvas` resolves a braille *dot* against a `2·cols × 4·rows` grid and a
//! printed *label* against a `(cols - 1, rows - 1)` grid, and truncates the
//! label's anchor into its cell. [`Grid`] bundles the inner rect and the
//! coordinate bounds and exposes both mappings, so a caller can put a glyph on
//! exactly the cell a dot at the same coordinate lands in.

use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::canvas::Context;

/// Braille dots per cell: a cell is 2 dots wide by 4 tall. `disc_bounds` in
/// `ui::skyplot` and the aspect correction in `ui::map` both rest on this.
pub(in crate::ui) const DOTS_X: u16 = 2;
pub(in crate::ui) const DOTS_Y: u16 = 4;

/// The canvas's cell grid: its inner area plus the coordinate bounds mapped
/// onto it. Bundles what [`col_of`](Grid::col_of)/[`x_of`](Grid::x_of) and the
/// marker-label layout in `ui::map` all need, so they don't each thread
/// `inner`, `x_bounds` and `y_bounds` through by hand.
#[derive(Clone, Copy)]
pub(in crate::ui) struct Grid {
    pub(in crate::ui) inner: Rect,
    pub(in crate::ui) x: [f64; 2],
    pub(in crate::ui) y: [f64; 2],
}

impl Grid {
    pub(in crate::ui) fn cols(&self) -> u16 {
        self.inner.width
    }

    pub(in crate::ui) fn rows(&self) -> u16 {
        self.inner.height
    }

    /// The cell column an x-coordinate lands on, mirroring the mapping
    /// `Canvas::render` itself uses for labels — so a text budget computed
    /// from it matches what's actually drawn. Rounds rather than truncating
    /// (as ratatui's own cast does) so it round-trips cleanly with
    /// [`Grid::x_of`]; ratatui draws the label itself from the raw x/y, so
    /// this only has to be accurate enough for our own budget math, not
    /// bit-identical to its truncation. The tiny epsilon before rounding
    /// makes a coordinate sitting exactly on a half-column boundary resolve
    /// the same way every time, rather than being at the mercy of which way
    /// rounding error happens to fall that frame — exactly the case a
    /// satellite held dead-centre in follow mode hits.
    pub(in crate::ui) fn col_of(&self, x: f64) -> u16 {
        let cols = self.cols();
        let span = (self.x[1] - self.x[0]).abs();
        if cols < 2 || span <= 0.0 {
            return 0;
        }
        (((x - self.x[0]) * (cols - 1) as f64 / span + 1e-9).round() as u16).min(cols - 1)
    }

    /// The x-coordinate at the left edge of cell column `col` — the inverse
    /// of [`Grid::col_of`].
    ///
    /// Written as a two-weight interpolation between the bounds rather than
    /// `x[0] + col * span / (cols - 1)` so that the last column lands on
    /// `x[1]` *bit-for-bit* (and column 0 on `x[0]`): with `t == 1.0` the
    /// first term is a clean `0.0 * x[0]` and the second an exact `1.0 *
    /// x[1]`. The stepped form reassociates as `x[0] + (cols - 1) *
    /// (span / (cols - 1))`, which lands a few ULPs either side of `x[1]`.
    /// That matters because `night_wash` in `ui::map` feeds these coordinates
    /// straight to ratatui's `Painter::get_point`, which *rejects* rather than
    /// clamps a point outside the canvas bounds — so an edge sample a single
    /// ULP over drops the whole rightmost column of the night wash for that
    /// frame, and in follow mode the bounds shift every frame, so the
    /// dropped column flickers in and out. Same reasoning drives [`Grid::y_of`].
    pub(in crate::ui) fn x_of(&self, col: u16) -> f64 {
        let cols = self.cols();
        if cols < 2 {
            return self.x[0];
        }
        let t = col as f64 / (cols - 1) as f64;
        self.x[0] * (1.0 - t) + self.x[1] * t
    }

    /// The x-coordinate to hand `ctx.print` for a label meant to land on
    /// `col`. [`x_of`](Grid::x_of) returns the column's *left edge*, and
    /// ratatui places a label with a truncating cast (`Canvas::render`), so an
    /// edge coordinate drops into the previous column whenever rounding lands
    /// it a hair short. Aiming at the column *centre* — half a cell of
    /// headroom, vast against ULP-scale error — makes the cast land on `col`
    /// every time. That matters because in follow mode the bounds shift every
    /// frame, so an edge coordinate flips back and forth across the boundary
    /// and the label jitters a cell with it. Clamped inside the bounds so the
    /// end columns don't nudge a hair past an edge, where the label filter
    /// would discard the print outright.
    pub(in crate::ui) fn x_print_of(&self, col: u16) -> f64 {
        let cols = self.cols();
        if cols < 2 {
            return self.x_of(col);
        }
        let half = (self.x[1] - self.x[0]) / (2.0 * (cols - 1) as f64);
        (self.x_of(col) + half).clamp(self.x[0].min(self.x[1]), self.x[0].max(self.x[1]))
    }

    /// The y-coordinate at the top edge of cell row `row` — the inverse of
    /// [`Grid::col_of`] along the other axis. Row 0 is the top of the canvas
    /// (`y[1]`), matching ratatui's own `Painter::get_point`, which maps `y`
    /// as `(top - y) * (rows - 1) / height`.
    ///
    /// Interpolated between the bounds for the exact-endpoints reason spelled
    /// out on [`x_of`](Grid::x_of): the last row must land on `y[0]`
    /// bit-for-bit or its wash samples fall through `Painter::get_point` and
    /// the bottom row of the night shading vanishes for the frame.
    pub(in crate::ui) fn y_of(&self, row: u16) -> f64 {
        let rows = self.rows();
        if rows < 2 {
            return self.y[1];
        }
        let t = row as f64 / (rows - 1) as f64;
        self.y[1] * (1.0 - t) + self.y[0] * t
    }

    /// [`x_print_of`](Grid::x_print_of)'s twin on the latitude axis: the
    /// y-coordinate at the *centre* of row `row`, so ratatui's truncating
    /// label cast lands on that row rather than the one above it. Same
    /// follow-mode reasoning — the y bounds move with the satellite every
    /// frame — and same clamp inside the bounds.
    pub(in crate::ui) fn y_print_of(&self, row: u16) -> f64 {
        let rows = self.rows();
        if rows < 2 {
            return self.y_of(row);
        }
        let half = (self.y[1] - self.y[0]) / (2.0 * (rows - 1) as f64);
        (self.y_of(row) - half).clamp(self.y[0].min(self.y[1]), self.y[0].max(self.y[1]))
    }

    /// The cell row a y-coordinate lands on — [`Grid::col_of`]'s twin on the
    /// latitude axis, with the same round-and-epsilon so it round-trips with
    /// [`Grid::y_of`]. Only used to reserve a label's row for collision
    /// checks, so approximate agreement with ratatui's own truncation is
    /// enough.
    pub(in crate::ui) fn row_of(&self, y: f64) -> u16 {
        let rows = self.rows();
        let span = (self.y[1] - self.y[0]).abs();
        if rows < 2 || span <= 0.0 {
            return 0;
        }
        (((self.y[1] - y) * (rows - 1) as f64 / span + 1e-9).round() as u16).min(rows - 1)
    }

    /// Whether longitude `x` falls within this grid's window. When the follow
    /// view is split across the dateline each pane's paint closure still sees
    /// every marker, so a marker is skipped unless the pane it belongs to is
    /// the one drawing it — without this the left-label branch of
    /// `print_marker` in `ui::map` would draw at an in-bounds column and leave
    /// a stray label pinned to a pane edge.
    pub(in crate::ui) fn contains(&self, x: f64) -> bool {
        let (lo, hi) = (self.x[0].min(self.x[1]), self.x[0].max(self.x[1]));
        x >= lo && x <= hi
    }

    /// The cell column a braille *dot* at x-coordinate `x` occupies. Where
    /// [`Grid::col_of`] resolves against the label grid (`cols - 1`), this
    /// resolves against the finer dot grid (`DOTS_X · cols - 1`) and then
    /// floors the dot index into its cell — so a glyph printed via
    /// [`Grid::print_dot`] lands on the same cell as the stroke drawn through
    /// that coordinate, which `col_of` alone gets wrong by up to a cell
    /// towards the canvas edges. `ui::skyplot` needs this because its markers
    /// sit *on* an arc of braille segments; `ui::map`'s markers are not tied
    /// to a braille feature that way and stay on `col_of`.
    pub(in crate::ui) fn dot_col_of(&self, x: f64) -> u16 {
        let cols = self.cols();
        let span = (self.x[1] - self.x[0]).abs();
        if cols < 2 || span <= 0.0 {
            return 0;
        }
        let dots = f64::from(cols) * f64::from(DOTS_X) - 1.0;
        let dot = ((x - self.x[0]) * dots / span).round();
        ((dot / f64::from(DOTS_X)).floor() as u16).min(cols - 1)
    }

    /// [`Grid::dot_col_of`]'s twin on the y axis: the cell row a braille dot at
    /// `y` occupies, resolved against the `DOTS_Y · rows - 1` dot grid.
    pub(in crate::ui) fn dot_row_of(&self, y: f64) -> u16 {
        let rows = self.rows();
        let span = (self.y[1] - self.y[0]).abs();
        if rows < 2 || span <= 0.0 {
            return 0;
        }
        let dots = f64::from(rows) * f64::from(DOTS_Y) - 1.0;
        let dot = ((self.y[1] - y) * dots / span).round();
        ((dot / f64::from(DOTS_Y)).floor() as u16).min(rows - 1)
    }

    /// `ctx.print`, landing the glyph on the cell a braille dot at `(x, y)`
    /// occupies rather than wherever the label mapping alone would put it —
    /// [`dot_col_of`](Grid::dot_col_of) resolves the cell, then
    /// [`x_print_of`](Grid::x_print_of) aims at its centre so ratatui's
    /// truncating cast keeps it there. A grid too small for either mapping to
    /// mean anything prints at the raw coordinate.
    pub(in crate::ui) fn print_dot(&self, ctx: &mut Context<'_>, x: f64, y: f64, span: Span<'static>) {
        if self.cols() < 2 || self.rows() < 2 {
            ctx.print(x, y, span);
            return;
        }
        ctx.print(self.x_print_of(self.dot_col_of(x)), self.y_print_of(self.dot_row_of(y)), span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::symbols::Marker;
    use ratatui::widgets::canvas::{Canvas, Points};
    use ratatui::widgets::Widget;

    /// The reason [`Grid::print_dot`] exists: ratatui resolves a braille dot
    /// and a printed glyph against different grids, so a glyph asked for at a
    /// stroke's coordinate can land a cell off it — worst towards the canvas
    /// edges. This checks the correction against a real render rather than a
    /// restatement of ratatui's own arithmetic, which would rot silently the
    /// moment upstream retunes it: a lone dot and a lone glyph asked for at
    /// the same coordinate must come back in the same buffer cell.
    #[test]
    fn print_dot_lands_a_glyph_on_the_cell_a_dot_at_the_same_coordinate_occupies() {
        let cell_of = |rect: Rect, buf: &Buffer, want_dot: bool| -> Option<(u16, u16)> {
            buf.content
                .iter()
                .position(|c| {
                    let s = c.symbol();
                    if want_dot {
                        s.chars().next().is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
                    } else {
                        s == "X"
                    }
                })
                .map(|i| (i as u16 % rect.width, i as u16 / rect.width))
        };

        // A polar disc's coordinate bounds, the same shape `skyplot::disc_bounds`
        // produces: equal coordinate span per dot on both axes so the rim is
        // round, radius-1 disc centred at the origin.
        let bounds = |rect: Rect| -> ([f64; 2], [f64; 2]) {
            let w = f64::from(rect.width) * f64::from(DOTS_X);
            let h = f64::from(rect.height) * f64::from(DOTS_Y);
            if w >= h {
                ([-w / h, w / h], [-1.0, 1.0])
            } else {
                ([-1.0, 1.0], [-h / w, h / w])
            }
        };
        let project = |az: f64, el: f64| -> (f64, f64) {
            let r = ((90.0 - el) / 90.0).clamp(0.0, 1.0);
            let (s, c) = az.to_radians().sin_cos();
            (r * s, r * c)
        };

        for (w, h) in [(40u16, 20u16), (44, 8), (60, 30), (80, 12), (31, 25)] {
            let rect = Rect::new(0, 0, w, h);
            let (xb, yb) = bounds(rect);
            let grid = Grid { inner: rect, x: xb, y: yb };

            for az in [0.0, 45.0, 90.0, 170.0, 200.0, 275.0, 330.0] {
                for el in [0.0, 5.0, 17.0, 40.0, 75.0, 90.0] {
                    let (x, y) = project(az, el);

                    let mut dot = Buffer::empty(rect);
                    Canvas::default()
                        .marker(Marker::Braille)
                        .x_bounds(xb)
                        .y_bounds(yb)
                        .paint(|ctx| ctx.draw(&Points { coords: &[(x, y)], color: Color::Reset }))
                        .render(rect, &mut dot);

                    let mut label = Buffer::empty(rect);
                    Canvas::default()
                        .marker(Marker::Braille)
                        .x_bounds(xb)
                        .y_bounds(yb)
                        .paint(|ctx| grid.print_dot(ctx, x, y, Span::raw("X")))
                        .render(rect, &mut label);

                    let d = cell_of(rect, &dot, true).expect("the dot is inside the pane");
                    let l = cell_of(rect, &label, false).expect("so is the glyph");
                    assert_eq!(d, l, "{w}x{h} az {az} el {el}: dot at {d:?} but glyph at {l:?}");
                }
            }
        }
    }

    #[test]
    fn dot_col_and_row_stay_in_range_at_the_bounds_and_on_a_degenerate_grid() {
        let g = Grid { inner: Rect::new(0, 0, 30, 12), x: [-2.0, 2.0], y: [-1.0, 1.0] };
        assert_eq!(g.dot_col_of(-2.0), 0);
        assert_eq!(g.dot_col_of(2.0), 29);
        assert_eq!(g.dot_row_of(1.0), 0, "y[1] is the top row");
        assert_eq!(g.dot_row_of(-1.0), 11);
        assert_eq!(g.dot_col_of(100.0), 29, "past the bound clamps to the last cell");

        let degenerate = Grid { inner: Rect::new(0, 0, 1, 1), x: [-2.0, 2.0], y: [-1.0, 1.0] };
        assert_eq!(degenerate.dot_col_of(0.0), 0);
        assert_eq!(degenerate.dot_row_of(0.0), 0);
    }
}
