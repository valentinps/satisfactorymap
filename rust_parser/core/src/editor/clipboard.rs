//! Cross-save clipboard: serialize selected objects to a portable JSON blob
//! (raw header/body bytes + version metadata) that travels through the OS
//! clipboard between tabs/sessions, and plan pasting such a blob into a
//! DIFFERENT save. Byte formats are version-locked, so the paste refuses
//! blobs from saves the target isn't byte-compatible with; the strict
//! re-parse remains the final gate as with every edit.

use crate::editor::apply::{
    self, expand_duplicate_set, lightweight_record_bytes, lightweight_subsystem,
    patch_add_u32, patch_add_u64, rotate_quat_yaw, transform_xy, transform_vec3, write_f32,
    write_f64, EditPlan,
};
use crate::editor::ops::{ExtRef, ForeignActor, ForeignLightweight, ForeignPayload, LwRef};
use crate::editor::session::{compress_body, decompress_body};
use crate::editor::rename;
use crate::error::{perr, PResult};
use crate::gamedata;
use crate::level::parse_one_header;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::object::parse_object;
use crate::reader::Cursor;
use crate::store::*;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Parse the frontend's lightweight-ref list (the wasm crate has no
/// serde_json of its own).
pub fn parse_lw_refs(json: &str) -> PResult<Vec<LwRef>> {
    serde_json::from_str(json).map_err(|e| perr!("Bad lightweight refs: {}", e))
}

/// Decode + inflate a v2 blob's compressed payload.
pub fn inflate_payload(z: &str, z_len: u64) -> PResult<ForeignPayload> {
    let compressed = B64.decode(z).map_err(|e| perr!("Bad clipboard data: {}", e))?;
    let raw = decompress_body(&compressed, z_len as usize)?;
    serde_json::from_slice(&raw).map_err(|e| perr!("Bad clipboard payload: {}", e))
}

/// The resource a world-static node actor yields in `store`: the per-node
/// game-mode override (randomized-nodes sessions write mResourceClassOverride)
/// when present, else the static table's desc class. None when neither is
/// known -- callers treat that as "cannot verify, don't relink".
fn node_resource_type(store: &SaveStore, li: usize, oi: usize) -> Option<String> {
    let data: &[u8] = &store.data;
    if let Ok(object) = store.parse_object_at(li, oi) {
        if let Some(r) = props::object_ref(&object.properties, data, b"mResourceClassOverride") {
            let path = r.path_name.bytes(data);
            if !path.is_empty() {
                return Some(props::lossy(props::short_name(path)));
            }
        }
    }
    let name = store.levels[li].headers[oi].instance_name().bytes(data);
    let entry = gamedata::get().resource_purity.get(std::str::from_utf8(name).ok()?)?;
    Some(entry.0.clone())
}

/// Relink gate: the TARGET save has the same-named actor, of the same class,
/// yielding the same resource the blob recorded from the source save. Any
/// uncertainty (unknown actor, class mismatch, undeterminable resource on
/// either side) fails the gate and the ref is tombstoned as before.
fn relink_target_matches(
    store: &SaveStore,
    scan: &SaveScan,
    path: &[u8],
    er: &ExtRef,
) -> bool {
    if er.res.is_empty() {
        return false;
    }
    let Some(&(li, oi)) = scan.by_instance_name.get(path) else {
        return false;
    };
    let Header::Actor(a) = &store.levels[li].headers[oi] else {
        return false;
    };
    if a.type_path.bytes(&store.data) != er.cls.as_bytes() {
        return false;
    }
    node_resource_type(store, li, oi).as_deref() == Some(er.res.as_str())
}

/// True when `type_path` is a world-static resource actor (resource node,
/// fracking core/satellite, geyser) -- the only refs a paste may relink.
fn is_world_static_class(type_path: &[u8]) -> bool {
    gamedata::get()
        .type_paths
        .mined_resources
        .iter()
        .any(|p| p.as_bytes() == type_path)
}

