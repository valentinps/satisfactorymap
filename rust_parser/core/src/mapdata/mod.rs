//! Port of map/sav_map_data.py: turns a parsed SaveStore into the frontend
//! map payload and the queryable save index. Every submodule is an exact
//! behavioral port of its Python reference; tools/diff_payload.py compares
//! the two implementations and is the regression gate. Where the Python code
//! has surprising behavior (stale indices, truthiness on 0, dict insertion
//! order), the port replicates it deliberately -- do not "fix" without
//! re-gating.

pub mod categories;
pub mod collectors;
pub mod consts;
pub mod describe;
pub mod display;
pub mod geometry;
pub mod index;
pub mod jsonval;
pub mod names;
pub mod props;
pub mod queries;
pub mod scan;

use crate::gamedata;
use crate::store::SaveStore;
use scan::SaveScan;
use serde_json::{json, Value};

/// 20 payload steps (sav_map_data._BUILD_STEP_COUNT's 17 + the static
/// spawners, mapLimits and caves steps) + the save index.
pub const BUILD_STEP_COUNT: u64 = 20 + 1;

/// Desc_ classes in readableNameCorrections that are NOT items: the game's
/// creature descriptors (FGCreatureDescriptor, plus the two enemy-group
/// pseudo-descriptors), which are in that table only so creature classes get
/// readable names. They can never sit in an inventory, so listing them
/// alongside Iron Plate in the item search was pure noise -- worse, they're
/// the only catalog entries with no icon at all, since no icons/items/ art
/// exists for any of them.
///
/// Deliberately explicit rather than inferred: every entry is a verified
/// "this class is an animal, not a thing you can hold". The creature-search
/// entries built from the Spawners/Entities layers (see filters.js's
/// wildlifeSearchEntries) are how you look these up instead, and they come
/// with the real per-species art. `creature_descriptors_are_not_items` guards
/// the list against a future Docs.json promoting one of them to a real item.
const CREATURE_DESCRIPTOR_CLASSES: [&str; 21] = [
    "Desc_HatcherBasic_C",
    "Desc_HogAlpha_C",
    "Desc_HogBasic_C",
    "Desc_HogCliff_C",
    "Desc_HogNuclear_C",
    "Desc_HostileCreature_C",
    "Desc_NonflyingBird_C",
    "Desc_SpaceGiraffe_C",
    "Desc_SpaceRabbit_C",
    "Desc_SpitterAquatic_Alpha_C",
    "Desc_SpitterAquatic_Small_C",
    "Desc_SpitterDesert_Alpha_C",
    "Desc_SpitterDesert_Small_C",
    "Desc_SpitterForest_Alpha_C",
    "Desc_SpitterForest_Red_Alpha_C",
    "Desc_SpitterForest_Small_C",
    "Desc_SpitterForest_Small_Red_C",
    "Desc_SpitterWave_C",
    "Desc_StingerAlpha_C",
    "Desc_StingerElite_C",
    "Desc_StingerSmall_C",
];

/// sav_map_data.listSearchableItems, minus the creature descriptors above.
fn list_searchable_items() -> Value {
    let mut items: Vec<(&str, &str)> = gamedata::get()
        .readable_name_corrections
        .iter()
        .filter(|(short_name, _)| short_name.starts_with("Desc_"))
        .filter(|(short_name, _)| !CREATURE_DESCRIPTOR_CLASSES.contains(&short_name.as_str()))
        .map(|(short_name, label)| (short_name.as_str(), label.as_str()))
        .collect();
    items.sort_by(|a, b| a.1.cmp(b.1)); // stable, by label
    Value::Array(
        items.into_iter().map(|(path, label)| json!({"itemPath": path, "label": label})).collect(),
    )
}

#[cfg(test)]
mod searchable_item_tests {
    use super::*;

    #[test]
    fn creature_descriptors_are_not_items() {
        let gd = gamedata::get();
        for class in CREATURE_DESCRIPTOR_CLASSES {
            assert!(
                gd.readable_name_corrections.contains_key(class),
                "{class} is no longer in readableNameCorrections -- drop it from the list"
            );
            // A real item has an FGItemDescriptor entry and/or extracted icon
            // art. If one of these ever gains either, it stopped being a
            // creature and belongs in the catalog.
            assert!(
                !gd.items.contains_key(class) && !gamedata::has_item_icon(class),
                "{class} is a real item descriptor now -- it should not be filtered out"
            );
        }
    }

