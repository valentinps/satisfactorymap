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
use crate::save_header::FIRST_1_0_SAVE_VERSION;
use crate::store::*;
use std::collections::{BTreeSet, HashMap};

/// The byte-level effect of one op, in PRE-op offsets. Patches never overlap
/// inserts/removes (they target count/size fields and transform blocks);
/// inserts never fall inside removed spans (apply_plan remaps their offsets
/// when a plan carries both).
#[derive(Default)]
pub struct EditPlan {
    pub(crate) patches: Vec<(usize, Vec<u8>)>,
    pub(crate) inserts: Vec<(usize, Vec<u8>)>,
    pub(crate) removes: Vec<(usize, usize)>,
}

impl EditPlan {
    pub(crate) fn patch(&mut self, at: usize, bytes: impl Into<Vec<u8>>) {
        self.patches.push((at, bytes.into()));
    }

    /// Total bytes the plan's inserts add to the body. Feeds
    /// session::planned_growth, whose sum backs the wasm session's
    /// too-big-edit refusal (a grown body that could never exist under the
    /// 4GiB ceiling is refused up front; see grown_body_can_exist).
    pub fn inserted_bytes(&self) -> usize {
        self.inserts.iter().map(|(_, b)| b.len()).sum()
    }
}

/// Pre-flight for both apply paths: validate the WHOLE plan before touching
/// a byte. Two reasons. (1) A mid-apply error would leave the body
/// half-mutated, and recovery from that is a full multi-second replay from
/// the pristine copy. (2) The splice logic assumes removes are disjoint,
/// patches never target removed bytes, and inserts never fall inside removed
/// spans -- a planner bug violating any of these used to corrupt silently
/// (same-length wrong bytes pass the strict re-parse) or underflow; now it
/// aborts loudly with the body intact. Sorts removes and inserts in place.
fn validate_plan(body_len: usize, plan: &mut EditPlan) -> PResult<()> {
    plan.removes.sort_by_key(|(at, _)| *at);
    plan.inserts.sort_by_key(|(at, _)| *at);
    let mut prev_end = 0usize;
    for &(at, len) in &plan.removes {
        let end = at
            .checked_add(len)
            .filter(|&e| e <= body_len)
            .ok_or_else(|| perr!("Edit plan remove out of range"))?;
        if at < prev_end {
            return Err(perr!("Edit plan removes overlap"));
        }
        prev_end = end;
    }
    let overlaps_remove = |start: usize, end: usize| {
        let idx = plan.removes.partition_point(|&(at, len)| at + len <= start);
        idx < plan.removes.len() && plan.removes[idx].0 < end
    };
    for (at, bytes) in &plan.patches {
        let end = at
            .checked_add(bytes.len())
            .filter(|&e| e <= body_len)
            .ok_or_else(|| perr!("Edit patch out of range"))?;
        if overlaps_remove(*at, end) {
            return Err(perr!("Edit patch overlaps a removed span"));
        }
    }
    for &(at, _) in &plan.inserts {
        if at > body_len {
            return Err(perr!("Edit plan insert out of range"));
        }
        // Inside a removed span (boundaries are fine): idx of the remove
        // whose end is past `at`; if it starts strictly before `at`, the
        // insert would land mid-removal.
        let idx = plan.removes.partition_point(|&(r, len)| r + len <= at);
        if idx < plan.removes.len() && plan.removes[idx].0 < at {
            return Err(perr!("Edit plan inserts inside a removed span"));
        }
    }
    Ok(())
}

