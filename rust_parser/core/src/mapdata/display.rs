//! sav_parse.toString port over the Rust PropertyValue tree. describeInstance's
//! "rawProperties" rows are `sav_parse.toString(value)` strings computed on the
//! CONVERTED Python value (py/convert.rs defines that conversion); this module
//! reproduces those exact strings without going through Python:
//! - str -> 'text' (single quotes, no escaping -- toString does f"'{val}'")
//! - list -> [a, b] with ", " separators (convert.rs never produces tuples or
//!   dicts inside property values, so those toString branches are unreachable)
//! - everything else -> str(val): Python int repr, Python float repr
//!   (py_float_repr below), bytes repr (py_bytes_repr), True/False, None, and
//!   ObjectReference's __str__ (identical between patches/sav_parse.py and
//!   py/mod.rs ObjectReferencePy -- no memory addresses involved).

use crate::store::*;
use std::fmt::Write;

/// Python repr(float) / str(float): the shortest decimal string that
/// round-trips, formatted with CPython's fixed/scientific switch (scientific
/// iff decimal exponent < -4 or >= 16), "e+NN"/"e-NN" with a 2+ digit
/// zero-padded exponent, and a ".0" suffix on integral fixed-notation values.
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf".to_string() } else { "inf".to_string() };
    }
    // Rust's LowerExp for f64 is a shortest round-trip representation, but
    // when the value sits EXACTLY halfway between the two shortest decimals
    // (dyadic values -- e.g. any f32 widened to f64 -- can do this), Rust's
    // shortest mode rounds the tie up while CPython's repr rounds it to even
    // (e.g. 250.272735595703125: Rust '...13', Python '...12'). Rust's
    // fixed-precision {:.*e} exact mode rounds ties to even like CPython, so
    // re-round at the shortest digit count -- but only KEEP that candidate if
    // it still round-trips: at binade boundaries (v a power of two, e.g.
    // 2^-44) the rounding interval is asymmetric and the correctly-rounded
    // shortest decimal falls outside it, where CPython keeps the round-trip
    // digits instead. (A tie can never carry: a dyadic's decimal expansion
    // ends in ...25 or ...75, so re-rounding flips at most the last digit.)
    let shortest = format!("{:e}", x);
    let digit_count =
        shortest.split_once('e').expect("LowerExp always has an exponent").0.chars()
            .filter(char::is_ascii_digit)
            .count();
    let mut formatted = format!("{:.*e}", digit_count - 1, x);
    if formatted != shortest
        && formatted.parse::<f64>().map(f64::to_bits) != Ok(x.to_bits())
    {
        formatted = shortest;
    }
    let (mantissa, exp_str) = formatted.split_once('e').expect("LowerExp always has an exponent");
    let exp: i32 = exp_str.parse().expect("exponent is an integer");
    let neg = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() > int_len {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            } else {
                out.push_str(&digits);
                for _ in digits.len()..int_len {
                    out.push('0');
                }
                out.push_str(".0");
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs());
    }
    out
}

/// Python repr(bytes) == str(bytes): b'...' with CPython's quote choice
/// (double quotes iff the content has ' and no ") and escape rules
/// (\\, \t, \n, \r, the active quote, \xHH lowercase for everything outside
/// printable ASCII).
pub fn py_bytes_repr(bytes: &[u8]) -> String {
    let has_single = bytes.contains(&b'\'');
    let has_double = bytes.contains(&b'"');
    let quote = if has_single && !has_double { b'"' } else { b'\'' };
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &c in bytes {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c as char);
            }
            0x20..=0x7e => out.push(c as char),
            c => {
                let _ = write!(out, "\\x{:02x}", c);
            }
        }
    }
    out.push(quote as char);
    out
}

/// sav_parse.toString(value) for one converted property value.
pub fn py_str(value: &PropertyValue, data: &[u8]) -> String {
    let mut out = String::new();
    write_value(&mut out, data, value);
    out
}

fn write_quoted(out: &mut String, data: &[u8], s: crate::reader::StrRef) {
    out.push('\'');
    out.push_str(&s.to_string(data));
    out.push('\'');
}