    #[test]
    fn the_item_catalog_keeps_real_items_and_drops_creatures() {
        let catalog = list_searchable_items();
        let labels: Vec<&str> =
            catalog.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap()).collect();
        assert!(labels.contains(&"Iron Plate"));
        assert!(labels.contains(&"Hard Drive")); // No icon, but genuinely searchable.
        assert!(!labels.contains(&"Alpha Hog"));
        assert!(!labels.contains(&"Lizard Doggo"));
        // Real drops/trophies named after a creature are still items.
        assert!(labels.contains(&"Hog Remains"));
        assert!(labels.contains(&"Lizard Doggo Statue"));
    }

    /// The hand-maintained list above is easy to leave one entry short (it
    /// was, for the Crab Hatcher). No species may reach the item search under
    /// EITHER of the two names the codebase knows it by -- creatures.json's
    /// official displayName, or readable_label's hand-curated one -- since a
    /// creature descriptor is named by one or the other, never something else.
    #[test]
    fn no_creature_name_survives_in_the_item_catalog() {
        let catalog = list_searchable_items();
        let labels: Vec<&str> =
            catalog.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap()).collect();
        for (class, info) in &gamedata::get().creatures {
            for name in [info.display_name.clone(), names::readable_label(class)] {
                assert!(
                    !labels.contains(&name.as_str()),
                    "{name:?} ({class}) is a creature but is in the item catalog -- \
                     add its Desc_ class to CREATURE_DESCRIPTOR_CLASSES"
                );
            }
        }
    }
}

/// Python datetime.fromtimestamp(ticks / TICKS_IN_SECOND -
/// EPOCH_1_TO_1970).strftime("%Y-%m-%d %H:%M:%S") -- local time, same float
/// math.
fn save_datetime_string(ticks: u64) -> String {
    use chrono::TimeZone;
    const TICKS_IN_SECOND: f64 = (10 * 1000 * 1000) as f64;
    const EPOCH_1_TO_1970: f64 = (719162i64 * 24 * 60 * 60) as f64;
    let ts = ticks as f64 / TICKS_IN_SECOND - EPOCH_1_TO_1970;
    let secs = ts.floor() as i64;
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

fn write_entry(out: &mut Vec<u8>, first: &mut bool, key: &str, value: &Value) {
    if !*first {
        out.push(b',');
    }
    *first = false;
    serde_json::to_writer(&mut *out, key).expect("write key");
    out.push(b':');
    serde_json::to_writer(&mut *out, value).expect("write value");
}

/// _buildMapPayload: the payload as serialized JSON bytes. Each step's Value
/// is serialized and dropped immediately so the full payload never exists as
/// one Value tree. `steps` limits which payload steps run (diff-gating the
/// port collector-by-collector); None means every step in
/// collectors::STEP_ORDER, which requires the full registry to be ported.
/// `progress(done, BUILD_STEP_COUNT)` ticks after each completed step.
pub fn build_payload_json(
    store: &SaveStore,
    steps: Option<&[String]>,
    progress: Option<&mut dyn FnMut(u64, u64)>,
) -> Result<Vec<u8>, String> {
    let registry = collectors::registry();
    let requested: Vec<&str> = match steps {
        Some(list) => {
            let mut ordered: Vec<&str> = Vec::new();
            for key in collectors::STEP_ORDER {
                if list.iter().any(|s| s == key) {
                    ordered.push(key);
                }
            }
            for key in list {
                if !collectors::STEP_ORDER.contains(&key.as_str()) {
                    return Err(format!("unknown payload step: {}", key));
                }
            }
            ordered
        }
        None => collectors::STEP_ORDER.to_vec(),
    };
    for key in &requested {
        if !registry.iter().any(|(k, _)| k == key) {
            return Err(format!("payload step not ported yet: {}", key));
        }
    }

    let scan = SaveScan::new(store);
    build_payload_json_with_scan(&scan, requested, progress)
}

/// Instance names in bulk id arrays overwhelmingly share this prefix; the
/// serializer strips it and save_client.js's expandPayloadIds re-adds it
/// (mirror rule: string arrays under a key named "ids" or ending in "Ids",
/// elements without a ':' get the prefix back; full names always contain
/// ':', stripped suffixes never do). Keep the two sides in step.
const ID_PREFIX: &str = "Persistent_Level:PersistentLevel.";

/// Serialization-boundary payload diet (post-parity-gate): drop the
/// worldPositions arrays -- raw world X/Y is exactly derivable client-side
/// from the projected points via editor.js's mapPxToWorldXY -- and strip
/// ID_PREFIX from bulk id arrays. Together with jnum's 2-decimal rounding
/// this roughly halves the payload. Only the JSON leaving for the frontend
/// is slimmed; the index build reads the unslimmed collector output.
fn slim_payload_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| key != "worldPositions" && !key.ends_with("WorldPositions"));
            for (key, v) in map.iter_mut() {
                if key == "ids" || key.ends_with("Ids") {
                    if let Value::Array(items) = v {
                        for item in items.iter_mut() {
                            if let Value::String(s) = item {
                                if let Some(stripped) = s.strip_prefix(ID_PREFIX) {
                                    *item = Value::String(stripped.to_string());
                                }
                            }
                        }
                        continue;
                    }
                }
                slim_payload_value(v);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                slim_payload_value(item);
            }
        }
        _ => {}
    }
}

