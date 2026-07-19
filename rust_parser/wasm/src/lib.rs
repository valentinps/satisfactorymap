//! wasm-bindgen boundary for the browser build: parse a .sav from bytes,
//! build the map payload + save index, hold both alive for detail queries.
//! Runs inside a Web Worker (map/static/map/worker.js); all methods are
//! synchronous from the worker's point of view.

// Allocator story (measured, not theoretical): V8 makes each wasm
// memory.grow progressively more expensive, so letting the allocator extend
// the heap thousands of times during a parse turns allocation cost
// quadratic (25M allocs took 175s; with a pre-grown heap the same pattern
// runs at a flat ~23ns/alloc). rlsf (TLSF) is the O(1) block manager; the
// GrowAhead wrapper watches for heap growth and immediately claims 25%
// headroom into the pool, making memory.grow logarithmic in total heap
// size regardless of save size.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
mod wasm_alloc {
    use core::alloc::{GlobalAlloc, Layout};
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

    static INNER: rlsf::GlobalTlsf = rlsf::GlobalTlsf::new();
    static LAST_PAGES: AtomicUsize = AtomicUsize::new(0);
    static IN_BALLAST: AtomicBool = AtomicBool::new(false);
    // Live (allocated minus freed) bytes. memory.buffer.byteLength only ever
    // grows, so this is the metric that shows the object-model drop.
    static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

    pub fn live_bytes() -> usize {
        LIVE_BYTES.load(Relaxed)
    }

    fn maybe_grow_ahead() {
        let pages = core::arch::wasm32::memory_size(0);
        if pages <= LAST_PAGES.load(Relaxed) {
            return;
        }
        if IN_BALLAST.swap(true, Relaxed) {
            return; // the ballast's own allocation landed here
        }
        // The pool just grew: claim generous headroom in ONE go so the next
        // thousands of allocations never touch memory.grow. Capped at 256MB
        // so peak memory overshoots the natural high-water mark by at most
        // that; past 3.5GB stop growing ahead entirely (4GB wasm ceiling --
        // rlsf then grows in small steps for the tail). Failure is fine.
        let heap = pages << 16;
        if heap <= 3_500 << 20 {
            let headroom = (heap / 4).clamp(32 << 20, 256 << 20);
            let mut ballast: Vec<u8> = Vec::new();
            let _ = ballast.try_reserve_exact(headroom);
            drop(ballast);
        }
        LAST_PAGES.store(core::arch::wasm32::memory_size(0), Relaxed);
        IN_BALLAST.store(false, Relaxed);
    }

    pub struct GrowAhead;

