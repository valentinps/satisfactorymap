//! Turns edit ops into byte transforms over the decompressed body. Each op
//! is first *planned* against the parsed store (producing only small patch/
//! insert/remove records), so the caller can drop the multi-GB parsed
//! structures before mutating the body in place -- load-bearing for the
//! 4GB-capped wasm heap on 600k-object saves. The strict re-parse after
//! every op is the corruption gate before anything reaches the user (or the
//! game).

use crate::editor::ops::{EditOp, LwRef};
use crate::editor::rename;
use crate::error::{perr, PResult};
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use std::collections::{BTreeSet, HashMap};

/// The byte-level effect of one op, in PRE-op offsets. Patches never overlap
/// inserts/removes (they target count/size fields and transform blocks), and
/// a single plan never mixes inserts with removes.
#[derive(Default)]
pub struct EditPlan {
    patches: Vec<(usize, Vec<u8>)>,
    inserts: Vec<(usize, Vec<u8>)>,
    removes: Vec<(usize, usize)>,
}

impl EditPlan {
    fn patch(&mut self, at: usize, bytes: impl Into<Vec<u8>>) {
        self.patches.push((at, bytes.into()));
    }
}

/// Mutate `body` per the plan. Length changes shift the tail with
/// copy_within instead of building a second body; the leading u64
/// uncompressedSize is refreshed at the end.
pub fn apply_plan(body: &mut Vec<u8>, mut plan: EditPlan) -> PResult<()> {
    for (at, bytes) in &plan.patches {
        if at + bytes.len() > body.len() {
            return Err(perr!("Edit patch out of range"));
        }
        body[*at..at + bytes.len()].copy_from_slice(bytes);
    }

    if !plan.removes.is_empty() && !plan.inserts.is_empty() {
        return Err(perr!("Edit plan mixes inserts and removes"));
    }

    if !plan.removes.is_empty() {
        plan.removes.sort_by_key(|(at, _)| *at);
        let mut write = plan.removes[0].0;
        let mut read = write;
        for &(at, len) in &plan.removes {
            let keep = at - read;
            body.copy_within(read..read + keep, write);
            write += keep;
            read = at + len;
        }
        let tail = body.len() - read;
        body.copy_within(read.., write);
        body.truncate(write + tail);
    }

    if !plan.inserts.is_empty() {
        plan.inserts.sort_by_key(|(at, _)| *at);
        let added: usize = plan.inserts.iter().map(|(_, b)| b.len()).sum();
        let old_len = body.len();
        body.reserve_exact(added);
        body.resize(old_len + added, 0);
        // Shift the pre-existing segments right-to-left so nothing is
        // clobbered, placing each insert as its gap opens up.
        let mut src_end = old_len;
        let mut shift = added;
        for (at, bytes) in plan.inserts.iter().rev() {
            body.copy_within(*at..src_end, at + shift);
            shift -= bytes.len();
            body[at + shift..at + shift + bytes.len()].copy_from_slice(bytes);
            src_end = *at;
        }
    }

    let size = (body.len() - 8) as u64;
    body[0..8].copy_from_slice(&size.to_le_bytes());
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Everything a transform can't meaningfully or safely apply to. Matched on
/// the actor's parsed trailing data and type path, not a hardcoded list of
/// every buildable.
fn move_refusal(store: &SaveStore, header: &Header, object: &Object) -> Option<&'static str> {
    let type_path = match header {
        Header::Actor(a) => a.type_path.to_string(&store.data),
        // Components have no transform; they move with their parent actor.
        Header::Component(_) => return Some("components move with their parent actor"),
    };
    if type_path.starts_with("/Script/FactoryGame.FGConveyorChainActor") {
        return Some("conveyor chain actors move via their belts");
    }
    match &object.actor_specific {
        ActorSpecific::Vehicles(_) => Some("vehicles are not editable"),
        ActorSpecific::Train { .. } => Some("trains are not editable"),
        ActorSpecific::PlayerStateType(_)
        | ActorSpecific::PlayerStateClient { .. } => Some("player state is not editable"),
        ActorSpecific::Lightweight { .. } => Some("the lightweight subsystem is not a building"),
        ActorSpecific::Circuits(_) => Some("subsystems are not editable"),
        ActorSpecific::RefList(_) => Some("game state is not editable"),
        _ => None,
    }
}

/// (sin, cos) of a yaw in degrees -- exact for the 90-degree steps the UI
/// produces so repeated rotations can't accumulate float drift.
fn yaw_sin_cos(deg: f64) -> (f64, f64) {
    match deg.rem_euclid(360.0) {
        0.0 => (0.0, 1.0),
        90.0 => (1.0, 0.0),
        180.0 => (0.0, -1.0),
        270.0 => (-1.0, 0.0),
        _ => deg.to_radians().sin_cos(),
    }
}