/// Mutate `body` per the plan. Length changes shift the tail with
/// copy_within instead of building a second body; the leading u64
/// uncompressedSize is refreshed at the end.
pub fn apply_plan(body: &mut Vec<u8>, mut plan: EditPlan) -> PResult<()> {
    validate_plan(body.len(), &mut plan)?;

    // Growth beyond the body's spare capacity forces a reallocation --
    // transiently ~2x the body, which the 4GB-capped wasm heap cannot
    // afford on GB-scale saves (BODY_EDIT_SLACK absorbs typical copies, but
    // a 100k-object paste overflows it). The streamed path rebuilds through
    // a compressed snapshot at ~1x peak instead; native builds keep the
    // plain realloc (plenty of RAM, no compression detour).
    let added: usize = plan.inserts.iter().map(|(_, b)| b.len()).sum();
    if cfg!(target_arch = "wasm32") && added > 0 && body.capacity() < body.len() + added {
        return apply_plan_streamed_validated(body, plan);
    }

    for (at, bytes) in &plan.patches {
        body[*at..at + bytes.len()].copy_from_slice(bytes);
    }

    // A plan may hold both (chained-belt deletes write items back into
    // surviving belts): removes run first, so shift each insert offset left
    // by the removed bytes before it. Inserts never target removed spans.
    if !plan.removes.is_empty() && !plan.inserts.is_empty() {
        for (at, _) in &mut plan.inserts {
            let mut shift = 0usize;
            for &(r, len) in &plan.removes {
                if r + len <= *at {
                    shift += len;
                } else if r >= *at {
                    break;
                } else {
                    return Err(perr!("Edit plan inserts inside a removed span"));
                }
            }
            *at -= shift;
        }
    }

    if !plan.removes.is_empty() {
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
        // Usually a no-op: bodies are allocated with BODY_EDIT_SLACK spare
        // capacity so growth stays in place. Beyond the slack this
        // reallocates (transiently ~2x the body) -- native builds only; on
        // wasm the streamed dispatch above never lets it get here.
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

/// apply_plan, but rebuilt through a compressed snapshot so peak memory
/// stays ~one body no matter how much the inserts grow it: patches apply in
/// place (size-neutral), the body is zlib-compressed (~15:1) and freed, and
/// the new body streams out of the snapshot with removes skipped and
/// inserts spliced in offset order. Produces byte-identical output to the
/// in-place path (the test suite asserts parity); costs one compression
/// round-trip, so it only runs when growth would otherwise realloc.
pub fn apply_plan_streamed(body: &mut Vec<u8>, mut plan: EditPlan) -> PResult<()> {
    validate_plan(body.len(), &mut plan)?;
    apply_plan_streamed_validated(body, plan)
}

fn apply_plan_streamed_validated(body: &mut Vec<u8>, plan: EditPlan) -> PResult<()> {
    use flate2::read::{ZlibDecoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Read;

    for (at, bytes) in &plan.patches {
        body[*at..at + bytes.len()].copy_from_slice(bytes);
    }
    let removed: usize = plan.removes.iter().map(|(_, len)| *len).sum();
    let added: usize = plan.inserts.iter().map(|(_, b)| b.len()).sum();
    let old_len = body.len();
    let new_len = old_len - removed + added;

    let mut compressed = Vec::with_capacity(old_len / 8);
    ZlibEncoder::new(&body[..], Compression::fast())
        .read_to_end(&mut compressed)
        .map_err(|e| perr!("Edit snapshot compression failed: {}", e))?;
    *body = Vec::new(); // free the old block before allocating the new one

    let mut out: Vec<u8> = Vec::with_capacity(new_len + crate::decompress::BODY_EDIT_SLACK);
    let mut dec = ZlibDecoder::new(&compressed[..]);
    let mut copy_exact = |out: &mut Vec<u8>, n: usize, discard: bool| -> PResult<()> {
        let copied = if discard {
            std::io::copy(&mut (&mut dec).take(n as u64), &mut std::io::sink())
        } else {
            std::io::copy(&mut (&mut dec).take(n as u64), out)
        }
        .map_err(|e| perr!("Edit snapshot decompression failed: {}", e))?;
        if copied != n as u64 {
            return Err(perr!("Edit snapshot truncated: {} != {}", copied, n));
        }
        Ok(())
    };

    // Merge removes and inserts by pre-op offset (both sorted by
    // validate_plan). An insert at a remove's start goes first -- same seam
    // the in-place path produces.
    let (mut pos, mut ri, mut ii) = (0usize, 0usize, 0usize);
    while ri < plan.removes.len() || ii < plan.inserts.len() {
        let r_at = plan.removes.get(ri).map_or(usize::MAX, |r| r.0);
        let i_at = plan.inserts.get(ii).map_or(usize::MAX, |i| i.0);
        if i_at <= r_at {
            copy_exact(&mut out, i_at - pos, false)?;
            pos = i_at;
            out.extend_from_slice(&plan.inserts[ii].1);
            ii += 1;
        } else {
            copy_exact(&mut out, r_at - pos, false)?;
            pos = r_at;
            copy_exact(&mut out, plan.removes[ri].1, true)?;
            pos += plan.removes[ri].1;
            ri += 1;
        }
    }
    copy_exact(&mut out, old_len - pos, false)?;
    if out.len() != new_len {
        return Err(perr!("Edit rebuild length mismatch: {} != {}", out.len(), new_len));
    }
    let size = (out.len() - 8) as u64;
    out[0..8].copy_from_slice(&size.to_le_bytes());
    *body = out;
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
pub(crate) fn yaw_sin_cos(deg: f64) -> (f64, f64) {
    match deg.rem_euclid(360.0) {
        0.0 => (0.0, 1.0),
        90.0 => (1.0, 0.0),
        180.0 => (0.0, -1.0),
        270.0 => (-1.0, 0.0),
        _ => deg.to_radians().sin_cos(),
    }
}

/// World-frame yaw composition: q' = q_z(theta) * q  (x,y,z,w layout).
pub(crate) fn rotate_quat_yaw(q: [f64; 4], deg: f64) -> [f64; 4] {
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
pub(crate) fn transform_xy(x: f64, y: f64, deg: f64, pivot: Option<[f64; 2]>, delta: &[f64; 3]) -> (f64, f64) {
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
pub(crate) fn rotate_dir_xy(x: f64, y: f64, deg: f64) -> (f64, f64) {
    if deg == 0.0 {
        return (x, y);
    }
    let (s, c) = yaw_sin_cos(deg);
    (x * c - y * s, x * s + y * c)
}

pub(crate) fn write_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_f64(buf: &mut [u8], off: usize, v: f64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn read_f64(buf: &[u8], off: usize) -> f64 {
    f64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

pub(crate) fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

pub(crate) fn read_u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

/// Patch that adds to a u32/u64 field (values read from the pre-op body,
/// which is `store.data`).
pub(crate) fn patch_add_u32(plan: &mut EditPlan, data: &[u8], off: usize, add: i64) {
    let v = (read_u32(data, off) as i64 + add) as u32;
    plan.patch(off, v.to_le_bytes().to_vec());
}

pub(crate) fn patch_add_u64(plan: &mut EditPlan, data: &[u8], off: usize, add: i64) {
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

/// Reset every serialized mPipeNetworkID IntProperty in `body` to -1
/// (unassigned). The network id is a plain int -- never an ObjectRef, so
/// ref tombstoning misses it -- and it names an FGPipeNetwork object of the
/// SOURCE save. A copy keeping it either points at a network that doesn't
/// exist (cross-save paste: the game asserts in
/// FGPipeSubsystem::AddFluidIntegrantToNetwork the moment anything touches
/// the network) or shares a live network with the original (same-save
/// duplicate: disjoint pipe graphs teleporting fluid). -1 makes the pipe
/// subsystem flood-fill fresh networks at register time, exactly as for
/// newly built pipes. Matched byte-exactly: [len 15]"mPipeNetworkID\0"
/// [len 12]"IntProperty\0", then the 9 header bytes of either property
/// layout (ue5 >= 1012: [type_a 0][size 4][pad 0]; older saves:
/// [size 4][array index 0][pad 0]), then the i32 value.
pub(crate) fn neutralize_pipe_network_ids(body: &mut [u8]) {
    const HEAD: &[u8] = b"\x0f\x00\x00\x00mPipeNetworkID\x00\x0c\x00\x00\x00IntProperty\x00";
    const NEW_TAIL: &[u8] = b"\x00\x00\x00\x00\x04\x00\x00\x00\x00";
    const OLD_TAIL: &[u8] = b"\x04\x00\x00\x00\x00\x00\x00\x00\x00";
    let mut i = 0;
    while i + HEAD.len() + NEW_TAIL.len() + 4 <= body.len() {
        if !body[i..].starts_with(HEAD) {
            i += 1;
            continue;
        }
        let t = i + HEAD.len();
        if body[t..].starts_with(NEW_TAIL) || body[t..].starts_with(OLD_TAIL) {
            let v = t + NEW_TAIL.len();
            body[v..v + 4].copy_from_slice(&(-1i32).to_le_bytes());
            i = v + 4;
        } else {
            i = t;
        }
    }
}

/// Slots of all actors whose type path exactly matches one of `candidates`.
pub(crate) fn actor_slots_of_types(store: &SaveStore, candidates: &[&str]) -> Vec<(usize, usize)> {
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
pub(crate) fn fetch(store: &SaveStore, li: usize, oi: usize) -> PResult<Object> {
    store.parse_object_at(li, oi)
}

// ---------------------------------------------------------------------------
// Move
// ---------------------------------------------------------------------------

/// Per-chain-belt patch target: where its world-space spline elements live.
struct ChainSplines {
    elements_off: usize,
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
pub(crate) fn wire_cached_locations(object: &Object, data: &[u8]) -> Vec<[f64; 3]> {
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
pub(crate) fn find_f64x3(hay: &[u8], v: [f64; 3]) -> Vec<usize> {
    let mut pat = [0u8; 24];
    for (i, x) in v.iter().enumerate() {
        pat[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
    }
    if hay.len() < 24 {
        return Vec::new();
    }
    (0..hay.len() - 23).filter(|&i| hay[i..i + 24] == pat).collect()
}

pub(crate) fn transform_vec3(v: [f64; 3], deg: f64, pivot: Option<[f64; 2]>, delta: &[f64; 3]) -> [f64; 3] {
    let (nx, ny) = transform_xy(v[0], v[1], deg, pivot, delta);
    [nx, ny, v[2] + delta[2]]
}

pub(crate) fn encode_f64x3(v: [f64; 3]) -> Vec<u8> {
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
    // Chain actors hold the belts' full item rings -- the biggest objects in
    // a late-game save -- and chain_splines_by_belt parses every one of
    // them. Only pay that when something being moved can actually be a
    // chained belt: every chainable belt/lift type path contains "Conveyor"
    // (gamedata's tests assert it for the belt table), so a move set without
    // one can never hit the `chains` map below.
    let moving_conveyor = names.iter().any(|name| {
        scan.by_instance_name
            .get(name.as_bytes())
            .is_some_and(|&(li, oi)| match &store.levels[li].headers[oi] {
                Header::Actor(a) => {
                    a.type_path.bytes(data).windows(8).any(|w| w == b"Conveyor")
                }
                Header::Component(_) => false,
            })
    });
    let chains =
        if moving_conveyor { chain_splines_by_belt(store)? } else { HashMap::new() };

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
            let span = &data[span_off..span_off + span_len as usize];
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
        // Wires are riders even when explicitly selected (they're
        // box-selectable): a wire moves only via the both-owners-moved pass
        // below -- moving one on its own would tear it off a pole it is
        // still attached to.
        if let Header::Actor(a) = header {
            let tp = a.type_path.bytes(data);
            if crate::object::POWER_LINES.iter().any(|c| c.as_bytes() == tp) {
                moved_slots.remove(&(li, oi));
                continue;
            }
        }
        let object = fetch(store, li, oi)?;
        if let Some(reason) = move_refusal(store, header, &object) {
            return Err(perr!("Cannot move {}: {}", name, reason));
        }
        moved_actor_names.insert(name.as_bytes().to_vec());
        move_one(li, oi, &object)?;
    }

    // Wires whose BOTH endpoint owners moved follow along rigidly (a wire
    // named in `names` was skipped above, so it moves exactly here or not
    // at all).
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
/// inside it by type path. Located by header type path and parsed on demand.
pub(crate) struct LwSubsystem {
    pub li: usize,
    pub oi: usize,
    pub version: u32,
    /// Offset in `SaveStore.data` of the subsystem's u32 group count.
    pub group_count_field_off: usize,
    /// Offset just past the last group (new-group insertion point).
    pub groups_end_off: usize,
    pub groups: Vec<LightweightGroup>,
}

pub(crate) fn lightweight_subsystem(store: &SaveStore) -> PResult<LwSubsystem> {
    for (li, oi) in actor_slots_of_types(store, &[crate::object::LIGHTWEIGHT_SUBSYSTEM]) {
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::Lightweight { version, group_count_field_off, groups_end_off, items } =
            object.actor_specific
        {
            return Ok(LwSubsystem {
                li,
                oi,
                version,
                group_count_field_off,
                groups_end_off,
                groups: items,
            });
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
    let groups = lightweight_subsystem(store)?.groups;
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

/// Expand the requested actor names into the full copy/delete set: each
/// actor plus its components, plus every power line whose BOTH endpoints
/// are owned by actors in the set. With `prune_unanchored_wires` (the copy
/// paths), wires are pure riders: explicitly selected ones without both
/// owners in the set are dropped, since their copies would dangle. Delete
/// passes false -- removing a lone wire is legitimate (and how the map's
/// single-wire delete works).
pub(crate) fn expand_duplicate_set(
    store: &SaveStore,
    scan: &SaveScan,
    names: &[String],
    prune_unanchored_wires: bool,
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
    let owner_in_set = |actor_names: &BTreeSet<Vec<u8>>, endpoint: &ObjectRef| -> bool {
        if endpoint.path_name.is_empty() {
            return false;
        }
        let path = endpoint.path_name.bytes(data);
        let Some(dot) = path.iter().rposition(|&b| b == b'.') else {
            return false;
        };
        actor_names.contains(&path[..dot])
    };
    // Wires are riders even when explicitly selected (they're
    // box-selectable): drop any named wire whose endpoint owners aren't both
    // in the set -- a copy of it would dangle (tombstoned endpoints), and
    // the rider scan below re-adds every wire that legitimately travels.
    let mut pruned: Vec<(usize, usize)> = Vec::new();
    for &(li, oi) in set.iter().filter(|_| prune_unanchored_wires) {
        let Header::Actor(a) = &store.levels[li].headers[oi] else { continue };
        let tp = a.type_path.bytes(data);
        if !crate::object::POWER_LINES.iter().any(|c| c.as_bytes() == tp) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
            if !(owner_in_set(&actor_names, a) && owner_in_set(&actor_names, b)) {
                pruned.push((li, oi));
            }
        }
    }
    for (li, oi) in pruned {
        set.remove(&(li, oi));
        if let Some((_, components)) = &fetch(store, li, oi)?.actor_reference_associations {
            for comp in components {
                if let Some(&slot) = scan.by_instance_name.get(comp.path_name.bytes(data)) {
                    set.remove(&slot);
                }
            }
        }
    }
    let mut wires: Vec<(usize, usize)> = Vec::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::POWER_LINES) {
        if set.contains(&(li, oi)) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
            if owner_in_set(&actor_names, a) && owner_in_set(&actor_names, b) {
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
    let set = expand_duplicate_set(store, scan, names, true)?;
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
    let renames = rename::build_rename_map(&actor_name_list, seed, &exists)?;
    let rename_matcher = rename::SubstMatcher::new(&renames);

    // External instance refs (outside the set) get same-length tombstones so
    // the copies don't claim connections the originals still own.
    let mut rng = rename::Rng(seed ^ 0x746f6d6273746f6e); // independent stream for tombstones
    let mut tombstones: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    for &(li, oi) in &set {
        let object = fetch(store, li, oi)?;
        let mut err: Option<crate::error::PError> = None;
        rename::visit_object_refs(&object, &mut |r: &ObjectRef| {
            if err.is_some() || r.path_name.is_empty() {
                return;
            }
            // Asset-scoped refs (SoftObjectProperty with the asset path in
            // the LEVEL part, e.g. level "/Game/.../BPW_Sign4x1_2", path
            // "BPW_Sign4x1_2_C" -- a sign layout's widget class) are static
            // data, not save instances: tombstoning one blanks every
            // duplicated sign. Same exemption as the paste path
            // (clipboard.rs) -- keep the two in step.
            if r.level_name.bytes(data).starts_with(b"/") {
                return;
            }
            let path = r.path_name.bytes(data);
            if rename_matcher.contains_any(path) || tombstones.contains_key(path) {
                return; // Internal ref: the rename substitution remaps it.
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
    // Two linear passes: rename keys never appear in tombstone targets (an
    // external path containing a renamed segment would have been internal)
    // and vice versa, so order doesn't matter.
    let tombstone_matcher = rename::SubstMatcher::new(&tombstones);

    // Copy, substitute, patch transforms.
    let mut new_headers: Vec<u8> = Vec::new();
    let mut new_bodies: Vec<u8> = Vec::new();
    let n_new = set.len() as i64;
    for &(li, oi) in &set {
        let (h_off, h_len) = store.levels[li].header_spans[oi];
        let (b_off, b_len) = store.levels[li].object_spans[oi];
        let mut header_copy = data[h_off..h_off + h_len as usize].to_vec();
        let mut body_copy = data[b_off..b_off + b_len as usize].to_vec();
        rename_matcher.substitute(&mut header_copy);
        rename_matcher.substitute(&mut body_copy);
        tombstone_matcher.substitute(&mut header_copy);
        tombstone_matcher.substitute(&mut body_copy);
        neutralize_pipe_network_ids(&mut body_copy);

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
    let LwSubsystem { li, oi, groups, .. } = lightweight_subsystem(store)?;

    let mut added_per_group: HashMap<usize, i64> = HashMap::new(); // count_field_off -> count
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

        let mut copy = lightweight_record_bytes(data, instance)?;

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

/// A lightweight record's bytes ready for re-placement: verbatim copy with
/// the blueprint proxy ref emptied (the copy is hand-placed; empty strings
/// are just an i32 0, so the record may shrink -- fine, it's fresh bytes).
pub(crate) fn lightweight_record_bytes(
    data: &[u8],
    instance: &LightweightInstance,
) -> PResult<Vec<u8>> {
    let r = instance.record_off as usize;
    let mut copy = data[r..r + instance.record_len as usize].to_vec();
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
    Ok(copy)
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

/// One belt-format item record (the pre-chain per-belt layout, v44 item
/// format -- chain actors only exist in saves past that gate): [u32 0
/// InventoryItem padding][item class string][u32 0 no item state]
/// [f32 position along the belt, cm].
fn belt_item_record(path: &[u8], state: &[u8], position: f32) -> Vec<u8> {
    let mut r = Vec::with_capacity(path.len() + state.len() + 17);
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&(path.len() as u32 + 1).to_le_bytes());
    r.extend_from_slice(path);
    r.push(0);
    // itemState: the has-state flag, then that record's bytes unchanged.
    r.extend_from_slice(&(!state.is_empty() as u32).to_le_bytes());
    r.extend_from_slice(state);
    r.extend_from_slice(&position.to_le_bytes());
    r
}

fn plan_delete_actors(
    store: &SaveStore,
    scan: &SaveScan,
    plan: &mut EditPlan,
    names: &[String],
) -> PResult<()> {
    let data: &[u8] = &store.data;
    let set = expand_duplicate_set(store, scan, names, false)?;
    if set.is_empty() {
        return Err(perr!("Nothing to delete"));
    }
    let level_idx = set.iter().next().unwrap().0;
    if set.iter().any(|&(li, _)| li != level_idx) {
        return Err(perr!("Cannot delete objects from different world levels at once"));
    }

    let mut deleted_actor_names: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
    for &(li, oi) in &set {
        deleted_actor_names.insert(store.levels[li].headers[oi].instance_name().bytes(data));
    }

    // A deleted belt drags its whole conveyor-chain ACTOR with it: the
    // chain's packed belt list cannot point at a deleted belt. The line's
    // other belts survive with a dangling mConveyorChainActor ref -- the
    // game reads that as null and rebuilds chains on load (the same
    // migration that upgrades pre-chain saves), re-splitting the line at
    // the gap. And like cutting a line in game, only the DELETED segments'
    // items are lost: every surviving belt gets its own slice of the
    // chain's item ring written back as per-belt records (the pre-chain
    // format that migration reads), so the rebuilt chains come up loaded.
    // Full rationale + what to do if the game ever drops this migration
    // path: docs/chained-belt-delete.md.
    let mut extra_chains: Vec<(usize, usize)> = Vec::new();
    // (belt li, belt oi, count field, insert offset, record bytes, count)
    let mut belt_writebacks: Vec<(usize, usize, usize, usize, Vec<u8>, i64)> = Vec::new();
    for (li, oi) in actor_slots_of_types(store, &crate::object::CONVEYOR_CHAINS) {
        if set.contains(&(li, oi)) {
            continue;
        }
        let object = fetch(store, li, oi)?;
        let ActorSpecific::ConveyorChain {
            belts, items, maximum_items, chain_lead_item_index, ..
        } = &object.actor_specific
        else {
            continue;
        };
        if !belts.iter().any(|cb| deleted_actor_names.contains(cb.belt.path_name.bytes(data))) {
            continue;
        }
        if li != level_idx {
            return Err(perr!(
                "Cannot delete: the belt's conveyor chain lives in a different world level"
            ));
        }
        extra_chains.push((li, oi));
        if items.is_empty() || *maximum_items <= 0 || *chain_lead_item_index < 0 {
            continue;
        }
        let maximum = *maximum_items as i64;
        for cb in belts {
            let belt_path = cb.belt.path_name.bytes(data);
            if deleted_actor_names.contains(belt_path)
                || cb.lead_item_index < 0
                || cb.tail_item_index < 0
            {
                continue;
            }
            let Some(&(bli, boi)) = scan.by_instance_name.get(belt_path) else { continue };
            // This belt's contiguous window of the chain's slot ring (the
            // same arithmetic the tooltip's per-segment item list uses).
            let start = (cb.lead_item_index as i64 - *chain_lead_item_index as i64)
                .rem_euclid(maximum) as usize;
            let count = (cb.tail_item_index as i64 - cb.lead_item_index as i64)
                .rem_euclid(maximum) as usize
                + 1;
            let slots: Vec<(usize, &[u8], &[u8])> = items
                .iter()
                .skip(start)
                .take(count)
                .enumerate()
                .filter_map(|(j, item)| {
                    let b = item.item_path.bytes(data);
                    // The per-item state (a jetpack's fuel, a weapon's
                    // magazine) rides along verbatim -- same record layout
                    // on a belt as in the chain's ring.
                    let st = item.state.map_or(&[][..], |s| s.bytes(data));
                    (!b.is_empty() && b.is_ascii()).then_some((j, b, st))
                })
                .collect();
            if slots.is_empty() {
                continue;
            }
            let belt_object = fetch(store, bli, boi)?;
            let ActorSpecific::ConveyorBelt { count_field_off, end_off, .. } =
                &belt_object.actor_specific
            else {
                continue; // modded belt type outside the known tables
            };
            // Belt length from the chain's spline chords: slots span the
            // belt uniformly, so slot index -> distance along the belt.
            // Approximate is fine -- the game re-derives exact positions.
            let mut belt_len = 0f64;
            for w in cb.elements.windows(2) {
                let (a, b) = (w[0][0], w[1][0]);
                belt_len +=
                    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            }
            if !belt_len.is_finite() || belt_len <= 0.0 {
                belt_len = 100.0;
            }
            let mut records = Vec::new();
            for &(j, path, state) in &slots {
                let position = (belt_len * (j as f64 + 0.5) / count as f64) as f32;
                records.extend_from_slice(&belt_item_record(path, state, position));
            }
            belt_writebacks.push((
                bli,
                boi,
                *count_field_off,
                *end_off,
                records,
                slots.len() as i64,
            ));
        }
    }
    // Also delete power lines with EITHER endpoint on a deleted actor -- a
    // wire to nowhere renders and simulates wrong in game.
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
    for (li, oi) in extra_wires.into_iter().chain(extra_chains) {
        full_set.insert((li, oi));
        // A cascade-deleted actor's own components (if any) go with it.
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

    // Belt-item write-backs grow surviving belt bodies (insert offsets are
    // pre-remove; apply_plan remaps them past the removed spans).
    let mut inserted_body_bytes = 0i64;
    for (bli, boi, count_field_off, end_off, records, n) in belt_writebacks {
        inserted_body_bytes += records.len() as i64;
        patch_add_u32(plan, data, count_field_off, n);
        // Belt object body grows: [gv u32][migrate u32][object_size u32].
        let object_size_field = store.levels[bli].object_spans[boi].0 as usize + 8;
        patch_add_u32(plan, data, object_size_field, records.len() as i64);
        plan.inserts.push((end_off, records));
    }

    let n_removed = full_set.len() as i64;
    let spans = &level.spans;
    patch_add_u64(plan, data, spans.header_size_field_off as usize, -removed_header_bytes);
    patch_add_u32(plan, data, spans.header_size_field_off as usize + 8, -n_removed);
    patch_add_u64(
        plan,
        data,
        spans.objects_size_field_off as usize,
        inserted_body_bytes - removed_body_bytes,
    );
    patch_add_u32(plan, data, spans.object_count_field_off as usize, -n_removed);
    Ok(())
}

fn plan_delete_lightweight(store: &SaveStore, plan: &mut EditPlan, items: &[LwRef]) -> PResult<()> {
    let data: &[u8] = &store.data;
    let LwSubsystem { li, oi, groups, .. } = lightweight_subsystem(store)?;

    let mut removed_per_group: HashMap<usize, i64> = HashMap::new();
    let mut seen: BTreeSet<(usize, u32)> = BTreeSet::new(); // (count_field_off, index)
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
///
/// Every edit route reaches the engine through here -- apply_op, session::step
/// and session::planned_growth all plan first, as do the wasm/Tauri bindings --
/// so the pre-1.0 refusal below cannot be walked around.
pub fn plan_op(store: &SaveStore, op: &EditOp) -> PResult<EditPlan> {
    // COMPAT EXPERIMENT (see save_header::FIRST_1_0_SAVE_VERSION): an Update 8
    // save parses and maps fine, but every record this engine writes is
    // 1.0-format -- actor headers gain a flags u32, levels gain a version
    // field. Splicing those into a v42 body yields a file the game cannot
    // load, and the damage only shows up when the player tries to load it, so
    // refuse up front instead. Viewing, searching and exporting an untouched
    // save are unaffected.
    if store.info.save_version < FIRST_1_0_SAVE_VERSION {
        return Err(perr!(
            "This save is from an older game version (save version {}), which can be viewed but not edited: the editor only writes 1.0-format records.",
            store.info.save_version
        ));
    }
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
        EditOp::PasteExternal {
            save_version,
            object_version,
            lightweight_version,
            z,
            z_len,
            actors,
            lightweight,
            anchor,
            delta,
            rotate_yaw_deg,
            seed,
        } => {
            crate::editor::clipboard::plan_paste_external(
                store,
                &mut plan,
                *save_version,
                *object_version,
                *lightweight_version,
                z.as_deref(),
                *z_len,
                actors,
                lightweight,
                *anchor,
                delta,
                *rotate_yaw_deg,
                *seed,
            )?;
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

#[cfg(test)]
mod plan_apply_tests {
    use super::*;

    fn base_body() -> Vec<u8> {
        let mut body: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        body[0..8].copy_from_slice(&((4096u64 - 8).to_le_bytes()));
        body
    }

    fn sample_plan() -> EditPlan {
        let mut plan = EditPlan::default();
        plan.patch(100, vec![0xAA; 16]);
        plan.patch(3000, vec![0xBB; 4]);
        plan.removes.push((512, 128));
        plan.removes.push((1024, 64));
        plan.inserts.push((512, vec![0x11; 300]));   // at a remove's start
        plan.inserts.push((2048, vec![0x22; 500]));
        plan.inserts.push((2048, vec![0x33; 100])); // same offset, keeps order
        plan.inserts.push((4096, vec![0x44; 50]));  // at end-of-body
        plan
    }

    /// A written-back belt item mirrors the slot the game itself writes: the
    /// leading u32, the item class path, the itemState flag and (when the
    /// item has one) its state record verbatim, then the position. Getting
    /// the flag wrong here writes a save the game reads as drift -- the same
    /// mistake that made stated items unreadable in the first place.
    #[test]
    fn belt_item_record_writes_the_item_state_slot() {
        let path = b"/Game/X/Desc_Y.Desc_Y_C";

        let plain = belt_item_record(path, &[], 12.5f32);
        let mut expected = Vec::new();
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.extend_from_slice(&(path.len() as u32 + 1).to_le_bytes());
        expected.extend_from_slice(path);
        expected.push(0);
        expected.extend_from_slice(&0u32.to_le_bytes()); // no state
        expected.extend_from_slice(&12.5f32.to_le_bytes());
        assert_eq!(plain, expected);

        // A state record rides along whole, behind a set flag.
        let state = [0xEFu8; 40];
        let stated = belt_item_record(path, &state, 12.5f32);
        assert_eq!(stated.len(), plain.len() + state.len());
        let flag_at = stated.len() - 4 - state.len() - 4;
        assert_eq!(&stated[..flag_at], &plain[..flag_at]);
        assert_eq!(&stated[flag_at..flag_at + 4], &1u32.to_le_bytes());
        assert_eq!(&stated[flag_at + 4..flag_at + 4 + state.len()], &state);
        assert_eq!(&stated[stated.len() - 4..], &12.5f32.to_le_bytes());
    }

    /// The streamed (compress-free-rebuild) path must produce byte-identical
    /// output to the in-place path for the same plan.
    #[test]
    fn streamed_matches_in_place() {
        let mut in_place = base_body();
        in_place.reserve_exact(4096); // headroom: stays on the in-place path
        apply_plan(&mut in_place, sample_plan()).unwrap();

        let mut streamed = base_body();
        apply_plan_streamed(&mut streamed, sample_plan()).unwrap();

        assert_eq!(in_place, streamed);
        assert_eq!(in_place.len(), 4096 - 128 - 64 + 300 + 500 + 100 + 50);
    }

    /// Inserts inside a removed span are a planner bug both paths refuse.
    #[test]
    fn insert_inside_removed_span_refused() {
        let mut plan = EditPlan::default();
        plan.removes.push((512, 128));
        plan.inserts.push((600, vec![1, 2, 3]));
        let mut body = base_body();
        assert!(apply_plan(&mut body, plan).is_err());
        assert_eq!(body, base_body(), "body must be untouched after refusal");
    }
}
