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

use sav_tauri_lib::server_api;
use sav_tauri_lib::session::AppSession;
use std::sync::Mutex;
use tauri::ipc::{Channel, Response};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

struct AppState {
    session: Mutex<Option<AppSession>>,
    /// Payload bytes mid-transfer when too big for one IPC response (WebView2
    /// truncates very large bodies and V8 caps strings at ~512MB, so the
    /// frontend pulls byte slices and parses per top-level chunk). Cleared by
    /// `payload_done` or replaced by the next load/edit.
    chunked_payload: Mutex<Option<Vec<u8>>>,
    /// Native clipboard slots: big copy blobs stay HERE and only a small
    /// pointer JSON crosses the webview/OS clipboard (WebView2 truncates
    /// huge IPC strings -- an 800k-object blob runs to ~100MB+). Keyed by a
    /// monotonic id; entries live until the app exits because committed
    /// pasteExternal ops replay from them on every undo, across save loads.
    clipboard_slots: Mutex<(u64, std::collections::HashMap<u64, String>)>,
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
    let ops = {
        let slots = state.clipboard_slots.lock().unwrap();
        resolve_clipboard_slots(&ops, &slots.1)?
    };
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

/// Where the game itself lists saves: %LOCALAPPDATA%\FactoryGame\Saved\
/// SaveGames, descending into the per-account folder when there's exactly
/// one. None (the dialog keeps its own default) when absent.
fn game_saves_dir() -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .join("FactoryGame")
        .join("Saved")
        .join("SaveGames");
    let mut subdirs = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.is_dir());
    match (subdirs.next(), subdirs.next()) {
        (Some(only), None) => Some(only),
        _ => Some(root),
    }
}

/// Native "Save as…" export: pick a destination with the OS dialog, then
/// re-serialize the current save straight to disk. The browser build's
/// anchor-click download becomes a silent, invisible WebView2 download in a
/// native shell, and the bytes never need to cross the IPC boundary anyway.
/// Returns the written path, or None when the dialog is cancelled.
#[tauri::command]
async fn export_save_dialog(
    default_name: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let bytes = {
        let guard = state.session.lock().unwrap();
        let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
        session.export_sav()?
    };
    // Dialog + write on a blocking thread: the dialog can stay open
    // indefinitely and must not pin an async-runtime worker.
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("Export edited save")
            .add_filter("Satisfactory save", &["sav"])
            .set_file_name(&default_name);
        if let Some(dir) = game_saves_dir() {
            dialog = dialog.set_directory(dir);
        }
        let Some(picked) = dialog.blocking_save_file() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| format!("Bad export path: {e}"))?;
        std::fs::write(&path, &bytes)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(|e| format!("Export task failed: {e}"))?
}

/// Put text on the OS clipboard native-side (the desktop copy path):
/// keeps the whole flow off WebView2's permission-gated clipboard API.
#[tauri::command]
async fn clipboard_write_text(text: String) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}

/// Ceiling for a cross-app paste blob read off the OS clipboard; matches the
/// frontend's own cap for the browser path.
const PASTE_BLOB_MAX: usize = 200_000_000;

