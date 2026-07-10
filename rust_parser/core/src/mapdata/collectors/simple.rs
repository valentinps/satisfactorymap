//! The structurally simple collectors: players, creatures, hub,
//! gameSettings, vehicles, dimensionalDepot (sav_map_data.py lines
//! ~668-696, 1505-1580, 3367-3391).

use crate::extract::find_prop;
use crate::mapdata::consts::*;
use crate::mapdata::geometry::{
    meters_to_pixel_length, project_xy, rendered_yaw, world_z_to_meters,
};
use crate::mapdata::jsonval::jnum;
use crate::mapdata::names::readable_label;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use serde_json::{json, Value};

fn f3(v: [f32; 3]) -> [f64; 3] {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

fn f4(v: [f32; 4]) -> [f64; 4] {
    [v[0] as f64, v[1] as f64, v[2] as f64, v[3] as f64]
}

/// The shared "{points: [x,y,z...], ids: [...]}" header-only bucket used by
/// collectPlayers and collectHub.
fn points_and_ids(scan: &SaveScan, type_path: &str) -> Value {
    let data = scan.data();
    let mut points: Vec<Value> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    for slot in scan.actor_slots_of_type(&[type_path]) {
        let actor = scan.actor(slot);
        let position = f3(actor.position);
        let [px, py] = project_xy(position[0], position[1]);
        points.push(jnum(px));
        points.push(jnum(py));
        points.push(jnum(world_z_to_meters(position[2])));
        ids.push(Value::String(props::lossy(actor.instance_name.bytes(data))));
    }
    json!({"points": points, "ids": ids})
}

pub fn collect_players(scan: &SaveScan) -> Value {
    points_and_ids(scan, PLAYER_TYPE_PATH)
}

pub fn collect_hub(scan: &SaveScan) -> Value {
    points_and_ids(scan, HUB_TYPE_PATH)
}

pub fn collect_creatures(scan: &SaveScan) -> Value {
    let data = scan.data();
    // Single-typePath bucket dict in Python; list-shaped output.
    let mut points: Vec<Value> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    let slots = scan.actor_slots_of_type(&[LIZARD_DOGGO_TYPE_PATH]);
    if slots.is_empty() {
        return json!([]);
    }
    for slot in &slots {
        let actor = scan.actor(*slot);
        let position = f3(actor.position);
        let [px, py] = project_xy(position[0], position[1]);
        points.push(jnum(px));
        points.push(jnum(py));
        points.push(jnum(world_z_to_meters(position[2])));
        ids.push(Value::String(props::lossy(actor.instance_name.bytes(data))));
    }
    json!([{
        "typePath": LIZARD_DOGGO_TYPE_PATH,
        "label": readable_label(LIZARD_DOGGO_TYPE_PATH),
        "points": points,
        "ids": ids,
    }])
}

/// sav_map_data._humanizeEnumValue over a raw EnumProperty value.
fn humanize_enum_value(value: Option<&PropertyValue>, data: &[u8]) -> Value {
    // Python receives ['EnumTypeName', 'EnumTypeName::SHORT_ValueName'] only
    // when the enum kept its type name; anything else -> None.
    let Some(PropertyValue::Enum { enum_name: Some(_), value }) = value else {
        return Value::Null;
    };
    let raw = props::lossy(value.bytes(data));
    let value_name = match raw.rfind("::") {
        Some(i) => &raw[i + 2..],
        None => raw.as_str(),
    };
    // re.sub(r"^[A-Z0-9]+_", "", ...): strip one leading ALLCAPS/digit run
    // followed by an underscore.
    let bytes = value_name.as_bytes();
    let mut prefix_len = 0usize;
    while prefix_len < bytes.len()
        && (bytes[prefix_len].is_ascii_uppercase() || bytes[prefix_len].is_ascii_digit())
    {
        prefix_len += 1;
    }
    let stripped = if prefix_len > 0 && bytes.get(prefix_len) == Some(&b'_') {
        &value_name[prefix_len + 1..]
    } else {
        value_name
    };
    // re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", ...): space at each
    // lower/digit -> upper boundary.
    let mut out = String::with_capacity(stripped.len() + 8);
    let mut prev: Option<char> = None;
    for ch in stripped.chars() {
        if ch.is_ascii_uppercase() {
            if let Some(p) = prev {
                if p.is_ascii_lowercase() || p.is_ascii_digit() {
                    out.push(' ');
                }
            }
        }
        out.push(ch);
        prev = Some(ch);
    }
    Value::String(out)
}

pub fn collect_game_settings(scan: &SaveScan) -> Value {
    let data = scan.data();
    for &slot in &scan.game_state_objects {
        // First match, same as the old early-returning scan.
        let properties = &scan.object(slot).properties;
        let power = match find_prop(properties, data, b"mEnergyCostMultiplier") {
            Some(PropertyValue::Float(f)) => jnum(*f as f64),
            Some(PropertyValue::Double(f)) => jnum(*f),
            _ => Value::Null,
        };
        return json!({
            "powerCostMultiplier": power,
            "nodePuritySettings":
                humanize_enum_value(find_prop(properties, data, b"mNodePuritySettings"), data),
            "nodeRandomization":
                humanize_enum_value(find_prop(properties, data, b"mNodeRandomization"), data),
        });
    }
    json!({})
}

/// sav_map_data._vehicleFootprintPixels.
pub fn vehicle_footprint_pixels(type_path: &str) -> Value {
    match vehicle_footprint_meters(type_path) {
        Some((length_meters, width_meters)) => json!([
            jnum(meters_to_pixel_length(length_meters / 2.0)),
            jnum(meters_to_pixel_length(width_meters / 2.0)),
        ]),
        None => Value::Null,
    }
}

pub fn collect_vehicles(scan: &SaveScan) -> Value {
    let data = scan.data();
    let railcars = railcar_type_paths();
    let vehicle_type_paths: Vec<&str> = VEHICLE_ICONS_BY_TYPE_PATH
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !railcars.contains(p))
        .collect();
    struct Bucket {
        type_path: String,
        label: String,
        icon: &'static str,
        points: Vec<Value>,
        ids: Vec<Value>,
        footprint: Value,
    }
    let mut buckets: Vec<Bucket> = Vec::new();
    for slot in scan.actor_slots_of_type(&vehicle_type_paths) {
        let actor = scan.actor(slot);
        let type_path = props::lossy(actor.type_path.bytes(data));
        let idx = match buckets.iter().position(|b| b.type_path == type_path) {
            Some(i) => i,
            None => {
                buckets.push(Bucket {
                    label: readable_label(&type_path),
                    icon: vehicle_icon(&type_path).expect("vehicle icon"),
                    points: Vec::new(),
                    ids: Vec::new(),
                    footprint: vehicle_footprint_pixels(&type_path),
                    type_path,
                });
                buckets.len() - 1
            }
        };
        let bucket = &mut buckets[idx];
        let position = f3(actor.position);
        let [px, py] = project_xy(position[0], position[1]);
        bucket.points.push(jnum(px));
        bucket.points.push(jnum(py));
        bucket.points.push(jnum(rendered_yaw(f4(actor.rotation))));
        bucket.points.push(jnum(world_z_to_meters(position[2])));
        bucket.ids.push(Value::String(props::lossy(actor.instance_name.bytes(data))));
    }
    // sorted(typeBuckets.items(), key=entry[1]["label"]) -- stable.
    buckets.sort_by(|a, b| a.label.cmp(&b.label));
    Value::Array(
        buckets
            .into_iter()
            .map(|b| {
                json!({
                    "typePath": b.type_path,
                    "label": b.label,
                    "icon": b.icon,
                    "points": b.points,
                    "ids": b.ids,
                    "footprintPixels": b.footprint,
                })
            })
            .collect(),
    )
}

