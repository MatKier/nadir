//! A fixed table of the brightest stars, drawn on the sky plot as reference
//! points behind the pass arc — the star field you'd actually recognise
//! overhead, so a pass can be read as "just under Vega, into shadow below
//! Altair" rather than only as a bearing and an elevation.
//!
//! Modelled on [`crate::ui::places`] for the same reason that table gives:
//! purely presentational data with no behaviour, consumed only by
//! [`crate::ui::skyplot`], and kept under `ui/` rather than in `orbit/` so
//! the "no I/O and no globals below `orbit/`" rule stays trivially intact —
//! `orbit::celestial` is pure geometry and carries no tables of its own.
//!
//! Coordinates are J2000 right ascension/declination, in degrees, to two
//! decimal places — a few arc-minutes, far finer than the sky plot's disc can
//! show and well inside the precession `orbit::celestial::star_look_angles`
//! already ignores.

/// One star: a name, its J2000 position, and how bright it is (lower
/// magnitude is brighter). The table is kept *sorted* by magnitude, so
/// `ui::skyplot` never has to sort it — it just walks the slice in order and
/// spends its few name labels on Vega before it spends them on Deneb.
pub(in crate::ui) struct Star {
    pub name: &'static str,
    /// Right ascension, J2000, degrees.
    pub ra_deg: f64,
    /// Declination, J2000, degrees.
    pub dec_deg: f64,
    /// Apparent visual magnitude. Brighter is a smaller (more negative)
    /// number. The table is kept sorted by this — `ui::skyplot` never reads
    /// the field itself, only the resulting order, so it's here to document
    /// each row's brightness and let a test assert the sort holds, the same
    /// role `Place::tier` plays in `ui::places`.
    #[allow(dead_code)]
    pub mag: f64,
}

