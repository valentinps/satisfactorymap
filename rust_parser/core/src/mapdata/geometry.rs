//! Projection, quaternion, and footprint geometry -- port of
//! sav_map_data.py lines ~204-577. Float expression order is copied verbatim
//! from Python: any reassociation breaks the bit-exact parity gate.

use crate::gamedata;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::OnceLock;

pub const MAP_SIZE: f64 = 8192.0; // map_highres.png dimensions.

// sav_to_html.adjPos()'s world-to-pixel calibration; see sav_map_data.py's
// long comment for the provenance of these numbers.
const WORLD_TO_PIXEL_SCALE: f64 = 22.887;
const WORLD_OFFSET: (f64, f64) = (18282.5, 20480.0);
const OLD_MAP_DESCALE: f64 = 20.0; // sav_to_html.MAP_DESCALE
const CROP_LO: f64 = 4096.0 / OLD_MAP_DESCALE; // 204.8, sav_to_html.CROP_SETTINGS
const CROP_HI: f64 = 36864.0 / OLD_MAP_DESCALE; // 1843.2
const CROP_SPAN: f64 = CROP_HI - CROP_LO; // 1638.4
const SCALE_TO_HIGHRES: f64 = MAP_SIZE / CROP_SPAN;

pub const WORLD_UNITS_PER_METER: f64 = 100.0; // UE's default unit is cm.

const PIXELS_PER_WORLD_UNIT: f64 = (1.0 / WORLD_TO_PIXEL_SCALE / OLD_MAP_DESCALE) * SCALE_TO_HIGHRES;

/// The 8-tuple both spline-polyline implementations project from
/// (sav_map_data._PROJECTION_PARAMS).
pub fn projection_params() -> crate::extract::Proj {
    crate::extract::Proj {
        scale: WORLD_TO_PIXEL_SCALE,
        off_x: WORLD_OFFSET.0,
        off_y: WORLD_OFFSET.1,
        descale: OLD_MAP_DESCALE,
        crop_lo: CROP_LO,
        to_highres: SCALE_TO_HIGHRES,
        map_size: MAP_SIZE,
        ppwu: PIXELS_PER_WORLD_UNIT,
    }
}

fn adj_pos_blank_map20(pos: f64, offset: f64) -> f64 {
    (pos / WORLD_TO_PIXEL_SCALE + offset) / OLD_MAP_DESCALE
}

fn adj_pos(pos: f64, offset: f64) -> f64 {
    (adj_pos_blank_map20(pos, offset) - CROP_LO) * SCALE_TO_HIGHRES
}

/// sav_map_data.projectXY: world (x, y) -> [px, MAP_SIZE - py] (Y flipped for
/// Leaflet's CRS.Simple).
pub fn project_xy(x: f64, y: f64) -> [f64; 2] {
    let px = adj_pos(x, WORLD_OFFSET.0);
    let py = adj_pos(y, WORLD_OFFSET.1);
    [px, MAP_SIZE - py]
}

pub fn world_z_to_meters(z: f64) -> f64 {
    z / WORLD_UNITS_PER_METER
}

pub fn meters_to_pixel_length(meters: f64) -> f64 {
    meters * WORLD_UNITS_PER_METER * PIXELS_PER_WORLD_UNIT
}

/// sav_map_data.projectVectorXY: a direction/delta, so no additive offset,
/// but the same Y flip.
pub fn project_vector_xy(x: f64, y: f64) -> [f64; 2] {
    [x * PIXELS_PER_WORLD_UNIT, -y * PIXELS_PER_WORLD_UNIT]
}

pub fn yaw_from_quaternion(rotation: [f64; 4]) -> f64 {
    let [qx, qy, qz, qw] = rotation;
    (2.0 * (qw * qz + qx * qy)).atan2(1.0 - 2.0 * (qy * qy + qz * qz))
}

