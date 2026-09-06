//! A fixed table of prominent places, drawn on the map behind the `p` key as
//! dim labelled reference points — somewhere to recognise the ground under the
//! satellite by once `f`/`+` have zoomed in past the point where the
//! continents alone say where you are.
//!
//! Purely presentational data with no behaviour, consumed only by
//! [`crate::ui::map`]. It lives under `ui/` rather than in `geo.rs` so the
//! "no I/O and no globals below `orbit/`" rule stays trivially intact —
//! `geo.rs` is pure geodetic math and carries no tables.
//!
//! Nothing is *gated* on [`Place::tier`]: the map decides how many labels fit
//! by collision, not by zoom level, so the table needs no thresholds kept in
//! step with `ZOOM_HALF_SPANS`. The tier is only a draw order, so the few
//! label slots a whole-world view has are spent on Tokyo before Bergen.

/// What a place is, which picks its glyph on the map.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum Kind {
    /// A city — a `·`.
    City,
    /// A satellite ground / tracking station — a `+`, the survey mark, which
    /// can't be taken for the user's own `▲`.
    Station,
}

/// A reference point: a name, where it is, and how prominent it is.
pub(in crate::ui) struct Place {
    pub name: &'static str,
    /// Latitude, degrees, north positive.
    pub lat: f64,
    /// Longitude, degrees, east positive.
    pub lon: f64,
    pub kind: Kind,
    /// Prominence, 0 highest. The table is kept *sorted* by this, so the map
    /// never reads the field — it just walks the slice in order. It's here to
    /// document each row's rank and to let a test assert the sort holds.
    #[allow(dead_code)]
    pub tier: u8,
}

impl Place {
    /// The map glyph for this place's [`Kind`].
    pub(in crate::ui) fn glyph(&self) -> &'static str {
        match self.kind {
            Kind::City => "·",
            Kind::Station => "+",
        }
    }
}

// Brought into scope so the big table below reads `City` / `Station`, not
// `Kind::City` on every row.
use Kind::{City, Station};

