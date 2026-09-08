//! Application wiring: shared state, background fetch tasks, the input thread and
//! the render loop.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::{mpsc, watch, Notify};

use crate::api;
use crate::api::celestrak::SatMatch;
use crate::api::launches::{Fetched, Launches};
use crate::api::swpc::{AuroraGrid, Indices};
use crate::cache::Cache;
use crate::config::Config;
use crate::orbit::{predict_passes, Pass, Tracker};
use crate::simclock::{ClockState, SimClock};
use crate::source::Source;
use crate::ui;

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Map,
    Tracked,
    Telemetry,
    Passes,
    Weather,
    Launches,
}

impl Panel {
    /// Every panel, in focus-cycle and key order — the single source of truth
    /// `next`, `key` and `from_key` all derive from, so a panel added or
    /// reordered here can't drift out of sync with the others.
    pub const ALL: [Panel; 6] = [
        Panel::Map,
        Panel::Tracked,
        Panel::Telemetry,
        Panel::Passes,
        Panel::Weather,
        Panel::Launches,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The `1`–`6` key that focuses this panel — shown in its header so the
    /// two stay visibly connected.
    pub fn key(self) -> u8 {
        self.index() as u8 + 1
    }

    /// The panel a `1`–`6` key digit focuses, if any.
    fn from_key(digit: u8) -> Option<Self> {
        Self::ALL.get((digit as usize).checked_sub(1)?).copied()
    }
}

/// A wake-up bell per data source, so `r` can force a specific feed to refetch
/// right now instead of waiting out its interval. The TLE feed doesn't need
/// one: it already wakes on `sat_tx`, reused here for "refresh" as well as
/// "satellite changed".
#[derive(Clone)]
struct Notifiers {
    weather: Arc<Notify>,
    aurora: Arc<Notify>,
    launches: Arc<Notify>,
}

impl Notifiers {
    fn new() -> Self {
        Self {
            weather: Arc::new(Notify::new()),
            aurora: Arc::new(Notify::new()),
            launches: Arc::new(Notify::new()),
        }
    }
}

/// The state of the "track satellite" popup's catalogue search: what's been
/// submitted, and (once it lands) what came back.
#[derive(Debug, Default, Clone)]
pub enum SearchState {
    /// Nothing submitted yet, or the query was edited since the last result.
    #[default]
    Idle,
    /// A search is in flight.
    Busy,
    /// `query` returned `results` — possibly empty, meaning no match.
    Done { query: String, results: Vec<SatMatch> },
    /// `query` failed outright (a network error, not "no match").
    Failed { query: String, msg: String },
}

/// All fetched, shared state. Written by background tasks, read by the renderer.
#[derive(Default)]
pub struct AppData {
    pub tle: Source<Tracker>,
    pub weather: Source<Indices>,
    pub aurora: Source<AuroraGrid>,
    pub launches: Source<Launches>,
    pub search: SearchState,
    pub log: Vec<String>,
}

impl AppData {
    pub fn note(&mut self, msg: impl Into<String>) {
        self.log.push(msg.into());
        if self.log.len() > 200 {
            self.log.drain(0..self.log.len() - 200);
        }
    }
}

/// State of the open "track satellite" popup: what's been typed, and which
/// search result (if any) is highlighted.
#[derive(Debug, Default)]
pub struct SatPicker {
    pub query: String,
    pub selected: usize,
}

/// State of the open "go to time" prompt (`g`): the raw text and the last parse
/// error to show beneath the field, if any. Parsing lives in
/// [`crate::simclock::parse_goto`].
#[derive(Debug, Default)]
pub struct TimeInput {
    pub buffer: String,
    pub error: Option<String>,
}

/// Renderer-owned UI state (not shared with fetch tasks).
pub struct App {
    pub config: Config,
    pub data: Arc<RwLock<AppData>>,
    pub focus: Panel,
    pub map_fullscreen: bool,
    pub follow: bool,
    /// Follow-window magnification: 0 is the widest follow level (×2 the whole
    /// world), rising to `crate::ui::MAX_ZOOM` (×16). Only meaningful while
    /// `follow` is set, but it *survives* an `f` toggle, so turning follow back
    /// on returns to the level you left.
    pub zoom: usize,
    /// Whether the map draws its layer of prominent-place labels (the `p`
    /// key). Renderer state only — like `follow`/`zoom`, it isn't persisted.
    pub places: bool,
    pub show_help: bool,
    /// Scroll offset within the help overlay, in lines; clamped against its
    /// content height at render time.
    pub help_scroll: u16,
    pub should_quit: bool,
    pub started: Instant,
    /// The displayed clock. Wall time until scrubbed; see [`SimClock`] and
    /// [`App::sim_now`].
    pub clock: SimClock,
    /// `Some` while the "track satellite" popup is open.
    pub sat_input: Option<SatPicker>,
    /// `Some` while the "go to time" prompt is open.
    pub time_input: Option<TimeInput>,
    pub passes: Vec<Pass>,
    passes_at: Option<Instant>,
    /// The simulated instant `passes` was computed for, so a time scrub can
    /// invalidate the list before its 20 s wall throttle would.
    passes_from: Option<DateTime<Utc>>,
    sat_tx: watch::Sender<u64>,
    /// Submits a catalogue search query to the search task; a no-op send
    /// (nothing listening) in `--offline` mode, where submission is handled
    /// locally instead.
    search_tx: watch::Sender<String>,
    notifiers: Notifiers,
    /// Position within whichever list the focused panel shows (tracked
    /// satellites, passes, or the Launches panel). Reset whenever focus moves
    /// elsewhere; clamped to the list's actual length at render time.
    pub list_pos: usize,
}

impl App {
    /// Wall-clock time since the session started.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// The instant the dashboard should draw — wall time, unless the clock has
    /// been scrubbed. Everything about the satellite (position, track,
    /// terminator, passes, the TLE-age thresholds) is a function of this;
    /// feed ages, the status chips and `uptime` are not — they stay on the
    /// real clock.
    pub fn sim_now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Switch focus to `panel`, resetting list scroll — a scroll position
    /// from a different list would be meaningless here.
    fn set_focus(&mut self, panel: Panel) {
        self.focus = panel;
        self.list_pos = 0;
    }

    /// Zoom the follow window one step tighter. When not following, this turns
    /// follow on at the remembered level rather than stepping — so `+` from the
    /// whole world and `f` land on the same place, and neither is ever a no-op
    /// while a tighter view exists.
    fn zoom_in(&mut self) {
        if !self.follow {
            self.follow = true;
        } else {
            self.zoom = (self.zoom + 1).min(crate::ui::MAX_ZOOM);
        }
    }

    /// Zoom the follow window one step wider. At the widest follow level this
    /// drops out of follow entirely to the whole world — the outermost stop of
    /// the same continuum — leaving `zoom` untouched so `f` or `+` returns to
    /// the level just left.
    fn zoom_out(&mut self) {
        if self.zoom == 0 {
            self.follow = false;
        } else {
            self.zoom -= 1;
        }
    }

