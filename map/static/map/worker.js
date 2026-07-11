// Web Worker hosting the WASM save parser + payload builder (sav_wasm).
// Protocol (id-based RPC):
//   in : {id, op: "load", buffer: ArrayBuffer}           (buffer transferred)
//        {id, op: "describeInstance", name}
//        {id, op: "findItem", item}
//        {id, op: "buildingInfo", types: [..]}
//        {id, op: "vehicleInfo", types: [..]}
//        {id, op: "trainInfo"}
//        {id, op: "selectionInventory", names: [..]}
//        {id, op: "exportSave"}                          (result: transferred
//        Uint8Array of re-serialized .sav bytes)
//        {op: "dispose"}
//   out: {id, ok: true, result}                          (load: result is a
//        transferred Uint8Array of payload JSON; queries: parsed objects)
//        {id, ok: false, error: {message}}
//        {type: "progress", phase, current, total}       (unsolicited, load)
// Progress phase strings match what the Flask server's /api/load-progress
// used to emit, so data.js's progress UI is unchanged.

importScripts("pkg/sav_wasm.js");

const PHASE_LABELS = ["Decompressing", "Parsing", "Building map data"];

let wasmExports = null;
let wasmReady = wasm_bindgen("pkg/sav_wasm_bg.wasm").then(function(exports) {
   wasmExports = exports;
});
let session = null;
// True in a worker whose session came from loadLean (post-handoff).
let leanSession = false;
// The unedited save body, zlib-compressed, captured before the first edit.
// Held HERE (JS memory) rather than inside the session: the wasm heap is
// capped at 4GB and a parsed 600k-object save already fills most of it.
let pristine = null;

// Current wasm linear-memory size -- the tab's dominant footprint; attached
// to progress events so perf runs can track the high-water mark for free.
function wasmMemBytes() {
   return wasmExports && wasmExports.memory ? wasmExports.memory.buffer.byteLength : 0;
}

// Live (allocated, not freed) heap bytes. Linear memory never shrinks, so
// memBytes stays at the high-water mark; this is the number that drops when
// the parsed object model is freed after load.
function wasmLiveBytes() {
   return wasmExports && wasmExports.live_heap_bytes ? wasmExports.live_heap_bytes() : 0;
}

function reply(id, result, transfer) {
   self.postMessage({ id, ok: true, result }, transfer || []);
}

function replyError(id, error, sessionLost) {
   self.postMessage({
      id,
      ok: false,
      error: { message: String(error && error.message || error), sessionLost: !!sessionLost },
   });
}

