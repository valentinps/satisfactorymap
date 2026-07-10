//! Instance renaming for duplication. The whole scheme rests on SAME-LENGTH
//! substitution: a copied object's bytes are memcpy'd and every occurrence
//! of a renamed actor's name segment is replaced by a new segment of the
//! exact same byte length (a fresh value for the trailing decimal run), so
//! no length-prefix or size field anywhere needs recomputation.
//!
//! Component instance names and cross-references embed the actor's segment
//! as a substring ("...Build_X_C_123.InputInventory"), so substituting the
//! actor segment renames the components AND remaps every internal
//! reference within the copied set in one pass. External references (to
//! objects outside the set) are "tombstoned" the same way -- rewritten to a
//! same-length path that resolves to nothing; the game null-resolves
//! missing references.

use crate::error::{perr, PResult};
use crate::store::*;
use std::collections::{HashMap, HashSet};

/// splitmix64: tiny deterministic PRNG -- rename generation must be a pure
/// function of the op's seed so undo replays produce identical names.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
}

/// The trailing decimal run of a byte string, as (start, len).
fn trailing_digit_run(s: &[u8]) -> Option<(usize, usize)> {
    let end = s.len();
    let mut start = end;
    while start > 0 && s[start - 1].is_ascii_digit() {
        start -= 1;
    }
    if start == end { None } else { Some((start, end - start)) }
}

/// The last decimal run anywhere in the string, as (start, len). Used for
/// tombstoning external refs, whose paths may end in a component name.
fn last_digit_run(s: &[u8]) -> Option<(usize, usize)> {
    let mut end = s.len();
    while end > 0 {
        if s[end - 1].is_ascii_digit() {
            let mut start = end;
            while start > 0 && s[start - 1].is_ascii_digit() {
                start -= 1;
            }
            return Some((start, end - start));
        }
        end -= 1;
    }
    None
}

/// Rewrite the digit run at (start, len) to a fresh same-width value,
/// probing +1 (wrapping within the width) until `taken` doesn't contain the
/// full result. Returns the rewritten string.
fn rewrite_digits(
    s: &[u8],
    run: (usize, usize),
    rng: &mut Rng,
    taken: &dyn Fn(&[u8]) -> bool,
) -> PResult<Vec<u8>> {
    let (start, len) = run;
    // Game-generated suffixes are int32 values; keep 10-digit rewrites inside
    // [10^9, i32::MAX] so UE's FName number parsing sees them the same way
    // (no leading zeros, no overflow into string-only names).
    let (lo, hi) = if len == 10 {
        (1_000_000_000u64, 2_147_483_647u64)
    } else {
        (0u64, 10u64.checked_pow(len.min(19) as u32).unwrap_or(u64::MAX) - 1)
    };
    let span = hi - lo + 1;
    let mut value = lo + rng.next() % span;
    let mut out = s.to_vec();
    for _ in 0..1_000_000 {
        let digits = format!("{:0width$}", value, width = len);
        out[start..start + len].copy_from_slice(&digits.as_bytes()[..len]);
        if !taken(&out) {
            return Ok(out);
        }
        value = lo + (value - lo + 1) % span;
    }
    Err(perr!("Could not find a free name for {}", String::from_utf8_lossy(s)))
}

/// old segment -> new segment, one entry per duplicated actor. Segments are
/// the part of the instance name after the last '.' (which must end in a
/// decimal run -- game-generated buildable names always do).
pub fn build_rename_map(
    actor_names: &[&[u8]],
    seed: u64,
    exists: &dyn Fn(&[u8]) -> bool,
) -> PResult<HashMap<Vec<u8>, Vec<u8>>> {
    let mut rng = Rng(seed);
    let mut map: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut generated: HashSet<Vec<u8>> = HashSet::new();
    for &name in actor_names {
        if name.iter().any(|&b| !b.is_ascii()) {
            return Err(perr!(
                "Cannot duplicate {}: non-ASCII instance name",
                String::from_utf8_lossy(name)
            ));
        }
        let seg_start = name.iter().rposition(|&b| b == b'.').map_or(0, |p| p + 1);
        let segment = &name[seg_start..];
        let Some(run) = trailing_digit_run(segment) else {
            return Err(perr!(
                "Cannot duplicate {}: instance name has no numeric suffix",
                String::from_utf8_lossy(name)
            ));
        };
        if map.contains_key(segment) {
            continue;
        }
        let prefix = &name[..seg_start];
        let taken = |candidate_seg: &[u8]| {
            let full = [prefix, candidate_seg].concat();
            exists(&full) || generated.contains(&full)
        };
        let new_segment = rewrite_digits(segment, run, &mut rng, &taken)?;
        generated.insert([prefix, &new_segment].concat());
        map.insert(segment.to_vec(), new_segment);
    }
    Ok(map)
}

