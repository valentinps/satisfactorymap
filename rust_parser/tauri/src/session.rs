//! Native-shell session, the desktop parallel of `sav_wasm`'s `SaveSession`
//! (rust_parser/wasm/src/lib.rs). Same `sav_core` orchestration -- parse ->
//! `build_all_json` -> hold store + index alive for queries, edit by folding
//! ops over the retained body -- with two deliberate differences:
//!
//!   * errors are plain `String` instead of `JsError`;
//!   * progress is a `&dyn Fn(u8, u64, u64)` the command layer wires to a Tauri
//!     `Channel`, instead of a `js_sys::Function`.
//!
//! None of the wasm 4GB-memory machinery is ported: no lean-worker handoff, no
//! `compress_pristine`/`serialize_index`, no object-model drop juggling. A
//! native allocator returns memory on its own, so `AppSession` just holds one
//! live session for the app's lifetime and keeps the pristine body (undo
//! baseline) uncompressed. See docs/tauri-desktop-app.md.

use sav_core::editor;
use sav_core::level::{parse_full_save_lean, ProgressFn};
use sav_core::mapdata::{self, index::MapIndex};
use sav_core::object::ClassTables;
use sav_core::save_header::SaveFileInfo;
use sav_core::store::SaveStore;

const SESSION_LOST: &str =
    "No usable save state (a failed edit was not recovered) -- reload the save file";

pub struct AppSession {
    /// The parsed (lean) save. Taken/replaced inside the edit paths so the old
    /// body can be freed before the edited one re-parses. `None` only after a
    /// failed incremental edit tore the store down.
    store: Option<SaveStore>,
    index: Option<MapIndex>,
    /// Kept outside the store so a session whose store was lost to a failed
    /// edit can still be restored, and so export/undo don't depend on it.
    file_header: Vec<u8>,
    info: SaveFileInfo,
    /// The pre-first-edit body, uncompressed. Undo/recovery replays the whole
    /// committed op list over this. Captured lazily on the first edit (viewing
    /// a save never pays for it), mirroring the wasm worker's `compress_pristine`
    /// timing minus the compression (native memory has no 4GB ceiling).
    pristine_body: Option<Vec<u8>>,
}

impl AppSession {
    fn store(&self) -> Result<&SaveStore, String> {
        self.store.as_ref().ok_or_else(|| SESSION_LOST.to_string())
    }

    fn index(&self) -> Result<&MapIndex, String> {
        self.index.as_ref().ok_or_else(|| SESSION_LOST.to_string())
    }

    /// True while the session holds usable state. The command layer probes this
    /// after an edit error to tell the frontend whether to recover by reloading.
    pub fn is_healthy(&self) -> bool {
        self.store.is_some() && self.index.is_some()
    }