/// Serialize the expanded selection (actors + their components + fully
/// internal wires, plus lightweight records) into the clipboard blob JSON.
/// Returns the (possibly huge) blob plus a small metadata JSON -- everything
/// the paste UI needs (anchor/bbox/count) without holding the blob itself.
pub fn extract_clipboard_with_meta(
    store: &SaveStore,
    names: &[String],
    items: &[LwRef],
) -> PResult<(String, String)> {
    let data: &[u8] = &store.data;
    let scan = SaveScan::new(store);
    let gd = gamedata::get();

    let mut actors: Vec<serde_json::Value> = Vec::new();
    let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    // Z range travels too (blob "anchorZ") so the paste panel can offer an
    // absolute-altitude field for cross-save pastes.
    let mut zbox = [f64::INFINITY, f64::NEG_INFINITY];
    let mut grow = |x: f64, y: f64, z: f64| {
        bbox[0] = bbox[0].min(x);
        bbox[1] = bbox[1].min(y);
        bbox[2] = bbox[2].max(x);
        bbox[3] = bbox[3].max(y);
        zbox[0] = zbox[0].min(z);
        zbox[1] = zbox[1].max(z);
    };

    // Refs from copied extractor-type actors to world-static resource actors
    // (nodes/cores/satellites/geysers): recorded with the resource they carry
    // in THIS save so the paste can relink them in a byte-compatible target
    // when -- and only when -- the target's same-named actor yields the same
    // resource (randomized-node sessions differ per save).
    let mut ext_refs: HashMap<String, ExtRef> = HashMap::new();
    let extractor_classes: Vec<&[u8]> =
        gd.type_paths.miners.iter().map(|p| p.as_bytes()).collect();

    let mut object_version: Option<i32> = None;
    if !names.is_empty() {
        let set = expand_duplicate_set(store, &scan, names)?;
        let level_idx = set.iter().next().map(|&(li, _)| li).unwrap_or(0);
        if set.iter().any(|&(li, _)| li != level_idx) {
            return Err(perr!("Cannot copy objects from different world levels together"));
        }
        object_version = Some(store.levels[level_idx].object_ue5_version);
        for &(li, oi) in &set {
            let (h_off, h_len) = store.levels[li].header_spans[oi];
            let (b_off, b_len) = store.levels[li].object_spans[oi];
            actors.push(json!({
                "h": B64.encode(&data[h_off..h_off + h_len as usize]),
                "b": B64.encode(&data[b_off..b_off + b_len as usize]),
            }));
            if let Header::Actor(a) = &store.levels[li].headers[oi] {
                grow(a.position[0] as f64, a.position[1] as f64, a.position[2] as f64);
                // Only extractor-type buildings reference world-static
                // resource actors, so everything else skips the re-parse.
                if extractor_classes.contains(&a.type_path.bytes(data)) {
                    let object = store.parse_object_at(li, oi)?;
                    rename::visit_object_refs(&object, &mut |r: &ObjectRef| {
                        let path = r.path_name.bytes(data);
                        if path.is_empty() || path.starts_with(b"/") {
                            return;
                        }
                        let Ok(path_str) = std::str::from_utf8(path) else { return };
                        if ext_refs.contains_key(path_str) {
                            return;
                        }
                        let Some(&(nli, noi)) = scan.by_instance_name.get(path) else { return };
                        let Header::Actor(node) = &store.levels[nli].headers[noi] else { return };
                        let cls = node.type_path.bytes(data);
                        if !is_world_static_class(cls) {
                            return;
                        }
                        ext_refs.insert(path_str.to_string(), ExtRef {
                            cls: props::lossy(cls),
                            res: node_resource_type(store, nli, noi).unwrap_or_default(),
                        });
                    });
                }
            }
        }
    }

    let mut lightweight: Vec<serde_json::Value> = Vec::new();
    let mut lightweight_version: Option<u32> = None;
    if !items.is_empty() {
        let (_, _, version, groups) = lightweight_subsystem(store)?;
        lightweight_version = Some(version);
        for item in items {
            let group = groups
                .iter()
                .find(|g| g.type_path.eq_ascii(data, &item.type_path))
                .ok_or_else(|| perr!("No lightweight group for {}", item.type_path))?;
            let instance = group
                .instances
                .get(item.index as usize)
                .ok_or_else(|| perr!("Lightweight index {} out of range for {}", item.index, item.type_path))?;
            lightweight.push(json!({
                "typePath": item.type_path,
                "r": B64.encode(&lightweight_record_bytes(data, instance)?),
            }));
            grow(instance.position[0], instance.position[1], instance.position[2]);
        }
    }

    if actors.is_empty() && lightweight.is_empty() {
        return Err(perr!("Nothing to copy"));
    }
    let object_version = object_version.unwrap_or_else(|| {
        store
            .levels
            .iter()
            .find(|l| l.level_name.is_none())
            .map(|l| l.object_ue5_version)
            .unwrap_or(-1)
    });

    let anchor = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
    let anchor_z = (zbox[0] + zbox[1]) / 2.0;
    let count = (names.len() + items.len()) as u64;
    // The payload is zlib-compressed: raw save bytes shrink ~6-10x, which is
    // what keeps six-figure copies inside OS-clipboard and JS-string limits.
    let payload =
        json!({ "actors": actors, "lightweight": lightweight, "extRefs": ext_refs }).to_string();
    let (compressed, raw_len) = compress_body(payload.as_bytes());
    drop(payload);
    let blob = json!({
        "smapPaste": 2,
        "saveVersion": store.info.save_version,
        "objectVersion": object_version,
        "lightweightVersion": lightweight_version,
        "anchor": anchor,
        "anchorZ": anchor_z,
        "bboxWorld": bbox,
        "count": count,
        "zLen": raw_len as u64,
        "z": B64.encode(&compressed),
    })
    .to_string();
    // Everything the paste UI needs to plan a paste of this blob without
    // holding the blob itself (the desktop shell keeps big blobs native-side
    // and hands the frontend only this + a slot id).
    let meta = json!({
        "smapPaste": 3,
        "anchor": anchor,
        "anchorZ": anchor_z,
        "bboxWorld": bbox,
        "count": count,
        "bytes": blob.len() as u64,
    })
    .to_string();
    Ok((blob, meta))
}

