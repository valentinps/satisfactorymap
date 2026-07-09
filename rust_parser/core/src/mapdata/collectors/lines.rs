//! Line-rendered buckets: collectSplinePaths / collectSplinePathGroups /
//! collectPowerLines / _annotateLineKinds (sav_map_data.py lines ~1039-1236,
//! 3393-3412). The per-segment spline geometry itself is
//! crate::extract::spline_polylines (the verbatim port both backends gate
//! against).

use crate::extract::spline_polylines;
use crate::gamedata;
use crate::mapdata::categories::{categorize_subcategory, categorize_type_path};
use crate::mapdata::consts::*;
use crate::mapdata::geometry::{project_xy, projection_params, world_z_to_meters};
use crate::mapdata::jsonval::jnum;
use crate::mapdata::names::readable_label;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use serde_json::{json, Value};

fn flat_points_value(flat: Vec<f64>) -> Value {
    Value::Array(flat.into_iter().map(jnum).collect())
}

/// collectSplinePaths: {"polylines", "ids", "pointStride": 7}.
fn collect_spline_paths(scan: &SaveScan, type_paths: &[&str], spline_property: &str) -> Value {
    let owned: Vec<String> = type_paths.iter().map(|s| s.to_string()).collect();
    let bulk = spline_polylines(scan.store, &owned, spline_property, &projection_params());
    let mut polylines: Vec<Value> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    for (instance_name, _type_path, flat) in bulk {
        polylines.push(flat_points_value(flat));
        ids.push(Value::String(instance_name));
    }
    json!({"polylines": polylines, "ids": ids, "pointStride": 7})
}

/// re.search(r"Mk\.?\s*\d+", label): first "Mk" + optional "." + whitespace
/// run + at least one digit.
fn mark_match(label: &str) -> Option<&str> {
    let bytes = label.as_bytes();
    let mut start = 0usize;
    while let Some(rel) = label[start..].find("Mk") {
        let begin = start + rel;
        let mut i = begin + 2;
        if bytes.get(i) == Some(&b'.') {
            i += 1;
        }
        while matches!(bytes.get(i), Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')) {
            i += 1;
        }
        let digits_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i > digits_start {
            return Some(&label[begin..i]);
        }
        start = begin + 1;
    }
    None
}

/// collectSplinePathGroups: one bucket per readable label, sorted by "mark".
fn collect_spline_path_groups(
    scan: &SaveScan,
    type_paths: &[&str],
    spline_property: &str,
) -> Value {
    // labelByTypePath in given order; typePathByLabel keeps the FIRST
    // typePath per label.
    let labels: Vec<(String, String)> =
        type_paths.iter().map(|tp| (tp.to_string(), readable_label(tp))).collect();
    let type_path_by_label = |label: &str| -> Option<&str> {
        labels.iter().find(|(_, l)| l == label).map(|(tp, _)| tp.as_str())
    };
    let label_of = |type_path: &str| -> &str {
        &labels.iter().find(|(tp, _)| tp == type_path).expect("typePath label").1
    };

    struct Group {
        label: String,
        polylines: Vec<Value>,
        ids: Vec<Value>,
    }
    let mut groups: Vec<Group> = Vec::new();

    let owned: Vec<String> = type_paths.iter().map(|s| s.to_string()).collect();
    for (instance_name, type_path, flat) in
        spline_polylines(scan.store, &owned, spline_property, &projection_params())
    {
        let label = label_of(&type_path);
        let idx = match groups.iter().position(|g| g.label == label) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    label: label.to_string(),
                    polylines: Vec::new(),
                    ids: Vec::new(),
                });
                groups.len() - 1
            }
        };
        groups[idx].polylines.push(flat_points_value(flat));
        groups[idx].ids.push(Value::String(instance_name));
    }

    let mut out: Vec<(String, Value)> = groups
        .into_iter()
        .map(|g| {
            let mark = mark_match(&g.label).unwrap_or(&g.label).to_string();
            let representative = type_path_by_label(&g.label);
            let entry = json!({
                "label": g.label,
                "mark": mark,
                "typePath": representative,
                "category": match representative {
                    Some(tp) => Value::String(categorize_type_path(tp).to_string()),
                    None => Value::String(OTHER_CATEGORY.to_string()),
                },
                "subcategory": match representative {
                    Some(tp) => categorize_subcategory(tp)
                        .map(|s| Value::String(s.to_string()))
                        .unwrap_or(Value::Null),
                    None => Value::Null,
                },
                "polylines": g.polylines,
                "ids": g.ids,
                "pointStride": 7,
            });
            (mark, entry)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0)); // stable, by mark
    Value::Array(out.into_iter().map(|(_, v)| v).collect())
}