/// See sav_map_data._renderedYaw's essay: by the time a rotation reaches
/// here it's known to be a pure yaw.
pub fn rendered_yaw(rotation: [f64; 4]) -> f64 {
    yaw_from_quaternion(rotation)
}

pub fn rotate_vector_by_quaternion(rotation: [f64; 4], vector: [f64; 3]) -> [f64; 3] {
    crate::extract::rotate_vector_by_quaternion(rotation, vector)
}

/// Below this, qx^2+qy^2 is floating-point noise around a pure yaw, not a
/// genuine tilt.
const TILT_THRESHOLD: f64 = 0.001;

pub fn tilt_intensity(rotation: [f64; 4]) -> f64 {
    let [qx, qy, _qz, _qw] = rotation;
    qx * qx + qy * qy
}

/// Standard monotone-chain convex hull over a small point set. Mirrors the
/// Python `sorted(set(points))` preprocessing: exact-duplicate points are
/// dropped, then lexicographic order.
fn convex_hull(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut pts: Vec<(f64, f64)> = points.to_vec();
    // total_cmp: a NaN coordinate (corrupt quaternion) must not panic here.
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    pts.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    if pts.len() <= 2 {
        return pts;
    }
    fn cross(o: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    }
    let mut lower: Vec<(f64, f64)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(f64, f64)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// sav_map_data._boxSilhouettePolygonPixels: top-down silhouette (convex
/// hull of the 8 rotated corners, projected) as a flat [x1,y1,x2,y2,...]
/// pixel-offset list. `corner_ranges_cm` is ((minX,maxX),(minY,maxY),
/// (minZ,maxZ)), deliberately not assumed origin-symmetric.
pub fn box_silhouette_polygon_pixels(
    rotation: [f64; 4],
    corner_ranges_cm: ((f64, f64), (f64, f64), (f64, f64)),
) -> Vec<f64> {
    let mut corners_pixels: Vec<(f64, f64)> = Vec::with_capacity(8);
    for &cx in &[corner_ranges_cm.0 .0, corner_ranges_cm.0 .1] {
        for &cy in &[corner_ranges_cm.1 .0, corner_ranges_cm.1 .1] {
            for &cz in &[corner_ranges_cm.2 .0, corner_ranges_cm.2 .1] {
                let rotated = rotate_vector_by_quaternion(rotation, [cx, cy, cz]);
                let p = project_vector_xy(rotated[0], rotated[1]);
                corners_pixels.push((p[0], p[1]));
            }
        }
    }
    let hull = convex_hull(&corners_pixels);
    let mut flat: Vec<f64> = Vec::with_capacity(hull.len() * 2);
    for (x, y) in hull {
        flat.push(x);
        flat.push(y);
    }
    flat
}

fn tilted_footprint_polygon(rotation: [f64; 4], half_extents_meters: (f64, f64, f64)) -> Vec<f64> {
    let (half_width_m, half_depth_m, half_height_m) = half_extents_meters;
    let (half_width_cm, half_depth_cm, half_height_cm) = (
        half_width_m * WORLD_UNITS_PER_METER,
        half_depth_m * WORLD_UNITS_PER_METER,
        half_height_m * WORLD_UNITS_PER_METER,
    );
    box_silhouette_polygon_pixels(
        rotation,
        (
            (-half_width_cm, half_width_cm),
            (-half_depth_cm, half_depth_cm),
            (-half_height_cm, half_height_cm),
        ),
    )
}

// ---------------------------------------------------------------------------
// Footprint tables from buildings.json
// ---------------------------------------------------------------------------

/// sav_map_data._shortClassName (returns "" where Python returns None -- all
/// callers only use it as a lookup key).
pub fn short_class_name(type_path: &str) -> &str {
    match type_path.rfind('.') {
        Some(pos) => &type_path[pos + 1..],
        None => type_path,
    }
}

/// FALLBACK_FOOTPRINTS_METERS: substring-matched marker sizes.
const FALLBACK_FOOTPRINTS_METERS: [(&str, (f64, f64)); 1] = [("ConveyorLift", (1.0, 1.0))];

struct FootprintTables {
    /// ClassName -> (widthMeters, depthMeters).
    footprints_meters: IndexMap<String, (f64, f64)>,
    /// ClassName -> (crossHalfACm, crossHalfBCm, defaultLengthCm).
    adaptive_beam_specs: IndexMap<String, (f64, f64, f64)>,
}

/// Python min()/max() over a generator: plain comparison fold.
fn fold_min(values: impl Iterator<Item = f64>) -> f64 {
    let mut it = values;
    let mut acc = it.next().expect("min() of empty sequence");
    for v in it {
        if v < acc {
            acc = v;
        }
    }
    acc
}

fn fold_max(values: impl Iterator<Item = f64>) -> f64 {
    let mut it = values;
    let mut acc = it.next().expect("max() of empty sequence");
    for v in it {
        if v > acc {
            acc = v;
        }
    }
    acc
}

fn clearance_boxes(entry: &Value) -> Option<&Vec<Value>> {
    // Python truthiness: present, non-null, non-empty.
    match entry.get("clearance") {
        Some(Value::Array(boxes)) if !boxes.is_empty() => Some(boxes),
        _ => None,
    }
}

fn box_axis(boxes: &[Value], end: &str, axis: &str) -> impl Iterator<Item = f64> + use<> {
    boxes
        .iter()
        .map(|b| b[end][axis].as_f64().expect("clearance box axis"))
        .collect::<Vec<f64>>()
        .into_iter()
}

/// Actor-frame ((minX, maxX), (minY, maxY)) of one clearance box, in cm. A
/// box may carry a RelativeTransform rotation (buildings.json "rotation"):
/// both Barriers' boxes are yawed 90 degrees, so reading the raw axes renders
/// them sideways. Pure-yaw rotations rotate the box's XY corners into the
/// actor frame; rotations with roll/pitch keep the raw axes -- those are the
/// Beams (whose adaptive-length path has its own axis handling) and the
/// Particle Accelerator's tilted ring, where a rotated 3D AABB would only
/// inflate the footprint.
fn box_xy_ranges(b: &Value) -> ((f64, f64), (f64, f64)) {
    let axis = |end: &str, ax: &str| b[end][ax].as_f64().expect("clearance box axis");
    let (min_x, max_x) = (axis("min", "x"), axis("max", "x"));
    let (min_y, max_y) = (axis("min", "y"), axis("max", "y"));
    if let Some(rot) = b.get("rotation") {
        let q = |ax: &str| rot[ax].as_f64().expect("clearance rotation axis");
        let (qx, qy, qz, qw) = (q("x"), q("y"), q("z"), q("w"));
        if qx * qx + qy * qy <= TILT_THRESHOLD {
            let yaw = yaw_from_quaternion([qx, qy, qz, qw]);
            let (sin, cos) = (yaw.sin(), yaw.cos());
            let (mut rx0, mut rx1) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut ry0, mut ry1) = (f64::INFINITY, f64::NEG_INFINITY);
            for &(x, y) in &[(min_x, min_y), (min_x, max_y), (max_x, min_y), (max_x, max_y)] {
                let px = x * cos - y * sin;
                let py = x * sin + y * cos;
                if px < rx0 { rx0 = px }
                if px > rx1 { rx1 = px }
                if py < ry0 { ry0 = py }
                if py > ry1 { ry1 = py }
            }
            return ((rx0, rx1), (ry0, ry1));
        }
    }
    ((min_x, max_x), (min_y, max_y))
}

