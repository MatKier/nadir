//! The TRACKED panel: every satellite ever tracked this session and earlier
//! ones, sorted by name. Highlighting an entry and pressing
//! `Enter` switches to it; `d` drops it from the list (refused for whichever
//! satellite is currently being tracked, marked with `●`).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::ui::panels::fmt::{dim, row_highlight, truncate};
use crate::ui::hit::{render_list, RowSpan};
use crate::ui::{is_focused, panel_block, Theme};

/// Draws the panel and returns the screen rows its visible entries landed on,
/// as `(y0, y1, index)`, for `ui::hit` — empty when there is nothing to click.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) -> Vec<RowSpan> {
    let focused = is_focused(app, Panel::Tracked);
    let block = panel_block(Panel::Tracked, "TRACKED", focused);

    if app.config.tracked.is_empty() {
        frame.render_widget(
            Paragraph::new(dim("  press s to search for a satellite")).block(block),
            area,
        );
        return Vec::new();
    }

    // Budget the name against the panel's actual width: two border columns,
    // the "●"/"  " active marker, the space `highlight_symbol` reserves, and
    // a fixed "NORAD nnnnn" column on the right.
    const ID_W: usize = 11;
    const OVERHEAD: usize = 2 + 2 + 2;
    let name_w = (area.width as usize).saturating_sub(OVERHEAD + ID_W).max(4);

    let items: Vec<ListItem> = app
        .config
        .tracked
        .iter()
        .map(|t| {
            let active = t.norad_id == app.config.sat;
            let color = if active { Theme::SAT } else { Theme::VALUE };
            ListItem::new(Line::from(vec![
                Span::styled(if active { "● " } else { "  " }, Style::new().fg(Theme::SAT)),
                Span::styled(format!("{:<name_w$}", truncate(&t.name, name_w)), Style::new().fg(color)),
                Span::styled(format!("NORAD {}", t.norad_id), Style::new().fg(Theme::LABEL)),
            ]))
        })
        .collect();

    // Unfocused, the panel still hands the list a selection — the active
    // satellite's — purely so ratatui scrolls that row into view (`hit.rs`:
    // it adjusts the offset during render to keep the selection visible). The
    // list is name-sorted, so with more entries than rows the `●` satellite
    // is not always in the first five. A blank symbol of the same width and an
    // empty style (ratatui patches highlight styles on, so this is a no-op)
    // keep that scroll invisible: no bar, no `▶`, no shifted columns.
    let last = app.config.tracked.len().saturating_sub(1);
    let active = app.config.tracked.iter().position(|t| t.norad_id == app.config.sat);
    let selected = if focused { Some(app.list_pos.min(last)) } else { active };
    // The block goes to the `List`, so its inner rect is taken before it moves.
    let inner = block.inner(area);
    render_list(frame, area, inner, items, selected, |list| {
        let list = list.block(block);
        if focused {
            list.highlight_style(row_highlight()).highlight_symbol("▶ ")
        } else {
            list.highlight_style(Style::new()).highlight_symbol("  ")
        }
    })
}
