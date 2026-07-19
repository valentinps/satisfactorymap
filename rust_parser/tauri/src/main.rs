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

struct AppState {
    session: Mutex<Option<AppSession>>,
    /// Payload bytes mid-transfer when too big for one IPC response (WebView2
    /// truncates very large bodies and V8 caps strings at ~512MB, so the
    /// frontend pulls byte slices and parses per top-level chunk). Cleared by
    /// `payload_done` or replaced by the next load/edit.
    chunked_payload: Mutex<Option<Vec<u8>>>,
}

/// Payloads at most this big go back as one raw `Response` (the standing
/// path); bigger ones switch to the chunked-slice protocol.
const DIRECT_PAYLOAD_MAX: usize = 200 * 1024 * 1024;
/// Target upper bound for one JSON-parseable chunk. A single top-level entry
/// larger than this becomes its own chunk (must stay under V8's ~512MB string
/// cap; the biggest observed entry is ~377MB on a 6.5M-object save).
const CHUNK_MAX: usize = 200 * 1024 * 1024;

/// Byte ranges (start, end) of the top-level `"key":value` entries of a
/// serialized JSON object, separating commas and outer braces excluded.
fn top_level_entries(payload: &[u8]) -> Vec<(usize, usize)> {
    let mut entries = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut start: Option<usize> = None;
    for (i, &b) in payload.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => {
                if depth == 1 && start.is_none() {
                    start = Some(i); // entries always begin at their key quote
                }
                in_string = true;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        entries.push((s, i));
                    }
                }
            }
            b',' if depth == 1 => {
                if let Some(s) = start.take() {
                    entries.push((s, i));
                }
            }
            _ => {}
        }
    }
    entries
}

/// Greedy-pack contiguous top-level entries into (offset, len) chunk ranges of
/// at most CHUNK_MAX bytes each (oversized single entries get their own chunk).
/// Each range parses as a JSON object once wrapped in braces.
fn chunk_ranges(payload: &[u8]) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (s, e) in top_level_entries(payload) {
        match ranges.last_mut() {
            Some((cs, ce)) if e - *cs <= CHUNK_MAX => *ce = e,
            _ => ranges.push((s, e)),
        }
    }
    ranges.into_iter().map(|(s, e)| (s as u64, (e - s) as u64)).collect()
}

/// Wrap a freshly built payload for the IPC boundary: small ones ship whole,
/// big ones are stashed and described by a marker the frontend resolves via
/// `payload_slice` calls.
fn payload_response(payload: Vec<u8>, state: &AppState) -> Response {
    let mut stash = state.chunked_payload.lock().unwrap();
    if payload.len() <= DIRECT_PAYLOAD_MAX {
        *stash = None;
        return Response::new(payload);
    }
    let ranges = chunk_ranges(&payload);
    let marker = serde_json::json!({ "__chunkedPayload": { "ranges": ranges } });
    let bytes = serde_json::to_vec(&marker).expect("marker json");
    *stash = Some(payload);
    Response::new(bytes)
}

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
    *state.session.lock().unwrap() = Some(app_session);
    Ok(payload_response(payload, &state))
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
    let mut guard = state.session.lock().unwrap();
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
        Ok(payload) => Ok(payload_response(payload, &state)),
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
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    Ok(Response::new(session.export_sav()?))
}

#[tauri::command]
async fn extract_clipboard(
    names: Vec<String>,
    lightweight: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.extract_clipboard(&names, &lightweight)
}

#[tauri::command]
async fn describe_instance(name: String, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.describe_instance(&name)
}

#[tauri::command]
async fn find_item(item: String, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.find_item(&item)
}

#[tauri::command]
async fn building_info(types: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.building_info(&types)
}

#[tauri::command]
async fn vehicle_info(types: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.vehicle_info(&types)
}

#[tauri::command]
async fn train_info(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
    session.train_info()
}

#[tauri::command]
async fn selection_inventory(
    names: Vec<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
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
    *state.session.lock().unwrap() = None;
    *state.chunked_payload.lock().unwrap() = None;
    Ok(())
}

/// One byte slice of the stashed chunked payload (see `payload_response`).
#[tauri::command]
async fn payload_slice(
    offset: u64,
    len: u64,
    state: State<'_, AppState>,
) -> Result<Response, String> {
    let stash = state.chunked_payload.lock().unwrap();
    let payload = stash.as_ref().ok_or("No chunked payload pending")?;
    let (o, l) = (offset as usize, len as usize);
    let end = o
        .checked_add(l)
        .filter(|&e| e <= payload.len())
        .ok_or("payload_slice out of range")?;
    Ok(Response::new(payload[o..end].to_vec()))
}

/// Frontend finished assembling the chunked payload; free the stash.
#[tauri::command]
async fn payload_done(state: State<'_, AppState>) -> Result<(), String> {
    *state.chunked_payload.lock().unwrap() = None;
    Ok(())
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn chunks_reassemble_to_the_original_object() {
        let payload = serde_json::json!({
            "a": 1,
            "s": "quote \" brace } bracket ] comma , backslash \\",
            "arr": [1, {"x": [2, 3]}, "y,z}"],
            "obj": {"nested": {"deep": [true, null]}},
            "tail": "end"
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let entries = top_level_entries(&bytes);
        assert_eq!(entries.len(), 5);
        // Every contiguous grouping must parse once brace-wrapped; merge all
        // single-entry chunks and compare against the original value.
        let mut merged = serde_json::Map::new();
        for (s, e) in entries {
            let mut chunk = vec![b'{'];
            chunk.extend_from_slice(&bytes[s..e]);
            chunk.push(b'}');
            let v: serde_json::Value = serde_json::from_slice(&chunk).unwrap();
            merged.extend(v.as_object().unwrap().clone());
        }
        assert_eq!(serde_json::Value::Object(merged), payload);
    }

    #[test]
    fn chunk_ranges_cover_all_entries_in_order() {
        let payload = serde_json::json!({"a": 1, "b": [2, 3], "c": "x,y"});
        let bytes = serde_json::to_vec(&payload).unwrap();
        // CHUNK_MAX far exceeds this payload: everything packs into one range.
        let ranges = chunk_ranges(&bytes);
        assert_eq!(ranges.len(), 1);
        let (s, l) = ranges[0];
        let mut chunk = vec![b'{'];
        chunk.extend_from_slice(&bytes[s as usize..(s + l) as usize]);
        chunk.push(b'}');
        let v: serde_json::Value = serde_json::from_slice(&chunk).unwrap();
        assert_eq!(v, payload);
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { session: Mutex::new(None), chunked_payload: Mutex::new(None) })
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
            payload_slice,
            payload_done,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