/// Union of every box's actor-frame XY ranges: ((minX,maxX),(minY,maxY)).
fn boxes_xy_union(boxes: &[Value]) -> ((f64, f64), (f64, f64)) {
    let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
    for b in boxes {
        let ((bx0, bx1), (by0, by1)) = box_xy_ranges(b);
        if bx0 < min_x { min_x = bx0 }
        if bx1 > max_x { max_x = bx1 }
        if by0 < min_y { min_y = by0 }
        if by1 > max_y { max_y = by1 }
    }
    ((min_x, max_x), (min_y, max_y))
}

/// sav_map_data._footprintMetersFromBuildingEntry.
fn footprint_meters_from_building_entry(entry: &Value) -> Option<(f64, f64)> {
    if let Some(boxes) = clearance_boxes(entry) {
        let ((min_x, max_x), (min_y, max_y)) = boxes_xy_union(boxes);
        let (mut width_cm, mut depth_cm) = (max_x - min_x, max_y - min_y);
        // Stale-clearance correction (see the BigGarageDoor note in Python):
        // bump the long axis up to dimensions.Width when that's bigger.
        let width = entry.get("dimensions").and_then(|d| d.get("Width")).and_then(Value::as_f64);
        if let Some(width) = width {
            if width > if width_cm >= depth_cm { width_cm } else { depth_cm } {
                if width_cm >= depth_cm {
                    width_cm = width;
                } else {
                    depth_cm = width;
                }
            }
        }
        return Some((width_cm / WORLD_UNITS_PER_METER, depth_cm / WORLD_UNITS_PER_METER));
    }
    let dimensions = entry.get("dimensions");
    let width = dimensions.and_then(|d| d.get("Width")).and_then(Value::as_f64);
    let depth = dimensions.and_then(|d| d.get("Depth")).and_then(Value::as_f64);
    match (width, depth) {
        (Some(w), Some(d)) => Some((w / WORLD_UNITS_PER_METER, d / WORLD_UNITS_PER_METER)),
        _ => None,
    }
}

