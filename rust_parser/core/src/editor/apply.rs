//! Applies edit ops as byte transforms over the decompressed body. Every
//! entry point takes the body the store was parsed from and returns a new
//! body; the caller re-parses it with the strict parser, which acts as the
//! corruption gate before anything reaches the user (or the game).

use crate::editor::ops::{EditOp, LwRef};
use crate::error::{perr, PResult};
use crate::mapdata::scan::SaveScan;
use crate::store::*;

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

/// Apply ONE op against `body` (the bytes `store` was parsed from, without
/// the quirk pad -- see `effective_body`). Returns the new body; the caller
/// must re-parse before applying the next op, so every op sees offsets and
/// values from the post-prior-op state (see `session::rebuild`).
pub fn apply_op(store: &SaveStore, body: &[u8], op: &EditOp) -> PResult<Vec<u8>> {
    let mut out = body.to_vec();
    match op {
        EditOp::MoveActors { names, delta, rotate_yaw_deg, pivot } => {
            let scan = SaveScan::new(store);
            move_actors(store, &scan, &mut out, names, delta, *rotate_yaw_deg, *pivot)?;
        }
        EditOp::MoveLightweight { items, delta, rotate_yaw_deg, pivot } => {
            move_lightweight(store, &mut out, items, delta, *rotate_yaw_deg, *pivot)?;
        }
        other => return Err(perr!("Edit op not yet supported: {:?}", other)),
    }
    Ok(out)
}
