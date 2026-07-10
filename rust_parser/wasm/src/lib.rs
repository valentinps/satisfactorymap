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
    store: Arc<SaveStore>,
    index: MapIndex,
    payload_json: Vec<u8>,
    /// The unedited body (no quirk pad), captured lazily on the first edit --
    /// undo replays the remaining ops from here.
    pristine_body: Option<Vec<u8>>,
    /// Ops applied to reach the current store, in order.
    ops: Vec<sav_core::editor::EditOp>,
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

        Ok(SaveSession {
            store: Arc::new(store),
            index,
            payload_json,
            pristine_body: None,
            ops: Vec::new(),
        })
    }

    /// Apply edit ops (JSON array of EditOp). `from_pristine` replaces the
    /// whole op list and replays it from the unedited body (the undo path);
    /// otherwise the ops append to the current state. Returns the rebuilt
    /// map payload JSON. On error the previous state is kept.
    /// `on_progress(phase, current, total)` reuses the load progress phases.
    pub fn apply_edits(
        &mut self,
        ops_json: &str,
        from_pristine: bool,
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
        let new_ops = sav_core::editor::ops::parse_ops_json(ops_json)
            .map_err(|e| JsError::new(&e.msg))?;
        let tables = ClassTables::embedded();

        if self.pristine_body.is_none() {
            self.pristine_body = Some(sav_core::editor::effective_body(&self.store).to_vec());
        }

        let new_store = if from_pristine {
            let pristine = self.pristine_body.as_ref().unwrap();
            let mut progress = |current: u64, total: u64| call(1, current, total);
            sav_core::editor::session::rebuild(
                pristine,
                &self.store.file_header,
                &self.store.info,
                &tables,
                &new_ops,
                Some(&mut progress),
            )
            .map_err(|e| JsError::new(&e.msg))?
        } else {
            let mut store: Option<SaveStore> = None;
            let total = new_ops.len() as u64;
            for (i, op) in new_ops.iter().enumerate() {
                let base: &SaveStore = store.as_ref().unwrap_or(&self.store);
                store = Some(
                    sav_core::editor::session::step(base, op, &tables)
                        .map_err(|e| JsError::new(&e.msg))?,
                );
                call(1, i as u64 + 1, total);
            }
            match store {
                Some(s) => s,
                None => return Err(JsError::new("No edit ops given")),
            }
        };

        // Swap in the new state, then rebuild index + payload. Drop the old
        // store first so peak memory stays bounded (wasm memory never
        // shrinks; the Arc here is the only holder).
        self.store = Arc::new(new_store);
        let mut build_progress = |current: u64, total: u64| call(2, current, total);
        let (payload_json, index) =
            mapdata::build_all_json(&self.store, Some(&mut build_progress))
                .map_err(|e| JsError::new(&e))?;
        self.index = index;
        if from_pristine {
            self.ops = new_ops;
        } else {
            self.ops.extend(new_ops);
        }
        Ok(payload_json)
    }

    /// The full map payload as JSON bytes (returned as a Uint8Array; decode +
    /// JSON.parse on the main thread). Consumed on first call.
    pub fn payload_json(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.payload_json)
    }

    pub fn describe_instance(&self, name: &str) -> String {
        mapdata::describe::describe_instance(&self.store, &self.index, name).to_string()
    }

    pub fn find_item(&self, item: &str) -> String {
        mapdata::queries::find_item_locations(&self.store, &self.index, item).to_string()
    }

    pub fn building_info(&self, types: Vec<String>) -> String {
        mapdata::queries::collect_building_info(&self.store, &self.index, &types).to_string()
    }

    pub fn vehicle_info(&self, types: Vec<String>) -> String {
        mapdata::queries::collect_vehicle_info(&self.store, &self.index, &types).to_string()
    }

    pub fn train_info(&self) -> String {
        mapdata::queries::collect_train_info(&self.store, &self.index).to_string()
    }

    /// Serialize the current save body back into a downloadable .sav
    /// (retained header + re-compressed chunks). Body-identical to the
    /// original file until edits are applied.
    pub fn export_sav(&self) -> Vec<u8> {
        sav_core::editor::export_sav(
            &self.store.file_header,
            sav_core::editor::effective_body(&self.store),
        )
    }

    pub fn selection_inventory(&self, names: Vec<String>) -> String {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        mapdata::queries::aggregate_selection_inventory(&self.store, &self.index, &names)
            .to_string()
    }
}
