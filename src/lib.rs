//! nadir — a mission-control console for the terminal.
//!
//! Split into a library so integration tests (see `tests/`) can exercise the
//! real orbital-mechanics pipeline directly, instead of a reimplementation.

pub mod api;
pub mod app;
pub mod cache;
pub mod config;
pub mod geo;
pub mod orbit;
pub mod simclock;
pub mod source;
pub mod ui;