const CENTRAL_STORAGE_SUBSYSTEM_TYPE_PATH: &str = "/Script/FactoryGame.FGCentralStorageSubsystem";

pub fn collect_dimensional_depot_contents(scan: &SaveScan) -> Value {
    let data = scan.data();
    let slots = scan.actor_slots_of_type(&[CENTRAL_STORAGE_SUBSYSTEM_TYPE_PATH]);
    let Some(&last) = slots.last() else {
        return json!([]);
    };
    let name = scan.actor(last).instance_name.bytes(data);
    let Some(object) = scan.object_by_name(name) else {
        return json!([]);
    };
    let stored_items: &[PropList] = match props::array_structs(&object.properties, data, b"mStoredItems") {
        Some(v) => v,
        None => &[], // `or []`
    };
    struct Row {
        item_path: String,
        label: String,
        count: i32,
    }
    let mut items: Vec<Row> = Vec::new();
    for entry in stored_items {
        // ItemClass is an ObjectProperty reference; `not getattr(itemClass,
        // "pathName", None)` also drops empty pathName.
        let Some(item_class) = props::object_ref(entry, data, b"ItemClass") else { continue };
        let path = item_class.path_name.bytes(data);
        if path.is_empty() {
            continue;
        }
        // `not amount` drops both missing and 0.
        let amount = props::int(entry, data, b"Amount").unwrap_or(0);
        if amount == 0 {
            continue;
        }
        let short = props::lossy(props::short_name(path));
        items.push(Row { label: readable_label(&short), item_path: short, count: amount });
    }
    // items.sort(key=count, reverse=True) -- stable descending.
    items.sort_by(|a, b| b.count.cmp(&a.count));
    Value::Array(
        items
            .into_iter()
            .map(|r| json!({"itemPath": r.item_path, "label": r.label, "count": r.count}))
            .collect(),
    )
}
