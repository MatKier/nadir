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
    row(&mut l, "+ / -", "zoom the follow window in / out (×2 to ×16); - past the");
    text(&mut l, "     widest level drops back to the whole world, + turns follow on");
    row(&mut l, "p", "toggle labelled cities and ground stations on the map");
    row(&mut l, "s", "search Celestrak's catalogue for a satellite, by name or NORAD id");
    row(&mut l, "Enter", "on Tracked: start tracking the highlighted satellite");
    row(&mut l, "d", "on Tracked: drop the highlighted satellite from the list");
    row(&mut l, "r", "refresh the focused panel's feed(s) now");
    row(&mut l, "?", "toggle this help");
    row(&mut l, "q / Esc", "quit (Esc closes this help first)");
    blank(&mut l);

    heading(&mut l, "Time");
    text(&mut l, "The displayed clock can be detached from wall time — the whole map,");
    text(&mut l, "telemetry, passes and the TLE-age warning then show that instant");
    text(&mut l, "instead of now. Feed ages, the status chips and \"up\" never scrub, and");
    text(&mut l, "neither does the launch countdown.");
    row(&mut l, "Space", "pause / resume the clock");
    row(&mut l, ", / .", "step the speed down / up — 1× 2× 5× 10× 60× 300× 900× 1800×,");
    text(&mut l, "     and past 1× straight into reverse (< / > do the same)");
    row(&mut l, "← / →", "step the clock back / forward one minute (h / l too)");
    row(&mut l, "[ / ]", "step the clock back / forward one hour");
    row(&mut l, "n", "jump to 30 s before the next pass rises, paused there");
    row(&mut l, "N", "same, but only naked-eye (★) passes");
    row(&mut l, "g", "go to a time — an instant (2026-09-08 04:30), a clock time");
    text(&mut l, "     (04:30, next occurrence) or an offset (+90m, -2h, +3d)");
    row(&mut l, "0", "snap back to now, running at 1×");
    blank(&mut l);

    heading(&mut l, "Title bar");
    text(&mut l, "Object name and NORAD catalogue number on the left, with the COSPAR");
    text(&mut l, "international designator (e.g. \"1998-067A\") between them when the");
    text(&mut l, "bar is wide enough to fit it; your ground station's lat/lon, session");
    text(&mut l, "uptime (\"up\") and the clock on the right.");
    text(&mut l, "The clock shows the simulated instant, green when it is live and amber");
    text(&mut l, "with a marker when scrubbed: ▸ drifted, ‖ paused, ▸▸60x / ◂◂5x warp.");
    text(&mut l, "While scrubbed it also shows the offset from now (Δ+2h14m, Δ-45m,");
    text(&mut l, "Δ+3d) beside the marker, when the bar is wide enough.");
    blank(&mut l);

    heading(&mut l, "Map");
    row(&mut l, "◆", "the sub-satellite point — directly beneath the satellite");
    row(&mut l, "▲", "your ground station");
    row(&mut l, "· / +", "with `p` on: a labelled city / satellite ground station");
    row(&mut l, "bright line", "the next 65 minutes of ground track");
    row(&mut l, "dim line", "the past 35 minutes of ground track");
    row(&mut l, "violet ring", "the visibility footprint — where the satellite is above 0°");
    row(&mut l, "amber dots", "the day/night terminator");
    row(&mut l, "dark ground", "the night side of the Earth");
    row(&mut l, "lighter band", "civil twilight — the Sun 0°–6° below the horizon");
    row(&mut l, "◉", "pad of the highlighted launch, with its provider, site and coordinates — see below");
    text(&mut l, "The map shows the whole world; `f` zooms it to a window centred on");
    text(&mut l, "the satellite, wrapping across the dateline to keep it dead centre.");
    text(&mut l, "`+` / `-` step that window through four magnifications — ×2, ×4, ×8,");
    text(&mut l, "×16 the whole-world scale, shown as `MAP ×N` in the header — and `-`");
    text(&mut l, "past the widest drops back to the whole world. `m` expands the map to");
    text(&mut l, "fill the screen.");
    text(&mut l, "`p` labels prominent cities and ground stations, drawing as many as");
    text(&mut l, "fit without overlapping — so a whole-world map shows only a scattered");
    text(&mut l, "few and more fill in the further `+` zooms in, or `m` widens the map.");
    text(&mut l, "While NEXT PASSES holds focus this pane shows a sky plot of the");
    text(&mut l, "highlighted pass instead — see below.");
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
    row(&mut l, "TLE", "time from the element-set epoch — amber past 36h, red past 72h");
    text(&mut l, "Names the epoch inline, e.g. \"18h since 09-06 23:11Z\" (UTC). Reads \"before\"");
    text(&mut l, "instead of \"since\" when the clock is scrubbed ahead of the epoch.");
    row(&mut l, "ACC", "modelled position error, and the along-track timing error it implies");
    text(&mut l, "Built from the element set's own drag term (B*) — re-propagated with B*");
    text(&mut l, "nudged 10% and the two positions differenced — floored by a per-regime");
    text(&mut l, "growth rate. So it is satellite-specific: a decaying LEO degrades faster");
    text(&mut l, "than a quiet high orbit. Amber past 3 days from epoch, red past 2 weeks;");
    text(&mut l, "past 30 days it reads \"beyond\" — SGP4 still answers, but nothing here can");
    text(&mut l, "say how wrong it is.");
    blank(&mut l);

    heading(&mut l, "Next passes");
    text(&mut l, "Passes peaking above 10° elevation within the next 96h, recomputed");
    text(&mut l, "every 20s. Each row: day, AOS–LOS in local time, duration in");
    text(&mut l, "minutes, peak elevation, and the AOS→LOS compass azimuths.");
    row(&mut l, "★", "visible to the naked eye — satellite sunlit while you're in darkness");
    text(&mut l, "A footer appears when the ACC timing error above exceeds a second,");
    text(&mut l, "bounding how far the AOS/LOS times on screen could slip.");
    text(&mut l, "The panel title names your ground station and its UTC offset — the");
    text(&mut l, "same offset the AOS–LOS times above are shown in.");
    text(&mut l, "Focus this panel (`4`) and scroll (`j`/`k`) to highlight a pass — the");
    text(&mut l, "map pane then shows a sky plot of it: a polar chart with the zenith");
    text(&mut l, "at the centre, the horizon at the rim, north up, and elevation rings");
    text(&mut l, "at 30° and 60°.");
    row(&mut l, "▲ / ▼", "the pass rising (AOS) and setting (LOS), on the horizon rim");
    row(&mut l, "◇", "culmination, labelled with its peak elevation");
    row(&mut l, "◆", "the satellite itself — only while the pass is under way");
    text(&mut l, "     Same filled diamond the map uses, and it moves with the clock: warp");
    text(&mut l, "     or step the time and watch it climb the arc from ▲ past ◇ out to ▼.");
    row(&mut l, "bright arc", "the satellite is sunlit along this stretch of the pass");
    row(&mut l, "dim arc", "it is in the Earth's shadow here");
    text(&mut l, "The plot's footer gives the AOS / culmination / LOS times, their");
    text(&mut l, "bearings and the pass length; it clears when focus leaves the panel.");
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
    text(&mut l, "A feed whose last fetch attempt failed reads at least amber whatever");
    text(&mut l, "its age, and logs why under Recent activity.");
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
    text(&mut l, "reads it straight from disk instead of refetching. A refresh that");
    text(&mut l, "fails retries on a 1m-to-30m backoff, not after another 12h.");
    text(&mut l, "Accuracy decays away from that epoch — roughly a km near it, tens of");
    text(&mut l, "km after a week in low orbit — which the TELEMETRY panel's ACC row");
    text(&mut l, "estimates and the amber/red TLE age flags.");
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
