# Tauri desktop app — a native backend for saves that don't fit wasm32

## Why

The browser build runs the parser in a WebAssembly worker, which is hard-capped
at 4 GB of linear memory (wasm32, 32-bit addressing). Standing memory on the
600k-object save is now ~2.2 GB after the streaming-builder work, so the current
biggest saves fit — but memory scales ~linearly with object count, so a save
2–4× larger (~1.2–2.4M objects, decompressed body ~1.7–3.4 GB) blows past 4 GB
and can't be loaded in the browser at all.

The parser core (`rust_parser/core`, crate `sav_core`) already runs natively with
no ceiling — that's what `core/examples/dump_payload.rs` exercises. This project
wraps that same core in a **Tauri v2 desktop app**: the existing static frontend
(`dist/`) rendered in a native webview, talking to `sav_core` through Tauri
commands instead of the wasm worker. No 4 GB limit, no wasm64 migration, and
essentially all logic is reused — the Tauri crate is a thin binding shell exactly
parallel to the wasm one.

**Goal:** a desktop app that loads, edits, and exports arbitrarily large saves,
reusing the current UI verbatim, organized cleanly inside the existing repo (one
Cargo workspace, no fork).

## Current architecture (read these first)

The system already has the two seams this work plugs into.

1. **`sav_core` is the transport-agnostic engine.** All real work lives here:
   `level::parse_full_save` / `parse_full_save_lean`, `mapdata::build_all_json`,
   `editor::session::{fold_ops, rebuild, step_owned}`, `editor::export`,
   `editor::clipboard::extract_clipboard`, `mapdata::describe` / `mapdata::queries`.
   Neither the browser nor a desktop shell contains parsing/build logic.

2. **`rust_parser/wasm` (crate `sav_wasm`) is a thin binding shell** over
   `sav_core`. `wasm/src/lib.rs` defines `SaveSession` — a stateful struct with
   `#[wasm_bindgen]` methods that (a) orchestrate load/edit/export over `sav_core`
   and (b) marshal to JS (`JsError`, `js_sys::Function` progress callbacks). The
   Tauri crate mirrors this file method-for-method.
   The methods to mirror (`wasm/src/lib.rs`):
   - `load(bytes, on_progress)` — parse (lean by default) → `build_all_json` →
     drop model → hold `SaveSession` fields. Returns payload JSON bytes.
   - `apply_edits(ops_json, from_pristine, on_progress)` → `fold_ops` →
     `finish_edit` (build + drop + swap). Returns new payload.
   - `apply_edits_from_pristine(ops_json, on_progress)` — undo/recovery: decompress
     pristine body → `rebuild` → `finish_edit`.
   - `payload_json`, `is_healthy`, `describe_instance`, `find_item`,
     `building_info`, `vehicle_info`, `train_info`, `selection_inventory`,
     `extract_clipboard`, `export_sav`.
   - **Drop these in the Tauri shell** (they exist only to reclaim wasm linear
     memory, which a native allocator frees on its own): `load_lean`,
     `serialize_index`, `file_header_bytes`, `compress_pristine`, and the
     `extractLeanState` op. The whole lean-worker handoff is a wasm-only hack.

3. **The frontend funnels every backend call through one function.**
   `map/static/map/save_client.js`: `SaveClient` is an IIFE whose `request(msg,
   transfer)` posts a message to the worker (`spawnWorker()` → `new
   Worker("worker.js")`) and resolves a Promise keyed by `id`. Every public method
   (`loadSave`, `applyEdits`, `describeInstance`, `findItem`, `buildingInfo`,
   `vehicleInfo`, `trainInfo`, `selectionInventory`, `memStats`, `extractClipboard`,
   `exportSave`, `reset`) routes through `request()`. Progress arrives as
   `{type:"progress", phase, current, total, memBytes, liveBytes}` messages handled
   in `attachHandlers`. `worker.js` is the wasm-side dispatcher: a `switch` on
   `msg.op` (`load`, `applyEdits`, `applyEdits_from_pristine`, `exportSave`,
   `extractClipboard`, `memStats`, `describeInstance`, `findItem`, `buildingInfo`,
   `vehicleInfo`, `trainInfo`, `selectionInventory`, `extractLeanState`) that calls
   the `SaveSession` methods.
   `scheduleLeanHandoff()` (called after every load/edit) is a browser-only
   optimization — **becomes a no-op under Tauri.**

4. **Build.** `tools/build_site.py` produces `dist/` (frontend + tiles +
   `dist/pkg/` wasm). `tools/serve_site.py 8791` serves it. The desktop app bundles
   the same `dist/`; it simply never spawns the worker.

