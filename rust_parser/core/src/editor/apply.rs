//! Applies edit ops as byte transforms over the decompressed body. Every
//! entry point takes the body the store was parsed from and returns a new
//! body; the caller re-parses it with the strict parser, which acts as the
//! corruption gate before anything reaches the user (or the game).

use crate::editor::ops::{EditOp, LwRef};
use crate::editor::rename;
use crate::error::{perr, PResult};
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use std::collections::{BTreeSet, HashMap};

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
    let norm = deg.rem_euclid(360.0);
    match norm {
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

fn write_f32(body: &mut [u8], off: usize, v: f32) {
    body[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_f64(body: &mut [u8], off: usize, v: f64) {
    body[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn read_f64(body: &[u8], off: usize) -> f64 {
    f64::from_le_bytes(body[off..off + 8].try_into().unwrap())
}

/// Per-chain-belt patch target: where its world-space spline elements live.
struct ChainSplines {
    elements_off: u32,
    element_count: usize,
}

/// belt instance name -> chain spline extents, across every chain actor.
fn chain_splines_by_belt<'a>(store: &'a SaveStore) -> std::collections::HashMap<&'a [u8], ChainSplines> {
    let mut map = std::collections::HashMap::new();
    for level in &store.levels {
        for object in &level.objects {
            if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
                for cb in belts {
                    map.insert(
                        cb.belt.path_name.bytes(&store.data),
                        ChainSplines { elements_off: cb.elements_off, element_count: cb.elements.len() },
                    );
                }
            }
        }
    }
    map
}

fn move_actors(
    store: &SaveStore,
    scan: &SaveScan,
    body: &mut [u8],
    names: &[String],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let chains = chain_splines_by_belt(store);
    for name in names {
        let Some(&(li, oi)) = scan.by_instance_name.get(name.as_bytes()) else {
            return Err(perr!("No such instance: {}", name));
        };
        let header = &store.levels[li].headers[oi];
        let object = &store.levels[li].objects[oi];
        if let Some(reason) = move_refusal(store, header, object) {
            return Err(perr!("Cannot move {}: {}", name, reason));
        }
        let Header::Actor(actor) = header else { unreachable!() };

        // Header transform: quat f32x4 then position f32x3.
        let t = actor.transform_off as usize;
        if rotate_yaw_deg != 0.0 {
            let q = rotate_quat_yaw(
                [actor.rotation[0] as f64, actor.rotation[1] as f64, actor.rotation[2] as f64, actor.rotation[3] as f64],
                rotate_yaw_deg,
            );
            for (i, v) in q.iter().enumerate() {
                write_f32(body, t + i * 4, *v as f32);
            }
        }
        let (nx, ny) = transform_xy(actor.position[0] as f64, actor.position[1] as f64, rotate_yaw_deg, pivot, delta);
        write_f32(body, t + 16, nx as f32);
        write_f32(body, t + 20, ny as f32);
        write_f32(body, t + 24, (actor.position[2] as f64 + delta[2]) as f32);

        // Chained belts: their chain actor's spline elements are world-space
        // [location, arriveTangent, leaveTangent] f64 triplets.
        if let Some(cs) = chains.get(name.as_bytes()) {
            for e in 0..cs.element_count {
                let base = cs.elements_off as usize + e * 72;
                let (lx, ly) = transform_xy(read_f64(body, base), read_f64(body, base + 8), rotate_yaw_deg, pivot, delta);
                write_f64(body, base, lx);
                write_f64(body, base + 8, ly);
                write_f64(body, base + 16, read_f64(body, base + 16) + delta[2]);
                for row in 1..3 {
                    let r = base + row * 24;
                    let (tx, ty) = rotate_dir_xy(read_f64(body, r), read_f64(body, r + 8), rotate_yaw_deg);
                    write_f64(body, r, tx);
                    write_f64(body, r + 8, ty);
                }
            }
        }
    }
    Ok(())
}

/// The one Lightweight subsystem object (persistent level) -- lightweight
/// edits address groups inside it by type path.
fn lightweight_groups<'a>(store: &'a SaveStore) -> PResult<&'a [LightweightGroup]> {
    for level in &store.levels {
        for object in &level.objects {
            if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                return Ok(items);
            }
        }
    }
    Err(perr!("Save has no lightweight buildable subsystem"))
}

