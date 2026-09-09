# Ideas

A backlog of things nadir could grow, with an honest note on what each one
actually costs. Nothing here is committed to; it is a menu, not a roadmap.

Ordered roughly by value-to-effort within each section.

## Bigger features

### 1. Doppler and range rate

Show the satellite's radial velocity relative to the ground station, and the
Doppler shift it implies for a configured frequency (e.g. the ISS's
145.800 MHz downlink):

```
RANGE   428 km   el 62.4°   ṙ −3.81 km/s
DOPPLER 145.800 MHz  →  145.8019 MHz  (+1.85 kHz)
```

This is the one thing real tracking software has that nadir does not, and it is
close to free. `geo::look_angles` already gives slant range; the range rate is
the ECEF relative-position unit vector dotted with the relative velocity.
`SatState` currently keeps only `speed_kms` (an inertial scalar), so this needs
the velocity vector carried through `state_at` and rotated TEME→ECEF alongside
the position, minus Earth-rotation velocity at the target. Perhaps 30 lines in
`orbit/propagate.rs` and `geo.rs`.

Touches: `orbit/propagate.rs` (`SatState` gains a velocity vector),
`geo.rs` (a `range_rate` alongside `look_angles`), `ui/panels/telemetry.rs`,
`config.rs` (a list of frequencies per satellite, or a global default),
and — for the lookup below — a small `api/satnogs.rs` and the existing picker
in `ui/mod.rs`.

Note that `ṙ` is worth showing on its own, with no frequency configured at all.
It is the quantity that drives a rotator or an SDR correction loop, and its sign
flip marks TCA. The Doppler line is only `Δf = −f₀·ṙ/c` on top of it, so the
orbital half of this idea stands alone and can land first.

The frequency itself is data nadir does not have — Celestrak publishes orbits,
not radios, and neither the GP endpoint nor SATCAT carries an RF field. Three
options:

1. A hand-edited list in `config.toml`. Trivial, but every satellite has to be
   looked up by hand somewhere else first.
2. A new SatNOGS DB feed. Per CLAUDE.md that touches six places, and adds a
   recurring fetch for something that is static for years at a time.
3. A *one-shot* SatNOGS lookup when a satellite is added to TRACKED, writing the
   chosen transmitter into `config.toml` and never querying again.

The third is the best trade, and is why this idea is cheaper than it first
looks. It is not a feed — no `Feed<T>`, no `Notifiers` field, no status chip, no
refresh-interval constant, no `load_all_from_cache` entry — so it skips the
six-place cost entirely while still sparing the user from typing frequencies by
hand. `https://db.satnogs.org/api/transmitters/?satellite__norad_cat_id=<id>` is
public and needs no auth; records carry `downlink_low`, `mode`, `baud`,
`service`, `status` and `alive`.

"The frequency" is not singular, though — the ISS alone returns a fistful of
transmitters with different modes and liveness — so the lookup has to end in a
choice rather than a `matches[0]`. That is the same shape as the satellite
picker in `ui/mod.rs`, a working list-select popup over a search result set, so
it is a matter of reusing it rather than building one — the same reuse idea 7
wants for places.

Config stays the source of truth and stays hand-editable, which is what you want
anyway when SatNOGS is wrong, out of date, or does not know the satellite.

### 2. Multi-satellite map overlay

Draw every satellite in TRACKED on the map at once — the active one bright `◆`
with its label, the rest dim `◇`. Turns the map from one object into a small
constellation view.

The most structural of these. `AppData.tle` is a single `Source<Tracker>`
and `switch_satellite` wipes it; this needs a map of NORAD id → `Source<Tracker>`
and a `tle_task` that maintains several. The saving grace is the disk cache:
`~/.cache/nadir/tle-<norad>.json` already holds an element set for every
satellite ever tracked, so `load_all_from_cache` can warm-start every marker
without a single extra request, and the 12-hour TTL applies per satellite as it
already does.

Touches: `app.rs` (`AppData`, `tle_task`, `load_all_from_cache`), `ui/map.rs`.

Watch the request budget: a long TRACKED list means several Celestrak fetches at
startup. Staggering them, or only refreshing the non-active ones lazily, keeps
this polite. Label collision on a small map is the other real problem — dim
markers probably want no labels at all below some width.

## Smaller features

### 3. Headless pass export

`nadir --passes 48h --json` (or `--ical`): compute upcoming passes, print, exit,
no TUI. Makes nadir cron-able and pipe-able, and turns pass prediction into
something you can put in a calendar.

Almost entirely `main.rs`. Everything but `main.rs` already sits behind
`lib.rs` specifically so the pipeline can be driven without a terminal, and
`predict_passes` needs only a `Tracker` and a `GeoPoint`. With a warm TLE cache
it works offline.

