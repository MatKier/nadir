//! The `?` reference overlay: a scrollable glossary of every field, symbol
//! and status chip the dashboard shows, plus an explanation of offline mode.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::ui::{centered, Theme};

/// Draw the overlay and clamp `app.help_scroll` to what the content actually
/// needs, so `End` (which jumps to `u16::MAX`) and repeated `j`/`PageDown`
/// settle on the true bottom by the next frame.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let lines = content(app);
    let total = lines.len() as u16;

    let width = area.width.saturating_sub(4).clamp(40, 100);
    let height = area.height.saturating_sub(2).max(10);
    let popup = centered(area, width, height);

    // Borders take one row top and bottom regardless of title text, so the
    // visible height is known before the title (which shows the position)
    // is built.
    let visible = popup.height.saturating_sub(2);
    let max_scroll = total.saturating_sub(visible);
    app.help_scroll = app.help_scroll.min(max_scroll);

    let pos = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}/{}", (app.help_scroll + visible).min(total), total)
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(Theme::FRAME_FOCUS))
        .title(Span::styled(
            " nadir — reference ",
            Style::new().fg(Theme::FRAME_FOCUS).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(
            format!(" j/k scroll · ? close  {pos} "),
            Style::new().fg(Theme::LABEL),
        )));

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.help_scroll, 0))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn content(app: &App) -> Vec<Line<'static>> {
    let mut l = Vec::new();

    heading(&mut l, "Keys");
    row(&mut l, "1 – 6", "focus map / tracked / telemetry / passes / weather / launches");
    text(&mut l, "     each panel's header shows its own number, so the key to focus it");
    text(&mut l, "     is always visible");
    row(&mut l, "Tab", "cycle focus");
    row(&mut l, "j / k", "scroll the focused list (tracked, passes, launches) or this help");
    row(&mut l, "PgUp / PgDn", "scroll this help by a page");
    row(&mut l, "Home / End", "jump to the top / bottom of this help");
    row(&mut l, "m", "toggle fullscreen map");
    row(&mut l, "f", "follow the satellite on the map");
    row(&mut l, "s", "search Celestrak's catalogue for a satellite, by name or NORAD id");
    row(&mut l, "Enter", "on Tracked: start tracking the highlighted satellite");
    row(&mut l, "d", "on Tracked: drop the highlighted satellite from the list");
    row(&mut l, "r", "refresh the focused panel's feed(s) now");
    row(&mut l, "?", "toggle this help");
    row(&mut l, "q / Esc", "quit (Esc closes this help first)");
    blank(&mut l);

    heading(&mut l, "Title bar");
    text(&mut l, "Object name and NORAD catalogue number on the left, with the COSPAR");
    text(&mut l, "international designator (e.g. \"1998-067A\") between them when the");
    text(&mut l, "bar is wide enough to fit it; your ground station's lat/lon, session");
    text(&mut l, "uptime (\"up\") and the UTC clock on the right.");
    blank(&mut l);

    heading(&mut l, "Map");
    row(&mut l, "◆", "the sub-satellite point — directly beneath the satellite");
    row(&mut l, "▲", "your ground station");
    row(&mut l, "bright line", "the next 65 minutes of ground track");
    row(&mut l, "dim line", "the past 35 minutes of ground track");
    row(&mut l, "circle", "the visibility footprint — where the satellite is above 0°");
    row(&mut l, "amber dots", "the day/night terminator");
    row(&mut l, "dark ground", "the night side of the Earth");
    row(&mut l, "lighter band", "civil twilight — the Sun 0°–6° below the horizon");
    row(&mut l, "◉", "pad of the highlighted launch, with its provider, site and coordinates — see below");
    text(&mut l, "The map shows the whole world; `f` zooms it to a 180°×90° window");
    text(&mut l, "that follows the satellite, and `m` expands it to fill the screen.");
    blank(&mut l);

    heading(&mut l, "Tracked");
    text(&mut l, "Every satellite ever tracked, most recently (re)tracked first —");
    text(&mut l, "picked with `Enter`, dropped with `d`.");
    row(&mut l, "●", "the satellite currently being tracked");
    text(&mut l, "The active satellite can't be dropped from the list — switch to");
    text(&mut l, "something else first.");
    blank(&mut l);

    heading(&mut l, "Telemetry");
    row(&mut l, "ALT", "altitude above the WGS-84 ellipsoid, in km");
    row(&mut l, "SPD", "inertial speed from SGP4 — not ground-relative — km/h and km/s");
    row(&mut l, "POS", "geodetic latitude / longitude of the sub-satellite point");
    row(&mut l, "FOOT", "radius of the ground circle that can see the satellite above 0°");
    row(&mut l, "ORB", "orbital regime, period and inclination from the element set");
    text(&mut l, "LEO/MEO/HEO/HIGH by altitude and eccentricity — HEO is eccentric, HIGH");
    text(&mut l, "is near-circular above the GEO belt; GEO is a near-circular, near-");
    text(&mut l, "equatorial sidereal orbit, GSO the same period but inclined or");
    text(&mut l, "eccentric. A `-P` or `-S` suffix marks a polar or sun-synchronous");
    text(&mut l, "plane, e.g. LEO-P, independent of the regime.");
    row(&mut l, "APSIS", "perigee × apogee altitude — the orbit's actual shape");
    text(&mut l, "Measured from the WGS-84 equatorial radius, not the local ellipsoid ALT");
    text(&mut l, "uses, so the two can read up to ~20 km apart away from the equator.");
    row(&mut l, "REV", "approximate revolution number since launch");
    row(&mut l, "SUN", "sunlit or eclipsed, and time to the next sunrise/sunset");
    row(&mut l, "RANGE", "slant range and elevation from your ground station");
    row(&mut l, "TLE", "age of the element set since its epoch — amber past 36h, red past 72h");
    blank(&mut l);

    heading(&mut l, "Next passes");
    text(&mut l, "Passes peaking above 10° elevation within the next 48h, recomputed");
    text(&mut l, "every 20s. Each row: day, AOS–LOS in local time, duration in");
    text(&mut l, "minutes, peak elevation, and the AOS→LOS compass azimuths.");
    row(&mut l, "★", "visible to the naked eye — satellite sunlit while you're in darkness");
    text(&mut l, "The panel title names your ground station and its UTC offset — the");
    text(&mut l, "same offset the AOS–LOS times above are shown in.");
    blank(&mut l);

    heading(&mut l, "Space weather");
    text(&mut l, "From NOAA's Space Weather Prediction Center.");
    row(&mut l, "Kp", "planetary K-index 0–9; sparkline is the last ~3 days of samples");
    text(&mut l, "     green below 4, amber from 4, red from 5 (storm level)");
    row(&mut l, "WIND", "solar wind bulk proton speed, km/s");
    row(&mut l, "Bz", "north–south interplanetary field, nT — strongly southward drives");
    text(&mut l, "     aurora; amber at ≤ −5, red at ≤ −10");
    row(&mut l, "STORM", "NOAA scales 0–5: R radio blackouts, S radiation storms, G geomagnetic");
    row(&mut l, "AUR", "OVATION aurora nowcast probability overhead at your ground station");
    text(&mut l, "An age marker under the panel title shows how stale the Kp/wind/storm");
    text(&mut l, "feed is (the aurora nowcast fetches separately, on its own schedule).");
    blank(&mut l);

    heading(&mut l, "Launches");
    text(&mut l, "A T- countdown (or \"in flight\" / \"TBD\"), name and provider. A");
    text(&mut l, "\"mirror\" or age marker under the panel title means the list isn't");
    text(&mut l, "a live pull from the primary launch-tracking host.");
    row(&mut l, "bright name", "the provider has committed to the date (Go)");
    row(&mut l, "dim name", "date not confirmed yet (TBD / TBC)");
    row(&mut l, "amber name", "the count is holding");
    row(&mut l, "red name", "the launch failed");
    text(&mut l, "Focus this panel (`6`) and scroll (`j`/`k`) to highlight a launch —");
    text(&mut l, "it expands to a second line with provider and pad, and its pad is");
    text(&mut l, "marked on the map with `◉`, the provider and vehicle name, and the");
    text(&mut l, "pad's site and coordinates. The marker disappears when focus moves");
    text(&mut l, "away, is absent for a launch whose pad coordinates the feed doesn't");
    text(&mut l, "give, and can fall outside the visible area while `f` is following");
    text(&mut l, "the satellite.");
    blank(&mut l);

    heading(&mut l, "Feed status chips");
    text(&mut l, "TLE element set · SWX space weather · AUR aurora nowcast ·");
    text(&mut l, "LCH launches.");
    row(&mut l, "wait", "not fetched yet this session");
    row(&mut l, "live", "fetched successfully, current");
    row(&mut l, "5s / 12m / 3h", "time since the last successful fetch");
    row(&mut l, "err", "the fetch failed and there's nothing to fall back on");
    text(&mut l, "Colour is judged against how often that feed refetches: green while");
    text(&mut l, "at most two refreshes could have been missed, amber up to six, then");
    text(&mut l, "red. Waiting and failed-with-nothing-cached also read amber and red.");
    row(&mut l, "SWX", "refetches every 5m — green to 10m, amber to 30m");
    row(&mut l, "AUR", "every 15m — green to 30m, amber to 90m");
    row(&mut l, "LCH", "every 30m — green to 1h, amber to 3h");
    row(&mut l, "TLE", "every 12h — green to 24h, amber to 72h");
    text(&mut l, "`r` refetches only the focused panel's feeds, without spending a");
    text(&mut l, "request on anything else.");
    blank(&mut l);

    heading(&mut l, "Offline mode — how the position is known with no network");
    text(&mut l, "The position is computed, not downloaded. Celestrak supplies a");
    text(&mut l, "GP/TLE element set — a compact orbit description valid for days,");
    text(&mut l, "not a position — and nadir runs SGP4/SDP4 on it locally at ~4 Hz to");
    text(&mut l, "get position and velocity at any instant. Everything derived from");
    text(&mut l, "that is pure math with no I/O: ground track, footprint, sunlit/");
    text(&mut l, "eclipsed, look angles, the terminator and every pass prediction —");
    text(&mut l, "so the map and passes panel work fully offline.");
    text(&mut l, "Only the element set needs the network, and at most every 12h; it's");
    text(&mut l, "cached at ~/.cache/nadir/tle-<norad>.json, keyed by the cache's own");
    text(&mut l, "age rather than session length — a restart with a cache under 12h old");
    text(&mut l, "reads it straight from disk instead of refetching. Accuracy decays");
    text(&mut l, "away from that epoch — roughly a km near it, tens of km after a week");
    text(&mut l, "in low orbit — which is exactly what the amber/red TLE age means.");
    text(&mut l, "What can't be computed, and so goes stale or blank offline: space");
    text(&mut l, "weather, aurora, and the launch manifest.");
    text(&mut l, "--offline makes zero network requests and loads the last cached");
    text(&mut l, "TLE, weather and launches, each labelled with its age. A");
    text(&mut l, "normal run warm-starts from that same cache while it fetches.");
    blank(&mut l);

    heading(&mut l, "Recent activity");
    let log: Vec<String> = app
        .data
        .read()
        .map(|d| d.log.iter().rev().take(8).rev().cloned().collect())
        .unwrap_or_default();
    if log.is_empty() {
        text(&mut l, "(nothing logged yet)");
    } else {
        for entry in log {
            text(&mut l, &entry);
        }
    }

    l
}

fn heading(l: &mut Vec<Line<'static>>, title: &str) {
    l.push(Line::from(Span::styled(
        format!("  {title}"),
        Style::new().fg(Theme::ACCENT).add_modifier(Modifier::BOLD),
    )));
}

fn row(l: &mut Vec<Line<'static>>, key: &str, desc: &str) {
    l.push(Line::from(vec![
        Span::styled(format!("  {key:<14}"), Style::new().fg(Theme::SAT)),
        Span::styled(desc.to_string(), Style::new().fg(Theme::VALUE)),
    ]));
}

fn text(l: &mut Vec<Line<'static>>, s: &str) {
    l.push(Line::from(Span::styled(
        format!("  {s}"),
        Style::new().fg(Theme::LABEL),
    )));
}

fn blank(l: &mut Vec<Line<'static>>) {
    l.push(Line::from(""));
}