/// collectPowerLines: straight source->destination pairs from
/// mWireInstances' "Locations" entries.
fn collect_power_lines(scan: &SaveScan) -> Value {
    let data = scan.data();
    let power_line_paths: Vec<&str> =
        gamedata::get().type_paths.power_line.iter().map(String::as_str).collect();
    let mut polylines: Vec<Value> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    for (actor_slot, object_slot) in scan.actors_of_type(&power_line_paths) {
        let Some(object_slot) = object_slot else { continue };
        let object = scan.object(object_slot);
        let actor = scan.actor(actor_slot);
        // wireInstances[0][0]: the FIRST array entry's inner props, scanned
        // for every "Locations" (name, Vector) pair.
        let Some(entries) = props::array_structs(&object.properties, data, b"mWireInstances")
        else {
            continue;
        };
        let Some(first) = entries.first() else { continue };
        for prop in &first.props {
            if prop.name.wide || prop.name.bytes(data) != b"Locations" {
                continue;
            }
            let PropertyValue::Struct(StructValue::Vector(destination)) = &prop.value else {
                continue;
            };
            let src = [actor.position[0] as f64, actor.position[1] as f64, actor.position[2] as f64];
            let [src_x, src_y] = project_xy(src[0], src[1]);
            let [dst_x, dst_y] = project_xy(destination[0], destination[1]);
            polylines.push(Value::Array(vec![
                jnum(src_x),
                jnum(src_y),
                jnum(world_z_to_meters(src[2])),
                jnum(dst_x),
                jnum(dst_y),
                jnum(world_z_to_meters(destination[2])),
            ]));
            ids.push(Value::String(props::lossy(
                scan.header(object_slot).instance_name().bytes(data),
            )));
        }
    }
    json!({"polylines": polylines, "ids": ids, "pointStride": 3})
}

/// _LINE_KIND_TYPEPATH + _annotateLineKinds over the "lines" payload step.
pub fn collect_lines(scan: &SaveScan) -> Value {
    const LINE_KIND_TYPEPATH: [(&str, &str); 3] = [
        ("powerLines", "/Game/FactoryGame/Buildable/Factory/PowerLine/Build_PowerLine.Build_PowerLine_C"),
        ("railroads", "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrack.Build_RailroadTrack_C"),
        ("hypertubes", "/Game/FactoryGame/Buildable/Factory/PipeHyper/Build_PipeHyper.Build_PipeHyper_C"),
    ];
    let mut lines = serde_json::Map::new();
    lines.insert("powerLines".into(), collect_power_lines(scan));
    lines.insert("railroads".into(), collect_spline_paths(scan, &RAILROAD_SEGMENTS, "mSplineData"));
    lines.insert("hypertubes".into(), collect_spline_paths(scan, &HYPERTUBE_SEGMENTS, "mSplineData"));
    for (key, line_data) in lines.iter_mut() {
        let type_path = LINE_KIND_TYPEPATH.iter().find(|(k, _)| k == key).map(|(_, tp)| *tp);
        let obj = line_data.as_object_mut().expect("line bucket");
        match type_path {
            Some(tp) => {
                obj.insert("category".into(), Value::String(categorize_type_path(tp).to_string()));
                obj.insert(
                    "subcategory".into(),
                    categorize_subcategory(tp)
                        .map(|s| Value::String(s.to_string()))
                        .unwrap_or(Value::Null),
                );
            }
            None => {
                obj.insert("category".into(), Value::String(OTHER_CATEGORY.to_string()));
                obj.insert("subcategory".into(), Value::Null);
            }
        }
    }
    Value::Object(lines)
}

pub fn collect_belts(scan: &SaveScan) -> Value {
    let paths: Vec<&str> = conveyor_belt_only_type_paths().iter().map(String::as_str).collect();
    collect_spline_path_groups(scan, &paths, "mSplineData")
}

pub fn collect_pipes(scan: &SaveScan) -> Value {
    collect_spline_path_groups(scan, &PIPELINE_SEGMENTS, "mSplineData")
}

pub fn collect_vehicle_paths(scan: &SaveScan) -> Value {
    collect_spline_path_groups(scan, &VEHICLE_PATH_SEGMENTS, "mSplinePoints")
}