/// The table, ordered by ascending [`Place::tier`] so the map's consumer is a
/// single flat walk with no sort and no allocation. Coordinates are to two
/// decimals — about a kilometre, far finer than a map cell at any zoom.
#[rustfmt::skip]
pub(in crate::ui) const PLACES: &[Place] = &[
    // ---- tier 0: a dozen anchors spread right round the globe --------------
    Place { name: "Tokyo",        lat:  35.68, lon:  139.77, kind: City, tier: 0 },
    Place { name: "London",       lat:  51.51, lon:   -0.13, kind: City, tier: 0 },
    Place { name: "New York",     lat:  40.71, lon:  -74.01, kind: City, tier: 0 },
    Place { name: "Beijing",      lat:  39.90, lon:  116.41, kind: City, tier: 0 },
    Place { name: "Moscow",       lat:  55.76, lon:   37.62, kind: City, tier: 0 },
    Place { name: "Delhi",        lat:  28.61, lon:   77.21, kind: City, tier: 0 },
    Place { name: "São Paulo",    lat: -23.55, lon:  -46.63, kind: City, tier: 0 },
    Place { name: "Cairo",        lat:  30.04, lon:   31.24, kind: City, tier: 0 },
    Place { name: "Lagos",        lat:   6.52, lon:    3.38, kind: City, tier: 0 },
    Place { name: "Sydney",       lat: -33.87, lon:  151.21, kind: City, tier: 0 },
    Place { name: "Los Angeles",  lat:  34.05, lon: -118.24, kind: City, tier: 0 },
    Place { name: "Mexico City",  lat:  19.43, lon:  -99.13, kind: City, tier: 0 },

    // ---- tier 1: the rest of the major capitals and metros ----------------
    Place { name: "Paris",        lat:  48.86, lon:    2.35, kind: City, tier: 1 },
    Place { name: "Berlin",       lat:  52.52, lon:   13.40, kind: City, tier: 1 },
    Place { name: "Madrid",       lat:  40.42, lon:   -3.70, kind: City, tier: 1 },
    Place { name: "Rome",         lat:  41.90, lon:   12.50, kind: City, tier: 1 },
    Place { name: "Istanbul",     lat:  41.01, lon:   28.98, kind: City, tier: 1 },
    Place { name: "Kyiv",         lat:  50.45, lon:   30.52, kind: City, tier: 1 },
    Place { name: "Lisbon",       lat:  38.72, lon:   -9.14, kind: City, tier: 1 },
    Place { name: "Amsterdam",    lat:  52.37, lon:    4.90, kind: City, tier: 1 },
    Place { name: "Stockholm",    lat:  59.33, lon:   18.06, kind: City, tier: 1 },
    Place { name: "Warsaw",       lat:  52.23, lon:   21.01, kind: City, tier: 1 },
    Place { name: "Athens",       lat:  37.98, lon:   23.73, kind: City, tier: 1 },
    Place { name: "Tehran",       lat:  35.69, lon:   51.39, kind: City, tier: 1 },
    Place { name: "Riyadh",       lat:  24.71, lon:   46.68, kind: City, tier: 1 },
    Place { name: "Dubai",        lat:  25.20, lon:   55.27, kind: City, tier: 1 },
    Place { name: "Baghdad",      lat:  33.31, lon:   44.36, kind: City, tier: 1 },
    Place { name: "Karachi",      lat:  24.86, lon:   67.01, kind: City, tier: 1 },
    Place { name: "Mumbai",       lat:  19.08, lon:   72.88, kind: City, tier: 1 },
    Place { name: "Kolkata",      lat:  22.57, lon:   88.36, kind: City, tier: 1 },
    Place { name: "Dhaka",        lat:  23.81, lon:   90.41, kind: City, tier: 1 },
    Place { name: "Bangkok",      lat:  13.76, lon:  100.50, kind: City, tier: 1 },
    Place { name: "Singapore",    lat:   1.35, lon:  103.82, kind: City, tier: 1 },
    Place { name: "Jakarta",      lat:  -6.21, lon:  106.85, kind: City, tier: 1 },
    Place { name: "Manila",       lat:  14.60, lon:  120.98, kind: City, tier: 1 },
    Place { name: "Shanghai",     lat:  31.23, lon:  121.47, kind: City, tier: 1 },
    Place { name: "Hong Kong",    lat:  22.32, lon:  114.17, kind: City, tier: 1 },
    Place { name: "Seoul",        lat:  37.57, lon:  126.98, kind: City, tier: 1 },
    Place { name: "Toronto",      lat:  43.65, lon:  -79.38, kind: City, tier: 1 },
    Place { name: "Chicago",      lat:  41.88, lon:  -87.63, kind: City, tier: 1 },
    Place { name: "Houston",      lat:  29.76, lon:  -95.37, kind: City, tier: 1 },
    Place { name: "San Francisco",lat:  37.77, lon: -122.42, kind: City, tier: 1 },
    Place { name: "Vancouver",    lat:  49.28, lon: -123.12, kind: City, tier: 1 },
    Place { name: "Bogotá",       lat:   4.71, lon:  -74.07, kind: City, tier: 1 },
    Place { name: "Lima",         lat: -12.05, lon:  -77.04, kind: City, tier: 1 },
    Place { name: "Santiago",     lat: -33.45, lon:  -70.67, kind: City, tier: 1 },
    Place { name: "Buenos Aires", lat: -34.60, lon:  -58.38, kind: City, tier: 1 },
    Place { name: "Rio de Janeiro",lat: -22.91, lon: -43.17, kind: City, tier: 1 },
    Place { name: "Johannesburg", lat: -26.20, lon:   28.05, kind: City, tier: 1 },
    Place { name: "Nairobi",      lat:  -1.29, lon:   36.82, kind: City, tier: 1 },
    Place { name: "Kinshasa",     lat:  -4.32, lon:   15.31, kind: City, tier: 1 },
    Place { name: "Casablanca",   lat:  33.57, lon:   -7.59, kind: City, tier: 1 },
    Place { name: "Addis Ababa",  lat:   9.03, lon:   38.74, kind: City, tier: 1 },
    // Deep-space network: the three 70 m complexes plus ESA's DSA sites, the
    // backbone every interplanetary mission is flown through.
    Place { name: "Goldstone DSN",   lat:  35.43, lon: -116.89, kind: Station, tier: 1 },
    Place { name: "Madrid DSN",      lat:  40.43, lon:   -4.25, kind: Station, tier: 1 },
    Place { name: "Canberra DSN",    lat: -35.40, lon:  148.98, kind: Station, tier: 1 },
    Place { name: "Kourou",          lat:   5.10, lon:  -52.65, kind: Station, tier: 1 },
    Place { name: "New Norcia DSA-1",lat: -31.05, lon:  116.19, kind: Station, tier: 1 },
    Place { name: "Malargüe DSA-3",  lat: -35.78, lon:  -69.40, kind: Station, tier: 1 },
    Place { name: "Hartebeesthoek",  lat: -25.89, lon:   27.69, kind: Station, tier: 1 },
    // Polar-orbit downlink: the high-latitude sites that see every pass.
    Place { name: "Svalbard (SvalSat)",lat: 78.23, lon: 15.39, kind: Station, tier: 1 },
    Place { name: "Kiruna (Esrange)",lat:  67.88, lon:   21.07, kind: Station, tier: 1 },

    // ---- tier 2: regional cities and the wider station network ------------
    Place { name: "Vienna",       lat:  48.21, lon:   16.37, kind: City, tier: 2 },
    Place { name: "Prague",       lat:  50.08, lon:   14.44, kind: City, tier: 2 },
    Place { name: "Budapest",     lat:  47.50, lon:   19.04, kind: City, tier: 2 },
    Place { name: "Bucharest",    lat:  44.43, lon:   26.10, kind: City, tier: 2 },
    Place { name: "Copenhagen",   lat:  55.68, lon:   12.57, kind: City, tier: 2 },
    Place { name: "Oslo",         lat:  59.91, lon:   10.75, kind: City, tier: 2 },
    Place { name: "Helsinki",     lat:  60.17, lon:   24.94, kind: City, tier: 2 },
    Place { name: "Dublin",       lat:  53.35, lon:   -6.26, kind: City, tier: 2 },
    Place { name: "Brussels",     lat:  50.85, lon:    4.35, kind: City, tier: 2 },
    Place { name: "Zurich",       lat:  47.37, lon:    8.54, kind: City, tier: 2 },
    Place { name: "Minsk",        lat:  53.90, lon:   27.57, kind: City, tier: 2 },
    Place { name: "St Petersburg",lat:  59.94, lon:   30.31, kind: City, tier: 2 },
    Place { name: "Ankara",       lat:  39.93, lon:   32.86, kind: City, tier: 2 },
    Place { name: "Tel Aviv",     lat:  32.09, lon:   34.78, kind: City, tier: 2 },
    Place { name: "Amman",        lat:  31.95, lon:   35.93, kind: City, tier: 2 },
    Place { name: "Algiers",      lat:  36.75, lon:    3.04, kind: City, tier: 2 },
    Place { name: "Tunis",        lat:  36.81, lon:   10.18, kind: City, tier: 2 },
    Place { name: "Tripoli",      lat:  32.89, lon:   13.19, kind: City, tier: 2 },
    Place { name: "Khartoum",     lat:  15.50, lon:   32.56, kind: City, tier: 2 },
    Place { name: "Abuja",        lat:   9.06, lon:    7.50, kind: City, tier: 2 },
    Place { name: "Accra",        lat:   5.56, lon:   -0.20, kind: City, tier: 2 },
    Place { name: "Dakar",        lat:  14.72, lon:  -17.47, kind: City, tier: 2 },
    Place { name: "Cape Town",    lat: -33.92, lon:   18.42, kind: City, tier: 2 },
    Place { name: "Luanda",       lat:  -8.84, lon:   13.23, kind: City, tier: 2 },
    Place { name: "Dar es Salaam",lat:  -6.79, lon:   39.21, kind: City, tier: 2 },
    Place { name: "Lusaka",       lat: -15.42, lon:   28.28, kind: City, tier: 2 },
    Place { name: "Kabul",        lat:  34.53, lon:   69.17, kind: City, tier: 2 },
    Place { name: "Tashkent",     lat:  41.31, lon:   69.24, kind: City, tier: 2 },
    Place { name: "Almaty",       lat:  43.24, lon:   76.95, kind: City, tier: 2 },
    Place { name: "Lahore",       lat:  31.55, lon:   74.34, kind: City, tier: 2 },
    Place { name: "Chennai",      lat:  13.08, lon:   80.27, kind: City, tier: 2 },
    Place { name: "Bengaluru",    lat:  12.97, lon:   77.59, kind: City, tier: 2 },
    Place { name: "Hyderabad",    lat:  17.38, lon:   78.49, kind: City, tier: 2 },
    Place { name: "Colombo",      lat:   6.93, lon:   79.85, kind: City, tier: 2 },
    Place { name: "Kathmandu",    lat:  27.72, lon:   85.32, kind: City, tier: 2 },
    Place { name: "Yangon",       lat:  16.87, lon:   96.20, kind: City, tier: 2 },
    Place { name: "Hanoi",        lat:  21.03, lon:  105.85, kind: City, tier: 2 },
    Place { name: "Ho Chi Minh City",lat: 10.82, lon: 106.63, kind: City, tier: 2 },
    Place { name: "Kuala Lumpur", lat:   3.15, lon:  101.69, kind: City, tier: 2 },
    Place { name: "Taipei",       lat:  25.03, lon:  121.57, kind: City, tier: 2 },
    Place { name: "Osaka",        lat:  34.69, lon:  135.50, kind: City, tier: 2 },
    Place { name: "Chengdu",      lat:  30.66, lon:  104.07, kind: City, tier: 2 },
    Place { name: "Guangzhou",    lat:  23.13, lon:  113.26, kind: City, tier: 2 },
    Place { name: "Ürümqi",       lat:  43.83, lon:   87.62, kind: City, tier: 2 },
    Place { name: "Novosibirsk",  lat:  55.03, lon:   82.92, kind: City, tier: 2 },
    Place { name: "Yekaterinburg",lat:  56.84, lon:   60.61, kind: City, tier: 2 },
    Place { name: "Irkutsk",      lat:  52.29, lon:  104.30, kind: City, tier: 2 },
    Place { name: "Vladivostok",  lat:  43.12, lon:  131.89, kind: City, tier: 2 },
    Place { name: "Ulaanbaatar",  lat:  47.89, lon:  106.91, kind: City, tier: 2 },
    Place { name: "Auckland",     lat: -36.85, lon:  174.76, kind: City, tier: 2 },
    Place { name: "Melbourne",    lat: -37.81, lon:  144.96, kind: City, tier: 2 },
    Place { name: "Brisbane",     lat: -27.47, lon:  153.03, kind: City, tier: 2 },
    Place { name: "Perth",        lat: -31.95, lon:  115.86, kind: City, tier: 2 },
    Place { name: "Port Moresby", lat:  -9.44, lon:  147.18, kind: City, tier: 2 },
    Place { name: "Montréal",     lat:  45.50, lon:  -73.57, kind: City, tier: 2 },
    Place { name: "Washington",   lat:  38.90, lon:  -77.04, kind: City, tier: 2 },
    Place { name: "Miami",        lat:  25.76, lon:  -80.19, kind: City, tier: 2 },
    Place { name: "Denver",       lat:  39.74, lon: -104.99, kind: City, tier: 2 },
    Place { name: "Seattle",      lat:  47.61, lon: -122.33, kind: City, tier: 2 },
    Place { name: "Havana",       lat:  23.11, lon:  -82.37, kind: City, tier: 2 },
    Place { name: "Panama City",  lat:   8.98, lon:  -79.52, kind: City, tier: 2 },
    Place { name: "Caracas",      lat:  10.49, lon:  -66.88, kind: City, tier: 2 },
    Place { name: "Quito",        lat:  -0.18, lon:  -78.47, kind: City, tier: 2 },
    Place { name: "La Paz",       lat: -16.50, lon:  -68.15, kind: City, tier: 2 },
    Place { name: "Brasília",     lat: -15.79, lon:  -47.88, kind: City, tier: 2 },
    Place { name: "Manaus",       lat:  -3.12, lon:  -60.02, kind: City, tier: 2 },
    Place { name: "Montevideo",   lat: -34.90, lon:  -56.16, kind: City, tier: 2 },
    Place { name: "Ny-Ålesund",      lat:  78.93, lon:  11.87, kind: Station, tier: 2 },
    Place { name: "Tromsø (KSAT)",   lat:  69.66, lon:  18.94, kind: Station, tier: 2 },
    Place { name: "Inuvik",          lat:  68.36, lon: -133.72, kind: Station, tier: 2 },
    Place { name: "Poker Flat",      lat:  65.13, lon: -147.47, kind: Station, tier: 2 },
    Place { name: "Wallops",         lat:  37.94, lon:  -75.47, kind: Station, tier: 2 },
    Place { name: "Kaena Point",     lat:  21.57, lon: -158.27, kind: Station, tier: 2 },
    Place { name: "Redu",            lat:  50.00, lon:    5.15, kind: Station, tier: 2 },
    Place { name: "Weilheim",        lat:  47.88, lon:   11.08, kind: Station, tier: 2 },
    Place { name: "Cebreros DSA-2",  lat:  40.45, lon:   -4.37, kind: Station, tier: 2 },
    Place { name: "Usuda",           lat:  36.13, lon:  138.36, kind: Station, tier: 2 },
    Place { name: "Dongara",         lat: -29.05, lon:  115.35, kind: Station, tier: 2 },
    Place { name: "Awarua",          lat: -46.53, lon:  168.38, kind: Station, tier: 2 },
    Place { name: "Troll (Antarctica)",lat: -72.01, lon:  2.53, kind: Station, tier: 2 },
    Place { name: "McMurdo Station",  lat: -77.85, lon:  166.67, kind: Station, tier: 2 },

    // ---- tier 3: anchors where nothing else is, so no window is blank -----
    Place { name: "Reykjavík",    lat:  64.13, lon:  -21.90, kind: City, tier: 3 },
    Place { name: "Tórshavn",     lat:  62.01, lon:   -6.77, kind: City, tier: 3 },
    Place { name: "Nuuk",         lat:  64.18, lon:  -51.72, kind: City, tier: 3 },
    Place { name: "Longyearbyen", lat:  78.22, lon:   15.63, kind: City, tier: 3 },
    Place { name: "Anchorage",    lat:  61.22, lon: -149.90, kind: City, tier: 3 },
    Place { name: "Fairbanks",    lat:  64.84, lon: -147.72, kind: City, tier: 3 },
    Place { name: "Utqiaġvik",    lat:  71.29, lon: -156.79, kind: City, tier: 3 },
    Place { name: "Nome",         lat:  64.50, lon: -165.41, kind: City, tier: 3 },
    Place { name: "Yellowknife",  lat:  62.45, lon: -114.37, kind: City, tier: 3 },
    Place { name: "Iqaluit",      lat:  63.75, lon:  -68.52, kind: City, tier: 3 },
    Place { name: "Murmansk",     lat:  68.97, lon:   33.09, kind: City, tier: 3 },
    Place { name: "Norilsk",      lat:  69.35, lon:   88.20, kind: City, tier: 3 },
    Place { name: "Yakutsk",      lat:  62.03, lon:  129.73, kind: City, tier: 3 },
    Place { name: "Tiksi",        lat:  71.64, lon:  128.87, kind: City, tier: 3 },
    Place { name: "Magadan",      lat:  59.56, lon:  150.80, kind: City, tier: 3 },
    Place { name: "Petropavlovsk",lat:  53.02, lon:  158.65, kind: City, tier: 3 },
    Place { name: "Honolulu",     lat:  21.31, lon: -157.86, kind: City, tier: 3 },
    Place { name: "Midway",       lat:  28.21, lon: -177.37, kind: City, tier: 3 },
    Place { name: "Papeete",      lat: -17.54, lon: -149.57, kind: City, tier: 3 },
    Place { name: "Suva",         lat: -18.14, lon:  178.44, kind: City, tier: 3 },
    Place { name: "Nouméa",       lat: -22.28, lon:  166.46, kind: City, tier: 3 },
    Place { name: "Apia",         lat: -13.83, lon: -171.77, kind: City, tier: 3 },
    Place { name: "Galápagos",    lat:  -0.74, lon:  -90.31, kind: City, tier: 3 },
    Place { name: "Ponta Delgada",lat:  37.74, lon:  -25.67, kind: City, tier: 3 },
    Place { name: "Malé",         lat:   4.18, lon:   73.51, kind: City, tier: 3 },
    Place { name: "Diego Garcia", lat:  -7.31, lon:   72.41, kind: City, tier: 3 },
    Place { name: "Antananarivo", lat: -18.88, lon:   47.51, kind: City, tier: 3 },
    Place { name: "Saint-Denis",  lat: -20.88, lon:   55.45, kind: City, tier: 3 },
    Place { name: "Port Louis",   lat: -20.16, lon:   57.50, kind: City, tier: 3 },
    Place { name: "Punta Arenas", lat: -53.16, lon:  -70.91, kind: City, tier: 3 },
    Place { name: "Ushuaia",      lat: -54.80, lon:  -68.30, kind: City, tier: 3 },
    Place { name: "Stanley",      lat: -51.69, lon:  -57.85, kind: City, tier: 3 },
    Place { name: "Hobart",       lat: -42.88, lon:  147.33, kind: City, tier: 3 },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_sits_on_the_globe_and_has_a_unique_name() {
        let mut seen = std::collections::HashSet::new();
        for p in PLACES {
            assert!((-90.0..=90.0).contains(&p.lat), "{}: lat {}", p.name, p.lat);
            assert!((-180.0..=180.0).contains(&p.lon), "{}: lon {}", p.name, p.lon);
            assert!(seen.insert(p.name), "duplicate place {}", p.name);
        }
    }

    #[test]
    fn the_table_is_ordered_by_tier_so_a_flat_walk_is_stable() {
        // The map walks PLACES in array order and relies on that being
        // prominence order — a stray out-of-tier row would silently take a
        // scarce whole-world label slot from a more important place.
        assert!(PLACES.windows(2).all(|w| w[0].tier <= w[1].tier));
    }

    #[test]
    fn every_sparse_region_and_both_poles_have_an_anchor() {
        // A handful of coarse lat/lon boxes that a plain city list leaves
        // empty: an `f` window landing in one must still have something to
        // label. (lat_lo, lat_hi, lon_lo, lon_hi, what).
        let boxes = [
            (66.0, 90.0, -180.0, 0.0, "Arctic, western hemisphere"),
            (66.0, 90.0, 0.0, 180.0, "Arctic, eastern hemisphere"),
            (-90.0, -50.0, -180.0, 0.0, "far south, western hemisphere"),
            (-90.0, -50.0, 0.0, 180.0, "far south, eastern hemisphere"),
            (-30.0, 30.0, 150.0, 180.0, "western Pacific"),
            (-30.0, 30.0, -180.0, -140.0, "central Pacific"),
            (30.0, 66.0, -40.0, -10.0, "North Atlantic"),
            (-40.0, 0.0, 40.0, 95.0, "southern Indian Ocean"),
            (50.0, 75.0, 90.0, 160.0, "Siberia"),
        ];
        for (lat_lo, lat_hi, lon_lo, lon_hi, what) in boxes {
            assert!(
                PLACES.iter().any(|p| {
                    (lat_lo..=lat_hi).contains(&p.lat) && (lon_lo..=lon_hi).contains(&p.lon)
                }),
                "no place anchors {what}"
            );
        }
    }
}
