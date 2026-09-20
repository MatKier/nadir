//! Where things were drawn last frame, so a mouse click can be mapped back to
//! them. The panel rects and each list's row spans are only known inside
//! `ui::draw` (they fall out of its layout ladder), and recomputing them in
//! `App::handle_mouse` would mean a second copy of that ladder that drifts the
//! first time a panel is resized — so `draw` records them here as it goes and
//! `handle_mouse` reads the previous frame's answer, the same way a click on a
//! real window lands on whatever was last painted there.
//!
//! Everything in this module is a plain function of its arguments, so it is
//! testable without a `Frame` or a terminal.

use ratatui::layout::{Position, Rect};
use ratatui::widgets::{List, ListItem, ListState};
use ratatui::Frame;

use crate::app::Panel;

/// The ` nadir ` chip at the left of the title bar — one string that both the
/// title bar's render and the badge's hit rect read, so the clickable area can
/// never drift from the painted one. The title bar's layout budget
/// (`PREFIX` in `ui::title_bar`) is this plus a one-column gap: measuring the
/// hit rect from *that* would put a dead cell of blank space inside it.
pub(crate) const BADGE: &str = " nadir ";

/// The badge's width in cells. `BADGE` is ASCII, so bytes are columns.
pub(crate) const BADGE_W: u16 = BADGE.len() as u16;

/// Whether a click at `(col, row)` lands on the title bar's badge. The title
/// bar is always the first row of the frame in both layouts (each splits
/// `frame.area()` with a leading `Length(1)`), and the badge is always its
/// leftmost cells, so no rect needs stashing for this one.
pub(crate) fn badge_hit(col: u16, row: u16) -> bool {
    row == 0 && col < BADGE_W
}

/// One visible list row's screen rows as `y0..y1` (half-open) plus its index in
/// the list — what each list panel's `draw` hands back for [`HitMap`].
pub(crate) type RowSpan = (u16, u16, usize);

/// What the last drawn frame put where. Emptied at the very top of every
/// `ui::draw`, so a frame that draws no dashboard — the boot splash, or the
/// "terminal too small" message — leaves nothing for a click to land on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HitMap {
    /// Each drawn panel's outer rect, for click-to-focus.
    pub panels: Vec<(Rect, Panel)>,
    /// One entry per visible list row: the panel it belongs to, the screen
    /// rows it covers as `y0..y1` (half-open), and its index in that panel's
    /// list. A row can span more than one screen row — the highlighted Launches
    /// entry expands to two lines.
    pub rows: Vec<(Panel, u16, u16, usize)>,
}

impl HitMap {
    /// Record `panel`'s visible list rows, as returned by its `draw`.
    pub(crate) fn add_rows(&mut self, panel: Panel, rows: Vec<RowSpan>) {
        self.rows.extend(rows.into_iter().map(|(y0, y1, i)| (panel, y0, y1, i)));
    }

    /// Whether the dashboard was drawn at all — the boot splash and the
    /// too-small message record no panels. The badge is not stored separately
    /// (see [`badge_hit`]), so this is what says it is on screen.
    pub(crate) fn is_drawn(&self) -> bool {
        !self.panels.is_empty()
    }

    /// The panel under `(col, row)`, if any. Panels never overlap, so the
    /// first containing rect is the only one.
    pub(crate) fn panel_at(&self, col: u16, row: u16) -> Option<Panel> {
        let at = Position::new(col, row);
        self.panels.iter().find(|(r, _)| r.contains(at)).map(|(_, p)| *p)
    }

    /// The list row under `(col, row)`, as its panel and index. Only rows the
    /// panel actually drew are recorded, so a click on the blank space below a
    /// short list finds nothing here and falls through to a plain panel focus.
    pub(crate) fn row_at(&self, col: u16, row: u16) -> Option<(Panel, usize)> {
        // A row span is only a y-range; resolving the panel first supplies the
        // x-range, so a click beside the panel on the same screen row is not
        // mistaken for one of its rows.
        let panel = self.panel_at(col, row)?;
        self.rows
            .iter()
            .find(|(p, y0, y1, _)| *p == panel && (*y0..*y1).contains(&row))
            .map(|(p, _, _, i)| (*p, *i))
    }
}

/// The screen rows each *visible* list item occupies, as `(y0, y1, index)`,
/// given every item's height, the list's scroll `offset` and the `inner` rect
/// it was rendered into.
///
/// Mirrors what ratatui's `List` actually paints: items stack from the top of
/// the inner area starting at `offset`, and only items that fit whole are drawn
/// (`get_items_bounds` stops at the first one that would overflow) — so a
/// half-visible last row is not clickable, because it is not on screen.
///
/// `offset` must be read from the `ListState` *after* it has been rendered:
/// ratatui adjusts it during render to keep the selection in view, so a value
/// read beforehand is stale exactly when the list has scrolled.
pub(crate) fn list_rows(heights: &[usize], offset: usize, inner: Rect) -> Vec<RowSpan> {
    let mut out = Vec::new();
    let mut y = inner.y;
    for (i, h) in heights.iter().enumerate().skip(offset) {
        let h = u16::try_from(*h).unwrap_or(u16::MAX);
        if y.saturating_add(h) > inner.bottom() {
            break;
        }
        out.push((y, y + h, i));
        y += h;
    }
    out
}