/// ObjectReference.__str__ (identical in patches/sav_parse.py and
/// py/mod.rs::ObjectReferencePy).
fn write_object_ref(out: &mut String, data: &[u8], r: &ObjectRef) {
    let level_name = r.level_name.to_string(data);
    let path_name = r.path_name.to_string(data);
    if level_name.is_empty() && path_name.is_empty() {
        out.push_str("<ObjectReference/>");
    } else {
        let _ = write!(out, "<ObjectReference: levelName={}, pathName={}>", level_name, path_name);
    }
}

/// Comma-joins `n` writes of `f(i)` inside brackets.
fn write_list(out: &mut String, n: usize, mut f: impl FnMut(&mut String, usize)) {
    out.push('[');
    for i in 0..n {
        if i > 0 {
            out.push_str(", ");
        }
        f(out, i);
    }
    out.push(']');
}

/// The converted [prop, propTypes] pair of a nested PropList (struct values,
/// array-of-struct elements).
fn write_prop_types_pair(out: &mut String, data: &[u8], pl: &PropList) {
    out.push('[');
    write_props(out, data, pl);
    out.push_str(", ");
    write_types(out, data, pl);
    out.push(']');
}

/// The converted properties list: [['name', value], ...].
fn write_props(out: &mut String, data: &[u8], pl: &PropList) {
    write_list(out, pl.props.len(), |out, i| {
        let p = &pl.props[i];
        out.push('[');
        write_quoted(out, data, p.name);
        out.push_str(", ");
        write_value(out, data, &p.value);
        out.push(']');
    });
}

/// The converted propertyTypes list: one meta list per property.
fn write_types(out: &mut String, data: &[u8], pl: &PropList) {
    write_list(out, pl.props.len(), |out, i| {
        let p = &pl.props[i];
        write_list(out, p.meta.len(), |out, j| write_meta(out, data, &p.meta[j], &p.value));
    });
}