/// sav_map_data._footprintHalfExtentsMeters.
fn footprint_half_extents_meters(class_name: &str) -> Option<(f64, f64, f64)> {
    let entry = gamedata::get().buildings.get(class_name)?;
    if let Some(boxes) = clearance_boxes(entry) {
        let ((min_x, max_x), (min_y, max_y)) = boxes_xy_union(boxes);
        let min_z = fold_min(box_axis(boxes, "min", "z"));
        let max_z = fold_max(box_axis(boxes, "max", "z"));
        return Some((
            (max_x - min_x) / 2.0 / WORLD_UNITS_PER_METER,
            (max_y - min_y) / 2.0 / WORLD_UNITS_PER_METER,
            (max_z - min_z) / 2.0 / WORLD_UNITS_PER_METER,
        ));
    }
    let dimensions = entry.get("dimensions");
    let width = dimensions.and_then(|d| d.get("Width")).and_then(Value::as_f64);
    let depth = dimensions.and_then(|d| d.get("Depth")).and_then(Value::as_f64);
    match (width, depth) {
        (Some(w), Some(d)) => {
            // Python: dimensions.get("Height") or 0.0 -- null/absent/0 all -> 0.
            let height = dimensions
                .and_then(|dd| dd.get("Height"))
                .and_then(Value::as_f64)
                .filter(|&h| h != 0.0)
                .unwrap_or(0.0);
            Some((
                w / 2.0 / WORLD_UNITS_PER_METER,
                d / 2.0 / WORLD_UNITS_PER_METER,
                height / 2.0 / WORLD_UNITS_PER_METER,
            ))
        }
        _ => None,
    }
}