fn move_lightweight(
    store: &SaveStore,
    body: &mut [u8],
    items: &[LwRef],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<()> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    let groups = lightweight_groups(store)?;
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
            for (i, v) in q.iter().enumerate() {
                write_f64(body, r + i * 8, *v);
            }
        }
        let (nx, ny) = transform_xy(instance.position[0], instance.position[1], rotate_yaw_deg, pivot, delta);
        write_f64(body, r + 32, nx);
        write_f64(body, r + 40, ny);
        write_f64(body, r + 48, instance.position[2] + delta[2]);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Duplication
// ---------------------------------------------------------------------------

fn read_u32(body: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(body[off..off + 4].try_into().unwrap())
}

fn read_u64_at(body: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(body[off..off + 8].try_into().unwrap())
}

fn add_u32(body: &mut [u8], off: usize, add: u32) {
    let v = read_u32(body, off) + add;
    body[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn add_u64(body: &mut [u8], off: usize, add: u64) {
    let v = read_u64_at(body, off) + add;
    body[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Assemble a new body from a (field-patched) original plus insertions at
/// original-space offsets, then fix the leading u64 uncompressedSize.
fn splice(body: Vec<u8>, mut insertions: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    insertions.sort_by_key(|(at, _)| *at);
    let added: usize = insertions.iter().map(|(_, b)| b.len()).sum();
    let mut out = Vec::with_capacity(body.len() + added);
    let mut cursor = 0usize;
    for (at, bytes) in insertions {
        out.extend_from_slice(&body[cursor..at]);
        out.extend_from_slice(&bytes);
        cursor = at;
    }
    out.extend_from_slice(&body[cursor..]);
    let size = (out.len() - 8) as u64;
    out[0..8].copy_from_slice(&size.to_le_bytes());
    out
}

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
        let object = &store.levels[li].objects[oi];
        if let Some(reason) = move_refusal(store, header, object) {
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
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, object) in level.objects.iter().enumerate() {
            if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
                if !set.contains(&(li, oi)) && owner_in_set(a) && owner_in_set(b) {
                    wires.push((li, oi));
                }
            }
        }
    }
    for (li, oi) in wires {
        add_actor(&mut set, &mut actor_names, li, oi)?;
    }
    Ok(set)
}

fn duplicate_actors(
    store: &SaveStore,
    scan: &SaveScan,
    body: Vec<u8>,
    names: &[String],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
    seed: u64,
) -> PResult<Vec<u8>> {
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
        let object = &store.levels[li].objects[oi];
        let mut err: Option<crate::error::PError> = None;
        rename::visit_object_refs(object, &mut |r: &ObjectRef| {
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
    let n_new = set.len() as u32;
    for &(li, oi) in &set {
        let (h_off, h_len) = store.levels[li].header_spans[oi];
        let (b_off, b_len) = store.levels[li].object_spans[oi];
        let mut header_copy = body[h_off as usize..(h_off + h_len) as usize].to_vec();
        let mut body_copy = body[b_off as usize..(b_off + b_len) as usize].to_vec();
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
        }
        new_headers.extend_from_slice(&header_copy);
        new_bodies.extend_from_slice(&body_copy);
    }

    // Count/size cascade (all offsets are original-space; splice() adjusts
    // the leading uncompressedSize afterwards).
    let mut patched = body;
    let spans = &level.spans;
    add_u64(&mut patched, spans.header_size_field_off as usize, new_headers.len() as u64);
    add_u32(&mut patched, spans.header_size_field_off as usize + 8, n_new);
    add_u64(&mut patched, spans.objects_size_field_off as usize, new_bodies.len() as u64);
    add_u32(&mut patched, spans.object_count_field_off as usize, n_new);

    Ok(splice(
        patched,
        vec![
            (spans.headers_insert_off as usize, new_headers),
            (spans.bodies_insert_off as usize, new_bodies),
        ],
    ))
}

fn duplicate_lightweight(
    store: &SaveStore,
    body: Vec<u8>,
    items: &[LwRef],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    pivot: Option<[f64; 2]>,
) -> PResult<Vec<u8>> {
    if rotate_yaw_deg != 0.0 && pivot.is_none() {
        return Err(perr!("rotate requires a pivot"));
    }
    // Locate the subsystem object (for its object_size field) and its groups.
    let mut located: Option<(usize, usize)> = None;
    'outer: for (li, level) in store.levels.iter().enumerate() {
        for (oi, object) in level.objects.iter().enumerate() {
            if matches!(object.actor_specific, ActorSpecific::Lightweight { .. }) {
                located = Some((li, oi));
                break 'outer;
            }
        }
    }
    let Some((li, oi)) = located else {
        return Err(perr!("Save has no lightweight buildable subsystem"));
    };
    let ActorSpecific::Lightweight { items: groups, .. } = &store.levels[li].objects[oi].actor_specific else {
        unreachable!()
    };

    let mut insertions: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut added_per_group: HashMap<u32, (u32, usize)> = HashMap::new(); // count_field_off -> (count, bytes)
    let mut total_added = 0usize;

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
        let mut copy = body[r..r + instance.record_len as usize].to_vec();

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

        total_added += copy.len();
        let entry = added_per_group.entry(group.count_field_off).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += copy.len();
        insertions.push((group.end_off as usize, copy));
    }

    let mut patched = body;
    for (count_field_off, (count, _)) in &added_per_group {
        add_u32(&mut patched, *count_field_off as usize, *count);
    }
    // Subsystem object body grows: [gv u32][migrate u32][object_size u32].
    let object_size_field = store.levels[li].object_spans[oi].0 as usize + 8;
    add_u32(&mut patched, object_size_field, total_added as u32);
    add_u64(&mut patched, store.levels[li].spans.objects_size_field_off as usize, total_added as u64);

    Ok(splice(patched, insertions))
}