/// World-frame yaw composition: q' = q_z(theta) * q  (x,y,z,w layout).
fn rotate_quat_yaw(q: [f64; 4], deg: f64) -> [f64; 4] {
    // Half-angle sin/cos, exact for the UI's 90-degree steps.
    let (s, c) = match deg.rem_euclid(360.0) {
        0.0 => (0.0, 1.0),
        90.0 => (std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2),
        180.0 => (1.0, 0.0),
        270.0 => (std::f64::consts::FRAC_1_SQRT_2, -std::f64::consts::FRAC_1_SQRT_2),
        _ => (deg / 2.0).to_radians().sin_cos(),
    };
    let [qx, qy, qz, qw] = q;
    [
        c * qx - s * qy,
        c * qy + s * qx,
        c * qz + s * qw,
        c * qw - s * qz,
    ]
}

/// Rotate a world XY about a pivot by yaw degrees, then translate.
fn transform_xy(x: f64, y: f64, deg: f64, pivot: Option<[f64; 2]>, delta: &[f64; 3]) -> (f64, f64) {
    let (mut nx, mut ny) = (x, y);
    if deg != 0.0 {
        let [px, py] = pivot.unwrap_or([0.0, 0.0]);
        let (s, c) = yaw_sin_cos(deg);
        let (dx, dy) = (x - px, y - py);
        nx = px + dx * c - dy * s;
        ny = py + dx * s + dy * c;
    }
    (nx + delta[0], ny + delta[1])
}

/// Rotate a direction vector (tangent) about Z; no translation.
fn rotate_dir_xy(x: f64, y: f64, deg: f64) -> (f64, f64) {
    if deg == 0.0 {
        return (x, y);
    }
    let (s, c) = yaw_sin_cos(deg);
    (x * c - y * s, x * s + y * c)
}

fn write_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut [u8], off: usize, v: f64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn read_f64(buf: &[u8], off: usize) -> f64 {
    f64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

/// Patch that adds to a u32/u64 field (values read from the pre-op body,
/// which is `store.data`).
fn patch_add_u32(plan: &mut EditPlan, data: &[u8], off: usize, add: i64) {
    let v = (read_u32(data, off) as i64 + add) as u32;
    plan.patch(off, v.to_le_bytes().to_vec());
}

fn patch_add_u64(plan: &mut EditPlan, data: &[u8], off: usize, add: i64) {
    let v = (read_u64_at(data, off) as i64 + add) as u64;
    plan.patch(off, v.to_le_bytes().to_vec());
}

// ---------------------------------------------------------------------------
// Lean object access
// ---------------------------------------------------------------------------
// Planning must work on a store whose parsed object model was dropped (the
// wasm session frees it for memory headroom): objects are re-parsed one at a
// time from their byte spans, and the special actors a plan needs wholesale
// (conveyor chains, power lines, the lightweight subsystem) are located by
// their HEADER type paths -- headers are always retained.

/// Slots of all actors whose type path exactly matches one of `candidates`.
fn actor_slots_of_types(store: &SaveStore, candidates: &[&str]) -> Vec<(usize, usize)> {
    let data: &[u8] = &store.data;
    let mut out = Vec::new();
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                let tp = a.type_path.bytes(data);
                if candidates.iter().any(|c| c.as_bytes() == tp) {
                    out.push((li, oi));
                }
            }
        }
    }
    out
}

/// One object, re-parsed on demand from its span (identical to the eagerly
/// parsed model; StrRefs point into the same `store.data`).
fn fetch(store: &SaveStore, li: usize, oi: usize) -> PResult<Object> {
    store.parse_object_at(li, oi)
}

// ---------------------------------------------------------------------------
// Move
// ---------------------------------------------------------------------------

/// Per-chain-belt patch target: where its world-space spline elements live.
struct ChainSplines {
    elements_off: u32,
    element_count: usize,
}

/// belt instance name -> chain spline extents, across every chain actor
/// (found by header type path, parsed on demand).
fn chain_splines_by_belt(store: &SaveStore) -> PResult<HashMap<Vec<u8>, ChainSplines>> {
    let mut map = HashMap::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::CONVEYOR_CHAINS) {
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
            for cb in belts {
                map.insert(
                    cb.belt.path_name.bytes(&store.data).to_vec(),
                    ChainSplines { elements_off: cb.elements_off, element_count: cb.elements.len() },
                );
            }
        }
    }
    Ok(map)
}

