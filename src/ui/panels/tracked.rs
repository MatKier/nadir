//! The TRACKED panel: every satellite ever tracked this session and earlier
//! ones, most recently (re)tracked first. Highlighting an entry and pressing
//! `Enter` switches to it; `d` drops it from the list (refused for whichever
//! satellite is currently being tracked, marked with `●`).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::ui::panels::fmt::{dim, row_highlight, truncate};
use crate::ui::{is_focused, panel_block, Theme};

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let focused = is_focused(app, Panel::Tracked);
    let block = panel_block(Panel::Tracked, "TRACKED", focused);

    if app.config.tracked.is_empty() {
        frame.render_widget(
            Paragraph::new(dim("  press s to search for a satellite")).block(block),
            area,
        );
        return;
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

    let selected = focused.then(|| app.list_pos.min(app.config.tracked.len().saturating_sub(1)));
    let list = List::new(items).block(block).highlight_style(row_highlight()).highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut ListState::default().with_selected(selected));
}