    /// Parse + build. `progress(phase, current, total)`: phase 0 = decompress,
    /// 1 = parse, 2 = payload/index build. Returns the session and the map
    /// payload JSON bytes.
    pub fn load(
        bytes: Vec<u8>,
        progress: &dyn Fn(u8, u64, u64),
    ) -> Result<(AppSession, Vec<u8>), String> {
        let tables = ClassTables::embedded();
        // Lean by default -- the builder re-parses objects on demand from their
        // spans, so the full model is never materialized. There is no
        // ?keepModel valve here: it exists only to A/B wasm memory behavior.
        let mut parse_progress = |phase: u8, current: u64, total: u64| progress(phase, current, total);
        let pf: ProgressFn = &mut parse_progress;
        let store = parse_full_save_lean(&bytes, &tables, Some(pf)).map_err(|e| e.msg)?;
        drop(bytes);

        let mut build_progress = |current: u64, total: u64| progress(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(&store, Some(&mut build_progress))?;

        let file_header = store.file_header.clone();
        let info = store.info.clone();
        let session = AppSession {
            store: Some(store),
            index: Some(index),
            file_header,
            info,
            pristine_body: None,
        };
        Ok((session, payload_json))
    }

    /// Rebuild index + payload from the new store and swap it in.
    fn finish_edit(
        &mut self,
        new_store: SaveStore,
        progress: &dyn Fn(u8, u64, u64),
    ) -> Result<Vec<u8>, String> {
        let mut build_progress = |current: u64, total: u64| progress(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(&new_store, Some(&mut build_progress))?;
        self.store = Some(new_store);
        self.index = Some(index);
        Ok(payload_json)
    }

    /// Apply edit ops (JSON array of EditOp) incrementally to the current state
    /// and return the rebuilt payload JSON. On failure the store may be torn
    /// down (`is_healthy()` goes false); the command layer signals the frontend
    /// to recover. `progress` reuses the load phases.
    pub fn apply_edits(
        &mut self,
        ops_json: &str,
        progress: &dyn Fn(u8, u64, u64),
    ) -> Result<Vec<u8>, String> {
        let new_ops = editor::ops::parse_ops_json(ops_json).map_err(|e| e.msg)?;
        if new_ops.is_empty() {
            return Err("No edit ops given".to_string());
        }
        let tables = ClassTables::embedded();

        // Dry-run the first op's plan while nothing is torn down: planning is
        // model-independent, so semantic refusals (uneditable object, unknown
        // name, chained belt) surface as clean errors with the session healthy.
        drop(editor::apply::plan_op(self.store()?, &new_ops[0]).map_err(|e| e.msg)?);

        // Capture the undo baseline before the first mutation (the immutable
        // borrow ends at the .to_vec()).
        if self.pristine_body.is_none() {
            let body = editor::effective_body(self.store()?).to_vec();
            self.pristine_body = Some(body);
        }

        self.index = None;
        let store = self.store.take().ok_or_else(|| SESSION_LOST.to_string())?;
        let mut progress_cb = |current: u64, total: u64| progress(1, current, total);
        match editor::session::fold_ops(store, &new_ops, &tables, Some(&mut progress_cb)) {
            Ok(store) => self.finish_edit(store, progress),
            Err(e) => Err(e.msg),
        }
    }

    /// Replace the whole edit state: drop the current save, re-parse the
    /// retained pristine body, replay `ops_json` over it, and return the
    /// rebuilt payload. This is both the undo path and the recovery path.
    pub fn apply_edits_from_pristine(
        &mut self,
        ops_json: &str,
        progress: &dyn Fn(u8, u64, u64),
    ) -> Result<Vec<u8>, String> {
        let new_ops = editor::ops::parse_ops_json(ops_json).map_err(|e| e.msg)?;
        let tables = ClassTables::embedded();
        // Clone so the baseline survives for the next undo.
        let pristine = self
            .pristine_body
            .clone()
            .ok_or_else(|| "No pristine baseline to undo/recover from".to_string())?;

        self.index = None;
        self.store = None;

        let mut progress_cb = |current: u64, total: u64| progress(1, current, total);
        let store = editor::session::rebuild(
            pristine,
            &self.file_header,
            &self.info,
            &tables,
            &new_ops,
            Some(&mut progress_cb),
        )
        .map_err(|e| e.msg)?;
        self.finish_edit(store, progress)
    }

    pub fn describe_instance(&self, name: &str) -> Result<String, String> {
        Ok(mapdata::describe::describe_instance(self.store()?, self.index()?, name).to_string())
    }

    pub fn find_item(&self, item: &str) -> Result<String, String> {
        Ok(mapdata::queries::find_item_locations(self.store()?, self.index()?, item).to_string())
    }

    pub fn building_info(&self, types: &[String]) -> Result<String, String> {
        Ok(mapdata::queries::collect_building_info(self.store()?, self.index()?, types).to_string())
    }

    pub fn vehicle_info(&self, types: &[String]) -> Result<String, String> {
        Ok(mapdata::queries::collect_vehicle_info(self.store()?, self.index()?, types).to_string())
    }

    pub fn train_info(&self) -> Result<String, String> {
        Ok(mapdata::queries::collect_train_info(self.store()?, self.index()?).to_string())
    }

    pub fn selection_inventory(&self, names: &[String]) -> Result<String, String> {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        Ok(mapdata::queries::aggregate_selection_inventory(self.store()?, self.index()?, &names)
            .to_string())
    }

    /// Cross-save clipboard blob (JSON) for the given edit targets.
    pub fn extract_clipboard(
        &self,
        names: &[String],
        lightweight_json: &str,
    ) -> Result<String, String> {
        let items = editor::clipboard::parse_lw_refs(lightweight_json).map_err(|e| e.msg)?;
        editor::clipboard::extract_clipboard(self.store()?, names, &items).map_err(|e| e.msg)
    }

    /// The current (possibly edited) save re-serialized to downloadable .sav
    /// bytes. Body-identical to the original until edits are applied.
    pub fn export_sav(&self) -> Result<Vec<u8>, String> {
        let store = self.store()?;
        Ok(editor::export_sav(&store.file_header, editor::effective_body(store)))
    }
}
