//! serde_json::Value helpers that replicate Python/orjson behavior the
//! payload parity gate depends on.

use serde_json::Value;

/// f64 -> Value with orjson's non-finite handling (NaN/Inf serialize as
/// null).
pub fn jnum(x: f64) -> Value {
    match serde_json::Number::from_f64(x) {
        Some(n) => Value::Number(n),
        None => Value::Null,
    }
}

/// Python round(x, ndigits): correctly-rounded decimal rounding with ties to
/// even. Implemented the same way CPython does it -- format to the decimal
/// string with the requested precision (Rust's formatter is also correctly
/// rounded with ties-to-even), then parse back.
pub fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", ndigits, x).parse().unwrap()
}

// ---------------------------------------------------------------------------
// math.hypot -- CPython does NOT call the platform hypot(); it implements a
// correctly-rounded vector norm (Modules/mathmodule.c vector_norm, CPython
// 3.12), which differs from the MSVC CRT hypot by 1 ulp on some inputs. This
// is a line-by-line port of the two-argument case.
// ---------------------------------------------------------------------------

/// Algorithm 1.1: compensated summation of two floats (|a| >= |b|).
fn dl_fast_sum(a: f64, b: f64) -> (f64, f64) {
    let x = a + b;
    let y = (a - x) + b;
    (x, y)
}

/// Algorithm 3.5: error-free transformation of a product. CPython uses fma()
/// (or Dekker splitting under UNRELIABLE_FMA) -- both are exact, so mul_add
/// reproduces either build bit-for-bit.
fn dl_mul(x: f64, y: f64) -> (f64, f64) {
    let z = x * y;
    let zz = x.mul_add(y, -z);
    (z, zz)
}

/// frexp()'s exponent output: x = m * 2^e with 0.5 <= |m| < 1 (x finite,
/// nonzero).
fn frexp_exp(x: f64) -> i32 {
    let biased = ((x.to_bits() >> 52) & 0x7ff) as i32;
    if biased != 0 {
        return biased - 1022;
    }
    // Subnormal: normalize by 2^64 first.
    let biased = (((x * f64::from_bits(0x43F0_0000_0000_0000)).to_bits() >> 52) & 0x7ff) as i32;
    biased - 1022 - 64
}

/// ldexp(1.0, e) for the exponent range vector_norm produces
/// (-1024 <= e <= 1023) -- exact power of two, possibly subnormal.
fn pow2(e: i32) -> f64 {
    if e >= -1022 {
        f64::from_bits(((e + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (e + 1074))
    }
}

/// CPython 3.12 Modules/mathmodule.c vector_norm().
fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    let n = vec.len();
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || n <= 1 {
        return max;
    }
    let max_e = frexp_exp(max);
    if max_e < -1023 {
        // When max_e < -1023, ldexp(1.0, -max_e) would overflow.
        for v in vec.iter_mut() {
            *v /= f64::MIN_POSITIVE; // convert subnormals to normals
        }
        let scaled_max = max / f64::MIN_POSITIVE;
        return f64::MIN_POSITIVE * vector_norm(vec, scaled_max, found_nan);
    }
    let scale = pow2(-max_e);
    let (mut csum, mut frac1, mut frac2) = (1.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let x = vec[i] * scale; // lossless scaling
        let (pr_hi, pr_lo) = dl_mul(x, x); // lossless squaring
        let (sm_hi, sm_lo) = dl_fast_sum(csum, pr_hi); // lossless addition
        csum = sm_hi;
        frac1 += pr_lo; // lossy addition
        frac2 += sm_lo; // lossy addition
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let (pr_hi, pr_lo) = dl_mul(-h, h);
    let (sm_hi, sm_lo) = dl_fast_sum(csum, pr_hi);
    csum = sm_hi;
    frac1 += pr_lo;
    frac2 += sm_lo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h); // differential correction
    h / scale
}

/// math.hypot(a, b), bit-exact with CPython.
pub fn py_hypot(a: f64, b: f64) -> f64 {
    let mut vec = [a.abs(), b.abs()];
    let mut max = 0.0f64;
    let mut found_nan = false;
    for &x in &vec {
        found_nan |= x.is_nan();
        if x > max {
            max = x;
        }
    }
    vector_norm(&mut vec, max, found_nan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_round_matches_python() {
        // Python: round(2.675, 2) == 2.67 (2.675 is stored below the
        // midpoint), round(0.125, 2) == 0.12 (exact tie -> even),
        // round(2.5, 0) == 2.0, round(0.1+0.2, 1) == 0.3.
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(0.1 + 0.2, 1), 0.3);
        assert_eq!(py_round(-1.5, 0), -2.0);
        assert_eq!(py_round(123.456, 1), 123.5);
    }

    #[test]
    fn py_hypot_matches_python() {
        // Fixtures generated with CPython 3.12.7 math.hypot on 2026-07-09
        // (the MSVC CRT hypot disagrees on the first two by 1 ulp).
        for (a, b, expected) in [
            (31.0, 5.0, f64::from_bits(0x403F6690246A9D20)),
            (
                2.7308078822038713,
                5.4616157644077425,
                f64::from_bits(0x40186CD2951812F6),
            ),
            (1e-320, 3e-320, f64::from_bits(0x0000000000001900)),
            (0.1, 0.2, f64::from_bits(0x3FCC9F25C5BFEDDA)),
            (12345.678, 0.00042, f64::from_bits(0x40C81CD6C8B4395C)),
            (3.0, 4.0, 5.0),
        ] {
            assert_eq!(py_hypot(a, b).to_bits(), expected.to_bits(), "hypot({}, {})", a, b);
        }
        assert!(py_hypot(f64::NAN, 1.0).is_nan());
        assert_eq!(py_hypot(f64::INFINITY, f64::NAN), f64::INFINITY);
        assert_eq!(py_hypot(0.0, 0.0), 0.0);
    }

    #[test]
    fn jnum_nonfinite_is_null() {
        assert_eq!(jnum(f64::NAN), Value::Null);
        assert_eq!(jnum(f64::INFINITY), Value::Null);
        assert_eq!(jnum(1.5), serde_json::json!(1.5));
    }
}