/// `extract_clipboard_with_meta` without the metadata -- the browser/wasm
/// path, where the whole blob goes onto the OS clipboard directly.
pub fn extract_clipboard(
    store: &SaveStore,
    names: &[String],
    items: &[LwRef],
) -> PResult<String> {
    Ok(extract_clipboard_with_meta(store, names, items)?.0)
}

/// One decoded foreign actor: header/body blobs plus the parsed model over a
/// combined [header || body] buffer, so refs and names resolve.
struct Foreign {
    header_blob: Vec<u8>,
    body_blob: Vec<u8>,
    combined: Vec<u8>,
    header: Header,
    object: Object,
}

fn decode_foreign(
    fa: &ForeignActor,
    save_version: u32,
    object_version: i32,
    tables: &crate::object::ClassTables,
) -> PResult<Foreign> {
    let header_blob = B64.decode(&fa.h).map_err(|e| perr!("Bad clipboard data: {}", e))?;
    let body_blob = B64.decode(&fa.b).map_err(|e| perr!("Bad clipboard data: {}", e))?;
    let mut combined = header_blob.clone();
    let body_start = combined.len();
    combined.extend_from_slice(&body_blob);

    let mut hc = Cursor::new(&combined, 0);
    let header = parse_one_header(&mut hc)?;
    if hc.pos != body_start {
        return Err(perr!("Clipboard header record has trailing bytes"));
    }
    let mut oc = Cursor::new(&combined, body_start);
    let mut scratch_extras = Vec::new();
    let object = parse_object(
        &mut oc,
        save_version,
        object_version,
        &header,
        tables,
        &mut scratch_extras,
    )?;
    if oc.pos != combined.len() {
        return Err(perr!("Clipboard object record has trailing bytes"));
    }
    Ok(Foreign { header_blob, body_blob, combined, header, object })
}

