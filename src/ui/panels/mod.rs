//! The text panels around the map: tracked satellites, telemetry, pass
//! predictions, space weather and launches.

mod fmt;
pub(in crate::ui) mod launches;
pub(in crate::ui) mod passes;
pub(in crate::ui) mod telemetry;
pub(in crate::ui) mod tracked;
pub(in crate::ui) mod weather;

pub(in crate::ui) use fmt::{
    compass, local_hm, local_hms, row_highlight, split_footer, station_label, truncate,
};
