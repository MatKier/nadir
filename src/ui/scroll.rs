//! The scrollbar every scrollable list wears on its right border, so a list
//! with more rows than it shows says so rather than looking like one that
//! simply ends there. One helper, so TRACKED, NEXT PASSES, LAUNCHES, both
//! pickers and the `?` overlay all draw the identical bar.
//!
//! Like `hit`, this takes plain numbers rather than an `App`, so it is testable
//! against a `TestBackend` without any of the dashboard around it.

use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;

use crate::ui::hit::RowSpan;
use crate::ui::Theme;

/// Draw a vertical scrollbar over the right-hand column of `area` — the panel's
/// or popup's *border* column, which is why the caller passes the outer rect
/// rather than the inner one. The bar spans `area` less its top and bottom rows
/// (the corners, for a whole panel), so its arrows sit on the border proper. A
/// popup passes a narrower rect: just the band of rows that hold its list.
///
/// `first` is the index of the first row on screen, `shown` how many rows are
/// on screen and `total` how many the list has. Nothing is drawn when
/// `shown >= total`: a bar that is always full would say only that the list
/// ends where it ends. Note that this reads "something is off-screen *or
/// clipped*", not merely "overflows" — `hit::list_rows` drops a last row that
/// would not fit whole, so five rows in room for four and a half arrive here as
/// `shown = 4, total = 5` and rightly get a bar.
///
/// The thumb takes the colour of the border cell it sits beside, read back off
/// the frame rather than passed in: the block was drawn just before, so whatever
/// it painted its border — the focus colour, the dim unfocused one, or NEXT
/// PASSES' pulsing AOS tint, which nothing outside `aos_block` could name —
/// the bar follows, with no second copy of that decision to drift. The track
/// stays the dim frame colour throughout.
pub(crate) fn vertical(frame: &mut Frame, area: Rect, first: usize, shown: usize, total: usize) {
    if shown == 0 || shown >= total || area.height < 3 {
        return;
    }
    let border = frame
        .buffer_mut()
        .cell((area.right().saturating_sub(1), area.y))
        .map_or(Theme::FRAME, |c| c.fg);
    // ratatui puts the thumb at the bottom only when `position` reaches
    // `content_length - 1`, and sizes it by `content_length - 1 + viewport`. A
    // list scrolled fully down has `first == total - shown`, so a content
    // length of `total - shown + 1` makes both of those come out to exactly
    // `total` — the thumb is `shown/total` of the track and lands flush at the
    // bottom. Passing `total` itself leaves a fully scrolled list visibly short
    // of the end.
    let mut state = ScrollbarState::new(total - shown + 1)
        .position(first.min(total - shown))
        .viewport_content_length(shown);
    let inner = area.inner(Margin { vertical: 1, horizontal: 0 });
    let mut bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .track_style(Style::new().fg(Theme::FRAME))
        .thumb_style(Style::new().fg(border))
        .begin_style(Style::new().fg(border))
        .end_style(Style::new().fg(border));
    // The two arrows come out of the track, and ratatui draws nothing at all
    // once they leave it empty — which is a one- or two-row panel, and TRACKED
    // is allowed to shrink to one entry. There the bar is thumb only, so the
    // smallest panel still says it has more.
    if inner.height < 3 {
        bar = bar.begin_symbol(None).end_symbol(None);
    }
    frame.render_stateful_widget(bar, inner, &mut state);
}

/// [`vertical`] for a popup whose list is a band of rows inside it rather than
/// the whole popup: `first_row_y` is the screen row of the first row shown, and
/// `shown` how many there are. The band handed to `vertical` runs one row above
/// the first to one below the last, since `vertical` keeps the outer two rows of
/// its rect for the border — so the bar's arrows land on the first and last
/// result rows, and the popup's input line and blank rows are not left beside
/// track that scrolls nothing.
pub(crate) fn band(
    frame: &mut Frame,
    popup: Rect,
    first_row_y: u16,
    first: usize,
    shown: usize,
    total: usize,
) {
    let band = Rect::new(popup.x, first_row_y.saturating_sub(1), popup.width, shown as u16 + 2);
    vertical(frame, band, first, shown, total);
}