/// Tombstone: rewrite an external path to a same-length one that resolves
/// to nothing (checked against `exists`). Returns None for digitless paths:
/// those are shared singletons (BuildableSubsystem, game state, ...) that a
/// copy legitimately references too, while exclusive per-instance targets
/// (connection components, blueprint proxies, chain actors) always carry a
/// game-generated numeric suffix.
pub fn tombstone_path(
    path: &[u8],
    rng: &mut Rng,
    exists: &dyn Fn(&[u8]) -> bool,
) -> PResult<Option<Vec<u8>>> {
    let Some(run) = last_digit_run(path) else {
        return Ok(None);
    };
    rewrite_digits(path, run, rng, exists).map(Some)
}

/// Replace every boundary-delimited occurrence of each key with its
/// same-length value inside `span`. Boundaries: the bytes just before and
/// after a match must not be ASCII alphanumeric, so "Build_X_C_123" cannot
/// match inside "Build_X_C_1234" (names appear inside length-prefixed
/// null-terminated strings, so real occurrences border '.', '\0', etc).
pub fn substitute_names(span: &mut [u8], map: &HashMap<Vec<u8>, Vec<u8>>) {
    // Longest keys first so a key that embeds another key wins.
    let mut keys: Vec<&Vec<u8>> = map.keys().collect();
    keys.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    for key in keys {
        let value = &map[key.as_slice()];
        debug_assert_eq!(key.len(), value.len());
        let k = key.as_slice();
        if k.is_empty() || span.len() < k.len() {
            continue;
        }
        let mut i = 0;
        while i + k.len() <= span.len() {
            if &span[i..i + k.len()] == k {
                let before_ok = i == 0 || !span[i - 1].is_ascii_alphanumeric();
                let after = i + k.len();
                let after_ok = after == span.len() || !span[after].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    span[i..after].copy_from_slice(value);
                    i = after;
                    continue;
                }
            }
            i += 1;
        }
    }
}

/// Visit every ObjectRef inside an object's parsed model (associations,
/// properties, actor-specific trailing data). Used to find external
/// references that need tombstoning -- the substitution itself is byte-based.
pub fn visit_object_refs(object: &Object, f: &mut dyn FnMut(&ObjectRef)) {
    if let Some((parent, components)) = &object.actor_reference_associations {
        f(parent);
        for r in components {
            f(r);
        }
    }
    visit_props(&object.properties, f);
    match &object.actor_specific {
        ActorSpecific::RefList(refs) => refs.iter().for_each(|r| f(r)),
        ActorSpecific::Circuits(cs) => cs.iter().for_each(|(_, r)| f(r)),
        ActorSpecific::PowerLine(a, b) => {
            f(a);
            f(b);
        }
        ActorSpecific::Train { previous, next } => {
            f(previous);
            f(next);
        }
        ActorSpecific::ConveyorChain { chain_actor, belts, .. } => {
            f(chain_actor);
            for cb in belts {
                f(&cb.belt);
            }
        }
        ActorSpecific::Lightweight { items, .. } => {
            for group in items {
                for inst in &group.instances {
                    f(&inst.swatch);
                    f(&inst.pattern);
                    f(&inst.paint_finish);
                    f(&inst.recipe);
                    f(&inst.blueprint_proxy);
                    if let Some(pl) = &inst.data_property {
                        visit_props(pl, f);
                    }
                }
            }
        }
        _ => {}
    }
}

fn visit_props(pl: &PropList, f: &mut dyn FnMut(&ObjectRef)) {
    for prop in &pl.props {
        visit_value(&prop.value, f);
    }
}

fn visit_value(v: &PropertyValue, f: &mut dyn FnMut(&ObjectRef)) {
    match v {
        PropertyValue::Object(r) | PropertyValue::SoftObject(r, _) => f(r),
        PropertyValue::Set { values, .. } => {
            if let SetValues::Refs(refs) = values {
                refs.iter().for_each(|r| f(r));
            }
        }
        PropertyValue::Array(av) => match av {
            ArrayValue::Refs(refs) => refs.iter().for_each(|r| f(r)),
            ArrayValue::SoftObj(refs) => refs.iter().for_each(|(r, _)| f(r)),
            ArrayValue::Structs(pls) => pls.iter().for_each(|pl| visit_props(pl, f)),
            _ => {}
        },
        PropertyValue::Struct(sv) => match sv {
            StructValue::RailroadTrackPosition(r, _, _) => f(r),
            StructValue::Props(pl) => visit_props(pl, f),
            StructValue::InventoryItem { item_properties, .. } => {
                if let InvItemProps::Props { props, .. } = item_properties {
                    visit_props(props, f);
                }
            }
            _ => {}
        },
        PropertyValue::Map(entries) => {
            for (k, val) in entries {
                if let MapKey::Ref(r) = k {
                    f(r);
                }
                match val {
                    MapVal::Props(pl) => visit_props(pl, f),
                    MapVal::Ref(r) => f(r),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
