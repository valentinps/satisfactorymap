//! collectBuildings + the lightweight-buildable helpers -- port of
//! sav_map_data.py lines ~906-1037 (_findLightweightBuildableGroups,
//! _lightweightBeamLengthCm, _newBuildingBucket, _appendBuildingInstance,
//! collectBuildings).

use crate::mapdata::categories::{categorize_subcategory, categorize_type_path, is_hidden_class};
use crate::mapdata::consts::*;
use crate::mapdata::geometry::{
    footprint_for_instance, footprint_pixels, project_xy, short_class_name, world_z_to_meters,
};
use crate::mapdata::jsonval::{jnum, py_hypot};
use crate::mapdata::names::readable_label;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

fn f3(v: [f32; 3]) -> [f64; 3] {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

fn f4(v: [f32; 4]) -> [f64; 4] {
    [v[0] as f64, v[1] as f64, v[2] as f64, v[3] as f64]
}

/// _findLightweightBuildableGroups: the FGLightweightBuildableSubsystem
/// actor's decoded actorSpecificInfo minus the leading lightweightVersion --
/// [(buildItemTypePath, [instance, ...]), ...].
pub(crate) fn find_lightweight_buildable_groups<'s>(
    scan: &'s SaveScan<'_>,
) -> &'s [LightweightGroup] {
    match scan.lightweight_subsystem_object().map(|o| &o.actor_specific) {
        Some(ActorSpecific::Lightweight { items, .. }) => items,
        _ => &[],
    }
}

/// _lightweightBeamLengthCm: BeamLength (FloatProperty, centimeters, f32
/// widened to f64) off the per-instance data property; None when the
/// property is absent or the instance carries no data property at all.
/// (Python `if not lightweightDataProperty` only fires on None -- the
/// converted (properties, propertyTypes) tuple is always truthy.)
fn lightweight_beam_length_cm(instance: &LightweightInstance, data: &[u8]) -> Option<f64> {
    let pl = instance.data_property.as_ref()?;
    props::float(pl, data, b"BeamLength")
}

struct Bucket {
    label: String,
    points: Vec<Value>,
    ids: Vec<Value>,
    footprint_pixels: Option<[f64; 2]>,
    /// Sparse pointIndex -> flat rotated-silhouette polygon, in point
    /// insertion order (a Python dict with int keys; orjson OPT_NON_STR_KEYS
    /// serializes them as decimal strings).
    tilted_footprints: Vec<(usize, Vec<f64>)>,
    max_footprint_radius: f64,
}

/// _newBuildingBucket.
fn new_building_bucket(type_path: &str) -> Bucket {
    let footprint = footprint_pixels(type_path);
    Bucket {
        label: readable_label(type_path),
        points: Vec::new(),
        ids: Vec::new(),
        tilted_footprints: Vec::new(),
        // math.hypot(a, b) if footprint is not None else 0.0.
        max_footprint_radius: match footprint {
            Some([a, b]) => py_hypot(a, b),
            None => 0.0,
        },
        footprint_pixels: footprint,
    }
}

/// _appendBuildingInstance.
fn append_building_instance(
    bucket: &mut Bucket,
    type_path: &str,
    rotation: [f64; 4],
    position: [f64; 3],
    instance_id: String,
    beam_length_cm: Option<f64>,
) {
    let [px, py] = project_xy(position[0], position[1]);
    let (yaw, tilted_polygon) = footprint_for_instance(
        type_path,
        rotation,
        bucket.footprint_pixels.as_ref(),
        beam_length_cm,
    );
    if let Some(polygon) = tilted_polygon {
        // max(hypot(p[i], p[i+1]) for pairs) -- Python max fold: the first
        // value wins ties, strictly-greater replaces.
        let mut polygon_radius = py_hypot(polygon[0], polygon[1]);
        for i in (2..polygon.len()).step_by(2) {
            let radius = py_hypot(polygon[i], polygon[i + 1]);
            if radius > polygon_radius {
                polygon_radius = radius;
            }
        }
        bucket.tilted_footprints.push((bucket.ids.len(), polygon));
        if polygon_radius > bucket.max_footprint_radius {
            bucket.max_footprint_radius = polygon_radius;
        }
    }
    bucket.points.push(jnum(px));
    bucket.points.push(jnum(py));
    bucket.points.push(jnum(yaw));
    bucket.points.push(jnum(world_z_to_meters(position[2])));
    bucket.ids.push(Value::String(instance_id));
}

/// Every placed SPWN (Build_PortalPotty_C) spawns its own BUILD_Potty_mk1_C
/// toilet actor at the machine, and the save records no parent link on it --
/// so proximity is the discriminator. Toilets within this radius of a SPWN
/// are dropped from the payload (the SPWN's own marker is the thing the
/// player placed and recognizes); the HUB's built-in toilet, nowhere near a
/// SPWN, stays in the "The HUB" group.
const SPWN_CLASS: &str = "Build_PortalPotty_C";
const SPWN_TOILET_CLASS: &str = "BUILD_Potty_mk1_C";
const SPWN_TOILET_RADIUS_CM: f64 = 1000.0;

fn near_any(positions: &[[f64; 2]], x: f64, y: f64, radius_cm: f64) -> bool {
    positions
        .iter()
        .any(|p| (p[0] - x).powi(2) + (p[1] - y).powi(2) <= radius_cm * radius_cm)
}

