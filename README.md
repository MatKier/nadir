# nadir

A terminal application that tracks a satellite overhead in real time, with a
world map, a telemetry readout, upcoming passes over your location, and some
space-weather and launch numbers on the side. It's a novelty project — but the
orbital mechanics under it are the real thing: it runs SGP4/SDP4 locally and
only ever fetches an orbit description, never a position.

*Nadir* is the point on Earth directly beneath a satellite, which is what the
centerpiece map tracks.

![nadir tracking the ISS from a Munich ground station: world map with ground track, footprint and station marker, the tracked-satellite list, live telemetry, upcoming Munich passes, space weather and launch panels](docs/screenshot.png)

> **Note:** this project is entirely AI-generated. No line of it was written or
> reviewed by hand. Treat it accordingly.

## How it works

Orbital state is computed locally rather than downloaded. Celestrak supplies a
GP/TLE element set — a compact orbit description valid for days, not a position
— and `src/orbit/propagate.rs` runs SGP4/SDP4 on it at ~4 Hz to get position and
velocity at any instant. Everything derived from that is math with no I/O:
ground track, footprint, sunlit/eclipsed state, look angles, the day/night
terminator and pass predictions.

Only the element set needs the network, and by default at most every 12 hours
(configurable — see [Refresh intervals](#refresh-intervals)). It is cached at
`~/.cache/nadir/tle-<norad>.json` and keyed by cache age rather than process
lifetime, so a restart with a cache younger than that interval reads from disk.
A refresh that fails retries on a 1m-to-30m exponential backoff rather than
waiting the full interval, and says so in the activity log.

### Accuracy

A propagated position is an estimate, and it gets worse the further the
displayed time is from the element set's epoch — the more so once you start
scrubbing the clock days out. The telemetry panel's `ACC` row puts a number on
it. A TLE carries no formal error, so this is a model, built from what the
element set does reveal about its own quality: nadir re-propagates with the drag
term `B*` nudged by 10% and measures how far the position moves, then floors
that against a per-regime growth rate. The result is satellite-specific — a
decaying LEO degrades several times faster than a quiet orbit well above the
drag — and it lands near the long-standing rule of thumb of ~1 km at epoch
growing to tens of km after a week in low orbit. The along-track part is a
timing error, so it also shows as `±N s`, and as a footer on the pass list
bounding how far the AOS/LOS times could slip. Past 30 days from epoch the model
stops quoting a figure: SGP4 still returns a position, but nothing local can say
how wrong it is.

The remaining panels do need the network: space weather, the aurora nowcast, and
the launch manifest. Each source fails independently — a dead feed degrades its
own panel to the last known value, labelled with its age, and leaves the rest of
the dashboard alone. Every panel warm-starts from the disk cache at launch, so
nothing sits on a placeholder waiting for the session's first fetch.

`--offline` makes no network requests at all and loads whatever each source last
cached. The map keeps tracking from the last cached TLE. What needs the network
and so is unavailable offline: the catalogue name search, the SatNOGS downlink
lookup, `--location`, and a `--sat` name (both of the latter are ignored with a
warning). A NORAD id still switches satellites, since nothing has to be looked
up first, and an already-cached downlink still drives `DOPP` — the Doppler
shift itself is local math.

## Installing

Requires Rust 1.88 or newer, declared as `rust-version` in `Cargo.toml`. The
floor comes from the dependencies, not from nadir's own code: ratatui 0.30 and
the IDNA stack under reqwest both need 1.88, and `time` (pulled in by ratatui)
uses let-chains, which do not compile at all on 1.87.

```sh
git clone https://github.com/MatKier/nadir
cd nadir
cargo build --release
```

The binary is `target/release/nadir`, self-contained aside from network access.

## Running

```sh
cargo run --release -- --location "Munich"
# or
cargo run --release -- --lat 48.137 --lon 11.575
```

A first run with no location guesses one from your IP address and saves it to
`~/.config/nadir/config.toml`, which you can edit at any time. Needs an 80×24
terminal or larger.

```
--location <place>    ground station by name, e.g. "Munich" or "Springfield, Illinois"
--lat, --lon, --alt   ground station by coordinates (alt in metres); not with --location
--sat <name-or-id>    satellite to track — a NORAD id, or a catalogue name to search for
--config <path>       use a specific config file
--offline             make no network requests; use cached data only
--no-geoip            never guess location from IP, even with none configured
```

`--location` is resolved once at startup via Open-Meteo's geocoding API and the
result saved like a geolocated or hand-typed one. An ambiguous name takes the
most prominent match; add a country or state to disambiguate.

### Refresh intervals

How often each network-backed feed refetches is set under `[intervals]` in
`config.toml`, as duration strings (`s` / `m` / `h` / `d`, same vocabulary as
the in-app time prompt). The defaults, written on a first run, are:

```toml
[intervals]
tle = "12h"     # element set — propagation is local, so this is rarely needed
weather = "5m"  # NOAA SWPC space-weather indices
aurora = "15m"  # OVATION aurora nowcast
launches = "30m" # Launch Library upcoming-launch manifest
```

Longer is always allowed. Shorter is clamped to a per-feed floor that keeps
nadir a well-behaved client of each upstream — `tle` ≥ 1h, `weather` ≥ 1m,
`aurora` ≥ 5m, `launches` ≥ 10m (Launch Library's anonymous tier allows only
~15 requests an hour). A value below its floor is raised and noted in the
activity log; the file is left as you wrote it. A value that doesn't parse
(`"banana"`, `"12"`, `"-5m"`) is a startup error, not a silent fallback.

The status chips and the `?` overlay judge freshness against whatever these are
set to, so a feed refetching on its configured schedule always reads green.

### Tracking other objects

nadir tracks the ISS by default. Any object in Celestrak's catalogue can be
tracked, by name or NORAD id:

- **At startup:** `--sat HST`, or a NORAD catalogue number such as
  `--sat 48274`. A name is resolved against Celestrak at startup and the
  top-ranked match is used; it has to be the object's official catalogue name
  (as listed at [celestrak.org/satcat/search.php](https://celestrak.org/satcat/search.php)),
  not a nickname.
- **In-app:** press `s`, type a name or NORAD id, press `Enter`. A name search
  lists every match — pick one with `↑`/`↓` and `Enter`. A NORAD id tracks the
  object directly.

Satellites you have tracked stay in the **TRACKED** panel (`2`), most recently
tracked first, so returning to one does not mean searching again: highlight it
and press `Enter`. `d` drops an entry; the satellite currently being tracked
cannot be dropped.

When a satellite is first tracked, nadir does a one-shot lookup on
[SatNOGS DB](https://db.satnogs.org) for its downlink frequencies, caching the
result under `~/.cache/nadir/transmitters-<norad>.json`. Press `T` to choose
one from what it found (or to look again); the choice drives the `DOPP`
telemetry row and is recorded in `config.toml`. The list itself is never
queried again — edit that cache file by hand when SatNOGS is wrong, out of
date, or has no entry for the object.

SGP4/SDP4 (via the `sgp4` crate, which implements both) covers all of Earth
orbit, including geostationary. It does not cover deep space — an object at a
Lagrange point such as JWST, or beyond Earth orbit, needs a different propagator
and will not work here.

## Keys

| Key | Action |
|---|---|
| `1`–`6` | focus map / tracked / telemetry / passes / weather / launches — each panel's header shows its number |
| `Tab` | cycle focus |
| `j` / `k` | scroll the focused panel's list (tracked, passes, launches) |
| `Enter` | on Tracked: start tracking the highlighted satellite |
| `d` / `Del` | on Tracked: drop the highlighted satellite from the list |
| `m` | toggle fullscreen map |
| `f` | follow the satellite (zoom the map on it) |
| `+` / `-` | zoom the follow window in / out (×2–×16); `-` past the widest drops back to the whole world |
| `p` | toggle labelled cities and ground stations on the map |
| `a` | toggle the aurora oval on the map |
| `o` | toggle the Sun/Moon markers on the map |
| `s` | search Celestrak's catalogue for another object, by name or NORAD id |
| `r` | refresh the focused panel's feed(s) now |
| `t` | step to the next configured downlink frequency (drives the `DOPP` row) |
| `T` | pick a downlink for the tracked satellite, or look one up on SatNOGS DB |
| `Space` | pause / resume the simulated clock |
| `,` / `.` | step the clock speed down / up (1× … 1800×), past 1× into reverse (`<` / `>` too) |
| `←` / `→` | step the clock ±1 minute (`h` / `l` too); `[` / `]` step ±1 hour |
| `n` / `N` | jump to 30 s before the next pass / next naked-eye pass, paused there |
| `g` | go to a time — `2026-09-08 04:30`, `04:30`, or an offset like `+90m` |
| `0` | snap the clock back to now, running at 1× |
| `?` | help — a scrollable in-app reference to every field, symbol and status chip |
| `q` / `Esc` / `Ctrl+C` | quit — `Ctrl+C` works from anywhere, including inside a prompt |

Focus affects behaviour as well as appearance. `j`/`k` scroll and highlight a row
in whichever panel has focus (Tracked, Passes and Launches have scrollable
lists), and `r` refetches only what the focused panel shows — focusing Weather
and pressing `r` forces a fresh Kp/aurora pull without spending one of the launch
feed's limited requests. The bottom bar follows focus too: it shows the focused
panel's own keys — the map keys under Map, `t`/`T` under Telemetry — rather than
one crowded list, but every key stays bound from every panel regardless of what
the bar is showing. Highlighting a launch expands its row to a second line
with provider and pad, and marks the pad on the map with `◉` plus the provider,
vehicle name, site and coordinates, for as long as the row stays highlighted.
Highlighting a pass, likewise, turns the map pane into a sky plot of it — a
polar azimuth/elevation chart, zenith at the centre and horizon at the rim,
with the star field, the Sun and the Moon placed as they'll actually be at
that pass's culmination, behind an arc bright while the satellite is sunlit
and dim while it is in the Earth's shadow, and the satellite marked on it
while the pass is under way — for as long as Passes keeps focus.

The clock keys detach the display from wall time so you can watch a pass play
out, step to where the satellite will be, or run the ground track backwards.
Everything geometric follows the simulated clock — the map, telemetry, the pass
list, and the `TLE` and `ACC` fields, which is the point: scrub a week out and
`ACC` climbs into the tens of km so the confident-looking map can't mislead you.
What stays on the real clock: the feed-status chips and
their ages, session uptime, and the launch countdown — those track real events,
not the view. The title-bar clock turns amber with a marker (`▸` drifted, `‖`
paused, `▸▸60x` / `◂◂5x` warp) whenever it is not live, so the display can never
quietly claim to be current when it isn't — and, when the bar is wide enough,
the offset from now beside it (`Δ+2h14m`, `Δ-45m`, `Δ+3d`).

While the `?` overlay is open, `j`/`k`/`PgUp`/`PgDn`/`Home`/`End` scroll it
instead, and `?` or `Esc` closes it.

## Reading the dashboard

The full glossary — map symbols, every telemetry field and space-weather term,
and what each status chip means — is available in-app by pressing `?`. The short
version:

**Tracked** — satellites you have tracked, most recently tracked first; `●`
marks the current one.

**Telemetry** — `ALT` altitude above the WGS-84 ellipsoid · `SPD` inertial speed
from SGP4, not ground-relative · `POS` sub-satellite lat/lon · `FOOT` radius of
the ground circle that can see the satellite above 0° elevation · `ORB` orbital
regime (LEO/MEO/GEO/GSO/HEO/HIGH) from altitude and eccentricity, plus a `-P`
(polar) or `-S` (sun-synchronous) suffix from inclination when it applies, and
period and inclination from the element set · `APSIS` perigee × apogee altitude
and eccentricity, measured from the WGS-84 equatorial radius rather than
`ALT`'s local ellipsoid, so the two can read up to ~20 km apart away from the
equator · `REV` approximate
revolution number since launch · `SUN` sunlit/eclipsed and time to the next
transition · `RANGE` slant range and elevation from your ground station · `RATE`
range rate ṙ in km/s — negative while the satellite closes, positive while it
opens, zero at closest approach; the number that drives a rotator or an SDR
correction loop · `DOPP` the configured downlink and the Doppler shift ṙ implies
for it (`Δf = −f₀·ṙ/c`), shown once a frequency is on file — looked up once from
SatNOGS DB when a satellite is first tracked (`T` to choose, `t` to step between
them) and cached thereafter · `TLE`
time from the element-set epoch and the epoch itself (UTC), e.g.
`18h since epoch 2026-09-06 23:11Z` — reads *before* instead of *since* when the
clock is scrubbed ahead of the epoch · `ACC` a
modelled position error and the along-track timing error it implies — see
[Accuracy](#accuracy). `RANGE`, `DOPP` and `TLE` spell themselves out in full on
a wide terminal and shorten a rung at a time as the right column narrows — `TLE`
down to `18h since 09-06 23:11Z`, then to a bare `18h old`. The title bar
likewise shows the satellite's COSPAR international designator next to its NORAD
id when the terminal is wide enough to fit it. Next Passes rows do the same with
their leading date — `Thu 2026-09-11` down to `Thu 09-11`, then to a bare `Thu`.

**Map** — the whole world by default; `f` zooms to a window centred on the
satellite, wrapping across the dateline to keep it dead centre. `+` / `-` step
that window through four magnifications — ×2, ×4, ×8, ×16 the whole-world scale,
shown as `MAP ×N` in the panel header — and `-` past the widest drops back to
the whole world. `◆` sub-satellite point · `▲` ground station · `◉` pad of the
launch currently highlighted in Launches, with its provider, site and
coordinates · `☉` the subsolar point · the Moon marker, whose glyph is its own
phase (`○ ◔ ◑ ◕ ● ◕ ◑ ◔` new through full and back) — `o` toggles both, off
by default. `p` toggles a layer of labelled reference points — `·` cities and
`+` satellite ground stations — drawn as many as fit without overlapping, so
a whole-world map shows only a scattered few and more fill in as you zoom.
`a` toggles a green aurora oval on the night side, from the same OVATION
nowcast the `AUR` row reads — off by default, drawing nothing until that feed
has ever returned data, and suppressed while the clock is warping since the
nowcast is a real-world snapshot that doesn't track the displayed instant.
While the Passes panel has focus this pane shows a sky plot of the
highlighted pass instead — `▲`/`▼` its rise and set on the horizon rim, `◇`
its culmination, the arc bright where the satellite is sunlit and dim where
it is eclipsed, and `◆` the satellite itself whenever the clock is inside the
pass, so warping the time walks it along the arc.

**Space weather** (NOAA SWPC) — `Kp` planetary K-index, 0–9 · `WIND` solar wind
speed · `Bt`/`Bz` interplanetary field magnitude and its north–south component
(strongly negative `Bz` drives aurora) · `STORM R/S/G` NOAA's radio-blackout /
radiation-storm / geomagnetic-storm scales, 0–5 · `AUR` aurora probability
overhead at your ground station · `MOON` phase, illuminated fraction and
elevation from your ground station — local math, not a feed, so it never goes
stale; it's what decides whether a `★` naked-eye pass in Next Passes is
actually worth walking outside for.

Space Weather and Launches show their feed's age under the panel title once it
has gone stale, e.g. `space weather · 1m old`.

**Status chips** (bottom left: `TLE` `SWX` `AUR` `LCH`) — each shows `wait`
(nothing fetched yet this session), `live`, an age such as `5s` / `12m` since
the last successful fetch, or `err` (failed with nothing to fall back on).
Colour is judged against how often that feed refetches: green while at most two
refreshes could have been missed, amber up to six, red beyond. So at the default
intervals `SWX` (every 5m) is green to 10m and amber to 30m, `AUR` (15m) green to
30m and amber to 90m, `LCH` (30m) green to 1h and amber to 3h, and `TLE` (12h)
green to 24h and amber to 72h; change an interval under `[intervals]` and its
chip's ladder scales with it. A feed whose last fetch attempt failed reads at
least amber whatever its age, with the reason in the Recent activity log.

## Data sources

All keyless; no account or API token is needed.

| Source | Supplies |
|---|---|
| [Celestrak](https://celestrak.org) | TLE element sets, propagated locally with SGP4 |
| [SatNOGS DB](https://db.satnogs.org) | Downlink frequencies — one-shot lookup when a satellite is first tracked, then read from the disk cache; never polled |
| [NOAA SWPC](https://www.swpc.noaa.gov) | K-index, solar wind, storm scales, aurora nowcast |
| [Launch Library 2](https://thespacedevs.com) | Upcoming launches (rate-guarded client-side; falls back to the public test mirror, `lldev.thespacedevs.com`, when the primary host throttles) |
| ipapi.co / ip-api.com | One-time IP geolocation on first run only |
| [Open-Meteo geocoding](https://open-meteo.com) | Resolves `--location "a place name"` to coordinates |

## Testing

```sh
cargo test                    # unit tests, all hermetic — no network
cargo test -- --ignored       # + the live tests, which do hit the network
```

The hermetic suite covers the geodesy, the SGP4 pipeline and pass prediction
against a fixed element set, plus the config store, catalogue-search ranking and
panel formatting.

The live tests are the ones worth running when the orbital mechanics or a live
integration change: one propagates a freshly fetched TLE and asserts the result
agrees with an independent feed (WhereTheISS.at) to within 25 km and matches its
eclipse state — a check that the maths is correct rather than only internally
consistent. Two pin how Celestrak actually answers a catalogue search, including
the 404 it returns for a name that matches nothing. Two more pin how SatNOGS DB
answers a downlink lookup, including that a satellite with no amateur radio
comes back as an empty list, not an error.

## Architecture

```
src/
  main.rs       CLI parsing only; hands a Config to app::run
  geo.rs        WGS-84 <-> ECEF, look angles — pure math, no I/O
  orbit/        SGP4 propagation, ground tracks, solar geometry, pass prediction
  api/          One module per upstream feed, all behind a shared HTTP client
  cache.rs      Disk cache so a flaky network / --offline still has data
  source.rs     Source<T>: a value plus how fresh and trustworthy it is
  config.rs     ~/.config/nadir/config.toml, overridable by CLI flags
  app.rs        Background fetch tasks, input handling, the render loop
  ui/           ratatui: the map, the ? overlay, and panels/ for the rest
```

Everything but `main.rs` sits behind a library target, so the integration test
drives the real propagation pipeline rather than a copy of it.

One tokio task per data source, each on its own interval (`[intervals]` in the
config, with per-feed floors), writing into shared state behind a lock. The
render loop never awaits the network, so a slow or dead feed cannot stall the
frame rate.

## License

MIT. See [LICENSE](LICENSE).
