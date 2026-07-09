//! wasm-bindgen boundary for the browser build: parse a .sav from bytes,
//! build the map payload + save index, hold both alive for detail queries.
//! Runs inside a Web Worker (map/static/map/worker.js); all methods are
//! synchronous from the worker's point of view.

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
        // as Python's buildAll.
        let mut build_progress =
            |current: u64, _total: u64| call(2, current, mapdata::BUILD_STEP_COUNT);
        let payload_json = mapdata::build_payload_json(&store, None, Some(&mut build_progress))
            .map_err(|e| JsError::new(&e))?;
        let index = MapIndex::build(&store);
        call(2, mapdata::BUILD_STEP_COUNT, mapdata::BUILD_STEP_COUNT);

        Ok(SaveSession { store: Arc::new(store), index, payload_json })
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

    pub fn selection_inventory(&self, names: Vec<String>) -> String {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        mapdata::queries::aggregate_selection_inventory(&self.store, &self.index, &names)
            .to_string()
    }
}
