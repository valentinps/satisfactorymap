//! Query functions over the SaveStore + MapIndex -- port of
//! sav_map_data.py's per-request endpoints and their support helpers (lines
//! ~2278-2461, 2484-2613, 2615-3041): findItemLocations,
//! aggregateSelectionInventory, collectBuildingInfo, collectVehicleInfo,
//! collectTrainInfo, plus the bottleneck/rated-power helpers describeInstance
//! will reuse. Exact behavioral ports: dict insertion order, int-vs-float
//! JSON types, py_round call sites and float op order all mirror Python.

use super::consts::*;
use super::geometry::{project_xy, short_class_name, world_z_to_meters};
use super::index::{MapIndex, PIPE_CONNECTOR_SUFFIXES};
use super::jsonval::{jnum, py_round};
use super::names::readable_label;
use super::props;
use crate::extract::{find_prop, short_name, stack_item, INVENTORY_PROPERTY_NAMES};
use crate::gamedata;
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Fluids (sav_map_data 2484-2491)
// ---------------------------------------------------------------------------

/// sav_map_data.FLUID_ITEM_SHORT_NAMES.
const FLUID_ITEM_SHORT_NAMES: [&str; 11] = [
    "Desc_Water_C",
    "Desc_LiquidOil_C",
    "Desc_HeavyOilResidue_C",
    "Desc_LiquidFuel_C",
    "Desc_LiquidTurboFuel_C",
    "Desc_AluminaSolution_C",
    "Desc_SulfuricAcid_C",
    "Desc_NitricAcid_C",
    "Desc_NitrogenGas_C",
    "Desc_LiquidBiofuel_C",
    "Desc_RocketFuel_C",
];

/// sav_map_data._isFluidItemPath.
pub fn is_fluid_item_path(item_path: &str) -> bool {
    let short = match item_path.rfind('.') {
        Some(i) => &item_path[i + 1..],
        None => item_path,
    };
    FLUID_ITEM_SHORT_NAMES.contains(&short)
}

fn is_fluid_short(path: &[u8]) -> bool {
    let short = short_name(path);
    FLUID_ITEM_SHORT_NAMES.iter().any(|s| s.as_bytes() == short)
}

// ---------------------------------------------------------------------------
// Shared inventory-walk helpers (sav_map_data 2493-2613, 2892-2936)
// ---------------------------------------------------------------------------

/// sav_map_data._resolveComponentObject. Owned: components re-parse from
/// their spans on demand (the parsed model is dropped post-load).
pub fn resolve_component_object(
    store: &SaveStore,
    index: &MapIndex,
    properties: &PropList,
    property_name: &[u8],
) -> Option<Object> {
    let data: &[u8] = &store.data;
    let reference = props::object_ref(properties, data, property_name)?;
    index.parse_object_by_name(store, reference.path_name.bytes(data))
}

/// sav_map_data._inventoryComponentObjects: referenced inventory components
/// first, then the vehicle name-convention ones, deduped by pathName keeping
/// the first.
pub fn inventory_component_objects(
    store: &SaveStore,
    index: &MapIndex,
    instance_name: &[u8],
    properties: &PropList,
) -> Vec<Object> {
    let data: &[u8] = &store.data;
    let mut seen_paths: Vec<&[u8]> = Vec::new();
    let mut components: Vec<Object> = Vec::new();
    for prop_name in INVENTORY_PROPERTY_NAMES {
        if let Some(PropertyValue::Object(r)) = find_prop(properties, data, prop_name) {
            let path = r.path_name.bytes(data);
            if let Some(object) = index.parse_object_by_name(store, path) {
                if !seen_paths.contains(&path) {
                    seen_paths.push(path);
                    components.push(object);
                }
            }
        }
    }
    let mut key: Vec<u8> = Vec::new();
    for suffix in crate::extract::VEHICLE_INVENTORY_COMPONENT_SUFFIXES {
        key.clear();
        key.extend_from_slice(instance_name);
        key.extend_from_slice(suffix);
        if !seen_paths.iter().any(|p| *p == key.as_slice()) {
            if let Some(object) = index.parse_object_by_name(store, &key) {
                components.push(object);
            }
        }
    }
    components
}

/// sav_map_data._vehicleStorageComponent.
pub fn vehicle_storage_component(
    store: &SaveStore,
    index: &MapIndex,
    instance_name: &[u8],
    properties: &PropList,
) -> Option<Object> {
    let data: &[u8] = &store.data;
    if let Some(r) = props::object_ref(properties, data, b"mStorageInventory") {
        let path = r.path_name.bytes(data);
        if !path.is_empty() {
            if let Some(object) = index.parse_object_by_name(store, path) {
                return Some(object);
            }
        }
    }
    let mut key = instance_name.to_vec();
    key.extend_from_slice(b".StorageInventory");
    index.parse_object_by_name(store, &key)
}