    unsafe impl GlobalAlloc for GrowAhead {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { INNER.alloc(layout) };
            if !ptr.is_null() {
                LIVE_BYTES.fetch_add(layout.size(), Relaxed);
            }
            maybe_grow_ahead();
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
            unsafe { INNER.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_ptr = unsafe { INNER.realloc(ptr, layout, new_size) };
            if !new_ptr.is_null() {
                LIVE_BYTES.fetch_add(new_size, Relaxed);
                LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
            }
            maybe_grow_ahead();
            new_ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { INNER.alloc_zeroed(layout) };
            if !ptr.is_null() {
                LIVE_BYTES.fetch_add(layout.size(), Relaxed);
            }
            maybe_grow_ahead();
            ptr
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
#[global_allocator]
static ALLOC: wasm_alloc::GrowAhead = wasm_alloc::GrowAhead;

use sav_core::level::{parse_full_save_lean, ProgressFn};
use sav_core::mapdata;
use sav_core::mapdata::index::MapIndex;
use sav_core::object::ClassTables;
use sav_core::store::SaveStore;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Live (allocated, not yet freed) heap bytes. memory.buffer.byteLength
/// never shrinks, so it stays at the load high-water mark; this is the
/// metric that shows the ~1.5-2GB object-model drop after load.
#[wasm_bindgen]
pub fn live_heap_bytes() -> f64 {
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    {
        wasm_alloc::live_bytes() as f64
    }
    #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
    {
        0.0
    }
}

#[wasm_bindgen]
pub struct SaveSession {
    /// Always Some between successful calls; taken/replaced inside the edit
    /// paths so the old parsed save can be freed before the edited body
    /// re-parses (wasm memory is capped at 4GB and one parsed 600k-object
    /// save is most of it). None after a failed edit until the worker
    /// restores via apply_edits_from_pristine.
    store: Option<Arc<SaveStore>>,
    /// Same story: dropped up front in the edit paths, rebuilt at the end.
    index: Option<MapIndex>,
    payload_json: Vec<u8>,
    /// Small copies kept outside the store so a session whose store was
    /// lost to a failed edit can still be restored from the pristine blob.
    file_header: Vec<u8>,
    info: sav_core::save_header::SaveFileInfo,
}

const SESSION_LOST: &str =
    "No usable save state (a failed edit was not recovered) -- reload the save file";

impl SaveSession {
    fn store(&self) -> Result<&SaveStore, JsError> {
        self.store.as_ref().map(|a| a.as_ref()).ok_or_else(|| JsError::new(SESSION_LOST))
    }

    fn index(&self) -> Result<&MapIndex, JsError> {
        self.index.as_ref().ok_or_else(|| JsError::new(SESSION_LOST))
    }

    /// Rebuild index + payload from the new (lean) store and swap it in. The
    /// builder re-parses objects on demand from their spans -- the store is
    /// already model-free, so there is nothing to drop.
    fn finish_edit(
        &mut self,
        new_store: SaveStore,
        call: &dyn Fn(u8, u64, u64),
    ) -> Result<Vec<u8>, JsError> {
        let mut build_progress = |current: u64, total: u64| call(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(&new_store, Some(&mut build_progress))
                .map_err(|e| JsError::new(&e))?;
        self.store = Some(Arc::new(new_store));
        self.index = Some(index);
        Ok(payload_json)
    }
}

#[wasm_bindgen]
impl SaveSession {
    /// Parse + build. `on_progress(phase, current, total)` fires from inside
    /// the parse/build (phases 0 = decompress file bytes, 1 = parse level
    /// bytes, 2 = payload + index build steps).
    #[wasm_bindgen(constructor)]
    pub fn load(bytes: Vec<u8>, on_progress: &js_sys::Function) -> Result<SaveSession, JsError> {
        let call = |phase: u8, current: u64, total: u64| {
            let this = JsValue::NULL;
            let _ = on_progress.call3(
                &this,
                &JsValue::from_f64(phase as f64),
                &JsValue::from_f64(current as f64),
                &JsValue::from_f64(total as f64),
            );
        };

        let tables = ClassTables::embedded();
        let mut parse_progress = |phase: u8, current: u64, total: u64| call(phase, current, total);
        let pf: ProgressFn = &mut parse_progress;
        // Always lean: the builder re-parses objects on demand from their
        // spans, so the full model (~1.5-2GB on a 600k-object save) is never
        // materialized -- peak memory stays ~body + headers + collector
        // outputs. (The eager parse_full_save + object-model drop remain in
        // sav_core purely as the differential test oracle; production is lean.)
        let store = parse_full_save_lean(&bytes, &tables, Some(pf))
            .map_err(|e| JsError::new(&e.msg))?;
        drop(bytes);

        // 17 payload steps + the index build = BUILD_STEP_COUNT ticks, same
        // as Python's buildAll; one shared SaveScan for both.
        let mut build_progress = |current: u64, total: u64| call(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(&store, Some(&mut build_progress))
                .map_err(|e| JsError::new(&e))?;

        let file_header = store.file_header.clone();
        let info = store.info.clone();
        Ok(SaveSession {
            store: Some(Arc::new(store)),
            index: Some(index),
            payload_json,
            file_header,
            info,
        })
    }

    /// Build a lean session in a FRESH wasm instance from state extracted
    /// out of a loaded one (compress_pristine + serialize_index +
    /// file_header_bytes). Decompresses the body and walks headers + byte
    /// spans only -- no parsed object model is ever materialized, so this
    /// instance's memory stays ~body + headers + index (~1.2GB on a
    /// 600k-object save) instead of the loaded worker's ~3.6GB high-water
    /// mark. Queries and edits re-parse from spans -- the model is never built.
    pub fn load_lean(
        pristine: &[u8],
        index_bytes: Vec<u8>,
        file_header: Vec<u8>,
        on_progress: &js_sys::Function,
    ) -> Result<SaveSession, JsError> {
        let call = |phase: u8, current: u64, total: u64| {
            let this = JsValue::NULL;
            let _ = on_progress.call3(
                &this,
                &JsValue::from_f64(phase as f64),
                &JsValue::from_f64(current as f64),
                &JsValue::from_f64(total as f64),
            );
        };
        if pristine.len() < 8 {
            return Err(JsError::new("Bad pristine blob"));
        }
        // Peak-memory ordering: deserialize the index and free its CBOR
        // buffer BEFORE the ~1GB body inflates.
        let mut index = MapIndex::from_cbor_partial(&index_bytes).map_err(|e| JsError::new(&e))?;
        drop(index_bytes);
        let raw_len = u64::from_le_bytes(pristine[0..8].try_into().unwrap()) as usize;
        let body = sav_core::editor::session::decompress_body(&pristine[8..], raw_len)
            .map_err(|e| JsError::new(&e.msg))?;
        let (info, _body_offset) =
            sav_core::save_header::parse_save_file_info(&file_header)
                .map_err(|e| JsError::new(&e.msg))?;
        let tables = ClassTables::embedded();
        let mut parse_progress = |phase: u8, current: u64, total: u64| call(phase, current, total);
        let pf: ProgressFn = &mut parse_progress;
        let store = sav_core::level::parse_body_bytes_lean(
            body,
            file_header,
            info.clone(),
            &tables,
            Some(pf),
        )
        .map_err(|e| JsError::new(&e.msg))?;
        index.rebuild_header_maps(&store);
        let file_header = store.file_header.clone();
        Ok(SaveSession {
            store: Some(Arc::new(store)),
            index: Some(index),
            payload_json: Vec::new(), // already delivered by the loaded worker
            file_header,
            info,
        })
    }

    /// CBOR MapIndex for the lean-worker handoff.
    pub fn serialize_index(&self) -> Result<Vec<u8>, JsError> {
        self.index()?.to_cbor().map_err(|e| JsError::new(&e))
    }

    /// Raw uncompressed .sav header prefix (for the lean-worker handoff).
    pub fn file_header_bytes(&self) -> Vec<u8> {
        self.file_header.clone()
    }

    /// The current effective body, zlib-compressed with an 8-byte LE raw
    /// length prefix. The worker keeps this in JS memory (outside the 4GB
    /// wasm heap) as the undo baseline and hands it back to
    /// apply_edits_from_pristine.
    pub fn compress_pristine(&self) -> Result<Vec<u8>, JsError> {
        let body = sav_core::editor::effective_body(self.store()?);
        let (compressed, raw_len) = sav_core::editor::session::compress_body(body);
        let mut out = Vec::with_capacity(compressed.len() + 8);
        out.extend_from_slice(&(raw_len as u64).to_le_bytes());
        out.extend_from_slice(&compressed);
        Ok(out)
    }

    /// Apply edit ops (JSON array of EditOp) incrementally to the current
    /// state and return the rebuilt map payload JSON. On failure the session
    /// may be left without a usable store (memory headroom on huge saves is
    /// why the old state can't be kept around) -- the worker then restores
    /// it via apply_edits_from_pristine with the committed op list.
    /// `on_progress(phase, current, total)` reuses the load progress phases.
    pub fn apply_edits(
        &mut self,
        ops_json: &str,
        on_progress: &js_sys::Function,
    ) -> Result<Vec<u8>, JsError> {
        let call = |phase: u8, current: u64, total: u64| {
            let this = JsValue::NULL;
            let _ = on_progress.call3(
                &this,
                &JsValue::from_f64(phase as f64),
                &JsValue::from_f64(current as f64),
                &JsValue::from_f64(total as f64),
            );
        };
        use sav_core::editor::session;

        let new_ops = sav_core::editor::ops::parse_ops_json(ops_json)
            .map_err(|e| JsError::new(&e.msg))?;
        if new_ops.is_empty() {
            return Err(JsError::new("No edit ops given"));
        }
        let tables = ClassTables::embedded();

        // Dry-run the first op's plan while nothing is torn down: planning is
        // model-independent (objects re-parse on demand from their spans), so
        // this works directly on the lean store and semantic refusals
        // (uneditable object, unknown name, chained belt) surface as clean
        // errors with the session left healthy.
        drop(
            sav_core::editor::apply::plan_op(self.store()?, &new_ops[0])
                .map_err(|e| JsError::new(&e.msg))?,
        );

        // Free the index and stale payload up front -- everything below is
        // memory-critical on 600k-object saves.
        self.index = None;
        self.payload_json = Vec::new();
        let arc = self.store.take().ok_or_else(|| JsError::new(SESSION_LOST))?;
        let store = Arc::try_unwrap(arc).map_err(|_| JsError::new("save store is shared"))?;
        // Every re-parse is lean; finish_edit's build re-parses objects on
        // demand, so the full model is never materialized.
        let mut progress = |current: u64, total: u64| call(1, current, total);
        let store = session::fold_ops(store, &new_ops, &tables, Some(&mut progress))
            .map_err(|e| JsError::new(&e.msg))?;
        self.finish_edit(store, &call)
    }

    /// Replace the whole edit state: drop the current save, decompress the
    /// worker-held pristine blob (8-byte LE raw length + zlib, from
    /// compress_pristine), replay `ops_json` over it, and return the rebuilt
    /// payload. This is both the undo path and the recovery path after a
    /// failed incremental edit.
    pub fn apply_edits_from_pristine(
        &mut self,
        ops_json: &str,
        pristine: &[u8],
        on_progress: &js_sys::Function,
    ) -> Result<Vec<u8>, JsError> {
        let call = |phase: u8, current: u64, total: u64| {
            let this = JsValue::NULL;
            let _ = on_progress.call3(
                &this,
                &JsValue::from_f64(phase as f64),
                &JsValue::from_f64(current as f64),
                &JsValue::from_f64(total as f64),
            );
        };
        use sav_core::editor::session;

        let new_ops = sav_core::editor::ops::parse_ops_json(ops_json)
            .map_err(|e| JsError::new(&e.msg))?;
        if pristine.len() < 8 {
            return Err(JsError::new("Bad pristine blob"));
        }
        let raw_len = u64::from_le_bytes(pristine[0..8].try_into().unwrap()) as usize;
        let tables = ClassTables::embedded();

        // Free everything before inflating the pristine body (the session's
        // own file_header/info copies survive independently of the store).
        self.index = None;
        self.payload_json = Vec::new();
        self.store = None;

        let body = session::decompress_body(&pristine[8..], raw_len)
            .map_err(|e| JsError::new(&e.msg))?;
        let mut progress = |current: u64, total: u64| call(1, current, total);
        let store = session::rebuild(
            body,
            &self.file_header,
            &self.info,
            &tables,
            &new_ops,
            Some(&mut progress),
        )
        .map_err(|e| JsError::new(&e.msg))?;
        self.finish_edit(store, &call)
    }

    /// The full map payload as JSON bytes (returned as a Uint8Array; decode +
    /// JSON.parse on the main thread). Consumed on first call.
    pub fn payload_json(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.payload_json)
    }

    /// False after a failed edit left the session without usable state --
    /// the worker then signals the client to recover by reloading.
    pub fn is_healthy(&self) -> bool {
        self.store.is_some() && self.index.is_some()
    }

    pub fn describe_instance(&self, name: &str) -> Result<String, JsError> {
        Ok(mapdata::describe::describe_instance(self.store()?, self.index()?, name).to_string())
    }

    pub fn find_item(&self, item: &str) -> Result<String, JsError> {
        Ok(mapdata::queries::find_item_locations(self.store()?, self.index()?, item).to_string())
    }

    pub fn building_info(&self, types: Vec<String>) -> Result<String, JsError> {
        Ok(mapdata::queries::collect_building_info(self.store()?, self.index()?, &types).to_string())
    }

    pub fn vehicle_info(&self, types: Vec<String>) -> Result<String, JsError> {
        Ok(mapdata::queries::collect_vehicle_info(self.store()?, self.index()?, &types).to_string())
    }

    pub fn train_info(&self) -> Result<String, JsError> {
        Ok(mapdata::queries::collect_train_info(self.store()?, self.index()?).to_string())
    }

    /// Cross-save clipboard blob (JSON) for the given edit targets -- raw
    /// object bytes + version metadata, pasteable into another tab/save via
    /// the pasteExternal edit op (see sav_core::editor::clipboard).
    pub fn extract_clipboard(
        &self,
        names: Vec<String>,
        lightweight_json: &str,
    ) -> Result<String, JsError> {
        let items = sav_core::editor::clipboard::parse_lw_refs(lightweight_json)
            .map_err(|e| JsError::new(&e.msg))?;
        sav_core::editor::clipboard::extract_clipboard(self.store()?, &names, &items)
            .map_err(|e| JsError::new(&e.msg))
    }

    /// Serialize the current save body back into a downloadable .sav
    /// (retained header + re-compressed chunks). Body-identical to the
    /// original file until edits are applied.
    pub fn export_sav(&self) -> Result<Vec<u8>, JsError> {
        let store = self.store()?;
        Ok(sav_core::editor::export_sav(
            &store.file_header,
            sav_core::editor::effective_body(store),
        ))
    }

    pub fn selection_inventory(&self, names: Vec<String>) -> Result<String, JsError> {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        Ok(mapdata::queries::aggregate_selection_inventory(self.store()?, self.index()?, &names)
            .to_string())
    }
}