/// Absolute world positions cached in a power line's mWireInstances
/// ("Locations" vectors) -- the wire-mesh endpoints the game and the map
/// renderer draw from. They must be transformed together with the wire.
fn wire_cached_locations(object: &Object, data: &[u8]) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    if let Some(entries) =
        crate::mapdata::props::array_structs(&object.properties, data, b"mWireInstances")
    {
        for entry in entries {
            for prop in &entry.props {
                if !prop.name.wide && prop.name.bytes(data) == b"Locations" {
                    if let PropertyValue::Struct(StructValue::Vector(v)) = &prop.value {
                        out.push(*v);
                    }
                }
            }
        }
    }
    out
}

/// Offsets of every 24-byte little-endian encoding of `v` inside `hay`
/// (wire objects are a few hundred bytes; a full scan is nothing). Property
/// value offsets aren't retained by the parser, so the values locate
/// themselves by their own bytes -- exact f64 bit patterns, no false hits in
/// practice, and the strict re-parse gates the result regardless.
fn find_f64x3(hay: &[u8], v: [f64; 3]) -> Vec<usize> {
    let mut pat = [0u8; 24];
    for (i, x) in v.iter().enumerate() {
        pat[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
    }
    if hay.len() < 24 {
        return Vec::new();
    }
    (0..hay.len() - 23).filter(|&i| hay[i..i + 24] == pat).collect()
}

fn transform_vec3(v: [f64; 3], deg: f64, pivot: Option<[f64; 2]>, delta: &[f64; 3]) -> [f64; 3] {
    let (nx, ny) = transform_xy(v[0], v[1], deg, pivot, delta);
    [nx, ny, v[2] + delta[2]]
}

fn encode_f64x3(v: [f64; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn plan_move_actors(
    store: &SaveStore,
    scan: &SaveScan,
    plan: &mut EditPlan,
    names: &[String],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let data: &[u8] = &store.data;
    let chains = chain_splines_by_belt(store)?;

    let mut move_one = |li: usize, oi: usize, object: &Object| -> PResult<()> {
        let header = &store.levels[li].headers[oi];
        let Header::Actor(actor) = header else { unreachable!() };
        let name = actor.instance_name.bytes(data);

        // Header transform: quat f32x4 then position f32x3.
        let t = actor.transform_off as usize;
        if rotate_yaw_deg != 0.0 {
            let q = rotate_quat_yaw(
                [actor.rotation[0] as f64, actor.rotation[1] as f64, actor.rotation[2] as f64, actor.rotation[3] as f64],
                rotate_yaw_deg,
            );
            let mut quat = [0u8; 16];
            for (i, v) in q.iter().enumerate() {
                write_f32(&mut quat, i * 4, *v as f32);
            }
            plan.patch(t, quat.to_vec());
        }
        let (nx, ny) = transform_xy(actor.position[0] as f64, actor.position[1] as f64, rotate_yaw_deg, pivot, delta);
        let mut pos = [0u8; 12];
        write_f32(&mut pos, 0, nx as f32);
        write_f32(&mut pos, 4, ny as f32);
        write_f32(&mut pos, 8, (actor.position[2] as f64 + delta[2]) as f32);
        plan.patch(t + 16, pos.to_vec());

        // Chained belts: their chain actor's spline elements are world-space
        // [location, arriveTangent, leaveTangent] f64 triplets.
        if let Some(cs) = chains.get(name) {
            for e in 0..cs.element_count {
                let base = cs.elements_off as usize + e * 72;
                let mut elem = data[base..base + 72].to_vec();
                let (lx, ly) = transform_xy(read_f64(&elem, 0), read_f64(&elem, 8), rotate_yaw_deg, pivot, delta);
                write_f64(&mut elem, 0, lx);
                write_f64(&mut elem, 8, ly);
                let z = read_f64(&elem, 16) + delta[2];
                write_f64(&mut elem, 16, z);
                for row in 1..3 {
                    let r = row * 24;
                    let (tx, ty) = rotate_dir_xy(read_f64(&elem, r), read_f64(&elem, r + 8), rotate_yaw_deg);
                    write_f64(&mut elem, r, tx);
                    write_f64(&mut elem, r + 8, ty);
                }
                plan.patch(base, elem);
            }
        }

        // Power lines: the wire mesh's endpoint positions are cached as
        // absolute world "Locations" vectors in the object's properties --
        // the map (and the game) draw the wire from them.
        let locations = wire_cached_locations(object, data);
        if !locations.is_empty() {
            let (span_off, span_len) = store.levels[li].object_spans[oi];
            let span = &data[span_off as usize..(span_off + span_len) as usize];
            for v in locations {
                let replacement = encode_f64x3(transform_vec3(v, rotate_yaw_deg, pivot, delta));
                for rel in find_f64x3(span, v) {
                    plan.patch(span_off as usize + rel, replacement.clone());
                }
            }
        }
        Ok(())
    };

    let mut moved_actor_names: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut moved_slots: BTreeSet<(usize, usize)> = BTreeSet::new();
    for name in names {
        let Some(&(li, oi)) = scan.by_instance_name.get(name.as_bytes()) else {
            return Err(perr!("No such instance: {}", name));
        };
        if !moved_slots.insert((li, oi)) {
            continue;
        }
        let header = &store.levels[li].headers[oi];
        let object = fetch(store, li, oi)?;
        if let Some(reason) = move_refusal(store, header, &object) {
            return Err(perr!("Cannot move {}: {}", name, reason));
        }
        moved_actor_names.insert(name.as_bytes().to_vec());
        move_one(li, oi, &object)?;
    }

    // Wires whose BOTH endpoint owners moved follow along rigidly (wires
    // aren't map-selectable, so they never appear in `names` themselves).
    let owner_moved = |endpoint: &ObjectRef| -> bool {
        if endpoint.path_name.is_empty() {
            return false;
        }
        let path = endpoint.path_name.bytes(data);
        match path.iter().rposition(|&b| b == b'.') {
            Some(dot) => moved_actor_names.contains(&path[..dot]),
            None => false,
        }
    };
    for (li, oi) in actor_slots_of_types(store, &crate::object::POWER_LINES) {
        if moved_slots.contains(&(li, oi)) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
            if owner_moved(a) && owner_moved(b) {
                move_one(li, oi, &object)?;
            }
        }
    }
    Ok(())
}

/// The one Lightweight subsystem object -- lightweight edits address groups
/// inside it by type path. Located by header type path and parsed on demand;
/// returns (level_idx, object_idx, owned groups).
fn lightweight_subsystem(store: &SaveStore) -> PResult<(usize, usize, Vec<LightweightGroup>)> {
    for (li, oi) in actor_slots_of_types(store, &[crate::object::LIGHTWEIGHT_SUBSYSTEM]) {
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::Lightweight { items, .. } = object.actor_specific {
            return Ok((li, oi, items));
        }
    }
    Err(perr!("Save has no lightweight buildable subsystem"))
}

fn plan_move_lightweight(
    store: &SaveStore,
    plan: &mut EditPlan,
    items: &[LwRef],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let (_, _, groups) = lightweight_subsystem(store)?;
    for item in items {
        let group = groups
            .iter()
            .find(|g| g.type_path.eq_ascii(&store.data, &item.type_path))
            .ok_or_else(|| perr!("No lightweight group for {}", item.type_path))?;
        let instance = group
            .instances
            .get(item.index as usize)
            .ok_or_else(|| perr!("Lightweight index {} out of range for {}", item.index, item.type_path))?;
        let r = instance.record_off as usize;
        if rotate_yaw_deg != 0.0 {
            let q = rotate_quat_yaw(instance.rotation, rotate_yaw_deg);
            let mut quat = [0u8; 32];
            for (i, v) in q.iter().enumerate() {
                write_f64(&mut quat, i * 8, *v);
            }
            plan.patch(r, quat.to_vec());
        }
        let (nx, ny) = transform_xy(instance.position[0], instance.position[1], rotate_yaw_deg, pivot, delta);
        let mut pos = [0u8; 24];
        write_f64(&mut pos, 0, nx);
        write_f64(&mut pos, 8, ny);
        write_f64(&mut pos, 16, instance.position[2] + delta[2]);
        plan.patch(r + 32, pos.to_vec());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Duplication
// ---------------------------------------------------------------------------

/// Expand the requested actor names into the full copy set: each actor plus
/// its components, plus every power line whose BOTH endpoints are owned by
/// actors in the set (wires aren't map-selectable, so connected copies
/// would otherwise always lose their wiring).
fn expand_duplicate_set(
    store: &SaveStore,
    scan: &SaveScan,
    names: &[String],
) -> PResult<BTreeSet<(usize, usize)>> {
    let data: &[u8] = &store.data;
    let mut set: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut actor_names: BTreeSet<Vec<u8>> = BTreeSet::new();

    let add_actor = |set: &mut BTreeSet<(usize, usize)>,
                     actor_names: &mut BTreeSet<Vec<u8>>,
                     li: usize,
                     oi: usize|
     -> PResult<()> {
        let header = &store.levels[li].headers[oi];
        let object = fetch(store, li, oi)?;
        if let Some(reason) = move_refusal(store, header, &object) {
            return Err(perr!(
                "Cannot copy {}: {}",
                header.instance_name().to_string(data),
                reason
            ));
        }
        set.insert((li, oi));
        actor_names.insert(header.instance_name().bytes(data).to_vec());
        if let Some((_, components)) = &object.actor_reference_associations {
            for comp in components {
                if comp.path_name.is_empty() {
                    continue;
                }
                let comp_name = comp.path_name.bytes(data);
                let Some(&slot) = scan.by_instance_name.get(comp_name) else {
                    return Err(perr!(
                        "Component {} of a copied actor was not found in the save",
                        String::from_utf8_lossy(comp_name)
                    ));
                };
                set.insert(slot);
            }
        }
        Ok(())
    };

    for name in names {
        let Some(&(li, oi)) = scan.by_instance_name.get(name.as_bytes()) else {
            return Err(perr!("No such instance: {}", name));
        };
        if matches!(store.levels[li].headers[oi], Header::Component(_)) {
            return Err(perr!("Cannot copy a component directly: {}", name));
        }
        add_actor(&mut set, &mut actor_names, li, oi)?;
    }

    // Wires: owner actor of an endpoint component "….Build_X_C_123.Conn" is
    // everything before the last '.'.
    let owner_in_set = |endpoint: &ObjectRef| -> bool {
        if endpoint.path_name.is_empty() {
            return false;
        }
        let path = endpoint.path_name.bytes(data);
        let Some(dot) = path.iter().rposition(|&b| b == b'.') else {
            return false;
        };
        actor_names.contains(&path[..dot])
    };
    let mut wires: Vec<(usize, usize)> = Vec::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::POWER_LINES) {
        if set.contains(&(li, oi)) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
            if owner_in_set(a) && owner_in_set(b) {
                wires.push((li, oi));
            }
        }
    }
    for (li, oi) in wires {
        add_actor(&mut set, &mut actor_names, li, oi)?;
    }
    Ok(set)
}

fn plan_duplicate_actors(
    store: &SaveStore,
    scan: &SaveScan,
    plan: &mut EditPlan,
    names: &[String],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
    seed: u64,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let data: &[u8] = &store.data;
    let set = expand_duplicate_set(store, scan, names)?;
    if set.is_empty() {
        return Err(perr!("Nothing to copy"));
    }
    let level_idx = set.iter().next().unwrap().0;
    if set.iter().any(|&(li, _)| li != level_idx) {
        return Err(perr!("Cannot copy objects from different world levels in one paste"));
    }
    let level = &store.levels[level_idx];

    // Same-length rename map for every actor in the set (components rename
    // via their embedded actor segment).
    let actor_name_list: Vec<&[u8]> = set
        .iter()
        .filter(|&&(li, oi)| matches!(store.levels[li].headers[oi], Header::Actor(_)))
        .map(|&(li, oi)| store.levels[li].headers[oi].instance_name().bytes(data))
        .collect();
    let exists = |candidate: &[u8]| scan.by_instance_name.contains_key(candidate);
    let mut substitutions = rename::build_rename_map(&actor_name_list, seed, &exists)?;

    // External instance refs (outside the set) get same-length tombstones so
    // the copies don't claim connections the originals still own.
    let contains_renamed_segment = |path: &[u8], map: &HashMap<Vec<u8>, Vec<u8>>| -> bool {
        map.keys().any(|k| {
            path.windows(k.len()).enumerate().any(|(i, w)| {
                w == k.as_slice()
                    && (i == 0 || !path[i - 1].is_ascii_alphanumeric())
                    && (i + k.len() == path.len() || !path[i + k.len()].is_ascii_alphanumeric())
            })
        })
    };
    let mut rng = rename::Rng(seed ^ 0x746f6d6273746f6e); // independent stream for tombstones
    let mut tombstones: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    for &(li, oi) in &set {
        let object = fetch(store, li, oi)?;
        let mut err: Option<crate::error::PError> = None;
        rename::visit_object_refs(&object, &mut |r: &ObjectRef| {
            if err.is_some() || r.path_name.is_empty() {
                return;
            }
            let path = r.path_name.bytes(data);
            if contains_renamed_segment(path, &substitutions) || tombstones.contains_key(path) {
                return;
            }
            if !scan.by_instance_name.contains_key(path) {
                return; // Class path / static asset: keep verbatim.
            }
            // Only numbered instances are tombstoned: those are exclusive
            // per-instance targets (another buildable's connection component,
            // a blueprint proxy, a chain actor). Digitless paths are shared
            // singletons (BuildableSubsystem parent, game state, ...) the
            // copy legitimately belongs to as well -- keep those.
            match rename::tombstone_path(path, &mut rng, &exists) {
                Ok(Some(t)) => {
                    tombstones.insert(path.to_vec(), t);
                }
                Ok(None) => {}
                Err(e) => err = Some(e),
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
    }
    substitutions.extend(tombstones);

    // Copy, substitute, patch transforms.
    let mut new_headers: Vec<u8> = Vec::new();
    let mut new_bodies: Vec<u8> = Vec::new();
    let n_new = set.len() as i64;
    for &(li, oi) in &set {
        let (h_off, h_len) = store.levels[li].header_spans[oi];
        let (b_off, b_len) = store.levels[li].object_spans[oi];
        let mut header_copy = data[h_off as usize..(h_off + h_len) as usize].to_vec();
        let mut body_copy = data[b_off as usize..(b_off + b_len) as usize].to_vec();
        rename::substitute_names(&mut header_copy, &substitutions);
        rename::substitute_names(&mut body_copy, &substitutions);

        if let Header::Actor(actor) = &store.levels[li].headers[oi] {
            let t = (actor.transform_off - h_off) as usize;
            if rotate_yaw_deg != 0.0 {
                let q = rotate_quat_yaw(
                    [actor.rotation[0] as f64, actor.rotation[1] as f64, actor.rotation[2] as f64, actor.rotation[3] as f64],
                    rotate_yaw_deg,
                );
                for (i, v) in q.iter().enumerate() {
                    write_f32(&mut header_copy, t + i * 4, *v as f32);
                }
            }
            let (nx, ny) = transform_xy(
                actor.position[0] as f64,
                actor.position[1] as f64,
                rotate_yaw_deg,
                pivot,
                delta,
            );
            write_f32(&mut header_copy, t + 16, nx as f32);
            write_f32(&mut header_copy, t + 20, ny as f32);
            write_f32(&mut header_copy, t + 24, (actor.position[2] as f64 + delta[2]) as f32);

            // Copied power lines: also transform the cached wire-mesh
            // endpoint "Locations" vectors (absolute world coordinates in
            // the object's properties -- the map and the game draw the wire
            // from them, so leaving them puts the copy's wire back on the
            // originals). Same-length f64 rewrites, found by value.
            let tp = actor.type_path.bytes(data);
            if crate::object::POWER_LINES.iter().any(|c| c.as_bytes() == tp) {
                let object = fetch(store, li, oi)?;
                for v in wire_cached_locations(&object, data) {
                    let replacement = encode_f64x3(transform_vec3(v, rotate_yaw_deg, pivot, delta));
                    for rel in find_f64x3(&body_copy, v) {
                        body_copy[rel..rel + 24].copy_from_slice(&replacement);
                    }
                }
            }
        }
        new_headers.extend_from_slice(&header_copy);
        new_bodies.extend_from_slice(&body_copy);
    }

    // Count/size cascade (apply_plan refreshes the leading uncompressedSize).
    let spans = &level.spans;
    patch_add_u64(plan, data, spans.header_size_field_off as usize, new_headers.len() as i64);
    patch_add_u32(plan, data, spans.header_size_field_off as usize + 8, n_new);
    patch_add_u64(plan, data, spans.objects_size_field_off as usize, new_bodies.len() as i64);
    patch_add_u32(plan, data, spans.object_count_field_off as usize, n_new);
    plan.inserts.push((spans.headers_insert_off as usize, new_headers));
    plan.inserts.push((spans.bodies_insert_off as usize, new_bodies));
    Ok(())
}

fn plan_duplicate_lightweight(
    store: &SaveStore,
    plan: &mut EditPlan,
    items: &[LwRef],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let data: &[u8] = &store.data;
    let (li, oi, groups) = lightweight_subsystem(store)?;

    let mut added_per_group: HashMap<u32, i64> = HashMap::new(); // count_field_off -> count
    let mut total_added = 0i64;

    for item in items {
        let group = groups
            .iter()
            .find(|g| g.type_path.eq_ascii(data, &item.type_path))
            .ok_or_else(|| perr!("No lightweight group for {}", item.type_path))?;
        let instance = group
            .instances
            .get(item.index as usize)
            .ok_or_else(|| perr!("Lightweight index {} out of range for {}", item.index, item.type_path))?;

        let r = instance.record_off as usize;
        let mut copy = data[r..r + instance.record_len as usize].to_vec();

        // The copy is hand-placed, not blueprint-placed: empty out a
        // non-empty blueprint proxy ref. (Empty strings are just an i32 0,
        // so the copied record shrinks -- fine, it's fresh bytes.)
        let proxy = &instance.blueprint_proxy;
        if !proxy.path_name.is_empty() || !proxy.level_name.is_empty() {
            if proxy.level_name.is_empty() || proxy.path_name.is_empty() || proxy.level_name.wide || proxy.path_name.wide {
                return Err(perr!("Unexpected blueprint proxy encoding on a copied foundation"));
            }
            let start = proxy.level_name.off as usize - 4 - r;
            let end = proxy.path_name.off as usize + proxy.path_name.len as usize + 1 - r;
            let mut rebuilt = Vec::with_capacity(copy.len() - (end - start) + 8);
            rebuilt.extend_from_slice(&copy[..start]);
            rebuilt.extend_from_slice(&[0u8; 8]); // two empty strings
            rebuilt.extend_from_slice(&copy[end..]);
            copy = rebuilt;
        }

        if rotate_yaw_deg != 0.0 {
            let q = rotate_quat_yaw(instance.rotation, rotate_yaw_deg);
            for (i, v) in q.iter().enumerate() {
                write_f64(&mut copy, i * 8, *v);
            }
        }
        let (nx, ny) = transform_xy(instance.position[0], instance.position[1], rotate_yaw_deg, pivot, delta);
        write_f64(&mut copy, 32, nx);
        write_f64(&mut copy, 40, ny);
        write_f64(&mut copy, 48, instance.position[2] + delta[2]);

        total_added += copy.len() as i64;
        *added_per_group.entry(group.count_field_off).or_insert(0) += 1;
        plan.inserts.push((group.end_off as usize, copy));
    }

    for (count_field_off, count) in &added_per_group {
        patch_add_u32(plan, data, *count_field_off as usize, *count);
    }
    // Subsystem object body grows: [gv u32][migrate u32][object_size u32].
    let object_size_field = store.levels[li].object_spans[oi].0 as usize + 8;
    patch_add_u32(plan, data, object_size_field, total_added);
    patch_add_u64(plan, data, store.levels[li].spans.objects_size_field_off as usize, total_added);
    Ok(())
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

/// belt instance names that appear in any conveyor chain (deleting one would
/// leave the chain actor's packed belt list pointing at nothing, which the
/// game -- and our own parser's chain handling -- can't tolerate). Chain
/// actors are found by header type path and parsed on demand.
fn chained_belt_names(store: &SaveStore) -> PResult<std::collections::HashSet<Vec<u8>>> {
    let mut set = std::collections::HashSet::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::CONVEYOR_CHAINS) {
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
            for cb in belts {
                set.insert(cb.belt.path_name.bytes(&store.data).to_vec());
            }
        }
    }
    Ok(set)
}

fn plan_delete_actors(
    store: &SaveStore,
    scan: &SaveScan,
    plan: &mut EditPlan,
    names: &[String],
) -> PResult<()> {
    let data: &[u8] = &store.data;
    let set = expand_duplicate_set(store, scan, names)?;
    if set.is_empty() {
        return Err(perr!("Nothing to delete"));
    }
    let level_idx = set.iter().next().unwrap().0;
    if set.iter().any(|&(li, _)| li != level_idx) {
        return Err(perr!("Cannot delete objects from different world levels at once"));
    }

    let chained = chained_belt_names(store)?;
    for &(li, oi) in &set {
        let name = store.levels[li].headers[oi].instance_name().bytes(data);
        if chained.contains(&name.to_vec()) {
            return Err(perr!(
                "Cannot delete {}: the belt is part of a conveyor chain (move it instead, or delete the whole line in game)",
                String::from_utf8_lossy(name)
            ));
        }
    }

    // Also delete power lines with EITHER endpoint on a deleted actor -- a
    // wire to nowhere renders and simulates wrong in game.
    let mut deleted_actor_names: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
    for &(li, oi) in &set {
        deleted_actor_names.insert(store.levels[li].headers[oi].instance_name().bytes(data));
    }
    let owner_deleted = |endpoint: &ObjectRef| -> bool {
        if endpoint.path_name.is_empty() {
            return false;
        }
        let path = endpoint.path_name.bytes(data);
        match path.iter().rposition(|&b| b == b'.') {
            Some(dot) => deleted_actor_names.contains(&path[..dot]),
            None => false,
        }
    };
    let mut full_set = set;
    let mut extra_wires: Vec<(usize, usize)> = Vec::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::POWER_LINES) {
        if full_set.contains(&(li, oi)) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
            if owner_deleted(a) || owner_deleted(b) {
                if li != level_idx {
                    return Err(perr!("Cannot delete: an attached power line lives in a different world level"));
                }
                extra_wires.push((li, oi));
            }
        }
    }
    for (li, oi) in extra_wires {
        full_set.insert((li, oi));
        // A wire's own components (if any) go with it.
        if let Some((_, components)) = &fetch(store, li, oi)?.actor_reference_associations {
            for comp in components {
                if comp.path_name.is_empty() {
                    continue;
                }
                if let Some(&slot) = scan.by_instance_name.get(comp.path_name.bytes(data)) {
                    full_set.insert(slot);
                }
            }
        }
    }

    let level = &store.levels[level_idx];
    let mut removed_header_bytes = 0i64;
    let mut removed_body_bytes = 0i64;
    for &(li, oi) in &full_set {
        let (h_off, h_len) = store.levels[li].header_spans[oi];
        let (b_off, b_len) = store.levels[li].object_spans[oi];
        plan.removes.push((h_off as usize, h_len as usize));
        plan.removes.push((b_off as usize, b_len as usize));
        removed_header_bytes += h_len as i64;
        removed_body_bytes += b_len as i64;
    }

    let n_removed = full_set.len() as i64;
    let spans = &level.spans;
    patch_add_u64(plan, data, spans.header_size_field_off as usize, -removed_header_bytes);
    patch_add_u32(plan, data, spans.header_size_field_off as usize + 8, -n_removed);
    patch_add_u64(plan, data, spans.objects_size_field_off as usize, -removed_body_bytes);
    patch_add_u32(plan, data, spans.object_count_field_off as usize, -n_removed);
    Ok(())
}

fn plan_delete_lightweight(store: &SaveStore, plan: &mut EditPlan, items: &[LwRef]) -> PResult<()> {
    let data: &[u8] = &store.data;
    let (li, oi, groups) = lightweight_subsystem(store)?;

    let mut removed_per_group: HashMap<u32, i64> = HashMap::new();
    let mut seen: BTreeSet<(u32, u32)> = BTreeSet::new(); // (count_field_off, index)
    let mut total_removed = 0i64;
    for item in items {
        let group = groups
            .iter()
            .find(|g| g.type_path.eq_ascii(data, &item.type_path))
            .ok_or_else(|| perr!("No lightweight group for {}", item.type_path))?;
        let instance = group
            .instances
            .get(item.index as usize)
            .ok_or_else(|| perr!("Lightweight index {} out of range for {}", item.index, item.type_path))?;
        if !seen.insert((group.count_field_off, item.index)) {
            continue; // deduplicate
        }
        plan.removes.push((instance.record_off as usize, instance.record_len as usize));
        total_removed += instance.record_len as i64;
        *removed_per_group.entry(group.count_field_off).or_insert(0) += 1;
    }

    for (count_field_off, count) in &removed_per_group {
        patch_add_u32(plan, data, *count_field_off as usize, -count);
    }
    let object_size_field = store.levels[li].object_spans[oi].0 as usize + 8;
    patch_add_u32(plan, data, object_size_field, -total_removed);
    patch_add_u64(plan, data, store.levels[li].spans.objects_size_field_off as usize, -total_removed);
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Plan ONE op against the parsed store. The plan holds only small buffers
/// (copied objects, field patches), so the caller can drop the store's
/// parsed structures before applying it to the body.
pub fn plan_op(store: &SaveStore, op: &EditOp) -> PResult<EditPlan> {
    let mut plan = EditPlan::default();
    match op {
        EditOp::MoveActors { names, delta, rotate_yaw_deg, pivot } => {
            let scan = SaveScan::new(store);
            plan_move_actors(store, &scan, &mut plan, names, delta, *rotate_yaw_deg, *pivot)?;
        }
        EditOp::MoveLightweight { items, delta, rotate_yaw_deg, pivot } => {
            plan_move_lightweight(store, &mut plan, items, delta, *rotate_yaw_deg, *pivot)?;
        }
        EditOp::DuplicateActors { names, delta, rotate_yaw_deg, pivot, seed } => {
            let scan = SaveScan::new(store);
            plan_duplicate_actors(store, &scan, &mut plan, names, delta, *rotate_yaw_deg, *pivot, *seed)?;
        }
        EditOp::DuplicateLightweight { items, delta, rotate_yaw_deg, pivot } => {
            plan_duplicate_lightweight(store, &mut plan, items, delta, *rotate_yaw_deg, *pivot)?;
        }
        EditOp::DeleteActors { names } => {
            let scan = SaveScan::new(store);
            plan_delete_actors(store, &scan, &mut plan, names)?;
        }
        EditOp::DeleteLightweight { items } => {
            plan_delete_lightweight(store, &mut plan, items)?;
        }
    }
    Ok(plan)
}

/// Convenience for tests / borrowed callers: plan + copy + apply. The
/// memory-conscious path is `session::step_owned`, which applies the plan in
/// place on the store's own body.
pub fn apply_op(store: &SaveStore, body: &[u8], op: &EditOp) -> PResult<Vec<u8>> {
    let plan = plan_op(store, op)?;
    let mut out = body.to_vec();
    apply_plan(&mut out, plan)?;
    Ok(out)
}
