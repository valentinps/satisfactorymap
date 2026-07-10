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
            maybe_grow_ahead();
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { INNER.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let ptr = unsafe { INNER.realloc(ptr, layout, new_size) };
            maybe_grow_ahead();
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { INNER.alloc_zeroed(layout) };
            maybe_grow_ahead();
            ptr
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
#[global_allocator]
static ALLOC: wasm_alloc::GrowAhead = wasm_alloc::GrowAhead;

use sav_core::level::{parse_full_save, ProgressFn};
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

    /// Swap in a new store and rebuild index + payload, returning the
    /// payload bytes.
    fn finish_edit(
        &mut self,
        new_store: SaveStore,
        call: &dyn Fn(u8, u64, u64),
    ) -> Result<Vec<u8>, JsError> {
        self.store = Some(Arc::new(new_store));
        let mut build_progress = |current: u64, total: u64| call(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(self.store()?, Some(&mut build_progress))
                .map_err(|e| JsError::new(&e))?;
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
        let store =
            parse_full_save(&bytes, &tables, Some(pf)).map_err(|e| JsError::new(&e.msg))?;
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

        // Dry-run the first op's plan while the session is still intact:
        // semantic refusals (uneditable object, unknown name, chained belt)
        // surface here as clean errors with nothing torn down. Only after
        // this do we start consuming state.
        drop(sav_core::editor::apply::plan_op(self.store()?, &new_ops[0])
            .map_err(|e| JsError::new(&e.msg))?);

        // Free the index and stale payload up front -- everything below is
        // memory-critical on 600k-object saves.
        self.index = None;
        self.payload_json = Vec::new();
        let arc = self.store.take().ok_or_else(|| JsError::new(SESSION_LOST))?;
        let mut store = Arc::try_unwrap(arc).map_err(|_| JsError::new("save store is shared"))?;
        let total = new_ops.len() as u64;
        for (i, op) in new_ops.iter().enumerate() {
            store = session::step_owned(store, op, &tables).map_err(|e| JsError::new(&e.msg))?;
            call(1, i as u64 + 1, total);
        }
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