fn tables() -> &'static FootprintTables {
    static TABLES: OnceLock<FootprintTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let data = gamedata::get();
        // HAND_CURATED_FOOTPRINTS_METERS_BY_CLASSNAME seeds the dict; the
        // buildings.json pass never overwrites them in practice (neither
        // class carries usable size data in Docs.json).
        let mut footprints_meters: IndexMap<String, (f64, f64)> = IndexMap::new();
        footprints_meters.insert("Build_Elevator_C".into(), (8.0, 8.0));
        footprints_meters.insert("Build_FloodlightWall_C".into(), (0.6, 0.3));
        for (class_name, entry) in &data.buildings {
            if let Some(footprint) = footprint_meters_from_building_entry(entry) {
                footprints_meters.insert(class_name.clone(), footprint);
            }
        }
        // categoryOverrides.json classFootprintsMeters: curated sizes for
        // classes with no usable size data in buildings.json (HUB props,
        // lift-mounted splitters, ...). Applied last so a curated value wins.
        if let Some(Value::Object(map)) = data.category_overrides.get("classFootprintsMeters") {
            for (class_name, value) in map {
                let Some(pair) = value.as_array() else { continue };
                if let (Some(w), Some(d)) = (
                    pair.first().and_then(Value::as_f64),
                    pair.get(1).and_then(Value::as_f64),
                ) {
                    footprints_meters.insert(class_name.clone(), (w, d));
                }
            }
        }

        // _loadAdaptiveBeamSpecs: adaptiveLength with truthy DefaultLength
        // AND MaxLength, plus truthy clearance.
        let mut adaptive_beam_specs: IndexMap<String, (f64, f64, f64)> = IndexMap::new();
        for (class_name, entry) in &data.buildings {
            let adaptive = entry.get("adaptiveLength");
            let max_length = adaptive.and_then(|a| a.get("MaxLength")).and_then(Value::as_f64);
            let default_length =
                adaptive.and_then(|a| a.get("DefaultLength")).and_then(Value::as_f64);
            // Python truthiness: missing, null, or 0 all fail the guard.
            if !max_length.is_some_and(|v| v != 0.0)
                || !default_length.is_some_and(|v| v != 0.0)
            {
                continue;
            }
            let Some(boxes) = clearance_boxes(entry) else { continue };
            let cross_half_a_cm =
                (fold_max(box_axis(boxes, "max", "x")) - fold_min(box_axis(boxes, "min", "x"))) / 2.0;
            let cross_half_b_cm =
                (fold_max(box_axis(boxes, "max", "y")) - fold_min(box_axis(boxes, "min", "y"))) / 2.0;
            adaptive_beam_specs
                .insert(class_name.clone(), (cross_half_a_cm, cross_half_b_cm, default_length.unwrap()));
        }

        FootprintTables { footprints_meters, adaptive_beam_specs }
    })
}

/// sav_map_data.footprintPixels: bucket-level [halfWidthPx, halfDepthPx], or
/// None to render as a plain point.
pub fn footprint_pixels(type_path: &str) -> Option<[f64; 2]> {
    let mut footprint = tables().footprints_meters.get(short_class_name(type_path)).copied();
    if footprint.is_none() {
        for (substring, fallback) in FALLBACK_FOOTPRINTS_METERS {
            if type_path.contains(substring) {
                footprint = Some(fallback);
                break;
            }
        }
    }
    let (width_meters, depth_meters) = footprint?;
    Some([meters_to_pixel_length(width_meters / 2.0), meters_to_pixel_length(depth_meters / 2.0)])
}