/// The shared row build + sort of aggregateSelectionInventory /
/// _sumInventoryComponentStacks: solids (short class name keys, int counts)
/// then fluids (readable-label keys, 1000x-m3 raw amounts rounded to m3),
/// stably sorted by count descending.
fn inventory_rows(solid: IndexMap<Vec<u8>, i64>, fluid: IndexMap<String, f64>) -> Value {
    let mut rows: Vec<(f64, Value)> = Vec::new();
    for (short, count) in solid {
        let short = props::lossy(&short);
        rows.push((
            count as f64,
            json!({"item": short, "label": readable_label(&short), "count": count, "isFluid": false}),
        ));
    }
    for (label, raw) in fluid {
        let rounded = py_round(raw / 1000.0, 1);
        rows.push((
            rounded,
            json!({"item": label, "label": label, "count": jnum(rounded), "isFluid": true}),
        ));
    }
    // list.sort(key=count, reverse=True): stable, equal counts keep order.
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("count NaN"));
    Value::Array(rows.into_iter().map(|(_, v)| v).collect())
}

/// sav_map_data._sumInventoryComponentStacks.
pub fn sum_inventory_component_stacks(data: &[u8], component_objects: &[Object]) -> Value {
    let mut solid: IndexMap<Vec<u8>, i64> = IndexMap::new();
    let mut fluid: IndexMap<String, f64> = IndexMap::new();
    for component_object in component_objects {
        let Some(stacks) = props::array_structs(&component_object.properties, data, b"mInventoryStacks")
        else {
            continue;
        };
        for stack in stacks {
            if let Some((item_path, num_items)) = stack_item(stack, data) {
                add_item(&mut solid, &mut fluid, short_name(item_path), num_items);
            }
        }
    }
    inventory_rows(solid, fluid)
}

/// aggregateSelectionInventory's addItem: fluids merge by readable label,
/// solids by short class name.
fn add_item(
    solid: &mut IndexMap<Vec<u8>, i64>,
    fluid: &mut IndexMap<String, f64>,
    short: &[u8],
    amount: i64,
) {
    if is_fluid_short(short) {
        let label = readable_label(&props::lossy(short));
        *fluid.entry(label).or_insert(0.0) += amount as f64;
    } else {
        *solid.entry(short.to_vec()).or_insert(0) += amount;
    }
}

// ---------------------------------------------------------------------------
// Conveyor chain ring buffer (sav_map_data 2284-2309)
// ---------------------------------------------------------------------------