self.onmessage = async (event) => {
   const msg = event.data;
   if (msg.op === "dispose") {
      if (session) { session.free(); session = null; }
      return;
   }
   const { id, op } = msg;
   try {
      await wasmReady;
      if (op === "load") {
         // Free the previous save BEFORE parsing the new one: wasm linear
         // memory never shrinks, so peak usage must stay bounded at one save.
         if (session) { session.free(); session = null; }
         pristine = null;
         if (msg.keepModel) {
            // ?keepModel=1 debug valve: keep the parsed object model
            // resident (memory A/B against the post-load drop).
            wasm_bindgen.set_keep_object_model(true);
         }
         const bytes = new Uint8Array(msg.buffer);
         session = new wasm_bindgen.SaveSession(bytes, (phase, current, total) => {
            self.postMessage({
               type: "progress",
               phase: PHASE_LABELS[phase] || "Loading",
               current,
               total,
               memBytes: wasmMemBytes(),
               liveBytes: wasmLiveBytes(),
            });
         });
         const payload = session.payload_json();
         reply(id, payload, [payload.buffer]);
         return;
      }
      if (op === "loadLean") {
         // Second half of the lean handoff: rebuild the session in THIS
         // fresh wasm instance from the loaded worker's extracted state.
         // Headers + byte spans only -- the parsed object model is never
         // materialized here, so this instance's linear memory tops out at
         // ~body + index instead of the loaded worker's full-parse peak.
         if (session) { session.free(); session = null; }
         // The store body and the undo baseline are distinct once edits
         // happened; straight after load they're the same blob.
         const bodyBlob = new Uint8Array(msg.body);
         pristine = msg.pristine ? new Uint8Array(msg.pristine) : bodyBlob;
         session = wasm_bindgen.SaveSession.load_lean(
            bodyBlob,
            new Uint8Array(msg.index),
            new Uint8Array(msg.fileHeader),
            (phase, current, total) => {
               self.postMessage({
                  type: "progress",
                  phase: PHASE_LABELS[phase] || "Loading",
                  current,
                  total,
                  memBytes: wasmMemBytes(),
                  liveBytes: wasmLiveBytes(),
               });
            },
         );
         leanSession = true;
         reply(id, { memBytes: wasmMemBytes(), liveBytes: wasmLiveBytes() });
         return;
      }
      if (!session) {
         throw new Error("No save loaded");
      }
      let raw;
      switch (op) {
         case "applyEdits": {
            // Rebuilds the store + payload inside the session; progress
            // events reuse the load phases ("Parsing"/"Building map data").
            const progressCb = (phase, current, total) => {
               self.postMessage({
                  type: "progress",
                  phase: PHASE_LABELS[phase] || "Applying edits",
                  current,
                  total,
                  memBytes: wasmMemBytes(),
                  liveBytes: wasmLiveBytes(),
               });
            };
            if (!pristine) {
               pristine = session.compress_pristine();
            }
            const opsJson = JSON.stringify(msg.ops);
            const payload = msg.fromPristine
               ? session.apply_edits_from_pristine(opsJson, pristine, progressCb)
               : session.apply_edits(opsJson, progressCb);
            reply(id, payload, [payload.buffer]);
            return;
         }
         case "exportSave": {
            const bytes = session.export_sav();
            reply(id, bytes, [bytes.buffer]);
            return;
         }
         case "extractClipboard":
            // Cross-save clipboard blob (JSON string) -- the main thread
            // puts it on the OS clipboard for other tabs.
            reply(id, session.extract_clipboard(msg.names, JSON.stringify(msg.lightweight)));
            return;
         case "memStats":
            reply(id, { memBytes: wasmMemBytes(), liveBytes: wasmLiveBytes(), lean: leanSession });
            return;
         case "extractLeanState": {
            // First half of the lean handoff: ship the CURRENT body
            // (compressed), the undo baseline (when edits happened -- after
            // load they're one and the same), the CBOR index and the .sav
            // header prefix to the main thread, which feeds them to a fresh
            // worker's loadLean and terminates this one (the only way to
            // give its linear memory back to the browser). The undo blob is
            // COPIED, not transferred: an aborted swap leaves this worker
            // serving, and its own baseline must survive.
            const body = session.compress_pristine();
            const indexBytes = session.serialize_index();
            const fileHeader = session.file_header_bytes();
            const undo = pristine ? pristine.slice() : null;
            console.info("Lean handoff: body " + body.length + "B compressed, index "
               + indexBytes.length + "B" + (undo ? ", undo baseline " + undo.length + "B" : ""));
            const transfer = [body.buffer, indexBytes.buffer, fileHeader.buffer];
            if (undo) {
               transfer.push(undo.buffer);
            }
            reply(id, { body, pristine: undo, index: indexBytes, fileHeader }, transfer);
            return;
         }
         case "describeInstance": raw = session.describe_instance(msg.name); break;
         case "findItem": raw = session.find_item(msg.item); break;
         case "buildingInfo": raw = session.building_info(msg.types); break;
         case "vehicleInfo": raw = session.vehicle_info(msg.types); break;
         case "trainInfo": raw = session.train_info(); break;
         case "selectionInventory":
            // The Flask endpoint wrapped the raw list as {"items": [...]} --
            // selection.js still expects that shape.
            reply(id, { items: JSON.parse(session.selection_inventory(msg.names)) });
            return;
         default: throw new Error("Unknown op: " + op);
      }
      reply(id, JSON.parse(raw));
   } catch (error) {
      // A wasm trap (out of memory on huge saves) can leave the session
      // without usable state; tell the client so it can recover by
      // reloading the save and replaying its edits.
      let sessionLost = false;
      if (session) {
         try {
            sessionLost = !session.is_healthy();
         } catch (probeError) {
            sessionLost = true;
         }
      }
      replyError(id, error, sessionLost);
   }
};