## Target shape

One workspace, one new crate. `dist/` stays byte-identical and works in both the
browser (worker transport) and the desktop app (Tauri transport) — a runtime
switch on `window.__TAURI__` picks the transport, so there is **no separate
frontend build and no fork of `save_client.js`'s public API.**

```
rust_parser/
  core/     sav_core    — the engine (unchanged)
  wasm/     sav_wasm     — browser binding (unchanged)
  tauri/    sav_tauri    — NEW: Tauri v2 app, thin shell over sav_core
```

Add `"tauri"` to `rust_parser/Cargo.toml` `[workspace] members`. `sav_tauri`
depends on `sav_core = { path = "../core" }` (like `sav_wasm` does) plus `tauri`,
`serde`, `serde_json`.

### Resolved design decisions (do these; don't re-litigate)

- **Mirror, don't refactor.** `sav_tauri` gets its own `AppSession` struct that
  ports the essential orchestration from `wasm/src/lib.rs` (load / apply_edits /
  apply_edits_from_pristine / finish_edit + the query pass-throughs), swapping
  `JsError` → `String` and the `js_sys::Function` progress callback → a Tauri
  progress channel. Do **not** hoist a shared session into `sav_core` in this pass
  — that would disturb the just-completed streaming-builder path in the wasm
  binding. (Note it as a possible future cleanup.)
- **Drop the lean-worker handoff entirely** in Tauri mode. Native memory frees
  normally, so there is no linear-memory-never-shrinks problem to work around.
  `AppSession` holds one live session for the app's lifetime.
- **Load from a path, not bytes.** Passing a 200 MB `.sav` (and a 150 MB+ payload
  back) through the IPC boundary is wasteful and defeats the point. The `load`
  command takes a file **path** (from the Tauri dialog plugin) and reads it
  natively; the payload is returned as a raw byte response, not a JSON-serialized
  array. Use Tauri v2's raw IPC response (`tauri::ipc::Response::new(bytes)`) for
  the payload, and a `tauri::ipc::Channel<ProgressMsg>` for progress.
- **Undo via a retained pristine body.** `AppSession` keeps the pristine
  (uncompressed — no memory pressure natively) body so `apply_edits_from_pristine`
  can `rebuild`, same contract the frontend already relies on.
- **`memStats`** returns process RSS (via the `sysinfo` crate) or a stub — the UI
  only uses it for instrumentation. Low priority; a `{memBytes, liveBytes, lean:
  false}` stub is acceptable for a first cut.

## Steps

1. **Scaffold the crate.** `rust_parser/tauri/` with `Cargo.toml` (deps: `sav_core`
   path, `tauri` v2, `tauri-plugin-dialog`, `serde`, `serde_json`, `sysinfo`
   optional), `tauri.conf.json`, `build.rs` (`tauri_build::build()`), and
   `src/main.rs` + `src/session.rs`. In `tauri.conf.json`: `build.frontendDist =
   "../../dist"`, `build.beforeBuildCommand = "python tools/build_site.py"` (run
   from repo root — set the Tauri `build` cwd accordingly, or use
   `--skip-wasm` since the desktop app never loads the wasm), app window
   `width/height`, and the identifier. WebView2 is preinstalled on Windows 11.

2. **`AppSession` (`src/session.rs`).** Port the orchestration from
   `wasm/src/lib.rs` `SaveSession` (see the method list above), `JsError`→`String`,
   progress callback → `Channel<ProgressMsg>` where `ProgressMsg { phase, current,
   total }`. Hold it in Tauri managed state: `Mutex<Option<AppSession>>`.

3. **Commands (`src/main.rs`), 1:1 with the worker `switch` cases.**
   `#[tauri::command] async fn load(path, channel, state) -> Result<Response,
   String>`; likewise `apply_edits`, `apply_edits_from_pristine`, `export_save`
   (writes to a path chosen via dialog, or returns bytes), `extract_clipboard`,
   `describe_instance`, `find_item`, `building_info`, `vehicle_info`, `train_info`,
   `selection_inventory`, `mem_stats`, `reset`. Register with
   `tauri::generate_handler![...]` and `.manage(Mutex::new(None))`.
   Query commands return JSON strings exactly as the wasm methods do (the frontend
   already `JSON.parse`s them).