/// sav_map_data._footprintForInstance: (yaw, Some(flatPolygon)) for one
/// placed instance. The polygon is present for adaptive-length Beams
/// (always) and genuinely tilted instances (when computable).
pub fn footprint_for_instance(
    type_path: &str,
    rotation: [f64; 4],
    bucket_footprint_pixels: Option<&[f64; 2]>,
    beam_length_cm: Option<f64>,
) -> (f64, Option<Vec<f64>>) {
    if let Some(&(cross_half_a_cm, cross_half_b_cm, default_length_cm)) =
        tables().adaptive_beam_specs.get(short_class_name(type_path))
    {
        // Python truthiness: None or 0 both fall back to the default.
        let length_cm = match beam_length_cm {
            Some(v) if v != 0.0 => v,
            _ => default_length_cm,
        };
        return (
            0.0,
            Some(box_silhouette_polygon_pixels(
                rotation,
                (
                    (0.0, length_cm),
                    (-cross_half_a_cm, cross_half_a_cm),
                    (-cross_half_b_cm, cross_half_b_cm),
                ),
            )),
        );
    }
    if bucket_footprint_pixels.is_none() || tilt_intensity(rotation) <= TILT_THRESHOLD {
        return (rendered_yaw(rotation), None);
    }
    match footprint_half_extents_meters(short_class_name(type_path)) {
        None => (rendered_yaw(rotation), None),
        Some(half_extents) => (0.0, Some(tilted_footprint_polygon(rotation, half_extents))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_matches_python_reference() {
        // Fixture values generated with the Python implementation
        // (map/sav_map_data.py) on 2026-07-09.
        assert_eq!(project_xy(0.0, 0.0), [3546.625, 4096.0]);
        assert_eq!(project_xy(100000.0, -50000.0), [4638.948152881549, 4642.161576440774]);
        assert_eq!(meters_to_pixel_length(8.0), 8.738585223052388);
        assert_eq!(project_vector_xy(100.0, 250.0), [1.0923231528815485, -2.7308078822038713]);
        assert_eq!(world_z_to_meters(12345.0), 123.45);
    }

    #[test]
    fn yaw_matches_python_reference() {
        // Python: yawFromQuaternion((0.1, 0.2, 0.3, 0.9)) on 2026-07-09.
        assert_eq!(yaw_from_quaternion([0.1, 0.2, 0.3, 0.9]), 0.6647744948173456);
        assert_eq!(tilt_intensity([0.1, 0.2, 0.3, 0.9]), 0.05000000000000001);
    }

    #[test]
    fn convex_hull_matches_python_shape() {
        // Square + interior point + duplicate: hull is the 4 corners starting
        // from the lexicographically smallest, lower chain then upper chain.
        let pts = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.5, 0.5), (1.0, 0.0)];
        let hull = convex_hull(&pts);
        assert_eq!(hull, vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
    }

    #[test]
    fn barrier_clearance_yaw_is_applied() {
        // Both Barriers' single clearance box spans x +-200cm, y +-30cm but
        // carries a 90-degree yaw RelativeTransform: in the actor's frame the
        // 4m side runs along Y. Ignoring the yaw rendered every barrier
        // sideways on the map.
        let fp = footprint_pixels("/Game/FactoryGame/Buildable/Building/Barrier/Build_Barrier_Low_01.Build_Barrier_Low_01_C")
            .expect("barrier footprint");
        let (half_width_px, half_depth_px) = (fp[0], fp[1]);
        // ~0.3m across, ~2.0m along (half extents).
        assert!((half_width_px - meters_to_pixel_length(0.3)).abs() < 0.01,
                "half width {half_width_px}");
        assert!((half_depth_px - meters_to_pixel_length(2.0)).abs() < 0.01,
                "half depth {half_depth_px}");
    }

    #[test]
    fn footprint_tables_load() {
        // Smelter: Python footprintPixels == [2.7308078822038713, 5.4616157644077425].
        let fp = footprint_pixels("/Game/FactoryGame/Buildable/Factory/SmelterMk1/Build_SmelterMk1.Build_SmelterMk1_C");
        assert_eq!(fp, Some([2.7308078822038713, 5.4616157644077425]));
        // ConveyorLift substring fallback.
        let fp = footprint_pixels("/Game/FactoryGame/Buildable/Factory/ConveyorLiftMk1/Build_ConveyorLiftMk1.Build_ConveyorLiftMk1_C");
        assert!(fp.is_some());
        // Beams have adaptive specs.
        assert!(tables().adaptive_beam_specs.keys().any(|k| k.contains("Beam")));
    }
}