/// [`vertical`] for a `List` panel, reading the window straight off the row
/// spans `hit::render_list` returned. Those are the post-render answer — the
/// `ListState` offset is only trustworthy *after* ratatui has scrolled to keep
/// the selection in view — so no second copy of that offset is kept.
pub(crate) fn for_rows(frame: &mut Frame, area: Rect, rows: &[RowSpan], total: usize) {
    let Some(&(_, _, first)) = rows.first() else { return };
    vertical(frame, area, first, rows.len(), total);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::widgets::Block;
    use ratatui::Terminal;

    /// The bar's column, top to bottom, drawn over a 12-row-tall, 10-wide area.
    fn column(first: usize, shown: usize, total: usize) -> Vec<String> {
        let area = Rect::new(0, 0, 10, 12);
        let mut terminal = Terminal::new(TestBackend::new(10, 12)).unwrap();
        terminal.draw(|f| vertical(f, area, first, shown, total)).unwrap();
        let buf = terminal.backend().buffer();
        (0..12).map(|y| buf[(9, y)].symbol().to_string()).collect()
    }

    #[test]
    fn a_list_that_fits_entirely_draws_no_scrollbar() {
        assert!(column(0, 8, 8).iter().all(|s| s == " "));
        assert!(column(0, 8, 3).iter().all(|s| s == " "));
    }

    #[test]
    fn a_list_with_a_clipped_last_row_still_draws_a_scrollbar() {
        assert!(column(0, 4, 5).iter().any(|s| s == "█"));
    }

    #[test]
    fn the_bar_leaves_the_border_corners_alone() {
        let col = column(0, 8, 20);
        assert_eq!(col[0], " ");
        assert_eq!(col[11], " ");
        assert_eq!(col[1], "▲");
        assert_eq!(col[10], "▼");
    }

    #[test]
    fn the_thumb_takes_the_colour_of_the_border_it_sits_beside() {
        // A border that is neither of the two the panels normally use, as NEXT
        // PASSES' AOS pulse is: the bar must follow it, not assume a focus colour.
        let tint = Color::Rgb(9, 200, 77);
        let area = Rect::new(0, 0, 10, 12);
        let mut terminal = Terminal::new(TestBackend::new(10, 12)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(Block::bordered().border_style(Style::new().fg(tint)), area);
                vertical(f, area, 0, 8, 20);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(9, 2)].symbol(), "█");
        assert_eq!(buf[(9, 2)].fg, tint);
        assert_eq!(buf[(9, 1)].fg, tint, "and so does the arrow");
    }

    #[test]
    fn a_one_or_two_row_panel_still_shows_a_thumb_because_the_arrows_are_dropped() {
        // Inner height 1 and 2 leave the two arrows no track between them, and
        // ratatui then draws nothing — so the arrows must go instead.
        for height in [3u16, 4] {
            let area = Rect::new(0, 0, 10, height);
            let mut terminal = Terminal::new(TestBackend::new(10, height)).unwrap();
            terminal.draw(|f| vertical(f, area, 0, 1, 6)).unwrap();
            let buf = terminal.backend().buffer();
            let col: Vec<&str> = (0..height).map(|y| buf[(9, y)].symbol()).collect();
            assert!(col.contains(&"█"), "height {height}: {col:?}");
            assert!(!col.contains(&"▲") && !col.contains(&"▼"), "height {height}: {col:?}");
        }
    }

    #[test]
    fn the_thumb_sits_at_the_top_when_the_first_row_is_visible() {
        let col = column(0, 8, 20);
        assert_eq!(col[2], "█", "the thumb starts straight after the ▲: {col:?}");
    }

    #[test]
    fn the_thumb_sits_at_the_bottom_when_the_last_row_is_visible() {
        // 20 rows, 8 on screen, scrolled all the way down: the thumb must end
        // flush against the ▼, not stop short of it.
        let col = column(12, 8, 20);
        assert_eq!(col[9], "█", "the thumb ends straight before the ▼: {col:?}");
        assert_eq!(col[2], "║", "and the track is what is left above it: {col:?}");
    }

    #[test]
    fn scrolling_further_down_never_moves_the_thumb_up() {
        let starts: Vec<usize> = (0..=12)
            .map(|first| column(first, 8, 20).iter().position(|s| s == "█").unwrap())
            .collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
    }
}