/// Plan pasting a foreign blob into `store`: rename every copied actor to a
/// fresh name in THIS save, remap internal refs, neutralize refs to the
/// source save's other objects, transform to the paste point, and splice
/// into the persistent level.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_paste_external(
    store: &SaveStore,
    plan: &mut EditPlan,
    save_version: u32,
    object_version: i32,
    lightweight_version: Option<u32>,
    z: Option<&str>,
    z_len: Option<u64>,
    foreign_actors: &[ForeignActor],
    foreign_lightweight: &[ForeignLightweight],
    anchor: [f64; 2],
    delta: &[f64; 3],
    rotate_yaw_deg: f64,
    seed: u64,
) -> PResult<()> {
    // v2 blobs carry the payload compressed; inflate it and shadow the
    // slices.
    let inflated: Option<ForeignPayload> = match (z, z_len) {
        (Some(z), Some(z_len)) => Some(inflate_payload(z, z_len)?),
        (None, None) => None,
        _ => return Err(perr!("Bad clipboard data: z/zLen must come together")),
    };
    let no_ext_refs: HashMap<String, ExtRef> = HashMap::new();
    let (foreign_actors, foreign_lightweight, ext_refs): (
        &[ForeignActor],
        &[ForeignLightweight],
        &HashMap<String, ExtRef>,
    ) = match &inflated {
        Some(p) => (&p.actors, &p.lightweight, &p.ext_refs),
        None => (foreign_actors, foreign_lightweight, &no_ext_refs),
    };

    let data: &[u8] = &store.data;
    if save_version != store.info.save_version {
        return Err(perr!(
            "The copied objects came from a save with game version {} but this save is version {} -- their formats are not byte-compatible",
            save_version,
            store.info.save_version
        ));
    }
    let target_li = store
        .levels
        .iter()
        .position(|l| l.level_name.is_none())
        .ok_or_else(|| perr!("Save has no persistent level"))?;
    let target_level = &store.levels[target_li];
    if !foreign_actors.is_empty() && object_version != target_level.object_ue5_version {
        return Err(perr!(
            "The copied objects use object format {} but this save uses {} -- not byte-compatible",
            object_version,
            target_level.object_ue5_version
        ));
    }
    let pivot = Some(anchor);
    let scan = SaveScan::new(store);
    let exists = |candidate: &[u8]| scan.by_instance_name.contains_key(candidate);

    // -- Actors ---------------------------------------------------------------
    let mut n_new = 0i64;
    let mut new_headers: Vec<u8> = Vec::new();
    let mut new_bodies: Vec<u8> = Vec::new();
    if !foreign_actors.is_empty() {
        let foreigns: Vec<Foreign> = foreign_actors
            .iter()
            .map(|fa| decode_foreign(fa, save_version, object_version, &store.tables))
            .collect::<PResult<_>>()?;

        // Fresh same-length names for every copied actor, unique in the
        // TARGET save.
        let actor_names: Vec<&[u8]> = foreigns
            .iter()
            .filter(|f| matches!(f.header, Header::Actor(_)))
            .map(|f| f.header.instance_name().bytes(&f.combined))
            .collect();
        let renames = rename::build_rename_map(&actor_names, seed, &exists)?;
        let rename_matcher = rename::SubstMatcher::new(&renames);

        // Refs to anything OUTSIDE the copied set point at objects of the
        // SOURCE save; in this save they'd be dangling or -- worse -- collide
        // with unrelated objects. Neutralize every numbered instance ref
        // (class/static paths start with '/' and are save-independent;
        // digitless instance refs are shared singletons that exist here too).
        // Exception: refs the blob recorded as world-static resource actors
        // (extRefs) survive when this save has the same-named actor with the
        // same class AND resource -- level-placed node names are identical
        // across saves of the map, so a miner pasted at its original spot
        // stays attached to its node. The resource gate matters because
        // randomized-node sessions reassign resources per save.
        let mut rng = rename::Rng(seed ^ 0x746f6d6273746f6e);
        let mut tombstones: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
        let mut relinked: HashSet<Vec<u8>> = HashSet::new();
        for f in &foreigns {
            let mut err: Option<crate::error::PError> = None;
            rename::visit_object_refs(&f.object, &mut |r: &ObjectRef| {
                if err.is_some() || r.path_name.is_empty() {
                    return;
                }
                let path = r.path_name.bytes(&f.combined);
                if path.starts_with(b"/")
                    || rename_matcher.contains_any(path)
                    || tombstones.contains_key(path)
                    || relinked.contains(path)
                {
                    return;
                }
                if let Some(er) = std::str::from_utf8(path).ok().and_then(|p| ext_refs.get(p)) {
                    if relink_target_matches(store, &scan, path, er) {
                        relinked.insert(path.to_vec());
                        return;
                    }
                }
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
        // Two linear passes (rename keys and tombstone targets are disjoint
        // by construction).
        let tombstone_matcher = rename::SubstMatcher::new(&tombstones);

        for f in &foreigns {
            let mut header_copy = f.header_blob.clone();
            let mut body_copy = f.body_blob.clone();
            rename_matcher.substitute(&mut header_copy);
            rename_matcher.substitute(&mut body_copy);
            tombstone_matcher.substitute(&mut header_copy);
            tombstone_matcher.substitute(&mut body_copy);

            if let Header::Actor(actor) = &f.header {
                // transform_off was recorded relative to the combined buffer,
                // whose header part starts at 0.
                let t = actor.transform_off as usize;
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

                // Copied power lines: transform the cached wire-mesh endpoint
                // vectors (see apply.rs -- same by-value byte search, offsets
                // relative to the body blob).
                for v in apply::wire_cached_locations(&f.object, &f.combined) {
                    let replacement =
                        apply::encode_f64x3(transform_vec3(v, rotate_yaw_deg, pivot, delta));
                    for rel in apply::find_f64x3(&body_copy, v) {
                        body_copy[rel..rel + 24].copy_from_slice(&replacement);
                    }
                }
            }
            new_headers.extend_from_slice(&header_copy);
            new_bodies.extend_from_slice(&body_copy);
            n_new += 1;
        }
    }

    // -- Lightweight records ----------------------------------------------------
    let mut lw_inserts: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut lw_added_per_group: HashMap<usize, i64> = HashMap::new();
    let mut lw_total = 0i64;
    let mut lw_subsystem: Option<(usize, usize)> = None;
    if !foreign_lightweight.is_empty() {
        let (li, oi, target_version, groups) = lightweight_subsystem(store)?;
        if li != target_li {
            return Err(perr!("Lightweight subsystem is not in the persistent level"));
        }
        lw_subsystem = Some((li, oi));
        if lightweight_version != Some(target_version) {
            return Err(perr!(
                "The copied foundations use record format {:?} but this save uses {} -- not byte-compatible",
                lightweight_version,
                target_version
            ));
        }
        for fl in foreign_lightweight {
            let group = groups
                .iter()
                .find(|g| g.type_path.eq_ascii(data, &fl.type_path))
                .ok_or_else(|| {
                    perr!(
                        "This save has no {} yet -- build one of that type first, then paste",
                        fl.type_path.rsplit('.').next().unwrap_or(&fl.type_path)
                    )
                })?;
            let mut record =
                B64.decode(&fl.r).map_err(|e| perr!("Bad clipboard data: {}", e))?;
            if record.len() < 56 {
                return Err(perr!("Clipboard foundation record too short"));
            }
            let rot = [
                apply::read_f64(&record, 0),
                apply::read_f64(&record, 8),
                apply::read_f64(&record, 16),
                apply::read_f64(&record, 24),
            ];
            let pos = [
                apply::read_f64(&record, 32),
                apply::read_f64(&record, 40),
                apply::read_f64(&record, 48),
            ];
            if rotate_yaw_deg != 0.0 {
                let q = rotate_quat_yaw(rot, rotate_yaw_deg);
                for (i, v) in q.iter().enumerate() {
                    write_f64(&mut record, i * 8, *v);
                }
            }
            let (nx, ny) = transform_xy(pos[0], pos[1], rotate_yaw_deg, pivot, delta);
            write_f64(&mut record, 32, nx);
            write_f64(&mut record, 40, ny);
            write_f64(&mut record, 48, pos[2] + delta[2]);

            lw_total += record.len() as i64;
            *lw_added_per_group.entry(group.count_field_off).or_insert(0) += 1;
            lw_inserts.push((group.end_off as usize, record));
        }
    }

    if n_new == 0 && lw_inserts.is_empty() {
        return Err(perr!("Nothing to paste"));
    }

    // -- Cascade + splice (single patch per field: the actor bodies and the
    // lightweight records both grow allObjectsSize, summed into one patch) ----
    let spans = &target_level.spans;
    let new_bodies_len = new_bodies.len() as i64;
    if n_new > 0 {
        patch_add_u64(plan, data, spans.header_size_field_off as usize, new_headers.len() as i64);
        patch_add_u32(plan, data, spans.header_size_field_off as usize + 8, n_new);
        patch_add_u32(plan, data, spans.object_count_field_off as usize, n_new);
        plan.inserts.push((spans.headers_insert_off as usize, new_headers));
        plan.inserts.push((spans.bodies_insert_off as usize, new_bodies));
    }
    patch_add_u64(plan, data, spans.objects_size_field_off as usize, new_bodies_len + lw_total);
    for (count_field_off, count) in &lw_added_per_group {
        patch_add_u32(plan, data, *count_field_off as usize, *count);
    }
    if let Some((li, oi)) = lw_subsystem {
        let object_size_field = store.levels[li].object_spans[oi].0 as usize + 8;
        patch_add_u32(plan, data, object_size_field, lw_total);
    }
    plan.inserts.extend(lw_inserts);
    Ok(())
}