fn write_meta(out: &mut String, data: &[u8], m: &Meta, value: &PropertyValue) {
    match m {
        Meta::Str(s) => write_quoted(out, data, *s),
        Meta::U8(v) => {
            let _ = write!(out, "{}", v);
        }
        Meta::U32(v) => {
            let _ = write!(out, "{}", v);
        }
        Meta::U64(v) => {
            let _ = write!(out, "{}", v);
        }
        Meta::Null => out.push_str("None"),
        Meta::Bytes(d) => out.push_str(&py_bytes_repr(d.bytes(data))),
        Meta::List(l) => write_list(out, l.len(), |out, i| write_meta(out, data, &l[i], value)),
        Meta::MapStructPropTypes => {
            // One propTypes list per struct-valued map entry (convert.rs
            // meta_value's Meta::MapStructPropTypes branch).
            let entries: Vec<&PropList> = match value {
                PropertyValue::Map(entries) => entries
                    .iter()
                    .filter_map(|(_, v)| match v {
                        MapVal::Props(pl) => Some(pl),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            write_list(out, entries.len(), |out, i| {
                let pl = entries[i];
                write_list(out, pl.props.len(), |out, j| {
                    let p = &pl.props[j];
                    write_list(out, p.meta.len(), |out, k| {
                        write_meta(out, data, &p.meta[k], &p.value)
                    });
                });
            });
        }
    }
}

fn write_text(out: &mut String, data: &[u8], t: &TextValue) {
    match t {
        TextValue::NoneHistory { flags, invariant, s } => {
            let _ = write!(out, "[{}, 255, {}, ", flags, invariant);
            write_quoted(out, data, *s);
            out.push(']');
        }
        TextValue::Base { flags, namespace, key, value } => {
            let _ = write!(out, "[{}, 0, ", flags);
            write_quoted(out, data, *namespace);
            out.push_str(", ");
            write_quoted(out, data, *key);
            out.push_str(", ");
            write_quoted(out, data, *value);
            out.push(']');
        }
        TextValue::ArgumentFormat { flags, uuid, format, args } => {
            let _ = write!(out, "[{}, 3, ", flags);
            write_quoted(out, data, *uuid);
            out.push_str(", ");
            write_quoted(out, data, *format);
            out.push_str(", ");
            write_list(out, args.len(), |out, i| {
                let (name, value, aflags) = &args[i];
                out.push('[');
                write_quoted(out, data, *name);
                out.push_str(", ");
                write_quoted(out, data, *value);
                let _ = write!(out, ", {}]", aflags);
            });
            out.push(']');
        }
        TextValue::StringTable { flags, table_id, text_key } => {
            let _ = write!(out, "[{}, 11, ", flags);
            write_quoted(out, data, *table_id);
            out.push_str(", ");
            write_quoted(out, data, *text_key);
            out.push(']');
        }
    }
}

fn write_value(out: &mut String, data: &[u8], v: &PropertyValue) {
    match v {
        // BoolProperty converts to a plain Python int (parseUint8 -- values
        // like 16 have been seen), so str() of it, not True/False.
        PropertyValue::Bool(b) => {
            let _ = write!(out, "{}", b);
        }
        // Int8Property converts to a one-byte bytes value (convert.rs).
        PropertyValue::Int8(b) => out.push_str(&py_bytes_repr(&[*b])),
        PropertyValue::Int(x) => {
            let _ = write!(out, "{}", x);
        }
        PropertyValue::UInt32(x) => {
            let _ = write!(out, "{}", x);
        }
        PropertyValue::Int64(x) => {
            let _ = write!(out, "{}", x);
        }
        PropertyValue::Float(x) => out.push_str(&py_float_repr(*x as f64)),
        PropertyValue::Double(x) => out.push_str(&py_float_repr(*x)),
        PropertyValue::Byte { enum_name, value } => {
            out.push('[');
            match enum_name {
                Some(s) => write_quoted(out, data, *s),
                None => out.push_str("None"),
            }
            out.push_str(", ");
            match value {
                ByteVal::U8(b) => {
                    let _ = write!(out, "{}", b);
                }
                ByteVal::Str(s) => write_quoted(out, data, *s),
            }
            out.push(']');
        }
        PropertyValue::Enum { enum_name, value } => {
            out.push('[');
            match enum_name {
                Some(s) => write_quoted(out, data, *s),
                None => out.push_str("None"),
            }
            out.push_str(", ");
            write_quoted(out, data, *value);
            out.push(']');
        }
        PropertyValue::Str(s) => write_quoted(out, data, *s),
        PropertyValue::Text(t) => write_text(out, data, t),
        PropertyValue::Set { set_type, values } => {
            out.push('[');
            write_quoted(out, data, *set_type);
            out.push_str(", ");
            match values {
                SetValues::U32(v) => write_list(out, v.len(), |out, i| {
                    let _ = write!(out, "{}", v[i]);
                }),
                SetValues::Guid(v) => write_list(out, v.len(), |out, i| {
                    let _ = write!(out, "[{}, {}]", v[i][0], v[i][1]);
                }),
                SetValues::Refs(v) => {
                    write_list(out, v.len(), |out, i| write_object_ref(out, data, &v[i]))
                }
            }
            out.push(']');
        }
        PropertyValue::Object(r) => write_object_ref(out, data, r),
        PropertyValue::SoftObject(r, x) => {
            out.push('[');
            write_object_ref(out, data, r);
            let _ = write!(out, ", {}]", x);
        }
        PropertyValue::Array(av) => write_array(out, data, av),
        PropertyValue::Struct(sv) => write_struct(out, data, sv),
        PropertyValue::Map(entries) => write_list(out, entries.len(), |out, i| {
            let (k, val) = &entries[i];
            out.push('[');
            match k {
                MapKey::IntVector([a, b, c]) => {
                    let _ = write!(out, "[{}, {}, {}]", a, b, c);
                }
                MapKey::Ref(r) => write_object_ref(out, data, r),
                MapKey::I32(x) => {
                    let _ = write!(out, "{}", x);
                }
                MapKey::Str(s) => write_quoted(out, data, *s),
            }
            out.push_str(", ");
            match val {
                // MapVal::Props converts to just the properties list (no
                // propTypes pair -- convert.rs prop_list(..).0).
                MapVal::Props(pl) => write_props(out, data, pl),
                MapVal::Bool(x) => {
                    let _ = write!(out, "{}", x);
                }
                MapVal::I32(x) => {
                    let _ = write!(out, "{}", x);
                }
                MapVal::I64(x) => {
                    let _ = write!(out, "{}", x);
                }
                MapVal::U8(x) => {
                    let _ = write!(out, "{}", x);
                }
                MapVal::F64(x) => out.push_str(&py_float_repr(*x)),
                MapVal::Ref(r) => write_object_ref(out, data, r),
                MapVal::Str(s) => write_quoted(out, data, *s),
            }
            out.push(']');
        }),
    }
}

fn write_f64_slice(out: &mut String, vals: &[f64]) {
    write_list(out, vals.len(), |out, i| out.push_str(&py_float_repr(vals[i])));
}

fn write_f32_slice(out: &mut String, vals: &[f32]) {
    write_list(out, vals.len(), |out, i| out.push_str(&py_float_repr(vals[i] as f64)));
}

fn write_array(out: &mut String, data: &[u8], av: &ArrayValue) {
    match av {
        ArrayValue::I32(v) => write_list(out, v.len(), |out, i| {
            let _ = write!(out, "{}", v[i]);
        }),
        ArrayValue::I64(v) => write_list(out, v.len(), |out, i| {
            let _ = write!(out, "{}", v[i]);
        }),
        ArrayValue::U8(v) => write_list(out, v.len(), |out, i| {
            let _ = write!(out, "{}", v[i]);
        }),
        ArrayValue::F32(v) => write_f32_slice(out, v),
        ArrayValue::F64(v) => write_f64_slice(out, v),
        ArrayValue::Str(v) => write_list(out, v.len(), |out, i| write_quoted(out, data, v[i])),
        ArrayValue::SoftObj(v) => write_list(out, v.len(), |out, i| {
            let (r, x) = &v[i];
            out.push('[');
            write_object_ref(out, data, r);
            let _ = write!(out, ", {}]", x);
        }),
        ArrayValue::Refs(v) => write_list(out, v.len(), |out, i| write_object_ref(out, data, &v[i])),
        ArrayValue::Text(v) => write_list(out, v.len(), |out, i| write_text(out, data, &v[i])),
        ArrayValue::LinearColor(v) => write_list(out, v.len(), |out, i| write_f32_slice(out, &v[i])),
        ArrayValue::Vector(v) => write_list(out, v.len(), |out, i| write_f64_slice(out, &v[i])),
        ArrayValue::Guid(v) => write_list(out, v.len(), |out, i| {
            let _ = write!(out, "[{}, {}]", v[i][0], v[i][1]);
        }),
        // One bytes blob then None-padding to arrayCount (convert.rs).
        ArrayValue::Opaque { blob, array_count } => {
            let n = (*array_count as usize).max(1);
            write_list(out, n, |out, i| {
                if i == 0 {
                    out.push_str(&py_bytes_repr(blob.bytes(data)));
                } else {
                    out.push_str("None");
                }
            });
        }
        ArrayValue::Structs(v) => {
            write_list(out, v.len(), |out, i| write_prop_types_pair(out, data, &v[i]))
        }
    }
}

fn write_struct(out: &mut String, data: &[u8], sv: &StructValue) {
    match sv {
        StructValue::InventoryItem { item_name, item_properties } => {
            out.push('[');
            write_quoted(out, data, *item_name);
            out.push_str(", ");
            match item_properties {
                InvItemProps::One => out.push('1'),
                InvItemProps::Two => out.push('2'),
                InvItemProps::Props { type_path, props } => {
                    out.push('[');
                    write_quoted(out, data, *type_path);
                    out.push_str(", ");
                    write_props(out, data, props);
                    out.push_str(", ");
                    write_types(out, data, props);
                    out.push(']');
                }
            }
            out.push(']');
        }
        StructValue::LinearColor(c) => write_f32_slice(out, c),
        StructValue::Vector2D(c) => write_f64_slice(out, c),
        StructValue::Vector(c) => write_f64_slice(out, c),
        StructValue::Quat(c) => write_f64_slice(out, c),
        StructValue::Box { vals, flag } => {
            out.push('[');
            for v in vals {
                out.push_str(&py_float_repr(*v));
                out.push_str(", ");
            }
            out.push_str(if *flag { "True" } else { "False" });
            out.push(']');
        }
        StructValue::FluidBox(x) => out.push_str(&py_float_repr(*x as f64)),
        StructValue::RailroadTrackPosition(r, o, f) => {
            out.push('[');
            write_object_ref(out, data, r);
            out.push_str(", ");
            out.push_str(&py_float_repr(*o as f64));
            out.push_str(", ");
            out.push_str(&py_float_repr(*f as f64));
            out.push(']');
        }
        StructValue::DateTime(x) => {
            let _ = write!(out, "{}", x);
        }
        StructValue::ClientIdentityInfo { uuid, identities } => {
            out.push('[');
            write_quoted(out, data, *uuid);
            out.push_str(", ");
            write_list(out, identities.len(), |out, i| {
                let (t, d) = &identities[i];
                let _ = write!(out, "[{}, {}]", t, py_bytes_repr(d.bytes(data)));
            });
            out.push(']');
        }
        StructValue::Raw(d) => out.push_str(&py_bytes_repr(d.bytes(data))),
        StructValue::Props(pl) => write_prop_types_pair(out, data, pl),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        // Fixtures generated with CPython 3.12 repr() on 2026-07-10.
        for (x, expected) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (1.5, "1.5"),
            (100.0, "100.0"),
            (435.0, "435.0"),
            (4.35, "4.35"),
            (0.1, "0.1"),
            (0.05, "0.05"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.1f32 as f64, "0.10000000149011612"),
            (1e15, "1000000000000000.0"),
            (9007199254740992.0, "9007199254740992.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (0.00001, "1e-05"),
            (1.2e-5, "1.2e-05"),
            (1.5e-10, "1.5e-10"),
            (-1e-7, "-1e-07"),
            (2.5e-9, "2.5e-09"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (3.4028234663852886e38, "3.4028234663852886e+38"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            // Exact-halfway ties (dyadic f32-as-f64 values): CPython repr
            // rounds ties to even -- down (...12) and up (...88) flavors.
            (250.272735595703125, "250.27273559570312"),
            (3.0 / 16777216.0, "1.7881393432617188e-07"),
            (7.0 / 67108864.0, "1.043081283569336e-07"),
            (123.45600128173828125, "123.45600128173828"),
            // Binade boundary: the correctly-rounded 16-digit decimal
            // (...801) does NOT round-trip to 2^-44 -- CPython keeps ...802.
            (2.0f64.powi(-44), "5.684341886080802e-14"),
            (-(2.0f64.powi(-44)), "-5.684341886080802e-14"),
            (f64::from_bits(0x3D2FFFFFFFFFFFFF), "5.684341886080801e-14"),
        ] {
            assert_eq!(py_float_repr(x), expected, "repr({:?})", x);
        }
    }

    #[test]
    fn bytes_repr_matches_python() {
        // Fixtures generated with CPython 3.12 repr() on 2026-07-10.
        assert_eq!(py_bytes_repr(b""), "b''");
        assert_eq!(py_bytes_repr(&[0x01]), "b'\\x01'");
        assert_eq!(py_bytes_repr(&[0x00, 0xff]), "b'\\x00\\xff'");
        assert_eq!(py_bytes_repr(b"abc DEF 123"), "b'abc DEF 123'");
        assert_eq!(py_bytes_repr(b"a\tb\nc\rd\\e"), "b'a\\tb\\nc\\rd\\\\e'");
        assert_eq!(py_bytes_repr(b"it's"), "b\"it's\"");
        assert_eq!(py_bytes_repr(b"say \"hi\""), "b'say \"hi\"'");
        assert_eq!(py_bytes_repr(b"'\""), "b'\\'\"'");
        assert_eq!(py_bytes_repr(&[0x7f]), "b'\\x7f'");
        assert_eq!(py_bytes_repr(&[0x1f]), "b'\\x1f'");
    }
}