/// sav_map_data._conveyorChainSegmentItemPaths: this belt's own contiguous
/// slice of the chain's ring-buffer item window. Python's `%` on possibly
/// negative operands == rem_euclid; the slice `chainItems[start:start+count]`
/// clamps at the end like Python slicing.
pub fn conveyor_chain_segment_item_paths<'a>(
    // NOT tied to 'a: the returned slices borrow only `data` (StrRefs are
    // offsets), so an owned re-parsed chain actor can be a short-lived local.
    chain_actor_info: &ActorSpecific,
    data: &'a [u8],
    belt_instance_name: &[u8],
) -> Vec<&'a [u8]> {
    let ActorSpecific::ConveyorChain { belts, items, maximum_items, chain_lead_item_index, .. } =
        chain_actor_info
    else {
        return Vec::new();
    };
    if items.is_empty() || *maximum_items <= 0 || *chain_lead_item_index < 0 {
        return Vec::new();
    }
    for chain_belt in belts {
        if chain_belt.belt.path_name.bytes(data) != belt_instance_name {
            continue;
        }
        if chain_belt.lead_item_index < 0 || chain_belt.tail_item_index < 0 {
            return Vec::new();
        }
        let maximum = *maximum_items as i64;
        let start = (chain_belt.lead_item_index as i64 - *chain_lead_item_index as i64)
            .rem_euclid(maximum) as usize;
        let count = (chain_belt.tail_item_index as i64 - chain_belt.lead_item_index as i64)
            .rem_euclid(maximum) as usize
            + 1;
        return items.iter().skip(start).take(count).map(|(path, _)| path.bytes(data)).collect();
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Rated flow + mixed-mark bottlenecks (sav_map_data 2311-2416)
// ---------------------------------------------------------------------------

/// sav_map_data._CONVEYOR_MARK_ITEMS_PER_MINUTE.
const CONVEYOR_MARK_ITEMS_PER_MINUTE: [(i64, i64); 6] =
    [(1, 60), (2, 120), (3, 270), (4, 480), (5, 780), (6, 1200)];

/// sav_map_data._conveyorItemsPerMinute: hand-rolled
/// re.search(r"Conveyor(?:Belt|Lift)Mk(\d+)") -- first match anywhere, greedy
/// digit run.
pub fn conveyor_items_per_minute(type_path: Option<&str>) -> Option<i64> {
    let type_path = type_path.unwrap_or("");
    let mut search_from = 0usize;
    while let Some(pos) = type_path[search_from..].find("Conveyor") {
        let start = search_from + pos;
        let rest = &type_path[start + "Conveyor".len()..];
        let after_kind = rest.strip_prefix("Belt").or_else(|| rest.strip_prefix("Lift"));
        if let Some(after_kind) = after_kind {
            if let Some(digits) = after_kind.strip_prefix("Mk") {
                let digit_len = digits.bytes().take_while(u8::is_ascii_digit).count();
                if digit_len > 0 {
                    let mark: i64 = digits[..digit_len].parse().ok()?;
                    return CONVEYOR_MARK_ITEMS_PER_MINUTE
                        .iter()
                        .find(|(m, _)| *m == mark)
                        .map(|(_, rate)| *rate);
                }
            }
        }
        search_from = start + 1;
    }
    None
}

/// sav_map_data._PIPE_FLOW_M3_PER_MINUTE_BY_CLASS.
const PIPE_FLOW_M3_PER_MINUTE_BY_CLASS: [(&str, i64); 6] = [
    ("Build_Pipeline_C", 300),
    ("Build_Pipeline_NoIndicator_C", 300),
    ("Build_PipelineMK2_C", 600),
    ("Build_PipelineMK2_NoIndicator_C", 600),
    ("Build_PipelinePump_C", 300),
    ("Build_PipelinePumpMk2_C", 600),
];

/// sav_map_data._pipeFlowLimitPerMinute.
pub fn pipe_flow_limit_per_minute(type_path: Option<&str>) -> Option<i64> {
    let short = short_class_name(type_path.unwrap_or(""));
    PIPE_FLOW_M3_PER_MINUTE_BY_CLASS.iter().find(|(k, _)| *k == short).map(|(_, rate)| *rate)
}

/// sav_map_data._BOTTLENECK_SEGMENT_LIMIT.
const BOTTLENECK_SEGMENT_LIMIT: usize = 50;

/// sav_map_data._flowBottleneck.
pub fn flow_bottleneck(
    store: &SaveStore,
    index: &MapIndex,
    rated_instance_names: &[&[u8]],
    hovered_type_path: Option<&str>,
    rate_of_type_path: &dyn Fn(Option<&str>) -> Option<i64>,
    scope: &str,
    unit: &str,
) -> Option<Value> {
    let data: &[u8] = &store.data;
    struct Ranked<'a> {
        rate: i64,
        instance_name: &'a [u8],
        actor: &'a ActorHeader,
        type_path: String,
    }
    let mut ranked_segments: Vec<Ranked> = Vec::new();
    for &segment_instance_name in rated_instance_names {
        // getattr(header, "typePath", None): None for components/missing.
        let actor = match index.header_by_name(store, segment_instance_name) {
            Some(Header::Actor(a)) => Some(a),
            _ => None,
        };
        let type_path: Option<String> = actor.map(|a| a.type_path.to_string(data));
        let Some(rate) = rate_of_type_path(type_path.as_deref()) else { continue };
        // A rate implies a typePath, which implies an ActorHeader.
        ranked_segments.push(Ranked {
            rate,
            instance_name: segment_instance_name,
            actor: actor.expect("rated segment without actor header"),
            type_path: type_path.expect("rated segment without type path"),
        });
    }
    if ranked_segments.is_empty() {
        return None;
    }
    let slowest_rate = ranked_segments.iter().map(|r| r.rate).min().unwrap();
    let fastest_rate = ranked_segments.iter().map(|r| r.rate).max().unwrap();
    if slowest_rate >= fastest_rate {
        return None; // Uniform marks -- nothing is holding anything back.
    }
    let limiting_segments: Vec<&Ranked> =
        ranked_segments.iter().filter(|r| r.rate == slowest_rate).collect();
    let mut result = Map::new();
    result.insert("scope".into(), Value::String(scope.to_string()));
    result.insert("unit".into(), Value::String(unit.to_string()));
    result.insert("limitPerMinute".into(), Value::from(slowest_rate));
    result.insert("fastestPerMinute".into(), Value::from(fastest_rate));
    result.insert("limitingSegmentCount".into(), Value::from(limiting_segments.len() as i64));
    result.insert(
        "limitingSegments".into(),
        Value::Array(
            limiting_segments
                .iter()
                .take(BOTTLENECK_SEGMENT_LIMIT)
                .map(|r| {
                    let position =
                        [r.actor.position[0] as f64, r.actor.position[1] as f64, r.actor.position[2] as f64];
                    let [px, py] = project_xy(position[0], position[1]);
                    json!({
                        "instanceName": props::lossy(r.instance_name),
                        "label": readable_label(&r.type_path),
                        "position": [jnum(px), jnum(py), jnum(world_z_to_meters(position[2]))],
                        "worldPosition": [jnum(position[0]), jnum(position[1])],
                    })
                })
                .collect(),
        ),
    );
    if let Some(hovered_rate) = rate_of_type_path(hovered_type_path) {
        result.insert("hoveredPerMinute".into(), Value::from(hovered_rate));
        result.insert("hoveredIsLimiting".into(), Value::Bool(hovered_rate == slowest_rate));
    }
    Some(Value::Object(result))
}

