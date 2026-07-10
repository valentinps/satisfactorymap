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
// The unedited save body, zlib-compressed, captured before the first edit.
// Held HERE (JS memory) rather than inside the session: the wasm heap is
// capped at 4GB and a parsed 600k-object save already fills most of it.
let pristine = null;

// Current wasm linear-memory size -- the tab's dominant footprint; attached
// to progress events so perf runs can track the high-water mark for free.
function wasmMemBytes() {
   return wasmExports && wasmExports.memory ? wasmExports.memory.buffer.byteLength : 0;
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
         const bytes = new Uint8Array(msg.buffer);
         session = new wasm_bindgen.SaveSession(bytes, (phase, current, total) => {
            self.postMessage({
               type: "progress",
               phase: PHASE_LABELS[phase] || "Loading",
               current,
               total,
               memBytes: wasmMemBytes(),
            });
         });
         const payload = session.payload_json();
         reply(id, payload, [payload.buffer]);
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
