//! Fold an ops list over a body: apply one op, re-parse (validation +
//! refreshed offsets for the next op), repeat. Used both for incremental
//! edits (one new op against the live store) and undo (full replay from the
//! pristine body).

use crate::editor::apply::apply_op;
use crate::editor::export::effective_body;
use crate::editor::ops::EditOp;
use crate::error::PResult;
use crate::level::parse_body_bytes;
use crate::object::ClassTables;
use crate::save_header::SaveFileInfo;
use crate::store::SaveStore;

/// Replay `ops` in order against `pristine_body` (no quirk pad) and return
/// the store of the final state. `progress(done_ops, total_ops)` fires after
/// each op's re-parse.
pub fn rebuild(
    pristine_body: &[u8],
    file_header: &[u8],
    info: &SaveFileInfo,
    tables: &ClassTables,
    ops: &[EditOp],
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> PResult<SaveStore> {
    let mut store = parse_body_bytes(
        pristine_body.to_vec(),
        file_header.to_vec(),
        info.clone(),
        tables,
        None,
    )?;
    let total = ops.len() as u64;
    for (i, op) in ops.iter().enumerate() {
        store = step(&store, op, tables)?;
        if let Some(cb) = progress.as_deref_mut() {
            cb(i as u64 + 1, total);
        }
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