/// Apply ONE op against `body` (the bytes `store` was parsed from, without
/// the quirk pad -- see `effective_body`). Returns the new body; the caller
/// must re-parse before applying the next op, so every op sees offsets and
/// values from the post-prior-op state (see `session::rebuild`).
pub fn apply_op(store: &SaveStore, body: &[u8], op: &EditOp) -> PResult<Vec<u8>> {
    match op {
        EditOp::MoveActors { names, delta, rotate_yaw_deg, pivot } => {
            let mut out = body.to_vec();
            let scan = SaveScan::new(store);
            move_actors(store, &scan, &mut out, names, delta, *rotate_yaw_deg, *pivot)?;
            Ok(out)
        }
        EditOp::MoveLightweight { items, delta, rotate_yaw_deg, pivot } => {
            let mut out = body.to_vec();
            move_lightweight(store, &mut out, items, delta, *rotate_yaw_deg, *pivot)?;
            Ok(out)
        }
        EditOp::DuplicateActors { names, delta, rotate_yaw_deg, pivot, seed } => {
            let scan = SaveScan::new(store);
            duplicate_actors(store, &scan, body.to_vec(), names, delta, *rotate_yaw_deg, *pivot, *seed)
        }
        EditOp::DuplicateLightweight { items, delta, rotate_yaw_deg, pivot } => {
            duplicate_lightweight(store, body.to_vec(), items, delta, *rotate_yaw_deg, *pivot)
        }
        other => return Err(perr!("Edit op not yet supported: {:?}", other)),
    }
}