/// The 40 brightest stars visible from Earth (magnitude ~1.6 or brighter, a
/// couple of fainter-but-famous pointer stars included for recognisability —
/// Polaris and the Big Dipper's Merak/Dubhe), ordered brightest first.
///
/// Magnitudes are approximate to two decimals — several of these stars are
/// mildly variable (Betelgeuse, Antares) — kept accurate enough to rank them,
/// which is all the draw order needs.
#[rustfmt::skip]
pub(in crate::ui) const STARS: &[Star] = &[
    Star { name: "Sirius",      ra_deg: 101.29, dec_deg: -16.72, mag: -1.46 },
    Star { name: "Canopus",     ra_deg:  95.99, dec_deg: -52.70, mag: -0.74 },
    Star { name: "Rigil Kent.", ra_deg: 219.90, dec_deg: -60.83, mag: -0.27 },
    Star { name: "Arcturus",    ra_deg: 213.92, dec_deg:  19.18, mag: -0.05 },
    Star { name: "Vega",        ra_deg: 279.23, dec_deg:  38.78, mag:  0.03 },
    Star { name: "Capella",     ra_deg:  79.17, dec_deg:  46.00, mag:  0.08 },
    Star { name: "Rigel",       ra_deg:  78.63, dec_deg:  -8.20, mag:  0.13 },
    Star { name: "Procyon",     ra_deg: 114.83, dec_deg:   5.22, mag:  0.34 },
    Star { name: "Achernar",    ra_deg:  24.43, dec_deg: -57.24, mag:  0.46 },
    Star { name: "Betelgeuse",  ra_deg:  88.79, dec_deg:   7.41, mag:  0.50 },
    Star { name: "Hadar",       ra_deg: 210.96, dec_deg: -60.37, mag:  0.61 },
    Star { name: "Altair",      ra_deg: 297.70, dec_deg:   8.87, mag:  0.76 },
    Star { name: "Acrux",       ra_deg: 186.65, dec_deg: -63.10, mag:  0.77 },
    Star { name: "Aldebaran",   ra_deg:  68.98, dec_deg:  16.51, mag:  0.85 },
    Star { name: "Antares",     ra_deg: 247.35, dec_deg: -26.43, mag:  0.91 },
    Star { name: "Spica",       ra_deg: 201.30, dec_deg: -11.16, mag:  0.97 },
    Star { name: "Pollux",      ra_deg: 116.33, dec_deg:  28.03, mag:  1.14 },
    Star { name: "Fomalhaut",   ra_deg: 344.41, dec_deg: -29.62, mag:  1.16 },
    Star { name: "Deneb",       ra_deg: 310.36, dec_deg:  45.28, mag:  1.25 },
    Star { name: "Mimosa",      ra_deg: 191.93, dec_deg: -59.69, mag:  1.25 },
    Star { name: "Regulus",     ra_deg: 152.09, dec_deg:  11.97, mag:  1.35 },
    Star { name: "Adhara",      ra_deg: 104.66, dec_deg: -28.97, mag:  1.50 },
    Star { name: "Castor",      ra_deg: 113.65, dec_deg:  31.89, mag:  1.58 },
    Star { name: "Gacrux",      ra_deg: 187.79, dec_deg: -57.11, mag:  1.59 },
    Star { name: "Shaula",      ra_deg: 263.40, dec_deg: -37.10, mag:  1.62 },
    Star { name: "Bellatrix",   ra_deg:  81.28, dec_deg:   6.35, mag:  1.63 },
    Star { name: "Elnath",      ra_deg:  81.57, dec_deg:  28.61, mag:  1.65 },
    Star { name: "Miaplacidus", ra_deg: 138.30, dec_deg: -69.72, mag:  1.67 },
    Star { name: "Alnilam",     ra_deg:  84.05, dec_deg:  -1.20, mag:  1.69 },
    Star { name: "Alnitak",     ra_deg:  85.19, dec_deg:  -1.94, mag:  1.72 },
    Star { name: "Alioth",      ra_deg: 193.51, dec_deg:  55.96, mag:  1.76 },
    Star { name: "Dubhe",       ra_deg: 165.93, dec_deg:  61.75, mag:  1.79 },
    Star { name: "Mirfak",      ra_deg:  51.08, dec_deg:  49.86, mag:  1.79 },
    Star { name: "Kaus Aust.",  ra_deg: 276.04, dec_deg: -34.38, mag:  1.79 },
    Star { name: "Wezen",       ra_deg: 107.10, dec_deg: -26.39, mag:  1.83 },
    Star { name: "Avior",       ra_deg: 125.63, dec_deg: -59.51, mag:  1.86 },
    Star { name: "Alkaid",      ra_deg: 206.89, dec_deg:  49.31, mag:  1.86 },
    Star { name: "Alnair",      ra_deg: 332.06, dec_deg: -46.96, mag:  1.87 },
    Star { name: "Sargas",      ra_deg: 264.33, dec_deg: -43.00, mag:  1.87 },
    Star { name: "Menkalinan",  ra_deg:  89.88, dec_deg:  44.95, mag:  1.90 },
    Star { name: "Merak",       ra_deg: 165.46, dec_deg:  56.38, mag:  2.37 },
    // Polaris earns its place on position, not brightness (mag 1.98, which
    // would otherwise sit it between Menkalinan and Merak): it is within a
    // degree of the north celestial pole and so barely moves all night — the
    // one star nadir's own tests single out (`orbit::celestial`'s
    // Polaris-elevation check) — which makes it worth labelling on a sky
    // plot even where brightness alone wouldn't earn it a slot. Kept last so
    // it never displaces a genuinely brighter star from the few label
    // positions a busy plot has room for.
    Star { name: "Polaris",     ra_deg:  37.95, dec_deg:  89.26, mag:  1.98 },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_by_magnitude_so_a_flat_walk_favours_the_brightest() {
        // `ui::skyplot` walks `STARS` in array order and spends its few name
        // labels on whatever it reaches first — this has to be brightest
        // first, or a faint star could claim a slot a bright one needed. The
        // one exception is Polaris, kept last for its position rather than
        // its brightness (see its own comment above).
        let (without_polaris, polaris) = STARS.split_at(STARS.len() - 1);
        assert_eq!(polaris[0].name, "Polaris");
        assert!(without_polaris.windows(2).all(|w| w[0].mag <= w[1].mag));
    }

    #[test]
    fn every_star_sits_on_the_celestial_sphere_and_has_a_unique_name() {
        let mut seen = std::collections::HashSet::new();
        for s in STARS {
            assert!((0.0..360.0).contains(&s.ra_deg), "{}: ra {}", s.name, s.ra_deg);
            assert!((-90.0..=90.0).contains(&s.dec_deg), "{}: dec {}", s.name, s.dec_deg);
            assert!(seen.insert(s.name), "duplicate star {}", s.name);
        }
    }
}