    /// Move the selection within the focused panel's list, if it has one.
    /// Clamped immediately against the list's real length so repeated presses
    /// at an edge don't build up a backlog that later presses have to work
    /// through before the selection visibly moves.
    fn scroll(&mut self, delta: i32) {
        let max_index = match self.focus {
            Panel::Tracked => self.config.tracked.len().saturating_sub(1),
            Panel::Passes => self.passes.len().saturating_sub(1),
            Panel::Launches => self
                .data
                .read()
                .ok()
                .and_then(|d| d.launches.get().map(|l| l.list.len()))
                .unwrap_or(0)
                .saturating_sub(1),
            Panel::Map | Panel::Telemetry | Panel::Weather => return,
        };
        let next = self.list_pos as i32 + delta;
        self.list_pos = next.clamp(0, max_index as i32) as usize;
    }

    /// Drop the cached pass list so `refresh_passes` rebuilds it on the next
    /// frame — after an explicit time jump the 20 s wall throttle is not the
    /// right gate.
    fn invalidate_passes(&mut self) {
        self.passes_at = None;
        self.passes_from = None;
    }

    /// Recompute pass predictions if the cache is stale or the inputs changed.
    fn refresh_passes(&mut self) {
        let now = self.sim_now();
        // A time scrub can move `now` well past the window the current list was
        // built for without the 20 s wall throttle having elapsed. Recompute
        // whenever the clock has drifted more than a few minutes from what
        // `passes` reflects — the coarse 30 s scan in `predict_passes` means a
        // few minutes of slop costs nothing.
        let scrubbed = self
            .passes_from
            .is_none_or(|from| (now - from).abs() > chrono::Duration::minutes(5));
        let stale = self
            .passes_at
            .map(|t| t.elapsed() > Duration::from_secs(20))
            .unwrap_or(true);
        if !scrubbed && !stale {
            return;
        }
        // Only the continuous drift of a warp needs a real-time floor. A step,
        // a `g` jump, `n`/`N` or `0` is bounded by the user's fingers and must
        // rebuild at once — and `invalidate_passes` clears `passes_at`, so
        // those paths never reach this test. At 1800× the 5-minute simulated
        // window above is crossed every ~170 ms of wall time, which without
        // this would put a full 96-hour scan inside every frame.
        let warping = matches!(self.clock.state(), ClockState::Warp(_));
        if warping && self.passes_at.is_some_and(|t| t.elapsed() < PASS_REBUILD_FLOOR) {
            return;
        }
        let Some(station) = self.config.ground_station() else {
            self.passes.clear();
            self.passes_at = Some(Instant::now());
            self.passes_from = Some(now);
            return;
        };
        let tracker = self.data.read().ok().and_then(|d| d.tle.get().cloned());
        let Some(tr) = tracker else {
            // No element set yet: leave the throttle unset so we retry as soon
            // as one arrives, rather than waiting out a full 20 s window for
            // nothing.
            return;
        };
        self.passes = predict_passes(&tr, &station, now, chrono::Duration::hours(96), 24);
        self.passes_at = Some(Instant::now());
        self.passes_from = Some(now);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        // Ctrl-C quits unconditionally, regardless of what mode swallows the
        // rest of the keys below.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        // Satellite-entry mode swallows most keys.
        if let Some(picker) = self.sat_input.as_mut() {
            match key.code {
                // Real catalogue names carry spaces, parentheses and dashes
                // (e.g. "ISS (ZARYA)", "NOAA 20") — anything printable is
                // allowed, not just alphanumerics.
                KeyCode::Char(c) if !c.is_control() && picker.query.chars().count() < 32 => {
                    picker.query.push(c);
                    picker.selected = 0;
                    self.reset_search();
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    self.reset_search();
                }
                KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
                KeyCode::Down => {
                    let len = self
                        .data
                        .read()
                        .ok()
                        .and_then(|d| match &d.search {
                            SearchState::Done { results, .. } => Some(results.len()),
                            _ => None,
                        })
                        .unwrap_or(0);
                    if len > 0 {
                        picker.selected = (picker.selected + 1).min(len - 1);
                    }
                }
                KeyCode::Enter => self.submit_sat_input(),
                KeyCode::Esc => self.sat_input = None,
                _ => {}
            }
            return;
        }

        // The go-to-time prompt swallows its keys too. After the satellite
        // picker (which eats every printable char) and before the help overlay,
        // so at most one modal is ever taking input.
        if let Some(input) = self.time_input.as_mut() {
            match key.code {
                KeyCode::Char(c) if !c.is_control() && input.buffer.chars().count() < 32 => {
                    input.buffer.push(c);
                    input.error = None;
                }
                KeyCode::Backspace => {
                    input.buffer.pop();
                    input.error = None;
                }
                KeyCode::Enter => self.submit_time_input(),
                KeyCode::Esc => self.time_input = None,
                _ => {}
            }
            return;
        }

        // The help overlay swallows its own scroll keys; everything else
        // (including quit) still works while it's open.
        if self.show_help {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll = self.help_scroll.saturating_add(1)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                KeyCode::Home => self.help_scroll = 0,
                // Clamped against actual content height at render time.
                KeyCode::End => self.help_scroll = u16::MAX,
                KeyCode::Char('?') | KeyCode::Esc => self.show_help = false,
                KeyCode::Char('q') => self.should_quit = true,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                self.help_scroll = 0;
            }
            KeyCode::Char('m') => self.map_fullscreen = !self.map_fullscreen,
            KeyCode::Char('f') => self.follow = !self.follow,
            KeyCode::Char('p') => self.places = !self.places,
            // `=`/`_` so the binding fires whether or not shift is held.
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom_in(),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom_out(),
            KeyCode::Tab => self.set_focus(self.focus.next()),
            KeyCode::Char(c @ '1'..='6') => {
                if let Some(panel) = Panel::from_key(c as u8 - b'0') {
                    self.set_focus(panel);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
            KeyCode::Char('s') => {
                self.sat_input = Some(SatPicker::default());
                self.reset_search();
            }
            KeyCode::Enter if self.focus == Panel::Tracked => self.track_selected(),
            KeyCode::Char('d') | KeyCode::Delete if self.focus == Panel::Tracked => {
                self.remove_tracked()
            }
            KeyCode::Char('r') => self.refresh_focused(),

            // Time scrubbing. The displayed clock only; feed ages and the
            // status chips stay on wall time. `<`/`>` and `h`/`l` are the
            // shift-agnostic partners of `,`/`.` and the arrows, the same way
            // `=`/`_` back up `+`/`-` above.
            KeyCode::Char(' ') => self.clock.toggle_pause(),
            KeyCode::Char('.') | KeyCode::Char('>') => self.clock.faster(),
            KeyCode::Char(',') | KeyCode::Char('<') => self.clock.slower(),
            KeyCode::Left | KeyCode::Char('h') => self.clock.jump(chrono::Duration::minutes(-1)),
            KeyCode::Right | KeyCode::Char('l') => self.clock.jump(chrono::Duration::minutes(1)),
            KeyCode::Char('[') => self.clock.jump(chrono::Duration::hours(-1)),
            KeyCode::Char(']') => self.clock.jump(chrono::Duration::hours(1)),
            KeyCode::Char('n') => self.jump_to_next_pass(false),
            KeyCode::Char('N') => self.jump_to_next_pass(true),
            KeyCode::Char('0') => {
                self.clock.reset();
                self.invalidate_passes();
            }
            KeyCode::Char('g') => self.time_input = Some(TimeInput::default()),

            _ => {}
        }
    }

    /// Force just the focused panel's data source(s) to refetch now, instead
    /// of waiting out their interval.
    fn refresh_focused(&mut self) {
        let refreshed = match self.focus {
            Panel::Map | Panel::Tracked | Panel::Telemetry | Panel::Passes => {
                let _ = self.sat_tx.send(self.config.sat);
                "orbital element set"
            }
            Panel::Weather => {
                self.notifiers.weather.notify_one();
                self.notifiers.aurora.notify_one();
                "space weather"
            }
            Panel::Launches => {
                self.notifiers.launches.notify_one();
                "launches"
            }
        };
        if let Ok(mut d) = self.data.write() {
            d.note(format!("refreshing {refreshed}"));
        }
    }

    /// Start tracking `norad_id`, recording it in the tracked list either
    /// way. `name` is the catalogue name to record when known (a fresh
    /// search result); `None` falls back to whatever's already on file for
    /// it, or a placeholder — `sync_tracked_name` fills that in for real once
    /// its element set arrives.
    fn switch_satellite(&mut self, norad_id: u64, name: Option<String>) {
        let display_name = name.unwrap_or_else(|| {
            self.config
                .tracked_name(norad_id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("NORAD {norad_id}"))
        });

        // Re-selecting the satellite already at the front of the tracked
        // list, under the same name, is a genuine no-op — skip the reorder
        // and the disk write so bouncing between two tracked entries with
        // `Enter`/`Enter` doesn't hit the filesystem on every keypress.
        let already_current = norad_id == self.config.sat
            && self
                .config
                .tracked
                .first()
                .is_some_and(|t| t.norad_id == norad_id && t.name == display_name);
        if already_current {
            return;
        }

        self.config.track(norad_id, display_name);

        if norad_id != self.config.sat {
            self.config.sat = norad_id;
            if let Ok(mut d) = self.data.write() {
                d.tle = Source::default();
                d.note(format!("switching to NORAD {norad_id}"));
            }
            self.passes.clear();
            self.passes_at = None;
            self.passes_from = None;
            let _ = self.sat_tx.send(norad_id);
        }
        // Persist the choice; ignore write errors (e.g. read-only home).
        let _ = self.config.save();
    }

    /// Resolve the go-to-time prompt: on a good parse, jump there and close;
    /// on a bad one, keep the prompt open with the error under the field.
    fn submit_time_input(&mut self) {
        let Some(input) = self.time_input.as_ref() else { return };
        match crate::simclock::parse_goto(&input.buffer, self.sim_now()) {
            Ok(target) => {
                self.clock.goto(target);
                self.invalidate_passes();
                self.time_input = None;
            }
            Err(e) => {
                if let Some(input) = self.time_input.as_mut() {
                    input.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// Jump the clock to 30 s before the next predicted pass rises, paused
    /// there. `visible_only` restricts to naked-eye (`★`) passes. A no-op with
    /// a note when there is no ground station or the window holds no such pass.
    fn jump_to_next_pass(&mut self, visible_only: bool) {
        // Land on AOS minus this, so the satellite is seen coming over the
        // horizon rather than already up.
        let lead = chrono::Duration::seconds(30);
        let now = self.sim_now();
        // `> now`, strictly: right after a jump `now == aos - lead` for the
        // pass we just landed on, so a second press must skip it and advance
        // to the following one rather than re-selecting the same pass.
        let target = self
            .passes
            .iter()
            .filter(|p| !visible_only || p.visible)
            .map(|p| p.aos - lead)
            .find(|&t| t > now);
        match target {
            Some(t) => {
                self.clock.goto(t);
                self.invalidate_passes();
            }
            None => {
                if let Ok(mut d) = self.data.write() {
                    d.note(if visible_only {
                        "no visible pass ahead in the prediction window"
                    } else {
                        "no pass ahead in the prediction window"
                    });
                }
            }
        }
    }

    /// Clear a stale search result — the query no longer matches what's
    /// being shown, either because it was just edited or a fresh popup opened.
    fn reset_search(&mut self) {
        if let Ok(mut d) = self.data.write() {
            d.search = SearchState::Idle;
        }
    }

    /// `Enter` in the "track satellite" popup. A bare NORAD id is
    /// unambiguous, so it tracks immediately — same as before this rework,
    /// and it works offline, since nothing needs to be looked up first (a bad
    /// id still surfaces later, the same way it always has, as a failed TLE
    /// fetch). A name is different: it may match several objects, so it
    /// selects the highlighted search result if one is already showing for
    /// this exact query, otherwise submits it for a fresh search.
    fn submit_sat_input(&mut self) {
        let Some(picker) = self.sat_input.as_ref() else { return };
        let query = picker.query.trim().to_string();
        let selected = picker.selected;
        if query.is_empty() {
            return;
        }

        if let Ok(id) = query.parse::<u64>() {
            self.switch_satellite(id, None);
            self.sat_input = None;
            return;
        }

        let results = self.data.read().ok().and_then(|d| match &d.search {
            SearchState::Done { query: q, results } if *q == query => Some(results.clone()),
            _ => None,
        });
        match results.filter(|r| !r.is_empty()) {
            Some(results) => {
                if let Some(m) = results.get(selected).or_else(|| results.first()) {
                    let (id, name) = (m.norad_id, m.name.clone());
                    self.switch_satellite(id, Some(name));
                    self.sat_input = None;
                }
            }
            None => self.submit_search(query),
        }
    }

    /// Submit a name search — reached only for a non-numeric query, a bare
    /// NORAD id having already been handled directly in `submit_sat_input`.
    fn submit_search(&mut self, query: String) {
        if self.config.offline {
            if let Ok(mut d) = self.data.write() {
                d.search = SearchState::Failed {
                    query,
                    msg: "search needs network — enter a NORAD id".to_string(),
                };
            }
            return;
        }
        let _ = self.search_tx.send(query);
    }

    /// `Enter` on the TRACKED panel: start tracking whichever entry is
    /// highlighted.
    fn track_selected(&mut self) {
        if let Some(t) = self.config.tracked.get(self.list_pos).cloned() {
            self.switch_satellite(t.norad_id, Some(t.name));
        }
    }

    /// `d` on the TRACKED panel: drop the highlighted entry, unless it's the
    /// satellite currently being tracked — the list always has to contain at
    /// least that one.
    fn remove_tracked(&mut self) {
        let Some(t) = self.config.tracked.get(self.list_pos) else { return };
        let norad_id = t.norad_id;
        if norad_id == self.config.sat {
            if let Ok(mut d) = self.data.write() {
                d.note("can't remove the satellite being tracked");
            }
            return;
        }
        self.config.untrack(norad_id);
        let _ = self.config.save();
        // `list_pos` doesn't need reclamping here: every reader already
        // clamps it against the list's current length (the render side, and
        // `scroll`'s own fresh `max_index` computation), so a momentarily
        // too-large value is harmless.
    }

    /// Adopt the tracked satellite's real catalogue name once its element set
    /// arrives, replacing whatever placeholder it was added under (e.g.
    /// `NORAD 43013` from a bare-id add). A no-op once the name already
    /// matches, so this can be called every frame without rewriting the
    /// config file each time.
    fn sync_tracked_name(&mut self) {
        let data = self.data.read().ok();
        let Some(name) = data.as_ref().and_then(|d| d.tle.get()).map(Tracker::name) else { return };
        // Compared as `&str` first so the common case — every frame once the
        // name has already caught up — costs nothing but the comparison, not
        // an allocation.
        if self.config.tracked_name(self.config.sat) != Some(name) {
            self.config.track(self.config.sat, name.to_string());
            let _ = self.config.save();
        }
    }
}

/// Entry point from `main`. Owns the terminal for the duration of the session.
pub async fn run(mut config: Config) -> Result<()> {
    let cache = Cache::open().context("opening the disk cache")?;
    let http = api::client()?;

    // An explicit --location always wins: resolve it and persist the result.
    if let Some(query) = config.location_query.take() {
        if config.offline {
            eprintln!("nadir: --location needs network access; ignoring it in --offline mode.");
        } else {
            match api::geocode::resolve(&http, &query).await {
                Ok(place) => {
                    config.set_location(place.lat, place.lon, Some(place.label.clone()));
                    if let Err(e) = config.save() {
                        eprintln!("nadir: could not save the resolved location: {e:#}");
                    } else {
                        eprintln!(
                            "nadir: tracking from {} ({:.3}, {:.3}).",
                            place.label, place.lat, place.lon
                        );
                    }
                }
                Err(e) => eprintln!("nadir: could not resolve --location \"{query}\": {e:#}"),
            }
        }
    }

    // An explicit --sat name (as opposed to a bare NORAD id, already applied
    // straight to `config.sat`) also needs resolving before the TUI starts.
    if let Some(query) = config.sat_query.take() {
        if config.offline {
            eprintln!("nadir: --sat name needs network access; ignoring it in --offline mode.");
        } else {
            match api::celestrak::search_by_name(&http, &query).await {
                Ok(matches) if !matches.is_empty() => {
                    let m = &matches[0];
                    config.sat = m.norad_id;
                    config.track(m.norad_id, m.name.clone());
                    if let Err(e) = config.save() {
                        eprintln!("nadir: could not save the resolved satellite: {e:#}");
                    } else {
                        eprintln!("nadir: tracking {} (NORAD {}).", m.name, m.norad_id);
                    }
                }
                Ok(_) => {
                    eprintln!("nadir: no satellite matches --sat \"{query}\"; keeping the current one.")
                }
                Err(e) => eprintln!("nadir: could not resolve --sat \"{query}\": {e:#}"),
            }
        }
    }

    // Make sure the tracked list always has at least the satellite we're
    // about to track — a first run, or one restored from a config file that
    // predates the tracked list, would otherwise start with an empty panel.
    if config.tracked_name(config.sat).is_none() {
        config.track(config.sat, format!("NORAD {}", config.sat));
        let _ = config.save();
    }

    // First-run location discovery, only when nothing more specific was given.
    if config.ground_station().is_none() {
        if config.offline || !config.allow_geoip {
            eprintln!(
                "nadir: no ground station configured. Pass --lat/--lon, --location, or edit {}.",
                config.path.display()
            );
        } else if let Ok(guess) = api::geoip::guess(&http).await {
            config.set_location(guess.lat, guess.lon, Some(guess.label.clone()));
            if let Err(e) = config.save() {
                eprintln!("nadir: could not save the guessed location: {e:#}");
            } else {
                eprintln!(
                    "nadir: guessed your location as {} ({:.2}, {:.2}) — edit {} to correct it.",
                    guess.label,
                    guess.lat,
                    guess.lon,
                    config.path.display()
                );
            }
        }
    }

    let data = Arc::new(RwLock::new(AppData::default()));
    let (sat_tx, sat_rx) = watch::channel(config.sat);
    let (search_tx, search_rx) = watch::channel(String::new());
    let notifiers = Notifiers::new();

    // Warm-start from whatever each source last cached, so panels show the
    // last known value immediately instead of a "pending" placeholder while
    // the first fetch of the session is still in flight.
    load_all_from_cache(&cache, &data, config.sat);
    if config.offline {
        if let Ok(mut d) = data.write() {
            d.note("offline mode — showing cached data only");
        }
    } else {
        spawn_fetch_tasks(&http, &cache, &data, sat_rx, search_rx, notifiers.clone());
    }

    let (input_tx, input_rx) = mpsc::unbounded_channel();
    spawn_input_thread(input_tx);

    let mut app = App {
        config,
        data,
        focus: Panel::Map,
        map_fullscreen: false,
        // The map opens on the whole world so the first frame has global
        // context; `f` zooms it to a window centred on the satellite.
        follow: false,
        zoom: 0,
        places: false,
        show_help: false,
        help_scroll: 0,
        should_quit: false,
        started: Instant::now(),
        clock: SimClock::new(),
        sat_input: None,
        time_input: None,
        passes: Vec::new(),
        passes_at: None,
        passes_from: None,
        sat_tx,
        search_tx,
        notifiers,
        list_pos: 0,
    };

    let mut terminal = ratatui::init();
    let result = render_loop(&mut terminal, &mut app, input_rx).await;
    ratatui::restore();
    result
}

/// Redraw cadence. At 1× the dashboard is a clock face and four frames a second
/// is plenty; a warp turns it into an animation, where a 250 ms frame at 60×
/// steps a quarter-hour of simulated time and the marker teleports rather than
/// moves. `FRAME_WARP` sits near a terminal's key auto-repeat rate on purpose —
/// holding a key already drove the loop that fast by waking the `select!` on
/// every repeat, which is exactly why a held key looked smoother than a warp at
/// the same speed.
const FRAME_LIVE: Duration = Duration::from_millis(250);
const FRAME_WARP: Duration = Duration::from_millis(40);

/// How long to wait for the next frame. Split out of `render_loop` so the
/// cadence can be asserted without a terminal.
fn frame_interval(state: ClockState) -> Duration {
    match state {
        // Only a warp animates on its own. `Drifted` moves at real speed and
        // `Paused` does not move at all, so both are as static as `Live`.
        ClockState::Warp(_) => FRAME_WARP,
        _ => FRAME_LIVE,
    }
}

async fn render_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut input_rx: mpsc::UnboundedReceiver<Event>,
) -> Result<()> {
    loop {
        // Fold elapsed wall time into the simulated clock before anything
        // reads it this frame.
        app.clock.advance();
        app.refresh_passes();
        app.sync_tracked_name();
        terminal
            .draw(|frame| ui::draw(frame, app))
            .context("drawing a frame")?;

        if app.should_quit {
            return Ok(());
        }

        // `sleep` rather than an `Interval`: tokio 1.53 has no
        // `Interval::set_period`, and a per-iteration sleep cannot build up the
        // catch-up burst `MissedTickBehavior::Skip` used to guard against.
        tokio::select! {
            _ = tokio::time::sleep(frame_interval(app.clock.state())) => {}
            maybe_event = input_rx.recv() => {
                match maybe_event {
                    Some(Event::Key(key)) => {
                        app.handle_key(key);
                        // Auto-repeat can queue keys faster than a frame takes
                        // to draw. Fold everything already waiting into this one
                        // frame rather than drawing a frame per keystroke and
                        // falling further behind the queue with each one.
                        while !app.should_quit {
                            match input_rx.try_recv() {
                                Ok(Event::Key(k)) => app.handle_key(k),
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    }
                    Some(_) => {}
                    None => return Ok(()), // input thread ended
                }
            }
        }
    }
}

/// Blocking `crossterm` reads live on their own OS thread and are forwarded here.
fn spawn_input_thread(tx: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(ev) => {
                if tx.send(ev).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });
}

// --- background fetching -------------------------------------------------------

const TLE_KEY: &str = "tle";
const WEATHER_KEY: &str = "weather";
const LAUNCHES_KEY: &str = "launches";
/// How long a cached element set is trusted before it's worth refetching.
pub(crate) const TLE_TTL: Duration = Duration::from_secs(12 * 3600);
/// How often each timer-driven feed refetches. Named rather than inline
/// because `ui::status_bar` derives each chip's colour thresholds from the
/// interval it is judging — a chip that hardcodes its own knees drifts away
/// from its feed's schedule the moment one of them is retuned.
pub(crate) const WEATHER_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub(crate) const AURORA_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub(crate) const LAUNCHES_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The shortest real interval between two warp-driven pass-list rebuilds. A
/// rebuild is a 96-hour scan — order twelve thousand SGP4 propagations, run
/// synchronously on the render thread, and measured at ~7 ms in a release
/// build: cheap in absolute terms, well under a 250 ms frame. What this floor
/// guards against is not the cost of one scan but their *rate*: the 5-minute
/// *simulated* window in `refresh_passes` is crossed every ~170 ms of wall
/// time at 1800×, which without a floor would put a rebuild inside every
/// frame. A fast warp is allowed to lag the list by up to this long instead,
/// invisible next to the 30 s coarse step `predict_passes` already scans at.
const PASS_REBUILD_FLOOR: Duration = Duration::from_millis(500);

/// One network-backed dashboard field, tied together in one place: which
/// `AppData` slot it publishes into, which cache key it persists under, and
/// how it is decoded off disk. Each fetch task below keeps its own timing and
/// error handling — a `Feed` only removes the repeated *(key, field, codec)*
/// plumbing those tasks would otherwise each spell out by hand.
struct Feed<T> {
    /// `None` for a source that is never written to disk — the aurora
    /// nowcast, refetched from scratch on its own schedule instead. `store`
    /// and `recover` are no-ops in that case.
    key: Option<String>,
    field: fn(&mut AppData) -> &mut Source<T>,
    decode: fn(&[u8]) -> Option<T>,
}

impl<T> Feed<T> {
    /// Record a fresh, successful value.
    fn live(&self, data: &Arc<RwLock<AppData>>, value: T) {
        if let Ok(mut d) = data.write() {
            (self.field)(&mut d).set_live(value);
        }
    }

    /// Note that a refresh failed.
    fn fail(&self, data: &Arc<RwLock<AppData>>, reason: String) {
        if let Ok(mut d) = data.write() {
            (self.field)(&mut d).set_failed(reason);
        }
    }

    /// Persist a raw, already-encoded payload under this feed's cache key.
    fn store(&self, cache: &Cache, bytes: &[u8]) {
        if let Some(key) = &self.key {
            let _ = cache.put(key, bytes);
        }
    }

    /// Load this feed's last cached value, unless one is already present —
    /// so warm-starting from disk never clobbers a fetch that already landed.
    fn recover(&self, cache: &Cache, data: &Arc<RwLock<AppData>>) {
        let Some(key) = &self.key else { return };
        let Ok(mut d) = data.write() else { return };
        if (self.field)(&mut d).get().is_some() {
            return;
        }
        if let Some(bytes) = cache.get(key) {
            if let Some(value) = (self.decode)(&bytes) {
                let age = cache.age(key).unwrap_or_default();
                (self.field)(&mut d).set_from_cache(value, age);
                d.note(format!("recovered {key} from cache ({} old)", crate::source::fmt_age(age)));
            }
        }
    }
}

impl<T: serde::Serialize> Feed<T> {
    /// Cache and publish a fresh value in one step, for the JSON-serialised
    /// feeds.
    fn publish(&self, cache: &Cache, data: &Arc<RwLock<AppData>>, value: T) {
        if let Ok(json) = serde_json::to_vec(&value) {
            self.store(cache, &json);
        }
        self.live(data, value);
    }
}

impl Feed<Tracker> {
    /// Not `Serialize` — Celestrak's own response text is cached verbatim via
    /// `store` instead of round-tripping through `publish`.
    fn tle(norad: u64) -> Self {
        Self {
            key: Some(format!("{TLE_KEY}-{norad}")),
            field: |d| &mut d.tle,
            decode: |bytes| {
                String::from_utf8(bytes.to_vec())
                    .ok()
                    .and_then(|s| Tracker::from_gp_json(&s).ok())
            },
        }
    }
}

impl Feed<Indices> {
    fn weather() -> Self {
        Self {
            key: Some(WEATHER_KEY.to_string()),
            field: |d| &mut d.weather,
            decode: |b| serde_json::from_slice::<Indices>(b).ok(),
        }
    }
}

impl Feed<Launches> {
    fn launches() -> Self {
        Self {
            key: Some(LAUNCHES_KEY.to_string()),
            field: |d| &mut d.launches,
            decode: |b| serde_json::from_slice::<Launches>(b).ok(),
        }
    }
}

impl Feed<AuroraGrid> {
    /// Never cached (see [`Feed::key`]); `decode` is consequently never
    /// called.
    fn aurora() -> Self {
        Self { key: None, field: |d| &mut d.aurora, decode: |_| None }
    }
}

fn spawn_fetch_tasks(
    http: &reqwest::Client,
    cache: &Cache,
    data: &Arc<RwLock<AppData>>,
    sat_rx: watch::Receiver<u64>,
    search_rx: watch::Receiver<String>,
    notifiers: Notifiers,
) {
    tle_task(http.clone(), cache.clone(), data.clone(), sat_rx);
    weather_task(http.clone(), cache.clone(), data.clone(), notifiers.weather);
    aurora_task(http.clone(), data.clone(), notifiers.aurora);
    launches_task(http.clone(), cache.clone(), data.clone(), notifiers.launches);
    search_task(http.clone(), data.clone(), search_rx);
}

/// Resolve catalogue name searches submitted from the "track satellite"
/// popup (a bare NORAD id is handled directly by `App::submit_sat_input` and
/// never reaches here). Unlike the other feeds, this isn't a timer loop — it
/// just waits for the next query. A `watch` channel (the same one `sat_tx`
/// uses) rather than an `mpsc` one means a fast typist's stale queries are
/// dropped for free: each `send` overwrites the pending value, so a query
/// this task is still busy with is never processed once superseded.
fn search_task(http: reqwest::Client, data: Arc<RwLock<AppData>>, mut rx: watch::Receiver<String>) {
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let query = rx.borrow().trim().to_string();
            if query.is_empty() {
                continue;
            }
            if let Ok(mut d) = data.write() {
                d.search = SearchState::Busy;
            }
            let result = api::celestrak::search_by_name(&http, &query).await;
            if let Ok(mut d) = data.write() {
                d.search = match result {
                    Ok(results) => SearchState::Done { query, results },
                    Err(e) => SearchState::Failed { query, msg: short(&e) },
                };
            }
        }
    });
}

fn tle_task(
    http: reqwest::Client,
    cache: Cache,
    data: Arc<RwLock<AppData>>,
    mut sat_rx: watch::Receiver<u64>,
) {
    tokio::spawn(async move {
        let mut prev_norad = *sat_rx.borrow();
        // Only an explicit "refresh now" of the satellite already being
        // tracked (the `r` key) should force a live fetch regardless of
        // cache age. A fresh start, or switching to a satellite whose own
        // cache is still within the TTL, just uses what's on disk instead
        // of spending a fetch nothing needed.
        let mut force = false;

        loop {
            let norad = *sat_rx.borrow();
            let switched = norad != prev_norad;
            prev_norad = norad;
            let feed = Feed::<Tracker>::tle(norad);
            let cache_age = feed.key.as_deref().and_then(|k| cache.age(k));
            let stale = cache_age.is_none_or(|age| age >= TLE_TTL);

            let wait = if stale || (force && !switched) {
                match api::celestrak::fetch_gp_json(&http, norad).await {
                    Ok(json) => match Tracker::from_gp_json(&json) {
                        Ok(tracker) => {
                            feed.store(&cache, json.as_bytes());
                            // Logged in the same write lock as `set_live` so
                            // the note and the value it describes always land
                            // together, never interleaved with another task.
                            if let Ok(mut d) = data.write() {
                                d.note(format!("TLE for {} updated", tracker.name()));
                                (feed.field)(&mut d).set_live(tracker);
                            }
                        }
                        Err(e) => feed.fail(&data, short(&e)),
                    },
                    Err(e) => {
                        feed.recover(&cache, &data);
                        feed.fail(&data, short(&e));
                    }
                }
                TLE_TTL
            } else {
                // Not stale and nothing forced a refetch — load it from disk
                // (a no-op once already loaded, e.g. by the startup warm
                // start) and just wait out whatever's left of its window.
                feed.recover(&cache, &data);
                TLE_TTL.saturating_sub(cache_age.unwrap_or(TLE_TTL))
            };

            tokio::select! {
                _ = tokio::time::sleep(wait) => { force = false; }
                _ = sat_rx.changed() => { force = true; }
            }
        }
    });
}

fn weather_task(http: reqwest::Client, cache: Cache, data: Arc<RwLock<AppData>>, notify: Arc<Notify>) {
    tokio::spawn(async move {
        let feed = Feed::<Indices>::weather();
        let mut ticker = tokio::time::interval(WEATHER_INTERVAL);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = notify.notified() => {}
            }
            match api::swpc::fetch_indices(&http).await {
                Ok(indices) => feed.publish(&cache, &data, indices),
                Err(e) => {
                    // A no-op once a value is already present — from the
                    // startup warm start, or a previous successful fetch.
                    feed.recover(&cache, &data);
                    feed.fail(&data, short(&e));
                }
            }
        }
    });
}

fn aurora_task(http: reqwest::Client, data: Arc<RwLock<AppData>>, notify: Arc<Notify>) {
    tokio::spawn(async move {
        let feed = Feed::<AuroraGrid>::aurora();
        let mut ticker = tokio::time::interval(AURORA_INTERVAL);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = notify.notified() => {}
            }
            match api::swpc::fetch_aurora(&http).await {
                Ok(grid) => feed.live(&data, grid),
                Err(e) => feed.fail(&data, short(&e)),
            }
        }
    });
}