/// sav_map_data._conveyorChainBottleneck.
pub fn conveyor_chain_bottleneck(
    store: &SaveStore,
    index: &MapIndex,
    chain_actor_info: &ActorSpecific,
    hovered_type_path: Option<&str>,
) -> Option<Value> {
    let data: &[u8] = &store.data;
    let ActorSpecific::ConveyorChain { belts, .. } = chain_actor_info else {
        return None;
    };
    let member_names: Vec<&[u8]> = belts
        .iter()
        .map(|b| b.belt.path_name.bytes(data))
        .filter(|p| !p.is_empty())
        .collect();
    flow_bottleneck(
        store,
        index,
        &member_names,
        hovered_type_path,
        &conveyor_items_per_minute,
        "line",
        "items/min",
    )
}

/// sav_map_data._pipeNetworkBottleneck.
pub fn pipe_network_bottleneck(
    store: &SaveStore,
    index: &MapIndex,
    member_names: &[String],
    hovered_type_path: Option<&str>,
) -> Option<Value> {
    let member_names: Vec<&[u8]> = member_names.iter().map(|s| s.as_bytes()).collect();
    flow_bottleneck(
        store,
        index,
        &member_names,
        hovered_type_path,
        &pipe_flow_limit_per_minute,
        "network",
        "m³/min",
    )
}

// ---------------------------------------------------------------------------
// Rated power (sav_map_data 2418-2478)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum RatedPower {
    Scalar(f64),
    Range(f64, f64),
}

pub enum ScaledPower {
    Scalar(f64),
    /// (min, max, mid) -- the Python 3-tuple.
    Range(f64, f64, f64),
}

/// sav_map_data._RATED_POWER_MW_BY_CLASSNAME (_loadRatedPowerMWByClassName).
fn rated_power_mw_by_class_name() -> &'static IndexMap<String, RatedPower> {
    static RATINGS: OnceLock<IndexMap<String, RatedPower>> = OnceLock::new();
    RATINGS.get_or_init(|| {
        let mut ratings: IndexMap<String, RatedPower> = IndexMap::new();
        for (class_name, entry) in &gamedata::get().buildings {
            // `if powerRange:` -- truthy = non-null, non-empty list.
            if let Some(Value::Array(power_range)) = entry.get("powerConsumptionRangeMW") {
                if !power_range.is_empty() {
                    ratings.insert(
                        class_name.clone(),
                        RatedPower::Range(
                            power_range[0].as_f64().unwrap_or(0.0),
                            power_range[1].as_f64().unwrap_or(0.0),
                        ),
                    );
                    continue;
                }
            }
            // `elif entry.get("powerConsumptionMW"):` -- truthy = nonzero.
            if let Some(power) = entry.get("powerConsumptionMW").and_then(Value::as_f64) {
                if power != 0.0 {
                    ratings.insert(class_name.clone(), RatedPower::Scalar(power));
                }
            }
        }
        ratings
    })
}

/// sav_map_data._VARIABLE_POWER_RANGE_MW_BY_RECIPE.
fn variable_power_range_mw_by_recipe() -> &'static IndexMap<String, RatedPower> {
    static RANGES: OnceLock<IndexMap<String, RatedPower>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let mut ranges: IndexMap<String, RatedPower> = IndexMap::new();
        for (class_name, entry) in &gamedata::get().recipes {
            if let Some(Value::Array(range)) = entry.get("variablePowerRangeMW") {
                if !range.is_empty() {
                    ranges.insert(
                        class_name.clone(),
                        RatedPower::Range(
                            range[0].as_f64().unwrap_or(0.0),
                            range[1].as_f64().unwrap_or(0.0),
                        ),
                    );
                }
            }
        }
        ranges
    })
}

pub const POWER_CLOCK_SPEED_EXPONENT: f64 = 1.321929;

/// sav_map_data._ratedPowerForTypePath.
pub fn rated_power_for_type_path(
    type_path: Option<&str>,
    recipe_path_name: Option<&str>,
) -> Option<RatedPower> {
    let type_path = type_path?;
    if let Some(recipe_path_name) = recipe_path_name {
        if !recipe_path_name.is_empty() {
            if let Some(range) =
                variable_power_range_mw_by_recipe().get(short_class_name(recipe_path_name))
            {
                return Some(*range);
            }
        }
    }
    rated_power_mw_by_class_name().get(short_class_name(type_path)).copied()
}

