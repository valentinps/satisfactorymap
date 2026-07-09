// Web Worker hosting the WASM save parser + payload builder (sav_wasm).
// Protocol (id-based RPC):
//   in : {id, op: "load", buffer: ArrayBuffer}           (buffer transferred)
//        {id, op: "describeInstance", name}
//        {id, op: "findItem", item}
//        {id, op: "buildingInfo", types: [..]}
//        {id, op: "vehicleInfo", types: [..]}
//        {id, op: "trainInfo"}
//        {id, op: "selectionInventory", names: [..]}
//        {op: "dispose"}
//   out: {id, ok: true, result}                          (load: result is a
//        transferred Uint8Array of payload JSON; queries: parsed objects)
//        {id, ok: false, error: {message}}
//        {type: "progress", phase, current, total}       (unsolicited, load)
// Progress phase strings match what the Flask server's /api/load-progress
// used to emit, so data.js's progress UI is unchanged.

importScripts("pkg/sav_wasm.js");

const PHASE_LABELS = ["Decompressing", "Parsing", "Building map data"];

let wasmReady = wasm_bindgen("pkg/sav_wasm_bg.wasm");
let session = null;

function reply(id, result, transfer) {
   self.postMessage({ id, ok: true, result }, transfer || []);
}

function replyError(id, error) {
   self.postMessage({ id, ok: false, error: { message: String(error && error.message || error) } });
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
         const bytes = new Uint8Array(msg.buffer);
         session = new wasm_bindgen.SaveSession(bytes, (phase, current, total) => {
            self.postMessage({
               type: "progress",
               phase: PHASE_LABELS[phase] || "Loading",
               current,
               total,
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
      replyError(id, error);
   }
};
