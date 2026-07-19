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

/// sav_map_data._BUILD_STEP_COUNT: 17 payload steps + the save index.
pub const BUILD_STEP_COUNT: u64 = 17 + 1;

/// sav_map_data.listSearchableItems.
fn list_searchable_items() -> Value {
    let mut items: Vec<(&str, &str)> = gamedata::get()
        .readable_name_corrections
        .iter()
        .filter(|(short_name, _)| short_name.starts_with("Desc_"))
        .map(|(short_name, label)| (short_name.as_str(), label.as_str()))
        .collect();
    items.sort_by(|a, b| a.1.cmp(b.1)); // stable, by label
    Value::Array(
        items.into_iter().map(|(path, label)| json!({"itemPath": path, "label": label})).collect(),
    )
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
        let (_, collector) = registry.iter().find(|(k, _)| *k == key).unwrap();
        let value = collector(scan);
        write_entry(&mut out, &mut first, key, &value);
        done += 1;
        if let Some(cb) = progress.as_deref_mut() {
            cb(done, BUILD_STEP_COUNT);
        }
    }
    // On-demand re-parses can't fail on bytes that already parsed, but once
    // the pipeline is lean this build validates edited bodies -- fail loud
    // rather than emit a payload with a silently-skipped object.
    if let Some(e) = scan.parse_error() {
        return Err(format!("object re-parse failed during payload build: {e}"));
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
    let map_index = index::MapIndex::build_with_scan(&scan);
    if let Some(e) = scan.parse_error() {
        return Err(format!("object re-parse failed during index build: {e}"));
    }
    tick(BUILD_STEP_COUNT, BUILD_STEP_COUNT);
    Ok((payload, map_index))
}
