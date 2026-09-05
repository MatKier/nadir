//! Application wiring: shared state, background fetch tasks, the input thread and
//! the render loop.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::{mpsc, watch, Notify};

use crate::api;
use crate::api::celestrak::SatMatch;
use crate::api::launches::{Fetched, Launches};
use crate::api::swpc::{AuroraGrid, Indices};
use crate::cache::Cache;
use crate::config::Config;
use crate::orbit::{predict_passes, Pass, Tracker};
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

/// Renderer-owned UI state (not shared with fetch tasks).
pub struct App {
    pub config: Config,
    pub data: Arc<RwLock<AppData>>,
    pub focus: Panel,
    pub map_fullscreen: bool,
    pub follow: bool,
    pub show_help: bool,
    /// Scroll offset within the help overlay, in lines; clamped against its
    /// content height at render time.
    pub help_scroll: u16,
    pub should_quit: bool,
    pub started: Instant,
    /// `Some` while the "track satellite" popup is open.
    pub sat_input: Option<SatPicker>,
    pub passes: Vec<Pass>,
    passes_at: Option<Instant>,
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

    /// Switch focus to `panel`, resetting list scroll — a scroll position
    /// from a different list would be meaningless here.
    fn set_focus(&mut self, panel: Panel) {
        self.focus = panel;
        self.list_pos = 0;
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

    /// Recompute pass predictions if the cache is stale or the inputs changed.
    fn refresh_passes(&mut self) {
        let due = self
            .passes_at
            .map(|t| t.elapsed() > Duration::from_secs(20))
            .unwrap_or(true);
        if !due {
            return;
        }
        let Some(station) = self.config.ground_station() else {
            self.passes.clear();
            self.passes_at = Some(Instant::now());
            return;
        };
        let tracker = self.data.read().ok().and_then(|d| d.tle.get().cloned());
        let Some(tr) = tracker else {
            // No element set yet: leave `due` unset so we retry as soon as one
            // arrives, rather than waiting out a full 20 s window for nothing.
            return;
        };
        self.passes = predict_passes(&tr, &station, Utc::now(), chrono::Duration::hours(48), 12);
        self.passes_at = Some(Instant::now());
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
            let _ = self.sat_tx.send(norad_id);
        }
        // Persist the choice; ignore write errors (e.g. read-only home).
        let _ = self.config.save();
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
        show_help: false,
        help_scroll: 0,
        should_quit: false,
        started: Instant::now(),
        sat_input: None,
        passes: Vec::new(),
        passes_at: None,
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

async fn render_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut input_rx: mpsc::UnboundedReceiver<Event>,
) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        app.refresh_passes();
        app.sync_tracked_name();
        terminal
            .draw(|frame| ui::draw(frame, app))
            .context("drawing a frame")?;

        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            _ = tick.tick() => {}
            maybe_event = input_rx.recv() => {
                match maybe_event {
                    Some(Event::Key(key)) => app.handle_key(key),
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
const TLE_TTL: Duration = Duration::from_secs(12 * 3600);

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
        let mut ticker = tokio::time::interval(Duration::from_secs(5 * 60));
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
        let mut ticker = tokio::time::interval(Duration::from_secs(15 * 60));
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
        let mut ticker = tokio::time::interval(Duration::from_secs(30 * 60));
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
            show_help: false,
            help_scroll: 0,
            should_quit: false,
            started: Instant::now(),
            sat_input: None,
            passes: Vec::new(),
            passes_at: None,
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
}