/// Read the OS clipboard native-side and return a paste blob if one is
/// there: WebView2's navigator.clipboard.readText pops a permission prompt
/// on every Ctrl+V, so the desktop build never touches it. Small blobs
/// return verbatim; a big one (copied in a browser tab -- desktop copies
/// that size already live in a slot) is stashed in a native slot and
/// returns a pointer, keeping huge strings off the IPC boundary.
/// Non-blob clipboard content returns None.
#[tauri::command]
async fn read_paste_blob(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) else {
        return Ok(None); // empty / non-text clipboard
    };
    if text.len() > PASTE_BLOB_MAX || !text.contains("\"smapPaste\"") {
        return Ok(None);
    }
    if text.len() <= INLINE_CLIPBOARD_MAX {
        return Ok(Some(text));
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(None);
    };
    if !matches!(parsed.get("smapPaste").and_then(|v| v.as_u64()), Some(1) | Some(2)) {
        return Ok(None);
    }
    // Same pointer shape extract_clipboard hands out for its own big blobs.
    let mut pointer = serde_json::Map::new();
    pointer.insert("smapPaste".into(), serde_json::json!(3));
    for key in ["anchor", "anchorZ", "bboxWorld", "count"] {
        match parsed.get(key) {
            Some(v) => {
                pointer.insert(key.to_string(), v.clone());
            }
            None if key == "anchorZ" => {} // absent in v1 blobs
            None => return Ok(None),
        }
    }
    let mut slots = state.clipboard_slots.lock().unwrap();
    // Re-pasting the same clipboard must not stash a fresh 100MB copy each
    // time: reuse the slot that already holds these exact bytes.
    let id = match slots.1.iter().find(|(_, v)| **v == text).map(|(k, _)| *k) {
        Some(id) => id,
        None => {
            slots.0 += 1;
            let id = slots.0;
            slots.1.insert(id, text);
            id
        }
    };
    pointer.insert("slot".into(), serde_json::json!(id));
    Ok(Some(serde_json::Value::Object(pointer).to_string()))
}

/// Blobs at most this big return whole (and land on the OS clipboard as the
/// portable v2 format other tabs/the browser build can paste). Bigger ones
/// stay in a native slot and only the pointer JSON crosses the IPC boundary.
const INLINE_CLIPBOARD_MAX: usize = 8 * 1024 * 1024;

#[tauri::command]
async fn extract_clipboard(
    names: Vec<String>,
    lightweight: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (blob, meta) = {
        let guard = state.session.lock().unwrap();
        let session = guard.as_ref().ok_or_else(|| "No save loaded".to_string())?;
        session.extract_clipboard_with_meta(&names, &lightweight)?
    };
    if blob.len() <= INLINE_CLIPBOARD_MAX {
        return Ok(blob);
    }
    let mut slots = state.clipboard_slots.lock().unwrap();
    slots.0 += 1;
    let id = slots.0;
    slots.1.insert(id, blob);
    // meta is `{"smapPaste":3,...}` from the core; add the slot id.
    let mut pointer: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&meta).map_err(|e| format!("Bad clipboard meta: {e}"))?;
    pointer.insert("slot".into(), serde_json::json!(id));
    Ok(serde_json::Value::Object(pointer).to_string())
}

/// Splice native-slot blobs back into pasteExternal ops: `{"op":...,"slot":N,
/// ...}` becomes the op's own fields merged with the stored blob's fields
/// (the blob JSON is inserted verbatim -- never parsed -- so a 100MB+ blob
/// costs one memcpy, not a serde round-trip).
fn resolve_clipboard_slots(
    ops: &str,
    slots: &std::collections::HashMap<u64, String>,
) -> Result<String, String> {
    if !ops.contains("\"slot\"") {
        return Ok(ops.to_string());
    }
    let parsed: Vec<serde_json::Value> =
        serde_json::from_str(ops).map_err(|e| format!("Bad edit ops: {e}"))?;
    let mut parts: Vec<String> = Vec::with_capacity(parsed.len());
    for mut op in parsed {
        let slot_id = op.as_object().and_then(|o| o.get("slot")).and_then(|v| v.as_u64());
        let Some(id) = slot_id else {
            parts.push(op.to_string());
            continue;
        };
        let blob = slots.get(&id).ok_or_else(|| {
            "The copied objects are no longer in memory (the app was restarted since the copy) \
             -- copy them again"
                .to_string()
        })?;
        let obj = op.as_object_mut().expect("op with slot is an object");
        obj.remove("slot");
        // The blob carries the authoritative anchor (same value the pointer
        // gave the frontend); dropping the op's copy avoids a duplicate key.
        obj.remove("anchor");
        let mut merged = op.to_string();
        merged.pop(); // trailing '}'
        merged.push(',');
        merged.push_str(&blob[1..]); // blob starts with '{'
        parts.push(merged);
    }
    Ok(format!("[{}]", parts.join(",")))
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

/// Result of `server_fetch_latest`: where the downloaded .sav landed (the
/// frontend feeds it to the normal path-based `load`) plus display info.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerFetchResult {
    path: String,
    save_name: String,
    session_name: String,
    save_date_time: String,
}

