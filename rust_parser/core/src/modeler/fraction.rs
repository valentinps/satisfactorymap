//! Modeler's numeric fields are *strings*, in two different notations, and
//! the sample files show which is which:
//!
//! - `Max` is a **mixed fraction**: `"84"`, `"50 2/5"`, `"30 9/20"`,
//!   `"53 1/3"`. Machine counts are whole, but rates are not -- recipe
//!   arithmetic and underclocking land on thirds, fifths and twentieths.
//! - `ClockSpeed` is a **decimal**: `"250"`, `"166.6666666"`,
//!   `"133.3333333333333"`. The varying precision is whatever the user
//!   typed, so this one is not string-stable -- only its value matters.
//!
//! Emitting the wrong notation is not a parse error in Modeler, it is a
//! silently wrong number, which is worse. Hence two formatters.

/// Largest denominator we will produce. Game rates are built from small
/// factors (clock speeds are percentages, recipe durations are whole
/// seconds), so anything needing a bigger denominator is float noise and
/// should snap to the nearest simple fraction instead.
const MAX_DENOMINATOR: u64 = 1000;

/// Best rational approximation `p/q` with `q <= MAX_DENOMINATOR`, via the
/// continued-fraction expansion. Returns exactly `(value, 1)` for integers.
fn approximate(value: f64) -> (u64, u64) {
    // Continued fractions: h/k are the convergents, each built from the
    // previous two. Stop when the denominator would exceed the cap or the
    // convergent is exact.
    let (mut h_prev, mut k_prev) = (1u64, 0u64);
    let (mut h, mut k) = (value.floor() as u64, 1u64);
    let mut x = value;
    while (h as f64 / k as f64 - value).abs() > 1e-9 {
        let frac = x - x.floor();
        if frac <= 1e-12 {
            break;
        }
        x = 1.0 / frac;
        let a = x.floor() as u64;
        let (h_next, k_next) = (a.saturating_mul(h) + h_prev, a.saturating_mul(k) + k_prev);
        if k_next > MAX_DENOMINATOR || h_next == 0 {
            break;
        }
        h_prev = h;
        k_prev = k;
        h = h_next;
        k = k_next;
    }
    (h, k.max(1))
}

/// `50.4 -> "50 2/5"`, `84.0 -> "84"`, `0.4 -> "2/5"`.
///
/// Negative and non-finite inputs clamp to `"0"` — no Modeler field is
/// meaningfully negative, and emitting `NaN` would make the file unloadable.
pub fn format(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "0".to_string();
    }
    let (numerator, denominator) = approximate(value);
    if denominator == 1 {
        return numerator.to_string();
    }
    let whole = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        return whole.to_string();
    }
    if whole == 0 {
        return format!("{remainder}/{denominator}");
    }
    format!("{whole} {remainder}/{denominator}")
}

/// `ClockSpeed`: a decimal percentage, not a fraction. `mCurrentPotential`
/// is an f32, so the raw percentage carries float noise (2.5 is exact, but
/// 5/3 arrives as 166.66666269302368); snap to the nearest simple rational
/// first, then print at most 10 decimal places with trailing zeros trimmed.
///
/// Modeler's own files are inconsistent here (`"166.6666666"` in one place,
/// `"133.3333333333333"` in another) because the string is whatever was
/// typed, so we only have to be *numerically* right, not byte-identical.
pub fn format_percent(percent: f64) -> String {
    if !percent.is_finite() || percent <= 0.0 {
        return "100".to_string();
    }
    let (numerator, denominator) = approximate(percent);
    if denominator == 1 {
        return numerator.to_string();
    }
    let text = format!("{:.10}", numerator as f64 / denominator as f64);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_string()
}

/// Parse Modeler's mixed-fraction form back to f64. Also accepts the plain
/// decimals `ClockSpeed` uses. Used by tests and the `--report` round-trip
/// check, but it keeps the two directions honest.
pub fn parse(text: &str) -> Option<f64> {
    let text = text.trim();
    let (whole, fraction) = match text.split_once(' ') {
        Some((w, f)) => (w.parse::<f64>().ok()?, Some(f)),
        None if text.contains('/') => (0.0, Some(text)),
        None => return text.parse::<f64>().ok(),
    };
    let fraction = fraction?;
    let (numerator, denominator) = fraction.split_once('/')?;
    let denominator: f64 = denominator.parse().ok()?;
    if denominator == 0.0 {
        return None;
    }
    Some(whole + numerator.parse::<f64>().ok()? / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_values_seen_in_the_sample_files() {
        // Every distinct Max/ClockSpeed string across SatisModeler/*.sfmd.
        for (value, text) in [
            (84.0, "84"),
            (50.4, "50 2/5"),
            (25.2, "25 1/5"),
            (2100.0, "2100"),
            (60.0, "60"),
            (5.0, "5"),
            (6000.0, "6000"),
            (8.4, "8 2/5"),
            (2.4, "2 2/5"),
            (14.4, "14 2/5"),
            (20.0, "20"),
            (3.0, "3"),
            (1.0, "1"),
            (250.0, "250"),
        ] {
            assert_eq!(format(value), text, "formatting {value}");
            assert_eq!(parse(text), Some(value), "parsing {text}");
        }
    }

    #[test]
    fn snaps_float_noise_to_the_exact_fraction() {
        // 8 machines x 6.3/min lands here in f64; it must not become
        // "50 179999/450000".
        assert_eq!(format(50.400000000000006), "50 2/5");
        assert_eq!(format(0.1 + 0.2), "3/10");
        // Thirds are what underclocking produces.
        assert_eq!(format(1.0 / 3.0), "1/3");
        assert_eq!(format(53.0 + 1.0 / 3.0), "53 1/3");
        assert_eq!(format(2.0 / 3.0), "2/3");
    }

    #[test]
    fn bare_fractions_have_no_leading_zero() {
        assert_eq!(format(0.4), "2/5");
        assert_eq!(parse("2/5"), Some(0.4));
    }

    #[test]
    fn clock_speeds_are_decimals_not_fractions() {
        // 250 % and 200 % are exact in the save and in Modeler.
        assert_eq!(format_percent(250.0), "250");
        assert_eq!(format_percent(200.0), "200");
        // f32 mCurrentPotential noise must not leak into the file.
        let five_thirds = (5.0f32 / 3.0) as f64 * 100.0;
        assert_eq!(format_percent(five_thirds), "166.6666666667");
        let four_thirds = (4.0f32 / 3.0) as f64 * 100.0;
        assert_eq!(format_percent(four_thirds), "133.3333333333");
        // Modeler's own files vary in precision, so compare numerically.
        for text in ["166.6666666", "133.3333333333333", "233.3333333333"] {
            let typed = parse(text).unwrap();
            let ours = parse(&format_percent(typed)).unwrap();
            assert!((ours - typed).abs() / typed < 1e-6, "{text} -> {ours}");
        }
    }

    #[test]
    fn an_absent_or_broken_clock_defaults_to_100_percent() {
        // A machine that never ran has no mCurrentPotential; 0 would stop
        // the node dead in the solver, so it must read as full speed.
        assert_eq!(format_percent(0.0), "100");
        assert_eq!(format_percent(f64::NAN), "100");
    }

    #[test]
    fn degenerate_inputs_clamp_rather_than_emit_nan() {
        assert_eq!(format(f64::NAN), "0");
        assert_eq!(format(f64::INFINITY), "0");
        assert_eq!(format(-1.0), "0");
        assert_eq!(format(0.0), "0");
    }
}
