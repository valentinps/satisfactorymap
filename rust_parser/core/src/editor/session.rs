//! Fold an ops list over a body: apply one op, re-parse (validation +
//! refreshed offsets for the next op), repeat. Used both for incremental
//! edits (one new op against the live store) and undo (full replay from the
//! pristine body).

use crate::editor::apply::apply_op;
use crate::editor::export::effective_body;
use crate::editor::ops::EditOp;
use crate::error::{perr, PResult};
use crate::level::{parse_body_bytes, parse_body_bytes_lean};
use crate::object::ClassTables;
use crate::save_header::SaveFileInfo;
use crate::store::SaveStore;
use flate2::{read::ZlibDecoder, read::ZlibEncoder, Compression};
use std::io::Read;

/// Replay `ops` in order against `pristine_body` (no quirk pad) and return
/// the FULL-model store of the final state (ready for payload building).
/// `progress(done_ops, total_ops)` fires after each op's re-parse. Planning
/// is model-independent, so the pristine parse and every intermediate
/// validation re-parse are LEAN (span walks); only the final state pays for
/// the full model. The prior state is dropped before each re-parse so peak
/// memory stays ~one parsed save (load-bearing for 4GB-limited wasm on
/// 600k-object saves).
pub fn rebuild(
    pristine_body: Vec<u8>,
    file_header: &[u8],
    info: &SaveFileInfo,
    tables: &ClassTables,
    ops: &[EditOp],
    progress: Option<&mut dyn FnMut(u64, u64)>,
) -> PResult<SaveStore> {
    if ops.is_empty() {
        return parse_body_bytes(pristine_body, file_header.to_vec(), info.clone(), tables, None);
    }
    let store = parse_body_bytes_lean(
        pristine_body,
        file_header.to_vec(),
        info.clone(),
        tables,
        None,
    )?;
    fold_ops(store, ops, tables, progress)
}

/// Apply ops in sequence over an owned store (lean or full): intermediate
/// validation re-parses are lean, the final one is full so the result is
/// ready for payload/index building.
pub fn fold_ops(
    mut store: SaveStore,
    ops: &[EditOp],
    tables: &ClassTables,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> PResult<SaveStore> {
    let total = ops.len() as u64;
    for (i, op) in ops.iter().enumerate() {
        let last = i + 1 == ops.len();
        store = step_owned_impl(store, op, tables, !last)?;
        if let Some(cb) = progress.as_deref_mut() {
            cb(i as u64 + 1, total);
        }
    }
    if !store.has_object_model() {
        store = rehydrate(store, tables, None)?; // ops was empty on a lean store
    }
    Ok(store)
}

/// Apply one op to a live store and return the re-parsed result.
pub fn step(store: &SaveStore, op: &EditOp, tables: &ClassTables) -> PResult<SaveStore> {
    let body = apply_op(store, effective_body(store), op)?;
    parse_body_bytes(
        body,
        store.file_header.clone(),
        store.info.clone(),
        tables,
        None,
    )
}

/// step(), but consumes the store: the op is planned (model-independently:
/// objects re-parse on demand from their spans, so a lean store works), the
/// parsed structures are dropped, and the plan is applied IN PLACE on the
/// store's own body buffer before the re-parse. Peak memory stays ~one
/// parsed save -- there is never a second body copy (load-bearing under the
/// 4GB wasm ceiling, where a loaded 600k-object save already sits at ~3.6GB).
pub fn step_owned(store: SaveStore, op: &EditOp, tables: &ClassTables) -> PResult<SaveStore> {
    step_owned_impl(store, op, tables, false)
}

fn step_owned_impl(
    store: SaveStore,
    op: &EditOp,
    tables: &ClassTables,
    lean: bool,
) -> PResult<SaveStore> {
    let plan = crate::editor::apply::plan_op(&store, op)?;
    let padded = store.padded;
    let SaveStore { data: mut body, file_header, info, .. } = store; // frees all parsed structs
    if padded {
        body.truncate(body.len() - 4); // drop the quirk pad; re-parse re-adds it
    }
    crate::editor::apply::apply_plan(&mut body, plan)?;
    if lean {
        parse_body_bytes_lean(body, file_header, info, tables, None)
    } else {
        parse_body_bytes(body, file_header, info, tables, None)
    }
}

/// Re-parse the full per-object model from the store's own body -- the step
/// before planning an edit on a store whose model was freed by
/// drop_object_model. Consumes the store (mirrors step_owned's destructure:
/// the lean headers/spans are freed, the body moves -- no second body copy).
/// A parse failure loses the state, recovered like a failed edit via
/// pristine replay. No-op when the model is already present.
pub fn rehydrate(
    store: SaveStore,
    tables: &ClassTables,
    progress: Option<crate::level::ProgressFn>,
) -> PResult<SaveStore> {
    if store.has_object_model() {
        return Ok(store);
    }
    let padded = store.padded;
    let SaveStore { data: mut body, file_header, info, .. } = store;
    if padded {
        body.truncate(body.len() - 4); // drop the quirk pad; re-parse re-adds it
    }
    parse_body_bytes(body, file_header, info, tables, progress)
}

/// Compress a pristine body for retention (wasm keeps the undo baseline as
/// ~1/15th-size zlib instead of a full second body). Returns (zlib bytes,
/// raw length).
pub fn compress_body(body: &[u8]) -> (Vec<u8>, usize) {
    let mut enc = ZlibEncoder::new(body, Compression::fast());
    let mut out = Vec::with_capacity(body.len() / 8);
    enc.read_to_end(&mut out).expect("in-memory zlib compression cannot fail");
    (out, body.len())
}

pub fn decompress_body(compressed: &[u8], raw_len: usize) -> PResult<Vec<u8>> {
    let mut dec = ZlibDecoder::new(compressed);
    let mut out = Vec::with_capacity(raw_len);
    dec.read_to_end(&mut out).map_err(|e| perr!("Pristine body decompression failed: {}", e))?;
    if out.len() != raw_len {
        return Err(perr!("Pristine body length mismatch: {} != {}", out.len(), raw_len));
    }
    Ok(out)
}