/// Download the newest save from a dedicated server through the official
/// HTTPS API (PasswordLogin -> EnumerateSessions -> DownloadSaveGame, see
/// `server_api`) into the app-data `server-saves/` folder. Stage labels for
/// the status line ride the channel. Runs on a blocking thread: the API
/// client is synchronous so the lib stays runtime-free.
#[tauri::command]
async fn server_fetch_latest(
    host: String,
    password: String,
    on_progress: Channel<String>,
    app: tauri::AppHandle,
) -> Result<ServerFetchResult, String> {
    let fetched = tauri::async_runtime::spawn_blocking(move || {
        server_api::fetch_latest(&host, &password, &|stage| {
            let _ = on_progress.send(stage);
        })
    })
    .await
    .map_err(|e| format!("Server fetch task failed: {e}"))??;

    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("No app data dir: {e}"))?
        .join("server-saves");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    // The server-side save name becomes the file name; strip characters
    // Windows forbids in file names.
    let stem: String = fetched
        .header
        .save_name
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let stem = if stem.trim().is_empty() { "server_save".to_string() } else { stem };
    let path = dir.join(format!("{stem}.sav"));
    std::fs::write(&path, &fetched.bytes)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    Ok(ServerFetchResult {
        path: path.to_string_lossy().into_owned(),
        save_name: fetched.header.save_name,
        session_name: fetched.header.session_name,
        save_date_time: fetched.header.save_date_time,
    })
}

#[cfg(test)]
mod slot_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn slot_op_merges_with_its_blob() {
        let blob = serde_json::json!({
            "smapPaste": 2, "saveVersion": 46, "objectVersion": 51,
            "lightweightVersion": 1, "anchor": [10.0, 20.0],
            "bboxWorld": [0.0, 0.0, 20.0, 40.0], "count": 3,
            "zLen": 12, "z": "abc="
        });
        let mut slots = HashMap::new();
        slots.insert(7u64, blob.to_string());
        let ops = serde_json::json!([
            {"op": "deleteActors", "names": ["X"]},
            {"op": "pasteExternal", "slot": 7, "anchor": [10.0, 20.0],
             "delta": [1.0, 2.0, 3.0], "rotateYawDeg": 90.0, "seed": 42}
        ])
        .to_string();
        let resolved = resolve_clipboard_slots(&ops, &slots).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&resolved).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["op"], "deleteActors");
        let p = &parsed[1];
        assert_eq!(p["op"], "pasteExternal");
        assert!(p.get("slot").is_none());
        assert_eq!(p["saveVersion"], 46);
        assert_eq!(p["z"], "abc=");
        assert_eq!(p["delta"][2], 3.0);
        assert_eq!(p["rotateYawDeg"], 90.0);
        assert_eq!(p["anchor"][1], 20.0);
        // The merged op string parses as a real EditOp.
        sav_core::editor::ops::parse_ops_json(&resolved).unwrap();
    }

    #[test]
    fn missing_slot_is_a_plain_error() {
        let ops = serde_json::json!([{"op": "pasteExternal", "slot": 99,
            "anchor": [0.0, 0.0], "delta": [0.0, 0.0, 0.0], "seed": 1}])
        .to_string();
        let err = resolve_clipboard_slots(&ops, &HashMap::new()).unwrap_err();
        assert!(err.contains("copy them again"), "{err}");
    }

    #[test]
    fn slotless_ops_pass_through_verbatim() {
        let ops = r#"[{"op":"moveActors","names":["A"],"delta":[1.0,2.0,0.0]}]"#;
        assert_eq!(resolve_clipboard_slots(ops, &HashMap::new()).unwrap(), ops);
    }
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            session: Mutex::new(None),
            chunked_payload: Mutex::new(None),
            clipboard_slots: Mutex::new((0, std::collections::HashMap::new())),
        })
        .invoke_handler(tauri::generate_handler![
            load,
            apply_edits,
            export_save,
            export_save_dialog,
            clipboard_write_text,
            read_paste_blob,
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
            server_fetch_latest,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