fn launches_task(
    http: reqwest::Client,
    cache: Cache,
    data: Arc<RwLock<AppData>>,
    notify: Arc<Notify>,
) {
    tokio::spawn(async move {
        let feed = Feed::<Launches>::launches();
        let guard = api::launches::rate_guard();
        let mut ticker = tokio::time::interval(LAUNCHES_INTERVAL);
        // Set only when a `429` needs a shorter, targeted retry instead of
        // waiting out the normal 30-minute interval.
        let mut retry_at: Option<Instant> = None;
        loop {
            let retry_sleep = async {
                match retry_at {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = ticker.tick() => {}
                _ = retry_sleep => {}
                _ = notify.notified() => {}
            }
            retry_at = None;

            match api::launches::fetch(&http, &guard).await {
                Ok(Fetched::Ok(launches)) => feed.publish(&cache, &data, launches),
                Ok(Fetched::Throttled(wait)) => {
                    retry_at = Some(Instant::now() + wait.max(Duration::from_secs(60)));
                    feed.recover(&cache, &data);
                    feed.fail(&data, "throttled — retrying".to_string());
                }
                Ok(Fetched::BudgetSpent) => {
                    feed.fail(&data, "rate budget spent".to_string());
                }
                Err(e) => {
                    feed.recover(&cache, &data);
                    feed.fail(&data, short(&e));
                }
            }
        }
    });
}

// --- shared task helpers -----------------------------------------------------

fn short(e: &anyhow::Error) -> String {
    let s = e.to_string();
    s.chars().take(48).collect()
}

/// Load whatever each source last cached, for the currently tracked
/// satellite. Used both to warm-start an online run and, on `--offline`, as
/// the only source of data.
fn load_all_from_cache(cache: &Cache, data: &Arc<RwLock<AppData>>, sat: u64) {
    Feed::<Tracker>::tle(sat).recover(cache, data);
    Feed::<Indices>::weather().recover(cache, data);
    Feed::<Launches>::launches().recover(cache, data);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every panel's header shows `Panel::key()`; `1`–`6` must focus exactly
    /// the panel whose header carries that digit, or the two silently drift
    /// apart the next time a panel is added or reordered.
    #[test]
    fn panel_key_and_from_key_round_trip_for_every_panel() {
        for panel in Panel::ALL {
            assert_eq!(Panel::from_key(panel.key()), Some(panel));
        }
    }

    #[test]
    fn panel_keys_are_1_through_6_with_no_gaps_or_repeats() {
        let mut keys: Vec<u8> = Panel::ALL.map(|p| p.key()).to_vec();
        keys.sort();
        assert_eq!(keys, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn from_key_rejects_digits_outside_1_to_6() {
        assert_eq!(Panel::from_key(0), None);
        assert_eq!(Panel::from_key(7), None);
    }

    fn test_app(config: Config) -> App {
        let (sat_tx, _sat_rx) = watch::channel(config.sat);
        let (search_tx, _search_rx) = watch::channel(String::new());
        App {
            config,
            data: Arc::new(RwLock::new(AppData::default())),
            focus: Panel::Tracked,
            map_fullscreen: false,
            follow: false,
            zoom: 0,
            places: false,
            show_help: false,
            help_scroll: 0,
            should_quit: false,
            started: Instant::now(),
            clock: SimClock::new(),
            sat_input: None,
            time_input: None,
            passes: Vec::new(),
            passes_at: None,
            passes_from: None,
            sat_tx,
            search_tx,
            notifiers: Notifiers::new(),
            list_pos: 0,
        }
    }

    #[test]
    fn remove_tracked_refuses_the_satellite_currently_being_tracked() {
        let mut config = Config::default();
        config.sat = 25544;
        config.track(25544, "ISS (ZARYA)");
        config.track(20580, "HST");
        // TRACKED lists most-recently-tracked first, so HST (list_pos 0) is
        // removable but the active ISS (list_pos 1) is not.
        let mut app = test_app(config);
        app.list_pos = 1;
        app.remove_tracked();
        assert_eq!(app.config.tracked.len(), 2, "the active satellite must not be removed");
    }

    #[test]
    fn remove_tracked_drops_an_inactive_entry() {
        let mut config = Config::default();
        config.sat = 25544;
        config.track(25544, "ISS (ZARYA)");
        config.track(20580, "HST");
        let mut app = test_app(config);
        app.list_pos = 0;
        app.remove_tracked();
        assert_eq!(app.config.tracked.len(), 1);
        assert_eq!(app.config.tracked[0].norad_id, 25544);
    }

    #[test]
    fn switch_satellite_records_the_entry_but_is_a_no_op_on_the_data_when_unchanged() {
        let mut config = Config::default();
        config.sat = 25544;
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        app.switch_satellite(25544, Some("ISS (ZARYA)".to_string()));
        assert_eq!(app.config.sat, 25544);
        assert_eq!(app.config.tracked[0].name, "ISS (ZARYA)");
        // Reselecting the satellite already tracked must not wipe its
        // already-live element set.
        assert!(app.data.read().unwrap().tle.get().is_some());
    }

    #[test]
    fn switch_satellite_to_a_new_id_resets_the_element_set_and_passes() {
        let config = Config::default(); // sat: DEFAULT_SAT (25544)
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        let now = Utc::now();
        app.passes.push(Pass {
            aos: now,
            los: now,
            peak: now,
            peak_elevation_deg: 0.0,
            aos_azimuth_deg: 0.0,
            los_azimuth_deg: 0.0,
            visible: false,
        });
        app.switch_satellite(20580, Some("HST".to_string()));
        assert_eq!(app.config.sat, 20580);
        assert!(app.data.read().unwrap().tle.get().is_none(), "the old tracker must be cleared");
        assert!(app.passes.is_empty());
        assert_eq!(app.config.tracked[0].norad_id, 20580);
    }

    #[test]
    fn sync_tracked_name_adopts_the_real_name_once_the_element_set_arrives() {
        let mut config = Config::default();
        config.sat = 25544;
        config.track(25544, "NORAD 25544");
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        app.sync_tracked_name();
        assert_eq!(app.config.tracked_name(25544), Some("ISS (ZARYA)"));
    }

    #[test]
    fn submit_sat_input_tracks_the_highlighted_result_for_a_matching_query() {
        let config = Config::default();
        let mut app = test_app(config);
        app.sat_input = Some(SatPicker { query: "hst".to_string(), selected: 0 });
        if let Ok(mut d) = app.data.write() {
            d.search = SearchState::Done {
                query: "hst".to_string(),
                results: vec![SatMatch { norad_id: 20580, name: "HST".to_string() }],
            };
        }
        app.submit_sat_input();
        assert_eq!(app.config.sat, 20580);
        assert!(app.sat_input.is_none(), "the popup should close once a result is picked");
    }

    #[test]
    fn submit_sat_input_submits_a_fresh_search_when_no_result_is_showing_yet() {
        let config = Config::default();
        let mut app = test_app(config);
        app.sat_input = Some(SatPicker { query: "hubble".to_string(), selected: 0 });
        app.submit_sat_input();
        // Nothing to select yet, so the popup must stay open for the result.
        assert!(app.sat_input.is_some());
    }

    fn press(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    fn press_code(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn zooming_in_from_the_whole_world_returns_to_the_level_it_left() {
        let mut app = test_app(Config::default());
        // Follow, climb to ×8 (zoom index 2)...
        press(&mut app, 'f');
        press(&mut app, '+');
        press(&mut app, '+');
        assert!(app.follow && app.zoom == 2);
        // ...toggle follow off with `f` — the level is only parked, not reset...
        press(&mut app, 'f');
        assert!(!app.follow && app.zoom == 2);
        // ...and `+` from the whole world resumes exactly where it left off.
        press(&mut app, '+');
        assert!(app.follow && app.zoom == 2);
    }

    #[test]
    fn zooming_out_at_the_widest_level_returns_to_the_whole_world() {
        let mut app = test_app(Config::default());
        press(&mut app, 'f'); // follow at the widest level, zoom index 0
        assert!(app.follow && app.zoom == 0);
        press(&mut app, '-'); // one step wider than the widest follow level
        assert!(!app.follow, "stepping out past the widest level drops follow");
        assert_eq!(app.zoom, 0);
    }

    #[test]
    fn zooming_in_saturates_at_the_tightest_level() {
        let mut app = test_app(Config::default());
        press(&mut app, 'f');
        for _ in 0..10 {
            press(&mut app, '+');
        }
        assert!(app.follow);
        assert_eq!(app.zoom, crate::ui::MAX_ZOOM);
    }

    #[test]
    fn p_toggles_place_labels_and_leaves_follow_and_zoom_alone() {
        let mut app = test_app(Config::default());
        assert!(!app.places, "the place layer is off until asked for");
        press(&mut app, 'f');
        press(&mut app, '+');
        let (follow, zoom) = (app.follow, app.zoom);
        press(&mut app, 'p');
        assert!(app.places);
        assert_eq!((app.follow, app.zoom), (follow, zoom), "`p` is orthogonal to the map view");
        press(&mut app, 'p');
        assert!(!app.places);
    }

    #[test]
    fn the_sky_plot_follows_the_highlighted_pass_only_while_next_passes_is_focused() {
        let mut app = test_app(Config::default());
        let now = Utc::now();
        app.passes =
            vec![dummy_pass(now, false), dummy_pass(now + chrono::Duration::hours(2), true)];

        // Any other focus: no highlight is showing, so nothing to plot.
        app.focus = Panel::Map;
        assert!(crate::ui::selected_pass(&app).is_none());

        // NEXT PASSES focused, second row highlighted: that pass, with its
        // index and the list length.
        app.focus = Panel::Passes;
        app.list_pos = 1;
        let (i, total, pass) = crate::ui::selected_pass(&app).expect("a pass while focused");
        assert_eq!((i, total), (1, 2));
        assert!(pass.visible);

        // A stale list_pos past the end clamps to the last row, never panics.
        app.list_pos = 99;
        assert_eq!(crate::ui::selected_pass(&app).unwrap().0, 1);

        // No passes: nothing to plot, even focused.
        app.passes.clear();
        assert!(crate::ui::selected_pass(&app).is_none());
    }

    // --- time scrubbing ---------------------------------------------------

    fn dummy_pass(aos: DateTime<Utc>, visible: bool) -> Pass {
        Pass {
            aos,
            los: aos + chrono::Duration::minutes(6),
            peak: aos + chrono::Duration::minutes(3),
            peak_elevation_deg: 30.0,
            aos_azimuth_deg: 200.0,
            los_azimuth_deg: 20.0,
            visible,
        }
    }

    /// A jump target compared against `sim_now`, allowing a couple of seconds
    /// for the wall clock to tick between `goto` and the read.
    fn about(a: DateTime<Utc>, b: DateTime<Utc>) -> bool {
        (a - b).abs() < chrono::Duration::seconds(2)
    }

    #[test]
    fn space_toggles_the_pause_state() {
        let mut app = test_app(Config::default());
        assert!(app.clock.is_live());
        press(&mut app, ' ');
        assert_eq!(app.clock.state(), ClockState::Paused);
        press(&mut app, ' ');
        assert!(app.clock.is_live());
    }

    #[test]
    fn comma_and_period_walk_the_rate_through_one_times_into_reverse() {
        let mut app = test_app(Config::default());
        press(&mut app, '.');
        press(&mut app, '.');
        assert_eq!(app.clock.state(), ClockState::Warp(5));
        press(&mut app, ',');
        press(&mut app, ',');
        assert!(app.clock.is_live(), "stepping back down lands exactly on live");
        press(&mut app, ',');
        assert_eq!(app.clock.state(), ClockState::Warp(-1), "one more crosses into reverse");
    }

    #[test]
    fn arrows_and_brackets_step_the_clock_and_leave_the_rate_alone() {
        let mut app = test_app(Config::default());
        let before = app.sim_now();
        press_code(&mut app, KeyCode::Right);
        press_code(&mut app, KeyCode::Right);
        press(&mut app, ']');
        let moved = app.sim_now() - before;
        let want = chrono::Duration::hours(1) + chrono::Duration::minutes(2);
        assert!((moved - want).abs() < chrono::Duration::seconds(2), "stepped {moved}");
        assert_eq!(app.clock.state(), ClockState::Drifted, "a step does not touch the rate");
    }

    #[test]
    fn snapping_back_to_now_clears_both_the_offset_and_the_rate() {
        let mut app = test_app(Config::default());
        press(&mut app, '.');
        press_code(&mut app, KeyCode::Right);
        press(&mut app, ' ');
        assert!(!app.clock.is_live());
        app.passes_at = Some(Instant::now());
        app.passes_from = Some(Utc::now());
        press(&mut app, '0');
        assert!(app.clock.is_live());
        assert!(app.passes_at.is_none() && app.passes_from.is_none(), "the pass cache is dropped");
    }

    #[test]
    fn scrub_keys_do_nothing_while_the_satellite_picker_is_open() {
        let mut app = test_app(Config::default());
        app.sat_input = Some(SatPicker::default());
        press(&mut app, ' ');
        press(&mut app, '.');
        press_code(&mut app, KeyCode::Right);
        assert!(app.clock.is_live(), "the picker swallows the keys");
        assert_eq!(app.sat_input.as_ref().unwrap().query, " .", "they went to the query instead");
    }

    #[test]
    fn g_opens_the_prompt_and_a_valid_time_jumps_the_clock() {
        let mut app = test_app(Config::default());
        press(&mut app, 'g');
        assert!(app.time_input.is_some());
        for c in "+3d".chars() {
            press(&mut app, c);
        }
        press_code(&mut app, KeyCode::Enter);
        assert!(app.time_input.is_none(), "a good parse closes the prompt");
        assert!(about(app.sim_now(), Utc::now() + chrono::Duration::days(3)));
    }

    #[test]
    fn the_prompt_keeps_a_bad_time_on_screen_with_an_error() {
        let mut app = test_app(Config::default());
        press(&mut app, 'g');
        for c in "banana".chars() {
            press(&mut app, c);
        }
        press_code(&mut app, KeyCode::Enter);
        let input = app.time_input.as_ref().expect("the prompt stays open on a bad parse");
        assert!(input.error.is_some());
        assert!(app.clock.is_live(), "and the clock has not moved");
    }

    #[test]
    fn pressing_n_twice_advances_past_the_pass_it_just_jumped_to() {
        let mut app = test_app(Config::default());
        let first = Utc::now() + chrono::Duration::hours(2);
        let second = first + chrono::Duration::hours(2);
        app.passes = vec![dummy_pass(first, false), dummy_pass(second, false)];
        let lead = chrono::Duration::seconds(30);

        press(&mut app, 'n');
        assert!(about(app.sim_now(), first - lead));

        press(&mut app, 'n');
        assert!(about(app.sim_now(), second - lead), "a second press skips the pass just landed on");
    }

    #[test]
    fn capital_n_jumps_only_to_naked_eye_visible_passes() {
        let mut app = test_app(Config::default());
        let dim = Utc::now() + chrono::Duration::hours(1);
        let bright = dim + chrono::Duration::hours(3);
        app.passes = vec![dummy_pass(dim, false), dummy_pass(bright, true)];
        press(&mut app, 'N');
        assert!(about(app.sim_now(), bright - chrono::Duration::seconds(30)));
    }

    #[test]
    fn a_next_pass_jump_with_no_passes_is_a_harmless_no_op() {
        let mut app = test_app(Config::default());
        press(&mut app, 'n');
        assert!(app.clock.is_live());
    }

    #[test]
    fn refresh_passes_recomputes_after_a_time_jump() {
        let mut config = Config::default();
        config.set_location(48.0, 11.0, None);
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        app.refresh_passes();
        let first = app.passes_from.expect("the window is built once");

        app.clock.jump(chrono::Duration::hours(12));
        app.refresh_passes();
        let second = app.passes_from.expect("and rebuilt after the jump");

        assert!(second - first > chrono::Duration::hours(11), "the window moved with the clock");
    }

    #[test]
    fn frame_interval_only_speeds_up_for_a_warp() {
        // The dashboard is a 4 fps clock face unless the clock is winding
        // itself forward — only a warp does that. A drift or a pause is as
        // static on screen as being live.
        assert_eq!(frame_interval(ClockState::Live), FRAME_LIVE);
        assert_eq!(frame_interval(ClockState::Drifted), FRAME_LIVE);
        assert_eq!(frame_interval(ClockState::Paused), FRAME_LIVE);
        assert_eq!(frame_interval(ClockState::Warp(2)), FRAME_WARP);
        assert_eq!(frame_interval(ClockState::Warp(-1800)), FRAME_WARP);
    }

    #[test]
    fn a_warp_does_not_rebuild_the_pass_list_on_every_frame() {
        let mut config = Config::default();
        config.set_location(48.0, 11.0, None);
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        app.refresh_passes();
        let built_for = app.passes_from.expect("the window is built once");

        // Wind the rate up, then scrub an hour ahead — well past the 5-minute
        // simulated window that forces a rebuild at 1×. Back-to-back frames of
        // a fast warp cross that window every few milliseconds of wall time,
        // and `PASS_REBUILD_FLOOR` is what keeps a 96-hour scan out of each one.
        press(&mut app, '.');
        assert!(matches!(app.clock.state(), ClockState::Warp(_)));
        app.clock.jump(chrono::Duration::hours(1));
        app.refresh_passes();

        assert_eq!(
            app.passes_from,
            Some(built_for),
            "a rebuild inside the wall-clock floor is skipped while warping",
        );
    }

    #[test]
    fn an_explicit_jump_still_rebuilds_at_once_despite_the_warp_floor() {
        let mut config = Config::default();
        config.set_location(48.0, 11.0, None);
        let mut app = test_app(config);
        if let Ok(mut d) = app.data.write() {
            d.tle.set_live(crate::orbit::test_tracker());
        }
        app.refresh_passes();
        let built_for = app.passes_from.expect("the window is built once");

        // A 1× step leaves the clock `Drifted`, not `Warp`, so the floor does
        // not apply — the list rebuilds immediately even though no real time
        // has passed since the last one.
        app.clock.jump(chrono::Duration::hours(1));
        assert_eq!(app.clock.state(), ClockState::Drifted);
        app.refresh_passes();

        assert!(
            app.passes_from.expect("rebuilt") - built_for > chrono::Duration::minutes(50),
            "the window moved with the clock",
        );
    }
}
