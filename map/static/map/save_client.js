// SaveClient: main-thread facade over the WASM parser worker (worker.js).
// Replaces the Flask endpoints: loadSave <- /api/map-data, describeInstance
// <- /api/instance, findItem <- /api/find-item, buildingInfo <-
// /api/building-info, vehicleInfo <- /api/vehicle-info (+ trainInfo for
// types=train), selectionInventory <- POST /api/selection-inventory.
// Every method returns a Promise resolving to an already-parsed object, so
// the former fetch(...).then(r => r.json()) call sites keep their bodies.
const SaveClient = (() => {
   let worker = null;
   let nextId = 1;
   const pending = new Map(); // id -> {resolve, reject}
   let activeProgress = null; // progress callback of the in-flight load

   function spawnWorker() {
      worker = new Worker("worker.js");
      worker.onmessage = (event) => {
         const msg = event.data;
         if (msg.type === "progress") {
            if (activeProgress) {
               // memBytes: wasm memory size, for perf instrumentation --
               // the UI's progress callback just ignores the extra arg.
               activeProgress(msg.phase, msg.current, msg.total, msg.memBytes);
            }
            return;
         }
         const entry = pending.get(msg.id);
         if (!entry) {
            return;
         }
         pending.delete(msg.id);
         if (msg.ok) {
            entry.resolve(msg.result);
         } else {
            entry.reject(new Error(msg.error.message));
         }
      };
      // A crashed worker (wasm panic / OOM) leaves indeterminate state:
      // reject everything and respawn fresh.
      worker.onerror = (event) => {
         const error = new Error("Save worker crashed: " + (event.message || "unknown error"));
         for (const entry of pending.values()) {
            entry.reject(error);
         }
         pending.clear();
         activeProgress = null;
         worker.terminate();
         spawnWorker();
      };
   }

   function request(msg, transfer) {
      if (!worker) {
         spawnWorker();
      }
      return new Promise((resolve, reject) => {
         const id = nextId++;
         pending.set(id, { resolve, reject });
         worker.postMessage(Object.assign({ id }, msg), transfer || []);
      });
   }

   return {
      // loadSave(arrayBuffer, onProgress) -> Promise<payload object>.
      // onProgress(phaseLabel, current, total) mirrors /api/load-progress.
      loadSave(buffer, onProgress) {
         activeProgress = onProgress || null;
         return request({ op: "load", buffer }, [buffer]).then((payloadBytes) => {
            activeProgress = null;
            return JSON.parse(new TextDecoder().decode(payloadBytes));
         }, (error) => {
            activeProgress = null;
            throw error;
         });
      },
      describeInstance(name) {
         return request({ op: "describeInstance", name });
      },
      findItem(item) {
         return request({ op: "findItem", item });
      },
      buildingInfo(types) {
         return request({ op: "buildingInfo", types });
      },
      vehicleInfo(types) {
         return request({ op: "vehicleInfo", types });
      },
      trainInfo() {
         return request({ op: "trainInfo" });
      },
      selectionInventory(names) {
         return request({ op: "selectionInventory", names });
      },
      // exportSave() -> Promise<Uint8Array of .sav bytes> (the current,
      // possibly edited, save re-serialized; the uploaded file is untouched).
      exportSave() {
         return request({ op: "exportSave" });
      },
      dispose() {
         if (worker) {
            worker.postMessage({ op: "dispose" });
         }
      },
   };
})();