/// collectBuildings -- payload["buildingCategories"].
pub fn collect_buildings(scan: &SaveScan) -> Value {
    let data = scan.data();
    let line_rendered = line_rendered_type_paths();
    // category -> typePath -> bucket, both insertion-ordered (Python dict
    // semantics): category/typePath order follows each typePath's FIRST
    // instance, bucket contents follow per-typePath save order.
    let mut category_buckets: IndexMap<&'static str, IndexMap<String, Bucket>> = IndexMap::new();

    // Pre-pass: SPWN world positions, for the toilet merge above.
    let mut spwn_positions: Vec<[f64; 2]> = Vec::new();
    for (_, seq_headers) in &scan.actor_seqs_by_type_path {
        let type_path = scan.actor(seq_headers[0].1).type_path.to_string(data);
        if short_class_name(&type_path) == SPWN_CLASS {
            for &(_, slot) in seq_headers {
                let p = scan.actor(slot).position;
                spwn_positions.push([p[0] as f64, p[1] as f64]);
            }
        }
    }

    for (_, seq_headers) in &scan.actor_seqs_by_type_path {
        // The bucket's key bytes come from its first actor; use that actor's
        // StrRef so wide (UTF-16) type paths decode exactly like Python str.
        let type_path = scan.actor(seq_headers[0].1).type_path.to_string(data);
        // The HUB (Build_TradingPost) is deliberately NOT excluded here: it
        // renders as a real 14x26m building (Special -> "The HUB", clearance
        // data from buildings.json) alongside collect_hub's icon pin.
        if line_rendered.contains(type_path.as_str())
            || EXCLUDED_BUILDING_TYPE_PATHS.contains(&type_path.as_str())
            || is_hidden_class(&type_path)
        {
            continue;
        }
        if VEHICLE_ICONS_BY_TYPE_PATH.iter().any(|(p, _)| *p == type_path) {
            continue; // Surfaced by collectVehicles, not an "Unknown" building.
        }
        if !(type_path.contains("/Buildable/") || type_path.contains("/Build_")) {
            continue;
        }
        // SPWN toilet merge (see SPWN_TOILET_RADIUS_CM above): filtered
        // BEFORE bucket creation so a save where every toilet belongs to a
        // SPWN doesn't leave an empty zero-count sidebar row behind.
        let is_spwn_toilet = short_class_name(&type_path) == SPWN_TOILET_CLASS;
        let kept_slots: Vec<_> = seq_headers
            .iter()
            .map(|&(_, slot)| slot)
            .filter(|&slot| {
                if !is_spwn_toilet {
                    return true;
                }
                let p = scan.actor(slot).position;
                !near_any(&spwn_positions, p[0] as f64, p[1] as f64, SPWN_TOILET_RADIUS_CM)
            })
            .collect();
        if kept_slots.is_empty() {
            continue;
        }
        let category = categorize_type_path(&type_path);
        let bucket = category_buckets
            .entry(category)
            .or_default()
            .entry(type_path.clone())
            .or_insert_with(|| new_building_bucket(&type_path));
        for slot in kept_slots {
            let actor = scan.actor(slot);
            append_building_instance(
                bucket,
                &type_path,
                f4(actor.rotation),
                f3(actor.position),
                props::lossy(actor.instance_name.bytes(data)),
                None,
            );
        }
    }

    for group in find_lightweight_buildable_groups(scan) {
        let type_path = group.type_path.to_string(data);
        if line_rendered.contains(type_path.as_str())
            || EXCLUDED_BUILDING_TYPE_PATHS.contains(&type_path.as_str())
            || is_hidden_class(&type_path)
        {
            continue;
        }
        let category = categorize_type_path(&type_path);
        let bucket = category_buckets
            .entry(category)
            .or_default()
            .entry(type_path.clone())
            .or_insert_with(|| new_building_bucket(&type_path));
        for (idx, instance) in group.instances.iter().enumerate() {
            append_building_instance(
                bucket,
                &type_path,
                instance.rotation,
                instance.position,
                format!("LightweightBuildable:{}:{}", type_path, idx),
                lightweight_beam_length_cm(instance, data),
            );
        }
    }

    let mut building_categories: Vec<Value> = Vec::new();
    for (category, type_buckets) in category_buckets {
        let mut types: Vec<Value> = Vec::new();
        for (type_path, bucket) in type_buckets {
            let footprint: Value = match bucket.footprint_pixels {
                Some([a, b]) => json!([jnum(a), jnum(b)]),
                None => Value::Null,
            };
            // `bucket["tiltedFootprints"] or None`: {} -> None.
            let tilted: Value = if bucket.tilted_footprints.is_empty() {
                Value::Null
            } else {
                let mut map = Map::new();
                for (idx, polygon) in bucket.tilted_footprints {
                    map.insert(
                        idx.to_string(),
                        Value::Array(polygon.into_iter().map(jnum).collect()),
                    );
                }
                Value::Object(map)
            };
            types.push(json!({
                "typePath": type_path,
                "label": bucket.label,
                "points": bucket.points,
                "ids": bucket.ids,
                "footprintPixels": footprint,
                "tiltedFootprints": tilted,
                "maxFootprintRadius": jnum(bucket.max_footprint_radius),
                "renderType": if bucket.footprint_pixels.is_some() { "rect" } else { "circle" },
                "subcategory": categorize_subcategory(&type_path),
            }));
        }
        building_categories.push(json!({"category": category, "types": types}));
    }
    Value::Array(building_categories)
}