### 4. Sun and Moon on the map

`solar::subsolar_point` is computed for the terminator and night wash but never
drawn; marking it `☀` is a couple of lines. The Moon is more interesting and
more work: a low-precision lunar ephemeris (Meeus, ~40 lines, arc-minute
accuracy is plenty at map resolution) gives a `☾` sub-lunar marker and a phase
readout.

Phase is the useful part rather than decoration — it is what decides whether a
`★` visible pass is actually worth walking outside for. It pairs with the
existing naked-eye flag in Passes.

Touches: a new `orbit/lunar.rs` (pure math, no I/O — the rule holds),
`ui/map.rs`, possibly `ui/panels/passes.rs`.

### 5. Pass alerts

A terminal bell and a toast overlay some minutes before a visible pass rises,
with the lead time in `config.toml`. `predict_passes` already runs every 20 s
and already flags `visible`; this is a timer and a notification surface, not new
math. An OSC 9 escape gives a real desktop notification on terminals that
support it, for free.

Touches: `app.rs` (render loop), `ui/mod.rs` (the toast), `config.rs`.

### 6. Configurable minimum elevation and horizon mask

`MIN_PEAK_ELEVATION_DEG = 10.0` is a hardcoded constant in `orbit/passes.rs`.
A station in a valley, or behind a building, or a dish user who cares about 5°,
all get predictions that are quietly wrong for them. Exposing the threshold in
`config.toml` is trivial; a per-azimuth horizon mask (a list of azimuth/elevation
pairs, interpolated) is the honest version and still small.

Touches: `orbit/passes.rs` (threshold becomes a parameter), `config.rs`,
`app.rs` (`refresh_passes` passes it through).

### 7. A place picker for `--location`

Satellites get a picker; places do not. `api/geocode.rs` hardcodes `count=1`
and takes whatever Open-Meteo ranks first, and the README documents the
resulting ambiguity as a limitation ("add a country or state to disambiguate").
`--sat <name>` at startup has the same shape — it silently takes `matches[0]`
with no confirmation.

The satellite picker in `ui/mod.rs` is already a working list-select popup over
a search result set, so both cases are a matter of reusing it rather than
building one. Raise `count`, show the matches, let the user choose.

### 8. An activity log panel

`AppData::log` retains 200 entries. Exactly 8 of them are ever visible, at the
bottom of the `?` overlay, where nobody looks. Every fetch failure, throttle,
mirror fallback and satellite switch is recorded there and effectively
invisible. A scrollable panel — or at minimum showing more of it, with
timestamps — makes the failure modes the architecture is proud of actually
legible.

Adding it as a seventh `Panel::ALL` entry means the `1`–`6` key pattern widens
to `1`–`7`, plus the three prose places CLAUDE.md names.

### 9. Cache hygiene

`cache.rs` has no eviction and no size limit. Every satellite ever tracked
leaves a permanent `tle-<norad>.json` in `~/.cache/nadir/`. They are small, so
this is tidiness rather than a problem — but a startup sweep of entries older
than some multiple of `TLE_TTL`, skipping anything still in `config.tracked`,
would be about fifteen lines.

## Constraints any of these must respect

From `CLAUDE.md`, repeated here so a future session does not have to rediscover
them:

- **No I/O below `orbit/`.** That is what makes the math testable without a
  network or a terminal, and it is why the headless-export idea is cheap in the
  first place.
- **The refresh intervals are deliberate.** 12 h for TLEs, 5/15/30 min for
  weather, aurora and launches. Launch Library's anonymous tier is ~15 requests
  an hour and `api::launches::rate_guard()` budgets 8. Do not shorten an
  interval to make a change easier to observe.
- **Adding a feed touches six places:** a `Feed<T>` constructor, a task fn, a
  `Notifiers` field, the chip list in `ui::status_bar`, a refresh-interval
  constant next to `TLE_TTL`, and `load_all_from_cache` if it is cached.
- **Adding or reordering a panel** means editing `Panel::ALL` and nothing else
  on the key-binding side — but panel *numbers appear in prose* in
  `src/ui/help.rs`, in the `status_bar` hint string, and in README's key table.
  A seventh panel also needs the `Char(c @ '1'..='6')` pattern in
  `App::handle_key` widened.
- **User-facing docs live in two places that must agree:** `README.md` and the
  `?` overlay in `src/ui/help.rs`.
- **Do not run `cargo fmt`.** The tree is hand-wrapped near 100 columns and is
  deliberately not rustfmt-clean.
- **MSRV is 1.88**, set by dependencies. nadir's own newest std API is
  `Option::is_none_or` (1.82). Check before reaching for anything newer.