/// Render `items` as a stateful list into `area` and return the rows it
/// painted inside `inner`, for [`HitMap::add_rows`]. `style` applies the
/// panel's own block, highlight style and symbol to the bare `List`.
///
/// The one place the render-then-read-offset order lives: item heights are
/// taken before `List::new` consumes the items, and the offset is read from the
/// kept `ListState` only after rendering, when ratatui has scrolled it to keep
/// the selection in view (see [`list_rows`]).
pub(crate) fn render_list<'a>(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    items: Vec<ListItem<'a>>,
    selected: Option<usize>,
    style: impl FnOnce(List<'a>) -> List<'a>,
) -> Vec<RowSpan> {
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(style(List::new(items)), area, &mut state);
    list_rows(&heights, state.offset(), inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badge_hit_rect_stops_at_the_seven_cell_chip_not_the_eight_char_prefix() {
        assert_eq!(BADGE_W, 7);
        assert!(badge_hit(0, 0));
        assert!(badge_hit(6, 0), "the chip's last cell is still the badge");
        assert!(!badge_hit(7, 0), "the gap after the chip is not part of it");
        assert!(!badge_hit(0, 1), "only the title row carries the badge");
    }

    #[test]
    fn list_rows_stacks_rows_from_the_top_of_the_inner_area() {
        let rows = list_rows(&[1, 1, 1], 0, Rect::new(2, 5, 20, 10));
        assert_eq!(rows, vec![(5, 6, 0), (6, 7, 1), (7, 8, 2)]);
    }

    #[test]
    fn list_rows_gives_a_two_line_row_two_screen_rows_and_shifts_those_below_it() {
        // The highlighted Launches entry is the case this exists for.
        let rows = list_rows(&[1, 2, 1], 0, Rect::new(0, 0, 20, 10));
        assert_eq!(rows, vec![(0, 1, 0), (1, 3, 1), (3, 4, 2)]);
    }

    #[test]
    fn list_rows_starts_at_the_scroll_offset_but_keeps_the_real_index() {
        let rows = list_rows(&[1; 10], 6, Rect::new(0, 3, 20, 2));
        assert_eq!(rows, vec![(3, 4, 6), (4, 5, 7)]);
    }

    #[test]
    fn list_rows_drops_a_last_row_that_does_not_fit_whole() {
        // Three rows of height 2 in 5 rows of room: the third would overflow,
        // and ratatui does not draw it, so it must not be clickable either.
        let rows = list_rows(&[2, 2, 2], 0, Rect::new(0, 0, 20, 5));
        assert_eq!(rows, vec![(0, 2, 0), (2, 4, 1)]);
    }

    #[test]
    fn list_rows_of_an_empty_list_is_empty() {
        assert!(list_rows(&[], 0, Rect::new(0, 0, 20, 5)).is_empty());
    }

    /// Guards the one thing `list_rows` reimplements: which rows ratatui paints
    /// where. Renders a real scrolled `List` and checks each span's first line
    /// really holds that item's text, so a change in ratatui's layout rules
    /// fails here rather than silently mis-aiming clicks.
    #[test]
    fn list_rows_agrees_with_where_ratatui_actually_paints_a_scrolled_list() {
        use ratatui::backend::TestBackend;
        use ratatui::text::Line;
        use ratatui::Terminal;

        // Item 1 and 4 are two lines tall; five rows of room and the last item
        // selected forces a scroll.
        let items = || -> Vec<ListItem<'static>> {
            (0..6)
                .map(|i| {
                    let mut lines = vec![Line::from(format!("item{i}"))];
                    if i % 3 == 1 {
                        lines.push(Line::from("detail"));
                    }
                    ListItem::new(lines)
                })
                .collect()
        };
        let area = Rect::new(0, 0, 20, 5);
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut spans = Vec::new();
        terminal
            .draw(|f| spans = render_list(f, area, area, items(), Some(5), |l| l))
            .unwrap();
        let buf = terminal.backend().buffer();
        assert!(spans.len() >= 2 && spans[0].2 > 0, "the list scrolled: {spans:?}");
        for (y0, _, index) in spans {
            let line: String = (0..20).map(|x| buf[(x, y0)].symbol().to_string()).collect();
            assert!(line.starts_with(&format!("item{index}")), "row {y0}: {line:?}");
        }
    }

    fn map() -> HitMap {
        HitMap {
            panels: vec![
                (Rect::new(0, 1, 40, 20), Panel::Map),
                (Rect::new(40, 1, 40, 10), Panel::Tracked),
            ],
            rows: vec![(Panel::Tracked, 2, 3, 0), (Panel::Tracked, 3, 4, 1)],
        }
    }

    #[test]
    fn panel_at_finds_the_panel_whose_rect_contains_the_click() {
        let m = map();
        assert_eq!(m.panel_at(5, 5), Some(Panel::Map));
        assert_eq!(m.panel_at(45, 5), Some(Panel::Tracked));
        assert_eq!(m.panel_at(45, 15), None, "below TRACKED's rect is nobody's");
    }

    #[test]
    fn row_at_finds_the_row_under_the_click_and_its_panel() {
        let m = map();
        assert_eq!(m.row_at(45, 2), Some((Panel::Tracked, 0)));
        assert_eq!(m.row_at(45, 3), Some((Panel::Tracked, 1)));
    }

    #[test]
    fn row_at_ignores_the_blank_space_below_the_last_row() {
        assert_eq!(map().row_at(45, 8), None);
    }

    #[test]
    fn row_at_ignores_a_click_beside_the_panel_on_a_row_the_panel_owns() {
        // Same screen row as TRACKED's first entry, but in the map's columns.
        assert_eq!(map().row_at(5, 2), None);
    }
}