/// build_payload_json over an existing SaveScan (shared with the index build
/// -- see build_all_json).
fn build_payload_json_with_scan(
    scan: &SaveScan,
    requested: Vec<&str>,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> Result<Vec<u8>, String> {
    let store = scan.store;
    let registry = collectors::registry();
    // The payload serializes to roughly a quarter of the decompressed save
    // on big saves; reserving up front avoids the doubling-realloc copies of
    // a ~100MB Vec (transient 2x spikes that permanently grow wasm memory).
    let mut out: Vec<u8> = Vec::with_capacity((store.data.len() / 4).max(1 << 20));
    out.push(b'{');
    let mut first = true;
    write_entry(&mut out, &mut first, "mapSize", &json!(8192));
    write_entry(&mut out, &mut first, "sessionName", &json!(store.info.session_name));
    write_entry(
        &mut out,
        &mut first,
        "saveDatetime",
        &json!(save_datetime_string(store.info.save_date_time_in_ticks)),
    );
    write_entry(&mut out, &mut first, "menuOrder", &categories::get().menu_order);
    write_entry(&mut out, &mut first, "itemCatalog", &list_searchable_items());

    let mut done: u64 = 0;
    for key in requested {
        // The steps the save-index build consumes too are memoized on the
        // scan (SaveScan::collectables etc.) -- write those by reference so
        // a full load computes each exactly once, clone-free.
        let cached: Option<&Value> = match key {
            "collectables" => Some(scan.collectables()),
            "hardDrives" => Some(scan.hard_drives()),
            "dimensionalDepot" => Some(scan.depot_contents()),
            _ => None,
        };
        match cached {
            Some(value) => {
                // The cache is shared with the index build, which needs the
                // unslimmed names/positions -- slim a copy (these three are
                // small; the big steps take the owned branch below).
                let mut value = value.clone();
                slim_payload_value(&mut value);
                write_entry(&mut out, &mut first, key, &value);
            }
            None => {
                let (_, collector) = registry.iter().find(|(k, _)| *k == key).unwrap();
                let mut value = collector(scan);
                slim_payload_value(&mut value);
                write_entry(&mut out, &mut first, key, &value);
            }
        }
        done += 1;
        if let Some(cb) = progress.as_deref_mut() {
            cb(done, BUILD_STEP_COUNT);
        }
    }
    out.push(b'}');
    Ok(out)
}

/// Full load: payload + save index sharing one SaveScan (and its cached
/// instance-slot table) -- the browser path.
pub fn build_all_json(
    store: &SaveStore,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> Result<(Vec<u8>, index::MapIndex), String> {
    let scan = SaveScan::new(store);
    // One local closure owns the progress borrow: passing the trait object
    // itself twice trips &'a mut (dyn ... + 'a) invariance.
    let mut tick = |current: u64, total: u64| {
        if let Some(cb) = progress.as_deref_mut() {
            cb(current, total);
        }
    };
    let payload = build_payload_json_with_scan(
        &scan,
        collectors::STEP_ORDER.to_vec(),
        Some(&mut tick),
    )?;
    // Objects whose bodies would not re-parse are skipped, not fatal: a
    // modded save serializes property shapes this parser has never seen, and
    // one such object must not sink a 40k-object save. Every collector
    // already skips a None object, and buildings render off the header
    // (position/rotation/typePath), so a skipped buildable still appears on
    // the map -- only its body-derived detail is missing. The set rides out
    // on the index; the edit paths diff it to keep catching real corruption.
    let map_index = index::MapIndex::build_with_scan(&scan);
    // The payload is serialized before the index is built, but objects the
    // INDEX build could not read belong in the same report -- so splice the
    // key in here, where both halves are in hand, rather than teach every
    // collector about it.
    let mut payload = payload;
    append_unreadable_report(&mut payload, &map_index.parse_failures);
    tick(BUILD_STEP_COUNT, BUILD_STEP_COUNT);
    Ok((payload, map_index))
}

/// Add `"unreadableObjects": {count, samples}` to a finished payload object,
/// so the frontend can tell the user what the map is missing. Absent
/// entirely on a save that parsed cleanly -- which is nearly all of them.
fn append_unreadable_report(payload: &mut Vec<u8>, failures: &scan::ParseFailures) {
    if failures.is_empty() {
        return;
    }
    debug_assert_eq!(payload.last(), Some(&b'}'));
    payload.pop();
    if payload.last() != Some(&b'{') {
        payload.push(b',');
    }
    let report = serde_json::json!({
        "count": failures.len(),
        "samples": failures.samples,
    });
    payload.extend_from_slice(b"\"unreadableObjects\":");
    payload.extend_from_slice(report.to_string().as_bytes());
    payload.push(b'}');
}