/// sav_map_data._scaleRatedPowerForClockSpeed.
pub fn scale_rated_power_for_clock_speed(
    rated_mw: RatedPower,
    clock_speed_fraction: f64,
) -> ScaledPower {
    let factor = clock_speed_fraction.powf(POWER_CLOCK_SPEED_EXPONENT);
    match rated_mw {
        RatedPower::Scalar(v) => ScaledPower::Scalar(py_round(v * factor, 1)),
        RatedPower::Range(lo, hi) => {
            let scaled_min = lo * factor;
            let scaled_max = hi * factor;
            ScaledPower::Range(
                py_round(scaled_min, 1),
                py_round(scaled_max, 1),
                py_round((scaled_min + scaled_max) / 2.0, 1),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// findItemLocations (sav_map_data 2615-2690)
// ---------------------------------------------------------------------------

pub fn find_item_locations(store: &SaveStore, index: &MapIndex, item_short_name: &str) -> Value {
    let data: &[u8] = &store.data;
    let empty: Vec<(Vec<u8>, i64)> = Vec::new();
    let entries = index.item_location_index.get(item_short_name.as_bytes()).unwrap_or(&empty);
    let is_fluid = is_fluid_item_path(item_short_name);

    let mut locations: Vec<(f64, Value)> = Vec::new();
    let mut total_count: f64 = 0.0; // Python starts at 0.0 -- totalCount is a float even for solids.
    for (instance_name, count) in entries {
        total_count += *count as f64;
        // Shouldn't be missing (the index is built from the same objects);
        // a component header would AttributeError in Python -- unreachable
        // for inventory holders, skipped here.
        let Some(Header::Actor(actor)) = index.header_by_name(store, instance_name) else {
            continue;
        };
        let position =
            [actor.position[0] as f64, actor.position[1] as f64, actor.position[2] as f64];
        let [px, py] = project_xy(position[0], position[1]);
        let type_path_bytes = actor.type_path.bytes(data);
        let type_path = actor.type_path.to_string(data);
        let mut label = readable_label(&type_path);
        if type_path_bytes == ITEM_PICKUP_TYPE_PATH.as_bytes() {
            label = "Dropped on the ground".to_string();
        }
        if type_path_bytes == PLAYER_TYPE_PATH.as_bytes() {
            let player_name = index
                .parse_object_by_name(store, instance_name)
                .and_then(|o| props::string(&o.properties, data, b"mCachedPlayerName"))
                .map(|s| s.to_string(data));
            label = match player_name {
                Some(name) if !name.is_empty() => format!("Player: {}", name),
                _ => "Player".to_string(),
            };
        }
        let (sort_key, count_value) = scaled_count(*count as f64, is_fluid, *count);
        locations.push((
            sort_key,
            json!({
                "instanceName": props::lossy(instance_name),
                "typePath": type_path,
                "label": label,
                "count": count_value,
                "position": [jnum(px), jnum(py), jnum(world_z_to_meters(position[2]))],
                "worldPosition": [jnum(position[0]), jnum(position[1])],
            }),
        ));
    }

    // Dimensional Depot pseudo-location (no position). `if depotCount:` --
    // truthy, so a zero count is skipped.
    if let Some(&depot_count) = index.dimensional_depot_by_item.get(item_short_name) {
        if depot_count != 0 {
            total_count += depot_count as f64;
            let (sort_key, count_value) = scaled_count(depot_count as f64, is_fluid, depot_count);
            locations.push((
                sort_key,
                json!({
                    "instanceName": "dimensional-depot",
                    "typePath": Value::Null,
                    "label": "Dimensional Depot",
                    "count": count_value,
                    "position": Value::Null,
                    "worldPosition": Value::Null,
                }),
            ));
        }
    }

    // Static pickups (slugs/spheres/hard drives/catalog drops) -- see
    // _collectStaticItemLocations. dict(staticEntry, count=...) keeps the
    // entry's key order with count replaced in place.
    if let Some(static_entries) = index.static_item_locations.get(item_short_name) {
        for static_entry in static_entries {
            total_count += static_entry.count as f64;
            let (sort_key, count_value) =
                scaled_count(static_entry.count as f64, is_fluid, static_entry.count);
            locations.push((
                sort_key,
                json!({
                    "instanceName": static_entry.instance_name,
                    "typePath": Value::Null,
                    "label": static_entry.label,
                    "count": count_value,
                    "position": static_entry.position,
                    "worldPosition": static_entry.world_position,
                }),
            ));
        }
    }

    // locations.sort(key=count, reverse=True) -- stable.
    locations.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("count NaN"));

    let total_value = if is_fluid {
        jnum(py_round(total_count / 1000.0, 1))
    } else {
        jnum(total_count) // a Python float either way
    };
    json!({
        "itemPath": item_short_name,
        "label": readable_label(item_short_name),
        "isFluid": is_fluid,
        "totalCount": total_value,
        "locations": Value::Array(locations.into_iter().map(|(_, v)| v).collect()),
    })
}

/// `round(count / scale, 1) if isFluid else count` plus the sort key the
/// final list is ordered by (the scaled value itself).
fn scaled_count(count_f: f64, is_fluid: bool, count_int: i64) -> (f64, Value) {
    if is_fluid {
        let rounded = py_round(count_f / 1000.0, 1);
        (rounded, jnum(rounded))
    } else {
        (count_f, Value::from(count_int))
    }
}

// ---------------------------------------------------------------------------
// aggregateSelectionInventory (sav_map_data 2707-2792)
// ---------------------------------------------------------------------------

pub fn aggregate_selection_inventory(
    store: &SaveStore,
    index: &MapIndex,
    instance_names: &[&str],
) -> Value {
    let data: &[u8] = &store.data;
    let mut solid: IndexMap<Vec<u8>, i64> = IndexMap::new();
    let mut fluid: IndexMap<String, f64> = IndexMap::new();

    let mut seen_instances: HashSet<&str> = HashSet::new();
    let mut connector_key: Vec<u8> = Vec::new();
    for &instance_name in instance_names {
        if !seen_instances.insert(instance_name) {
            continue;
        }
        let name_bytes = instance_name.as_bytes();
        let Some(object) = index.parse_object_by_name(store, name_bytes) else { continue };
        let properties = &object.properties;

        // Building/player/vehicle inventories.
        for component_object in inventory_component_objects(store, index, name_bytes, properties) {
            let Some(stacks) =
                props::array_structs(&component_object.properties, data, b"mInventoryStacks")
            else {
                continue;
            };
            for stack in stacks {
                if let Some((item_path, num_items)) = stack_item(stack, data) {
                    add_item(&mut solid, &mut fluid, short_name(item_path), num_items);
                }
            }
        }

        // Belt segments: only the items physically on THIS segment.
        if let Some(chain_reference) = props::object_ref(properties, data, b"mConveyorChainActor")
        {
            let chain_path = chain_reference.path_name.bytes(data);
            if !chain_path.is_empty() {
                if let Some(chain_actor) = index.parse_object_by_name(store, chain_path) {
                    for item_path in conveyor_chain_segment_item_paths(
                        &chain_actor.actor_specific,
                        data,
                        name_bytes,
                    ) {
                        if !item_path.is_empty() {
                            add_item(&mut solid, &mut fluid, short_name(item_path), 1);
                        }
                    }
                }
            }
        }

        // Pipe segments: mFluidBox is m3, scaled up 1000x to join the
        // inventory-stack fluid convention.
        if let Some(fluid_amount) = props::fluid_box(properties, data, b"mFluidBox") {
            if fluid_amount != 0.0 {
                for connector_suffix in PIPE_CONNECTOR_SUFFIXES {
                    connector_key.clear();
                    connector_key.extend_from_slice(name_bytes);
                    connector_key.extend_from_slice(connector_suffix.as_bytes());
                    let Some(connector_object) = index.parse_object_by_name(store, &connector_key)
                    else {
                        continue;
                    };
                    let network_id =
                        props::int(&connector_object.properties, data, b"mPipeNetworkID");
                    let fluid_label =
                        network_id.and_then(|id| index.pipe_network_id_to_fluid.get(&id));
                    if let Some(fluid_label) = fluid_label {
                        *fluid.entry(fluid_label.clone()).or_insert(0.0) += fluid_amount * 1000.0;
                        break;
                    }
                }
            }
        }
    }

    inventory_rows(solid, fluid)
}

// ---------------------------------------------------------------------------
// collectBuildingInfo (sav_map_data 2794-2890)
// ---------------------------------------------------------------------------

pub fn collect_building_info(store: &SaveStore, index: &MapIndex, type_paths: &[String]) -> Value {
    let data: &[u8] = &store.data;
    let empty: Vec<String> = Vec::new();

    let mut all_instance_names: Vec<&str> = Vec::new();
    let mut recipe_counts: IndexMap<String, i64> = IndexMap::new(); // insertion order == recipeOrder
    let mut no_recipe_count: i64 = 0;
    let mut has_recipe_capable_instance = false;
    let mut total_power_min_mw: f64 = 0.0;
    let mut total_power_max_mw: f64 = 0.0;
    let mut has_power_consumer = false;
    let mut total_power_production_mw: f64 = 0.0;
    let mut has_power_producer = false;

    for type_path in type_paths {
        let is_generator = type_path.contains("Generator");
        let instance_names = index.instance_names_by_type_path.get(type_path).unwrap_or(&empty);
        for instance_name in instance_names {
            all_instance_names.push(instance_name.as_str());
        }
        for instance_name in instance_names {
            // Lightweight buildables -- no recipe/power/inventory concept.
            let Some(object) = index.parse_object_by_name(store, instance_name.as_bytes()) else {
                continue;
            };
            let properties = &object.properties;

            let recipe = find_prop(properties, data, b"mCurrentRecipe");
            let recipe_path_name: Option<String> = match recipe {
                Some(PropertyValue::Object(r)) if !r.path_name.bytes(data).is_empty() => {
                    Some(r.path_name.to_string(data))
                }
                _ => None,
            };
            if let Some(recipe_path_name) = &recipe_path_name {
                has_recipe_capable_instance = true;
                let recipe_name =
                    super::collectors::trains_progression::recipe_label(recipe_path_name);
                *recipe_counts.entry(recipe_name).or_insert(0) += 1;
            } else if recipe.is_some() {
                // A recipe reference exists but couldn't be resolved to a
                // name -- counts as "no recipe set".
                has_recipe_capable_instance = true;
                no_recipe_count += 1;
            }

            let can_overclock = recipe.is_some()
                || find_prop(properties, data, b"mExtractableResource").is_some()
                || is_generator;
            let clock_speed_fraction = if can_overclock {
                props::float(properties, data, b"mCurrentPotential").unwrap_or(1.0)
            } else {
                1.0
            };

            if is_generator {
                if let Some(power_component) =
                    resolve_component_object(store, index, properties, b"mPowerInfo")
                {
                    let production = props::float(
                        &power_component.properties,
                        data,
                        b"mDynamicProductionCapacity",
                    )
                    .or_else(|| props::float(&power_component.properties, data, b"mBaseProduction"));
                    if let Some(production) = production {
                        has_power_producer = true;
                        total_power_production_mw += production;
                    }
                }
            } else if let Some(rated) =
                rated_power_for_type_path(Some(type_path), recipe_path_name.as_deref())
            {
                has_power_consumer = true;
                match scale_rated_power_for_clock_speed(rated, clock_speed_fraction) {
                    ScaledPower::Scalar(scaled) => {
                        total_power_min_mw += scaled;
                        total_power_max_mw += scaled;
                    }
                    ScaledPower::Range(lo, hi, _) => {
                        total_power_min_mw += lo;
                        total_power_max_mw += hi;
                    }
                }
            }
        }
    }

    let mut result = Map::new();
    result.insert("count".into(), Value::from(all_instance_names.len() as i64));
    result.insert(
        "inventory".into(),
        aggregate_selection_inventory(store, index, &all_instance_names),
    );
    if has_recipe_capable_instance {
        let mut recipe_rows: Vec<(i64, Value)> = recipe_counts
            .into_iter()
            .map(|(label, count)| (count, json!({"label": label, "count": count})))
            .collect();
        if no_recipe_count != 0 {
            recipe_rows.push((
                no_recipe_count,
                json!({"label": "No recipe set", "count": no_recipe_count}),
            ));
        }
        // sort(key=count, reverse=True) -- stable.
        recipe_rows.sort_by(|a, b| b.0.cmp(&a.0));
        result.insert(
            "recipes".into(),
            Value::Array(recipe_rows.into_iter().map(|(_, v)| v).collect()),
        );
    }
    if has_power_consumer {
        let rounded_min = py_round(total_power_min_mw, 1);
        let rounded_max = py_round(total_power_max_mw, 1);
        if rounded_min == rounded_max {
            result.insert("powerConsumptionMW".into(), jnum(rounded_min));
        } else {
            result.insert(
                "powerConsumptionRangeMW".into(),
                json!([jnum(rounded_min), jnum(rounded_max)]),
            );
        }
    }
    if has_power_producer {
        result.insert("powerProductionMW".into(), jnum(py_round(total_power_production_mw, 1)));
    }
    Value::Object(result)
}

// ---------------------------------------------------------------------------
// collectVehicleInfo (sav_map_data 2938-2989)
// ---------------------------------------------------------------------------

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `"DS_DOCKED" in sav_parse.toString(dockingState)`: toString stringifies
/// every nested element, so the check is a substring test over any string
/// leaf. mCurrentDockingState is a StructProperty wrapping an inner
/// EnumProperty "State" whose value is "EDroneDockingState::DS_DOCKED" --
/// recurse through struct layers to reach it.
fn docking_state_is_docked(value: &PropertyValue, data: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"DS_DOCKED";
    match value {
        PropertyValue::Enum { enum_name, value } => {
            enum_name.is_some_and(|s| find_subslice(s.bytes(data), NEEDLE))
                || find_subslice(value.bytes(data), NEEDLE)
        }
        PropertyValue::Byte { enum_name, value } => {
            enum_name.is_some_and(|e| find_subslice(e.bytes(data), NEEDLE))
                || matches!(value, ByteVal::Str(s) if find_subslice(s.bytes(data), NEEDLE))
        }
        PropertyValue::Str(s) => find_subslice(s.bytes(data), NEEDLE),
        PropertyValue::Struct(StructValue::Props(pl)) => {
            pl.props.iter().any(|p| docking_state_is_docked(&p.value, data))
        }
        PropertyValue::Array(ArrayValue::Structs(lists)) => lists
            .iter()
            .any(|pl| pl.props.iter().any(|p| docking_state_is_docked(&p.value, data))),
        _ => false,
    }
}

pub fn collect_vehicle_info(store: &SaveStore, index: &MapIndex, type_paths: &[String]) -> Value {
    let data: &[u8] = &store.data;
    let empty: Vec<String> = Vec::new();
    let mut count: i64 = 0;
    let mut automated_count: i64 = 0;
    let mut docked_count: i64 = 0;
    let mut has_docking_state = false;
    let mut cargo_components: Vec<Object> = Vec::new();
    let mut fuel_components: Vec<Object> = Vec::new();
    let mut fuel_key: Vec<u8> = Vec::new();
    for type_path in type_paths {
        for instance_name in index.instance_names_by_type_path.get(type_path).unwrap_or(&empty) {
            count += 1;
            let name_bytes = instance_name.as_bytes();
            let Some(object) = index.parse_object_by_name(store, name_bytes) else { continue };
            let properties = &object.properties;
            if let Some(storage_component) =
                vehicle_storage_component(store, index, name_bytes, properties)
            {
                cargo_components.push(storage_component);
            }
            fuel_key.clear();
            fuel_key.extend_from_slice(name_bytes);
            fuel_key.extend_from_slice(b".FuelInventory");
            if let Some(fuel_component) = index.parse_object_by_name(store, &fuel_key) {
                fuel_components.push(fuel_component);
            }
            if find_prop(properties, data, b"mCurrentVehiclePathSegment").is_some() {
                automated_count += 1;
            }
            if let Some(docking_state) = find_prop(properties, data, b"mCurrentDockingState") {
                has_docking_state = true;
                if docking_state_is_docked(docking_state, data) {
                    docked_count += 1;
                }
            }
        }
    }
    let mut result = Map::new();
    result.insert("count".into(), Value::from(count));
    result.insert("inventory".into(), sum_inventory_component_stacks(data, &cargo_components));
    let fuel_inventory = sum_inventory_component_stacks(data, &fuel_components);
    // `if fuelInventory:` -- non-empty list.
    if fuel_inventory.as_array().is_some_and(|rows| !rows.is_empty()) {
        result.insert("fuelInventory".into(), fuel_inventory);
    }
    if automated_count != 0 {
        result.insert("automatedCount".into(), Value::from(automated_count));
    }
    if has_docking_state {
        result.insert("dockedCount".into(), Value::from(docked_count));
    }
    Value::Object(result)
}

// ---------------------------------------------------------------------------
// collectTrainInfo (sav_map_data 2991-3041)
// ---------------------------------------------------------------------------

fn composition_label(locomotives: i64, wagons: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if locomotives != 0 {
        parts.push(format!("{} loco{}", locomotives, if locomotives != 1 { "s" } else { "" }));
    }
    if wagons != 0 {
        parts.push(format!("{} wagon{}", wagons, if wagons != 1 { "s" } else { "" }));
    }
    if parts.is_empty() {
        "empty".to_string()
    } else {
        parts.join(" + ")
    }
}

pub fn collect_train_info(store: &SaveStore, index: &MapIndex) -> Value {
    let data: &[u8] = &store.data;
    let trains = &index.train_consists;
    let mut car_count: i64 = 0;
    let mut locomotive_count: i64 = 0;
    let mut wagon_count: i64 = 0;
    let mut composition_counts: IndexMap<(i64, i64), i64> = IndexMap::new();
    let mut cargo_components: Vec<Object> = Vec::new();
    for train in trains {
        let locomotives =
            train.cars.iter().filter(|car| car.type_path == LOCOMOTIVE_TYPE_PATH).count() as i64;
        let wagons = train.cars.len() as i64 - locomotives;
        car_count += train.cars.len() as i64;
        locomotive_count += locomotives;
        wagon_count += wagons;
        *composition_counts.entry((locomotives, wagons)).or_insert(0) += 1;
        for car in &train.cars {
            if car.type_path != FREIGHT_WAGON_TYPE_PATH {
                continue;
            }
            let Some(car_object) = index.parse_object_by_name(store, car.id.as_bytes()) else {
                continue;
            };
            if let Some(storage_component) =
                vehicle_storage_component(store, index, car.id.as_bytes(), &car_object.properties)
            {
                cargo_components.push(storage_component);
            }
        }
    }

    // sorted(compositionCounts, key=(locos + wagons, locos)) -- keys are
    // unique, so tie behavior never matters.
    let mut keys: Vec<(i64, i64)> = composition_counts.keys().copied().collect();
    keys.sort_by_key(|&(locomotives, wagons)| (locomotives + wagons, locomotives));
    let consist_breakdown: Vec<Value> = keys
        .into_iter()
        .map(|(locomotives, wagons)| {
            json!({
                "label": composition_label(locomotives, wagons),
                "count": composition_counts[&(locomotives, wagons)],
            })
        })
        .collect();

    json!({
        "count": trains.len() as i64,
        "carCount": car_count,
        "locomotiveCount": locomotive_count,
        "wagonCount": wagon_count,
        "consistBreakdown": consist_breakdown,
        "inventory": sum_inventory_component_stacks(data, &cargo_components),
    })
}
