# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo build --release
cargo run -- --offline               # debug run; --offline makes zero network requests
cargo test                           # unit tests, all hermetic
cargo test -- --ignored              # + the live tests (Celestrak / SatNOGS / WhereTheISS.at)
cargo test rank_by_name              # one test, by name substring
cargo test --lib config::            # one module's tests
```

**MSRV is 1.88**, declared as `rust-version` in `Cargo.toml` and verified by
building on both 1.88.0 (clean, tests pass) and 1.87.0 (hard failure). The floor
is set entirely by dependencies — ratatui 0.30 and its `time`/`darling` deps,
plus the `icu_*` crates reqwest pulls in through `url`/`idna`. `time` uses
let-chains, so 1.87 fails to compile even with `--ignore-rust-version`. nadir's
own code is far below that line: the newest std API it touches is
`Option::is_none_or` (1.82), and it is edition 2021 with no let-chains or async
closures. So raising MSRV is a dependency decision; don't reach for a newer std
API without checking it against the declared floor.

Running the TUI takes over the terminal and only quits on a keypress, so prefer
`cargo test` / `cargo build` for verification; use `--offline` when you do need
to launch it, so a check doesn't spend the launch feed's request budget.

There is no rustfmt config and the tree is **not** rustfmt-clean: it is
hand-wrapped near 100 columns, with single-line `let … else { return };` and
struct literals kept on one line. Don't run `cargo fmt` — it would reflow every
file. Match the surrounding formatting by hand.

## Architecture

`main.rs` parses the CLI and hands a `Config` to `app::run`; everything else
lives behind `lib.rs` so `tests/` can drive the real orbital pipeline rather
than a reimplementation.

### Local math, network only for element sets

Position is computed, not fetched. `api::celestrak` pulls a GP/TLE element set
at most once per `config::Intervals::tle` (default 12h, floor 1h) and
`orbit::propagate::Tracker` runs SGP4/SDP4 on it. Ground track, footprint, eclipse state, look angles, the
terminator and pass prediction are all pure functions over that result —
`geo.rs` and `orbit/` do no I/O whatsoever, which is what makes them testable
without a network or a terminal. Keep it that way: no I/O below `orbit/`.

### Freshness is a first-class value

Every network-backed field is a `Source<T>` (`source.rs`): a value plus a
`Health` of Pending / Live / Stale(age) / Error. The render loop never awaits a
fetch — it draws whatever is in the `Source` and lets the status chip say how
old it is. A failed refresh keeps the previous value and downgrades its
health; it never blanks the panel.

### Fetch tasks

`app::spawn_fetch_tasks` starts one tokio task per feed, each on its own
interval, all writing into a shared `Arc<RwLock<AppData>>`. A `Feed<T>` bundles
the three things every task would otherwise respell — cache key, `AppData` slot,
decoder — and a `key: None` feed (aurora) is deliberately never persisted.
`Notifiers` holds one `Notify` per feed so `r` can force just the
focused panel's feed to refetch. The TLE task is the exception: it wakes on the
`sat_tx` watch channel, which doubles as "satellite changed" and "refresh now".

`--offline` skips `spawn_fetch_tasks` entirely. `load_all_from_cache` runs
either way, so panels warm-start from disk instead of sitting on a placeholder.

Adding a feed touches six places: a `Feed<T>` constructor, a task fn, a
`Notifiers` field, the chip list in `status_bar` (`src/ui/mod.rs`), a field on
`config::Intervals` (its default, its floor, and the `Duration` threaded into
the task via `spawn_fetch_tasks` — the chip derives its colour thresholds from
that same value), and — if it is cached — `load_all_from_cache`.

### Panels

`app::Panel::ALL` is the single source of truth for focus order, the `1`–`6`
keys and the digit in each panel header; `key()` and `from_key()` both derive
from array position and tests in `app.rs` enforce the round-trip. Reordering or
adding a panel means editing `ALL` and nothing else on the key-binding side —
but panel *numbers also appear in prose* in `src/ui/help.rs` and in README's key
table; update both. The bottom-bar hint is focus-scoped in `key_hints`
(`src/ui/mod.rs`): it no longer prints the `1`–`6` digits, but a panel key added
there (like Tracked's `Enter`/`d` or Telemetry's `t`/`T`) needs a `match focus`
arm so the bar advertises it.

`ui::draw` is the sole render entry point, called every 250 ms by `render_loop`.
It takes the `RwLock` read guard once per frame and drops it before drawing the
help overlay.

### Network etiquette is load-bearing

Launch Library's anonymous tier allows roughly 15 requests an hour;
`api::launches::rate_guard()` budgets 8, and `RateGuard::try_take` refuses
rather than sleeps so the caller can serve cache instead of queueing. A `429`
falls back once to the `lldev.thespacedevs.com` mirror. The feed intervals
(`config::Intervals`, default 12h TLE / 5m weather / 15m aurora / 30m launches)
are user-configurable but only *upward*: `Intervals::clamped` enforces a floor
per feed (1h / 1m / 5m / 10m) so no config file can turn nadir into a bad
upstream client. Don't shorten the defaults, or weaken a floor, to make a change
easier to observe.

## Conventions

- Comments explain *why*, at length, wherever a decision is non-obvious — why
  Celestrak's 404-on-no-match is tolerated, why search uses a `watch` channel
  rather than `mpsc`, why `list_pos` is not reclamped after a removal. Match
  that density.
- Test names are sentences stating the invariant
  (`remove_tracked_refuses_the_satellite_currently_being_tracked`). Anything
  touching the network carries `#[ignore = "hits the live network"]`.
- Tests needing a propagator use the shared fixture `orbit::test_tracker()` /
  `orbit::ISS_GP_JSON`, not a fresh copy-pasted element set.
- Errors are `anyhow` with `.context(…)`. A fetch failure surfaces through
  `Source::set_failed` plus `AppData::note`; a background task never dies on one.
- `Config::save()` and `Cache` write to the real user directories. Unit tests
  use `Config::default()`, whose `path` is empty, and must not call `save()`.
- User-facing documentation lives in two places that have to agree: `README.md`
  and the `?` overlay in `src/ui/help.rs`.
- `Config::track` is called every frame by `App::sync_tracked_name`, and it
  lifts the existing `TrackedSat` out and puts it back rather than rebuilding
  it — any per-satellite field added to `TrackedSat` (like `active_transmitter`)
  must be carried forward there or the render loop wipes it. `Config::downlinks`
  (the SatNOGS lookup cache, keyed by NORAD id rather than nested in
  `TrackedSat`) doesn't need this care, since a re-track can't touch it.