4. **Frontend transport switch (`map/static/map/save_client.js`).** Extract the
   worker-specific pieces (`spawnWorker`, `request`, `attachHandlers`, progress
   dispatch) behind a small transport interface with two impls:
   - `workerTransport` — today's code, unchanged.
   - `tauriTransport` — `request(msg)` maps `msg.op` to `invoke("<command>",
     args)`; progress via a `Channel` passed into `load`/`applyEdits` that calls
     the same `activeProgress(phase, current, total, memBytes, liveBytes)`.
   Pick at init: `const transport = window.__TAURI__ ? tauriTransport :
   workerTransport;`. Make `scheduleLeanHandoff()`/`abortHandoff()` no-ops under
   Tauri. Keep `SaveClient`'s public method bodies (loadSave, applyEdits, …)
   unchanged — they call `request()` / `transport`.
   For `loadSave` under Tauri: instead of reading a `File` to an ArrayBuffer, use
   the Tauri dialog plugin to get a path and pass the path to the `load` command.
   Gate that on the transport so the browser `#uploadFileInput` flow is untouched.

5. **Payload/large-binary handling.** Return the payload as a raw byte `Response`
   from the `load`/`apply_edits` commands; on the JS side decode with
   `new TextDecoder().decode(bytes)` then `JSON.parse` (same as today). Do **not**
   round-trip 150 MB+ through `serde_json`.

6. **Package.** `cargo tauri dev` for the loop, `cargo tauri build` for a Windows
   installer (MSI/NSIS). Add a `tools/run_desktop.py` or document the `cargo tauri`
   commands in `README.md`.

## Constraints — do not regress these

- **Don't touch the browser path.** The wasm crate, `worker.js`, and the browser
  behavior of `save_client.js` must keep working byte-for-byte. All Tauri code is
  additive; the frontend change is a transport branch gated on `window.__TAURI__`.
- **Op parity.** Every worker op must have a Tauri command with identical
  request/response shape, so the shared `SaveClient` public methods work unchanged
  on both transports.
- **Payload parity.** The desktop app must produce the **same payload/export bytes**
  as the browser for a save that fits both — it's the same `sav_core` calls. Gate
  it: load a mid-size save (e.g. `map/uploads/solo_autosave_1.sav`) in both,
  compare the payload JSON.
- **No lean-handoff leakage.** The `load_lean`/`extractLeanState`/`serialize_index`
  machinery is wasm-only; don't port it and don't call it from the Tauri transport.

## Verification

- `cd rust_parser && cargo build -p sav_tauri --release` compiles clean.
- `cargo tauri dev` launches the window; the existing UI renders.
- Load `map/uploads/BuildITBIIIIIG_autosave_0.sav` (50 MB) — parses, map renders,
  search/sidebar/modals work; then load a genuinely large save (2–4×, if
  available) and confirm it loads where the browser would OOM.
- Move/paste/delete/undo an object; **export** and re-import the exported `.sav`
  (round-trip must reproduce a valid save) — mirror `tools/e2e_editor.py`'s flow.
- Payload-parity: load `solo_autosave_1.sav` in the desktop app and in the browser
  (`serve_site.py`), dump both payloads, `cmp` — must be identical.
- Confirm the browser build is unaffected: `python tools/build_site.py &&
  python tools/serve_site.py 8791 && python tools/e2e_editor.py` still green.

## Build loop

```
# Rust core is shared; if you change it, the browser gates still apply:
cd rust_parser && cargo test -p sav_core --release

# Desktop app:
cd rust_parser && cargo build -p sav_tauri --release
cargo tauri dev            # from the tauri crate dir (or `cargo tauri dev --config ...`)
cargo tauri build          # Windows installer

# Browser regression (must stay green):
python tools/build_site.py
python tools/serve_site.py 8791
python tools/e2e_editor.py
```
`python`, not `py`. Prereqs: Rust, Tauri CLI (`cargo install tauri-cli --version
'^2'`), WebView2 (bundled on Windows 11).

## Open decisions for the executing agent / user

- **Tauri crate location:** `rust_parser/tauri/` (keeps all Rust in one workspace,
  recommended) vs a top-level `desktop/src-tauri/`. The plan assumes the former.
- **Export UX:** native "Save As" dialog writing the `.sav` directly (recommended
  for big files) vs returning bytes to the JS download flow.
- **Distribution/signing** of the Windows installer is out of scope for the first
  cut — get `cargo tauri build` producing an unsigned installer, decide signing
  later.
- **Future cleanup (not now):** if drift between the wasm and Tauri session shells
  becomes a maintenance cost, hoist the load/edit/export orchestration into a
  transport-agnostic `sav_core::session` that both shells wrap.
