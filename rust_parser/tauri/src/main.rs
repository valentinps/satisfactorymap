//! Tauri v2 desktop shell for the Satisfactory save map. Bundles the same
//! static frontend as the browser build (`dist/`) in a native webview and
//! serves it through `sav_core` directly instead of the wasm worker -- no 4GB
//! linear-memory ceiling, so saves that would OOM the browser load here.
//!
//! Every command is 1:1 with a `map/static/map/worker.js` op, so the shared
//! `SaveClient` public API works unchanged over either transport (the frontend
//! picks by `window.__TAURI__`). Big binaries (the map payload, the exported
//! .sav) cross the IPC boundary as a raw `Response`, never round-tripped
//! through `serde_json`; the `.sav` is loaded from a path, not shipped as bytes.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use sav_tauri_lib::session::AppSession;
use std::sync::Mutex;
use tauri::ipc::{Channel, Response};
use tauri::State;

struct AppState(Mutex<Option<AppSession>>);

/// Progress tick delivered over a Tauri `Channel`. `phase` matches the wasm
/// worker's numeric phase (0 decompress, 1 parse, 2 build); the frontend maps
/// it to the same label strings the worker path emits.
#[derive(Clone, serde::Serialize)]
struct ProgressMsg {
    phase: u8,
    current: u64,
    total: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MemStats {
    mem_bytes: u64,
    live_bytes: u64,
    lean: bool,
}

/// Prefix on error strings that mean the session was torn down and the
/// frontend should recover by reloading the save. `save_client.js`'s Tauri
/// transport strips it and sets `error.sessionLost`.
const SESSION_LOST_PREFIX: &str = "SESSION_LOST:";

/// Parse a .sav read from `path` and build the map payload. Returns the payload
/// JSON as raw bytes (decoded + `JSON.parse`d on the JS side).
#[tauri::command]
async fn load(
    path: String,
    on_progress: Channel<ProgressMsg>,
    state: State<'_, AppState>,
) -> Result<Response, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
    let progress = |phase: u8, current: u64, total: u64| {
        let _ = on_progress.send(ProgressMsg { phase, current, total });
    };
    let (app_session, payload) = AppSession::load(bytes, &progress)?;
    *state.0.lock().unwrap() = Some(app_session);
    Ok(Response::new(payload))
}

/// Apply edit ops. `from_pristine` replaces the whole op list (undo/recovery);
/// otherwise the ops append incrementally. Returns the rebuilt payload bytes.
#[tauri::command]
async fn apply_edits(
    ops: String,
    from_pristine: bool,
    on_progress: Channel<ProgressMsg>,
    state: State<'_, AppState>,
) -> Result<Response, String> {
    let mut guard = state.0.lock().unwrap();
    let session = guard
        .as_mut()
        .ok_or_else(|| format!("{}No save loaded", SESSION_LOST_PREFIX))?;
    let progress = |phase: u8, current: u64, total: u64| {
        let _ = on_progress.send(ProgressMsg { phase, current, total });
    };
    let result = if from_pristine {
        session.apply_edits_from_pristine(&ops, &progress)
    } else {
        session.apply_edits(&ops, &progress)
    };
    match result {
        Ok(payload) => Ok(Response::new(payload)),
        // A failed incremental edit can leave the store torn down. Flag it so
        // the frontend reloads + replays (semantic refusals keep the session
        // healthy and come back as a plain error).
        Err(e) if !session.is_healthy() => Err(format!("{}{}", SESSION_LOST_PREFIX, e)),
        Err(e) => Err(e),
    }
}

/// Re-serialize the current (possibly edited) save to .sav bytes.
#[tauri::command]
async fn export_save(state: State<'_, AppState>) -> Result<Response, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    Ok(Response::new(session.export_sav()?))
}

#[tauri::command]
async fn extract_clipboard(
    names: Vec<String>,
    lightweight: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.extract_clipboard(&names, &lightweight)
}

#[tauri::command]
async fn describe_instance(name: String, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.describe_instance(&name)
}

#[tauri::command]
async fn find_item(item: String, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.find_item(&item)
}

#[tauri::command]
async fn building_info(types: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.building_info(&types)
}

#[tauri::command]
async fn vehicle_info(types: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.vehicle_info(&types)
}

#[tauri::command]
async fn train_info(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.train_info()
}

#[tauri::command]
async fn selection_inventory(
    names: Vec<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.0.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.selection_inventory(&names)
}

/// Instrumentation only. Native memory has no 4GB story to track, so this is a
/// stub -- the UI just ignores the numbers when they're zero.
#[tauri::command]
async fn mem_stats(_state: State<'_, AppState>) -> Result<MemStats, String> {
    Ok(MemStats { mem_bytes: 0, live_bytes: 0, lean: false })
}

/// Drop the current session (frontend recovery after a lost edit).
#[tauri::command]
async fn reset(state: State<'_, AppState>) -> Result<(), String> {
    *state.0.lock().unwrap() = None;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            load,
            apply_edits,
            export_save,
            extract_clipboard,
            describe_instance,
            find_item,
            building_info,
            vehicle_info,
            train_info,
            selection_inventory,
            mem_stats,
            reset,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
