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

   // Bumped by every state-changing request (load, edits); an in-flight
   // lean handoff refuses to swap if it changed under it.
   let stateVersion = 0;
   let handoffWorker = null; // lean worker being prepared in the background

   function attachHandlers(w) {
      w.onmessage = (event) => {
         const msg = event.data;
         if (msg.type === "progress") {
            if (activeProgress) {
               // memBytes: wasm memory size, for perf instrumentation --
               // the UI's progress callback just ignores the extra args.
               activeProgress(msg.phase, msg.current, msg.total, msg.memBytes, msg.liveBytes);
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
            const error = new Error(msg.error.message);
            error.sessionLost = !!msg.error.sessionLost;
            entry.reject(error);
         }
      };
      // A crashed worker (wasm panic / OOM) leaves indeterminate state:
      // reject everything and respawn fresh.
      w.onerror = (event) => {
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

   function spawnWorker() {
      worker = new Worker("worker.js");
      attachHandlers(worker);
   }

   function abortHandoff() {
      if (handoffWorker) {
         handoffWorker.terminate();
         handoffWorker = null;
      }
   }

   // Kick off a lean-worker swap unless the debug valves disable it.
   function scheduleLeanHandoff() {
      const params = new URLSearchParams(location.search);
      if (params.get("keepModel") === "1" || params.get("noLean") === "1") {
         return;
      }
      setTimeout(startLeanHandoff, 0);
   }

   // Lean-worker handoff: wasm linear memory never shrinks, so after load
   // the loaded worker's heap is stuck at the full-parse high-water mark
   // (~3.6GB on 600k-object saves) even though most of it is free. Extract
   // the compressed body + index into a FRESH worker that re-walks headers
   // and byte spans only (~1.2GB), then terminate the loaded worker --
   // terminate() is the only way to give its memory back to the browser.
   // Purely an optimization: any failure or interleaved edit aborts the
   // swap and the loaded worker keeps serving.
   function startLeanHandoff() {
      const version = stateVersion;
      request({ op: "extractLeanState" })
         .then((state) => new Promise((resolve, reject) => {
            if (version !== stateVersion) {
               reject(new Error("Lean handoff superseded"));
               return;
            }
            const next = new Worker("worker.js");
            handoffWorker = next;
            next.onmessage = (event) => {
               const msg = event.data;
               if (msg.type === "progress") {
                  return; // background work -- no UI
               }
               if (msg.ok) {
                  resolve(next);
               } else {
                  reject(new Error(msg.error.message));
               }
            };
            next.onerror = (event) => {
               reject(new Error("Lean worker crashed: " + (event.message || "unknown error")));
            };
            const transfer = [state.body.buffer, state.index.buffer, state.fileHeader.buffer];
            if (state.pristine) {
               transfer.push(state.pristine.buffer);
            }
            next.postMessage(
               {
                  id: 0,
                  op: "loadLean",
                  body: state.body,
                  pristine: state.pristine,
                  index: state.index,
                  fileHeader: state.fileHeader,
               },
               transfer,
            );
         }))
         .then((next) => {
            const trySwap = () => {
               if (version !== stateVersion || handoffWorker !== next) {
                  next.terminate(); // state moved on (edit / new load): stale
                  if (handoffWorker === next) {
                     handoffWorker = null;
                  }
                  return;
               }
               if (pending.size > 0) {
                  setTimeout(trySwap, 100); // don't strand in-flight replies
                  return;
               }
               const old = worker;
               worker = next;
               attachHandlers(worker);
               handoffWorker = null;
               old.terminate();
               console.info("SaveClient: swapped to lean worker");
            };
            trySwap();
         })
         .catch((error) => {
            console.warn("SaveClient: lean handoff aborted:", error);
            abortHandoff(); // loaded worker keeps serving
         });
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
         stateVersion++;
         abortHandoff();
         // ?keepModel=1: debug valve -- keep the parsed object model resident
         // in wasm after load (memory A/B against the default drop).
         // ?noLean=1 skips only the lean-worker handoff.
         const keepModel = new URLSearchParams(location.search).get("keepModel") === "1";
         return request({ op: "load", buffer, keepModel }, [buffer]).then((payloadBytes) => {
            activeProgress = null;
            scheduleLeanHandoff();
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
      // {memBytes, liveBytes}: wasm linear-memory size (high-water, never
      // shrinks) and currently-allocated heap bytes.
      memStats() {
         return request({ op: "memStats" });
      },
      // applyEdits(ops, fromPristine, onProgress) -> Promise<payload object>.
      // ops: array of edit-op objects (see rust editor/ops.rs). fromPristine
      // replaces the whole op list (undo); otherwise ops append.
      applyEdits(ops, fromPristine, onProgress) {
         activeProgress = onProgress || null;
         stateVersion++;
         abortHandoff();
         return request({ op: "applyEdits", ops, fromPristine }).then((payloadBytes) => {
            activeProgress = null;
            // Every edit cycle grows and fragments the wasm heap a little;
            // swapping to a fresh lean worker after each one keeps repeated
            // edits on 600k-object saves away from the 4GB ceiling.
            scheduleLeanHandoff();
            return JSON.parse(new TextDecoder().decode(payloadBytes));
         }, (error) => {
            activeProgress = null;
            throw error;
         });
      },
      // exportSave() -> Promise<Uint8Array of .sav bytes> (the current,
      // possibly edited, save re-serialized; the uploaded file is untouched).
      exportSave() {
         return request({ op: "exportSave" });
      },
      dispose() {
         abortHandoff();
         if (worker) {
            worker.postMessage({ op: "dispose" });
         }
      },
      // Terminate the worker and start over with a fresh wasm instance --
      // used to recover from a lost session (wasm memory never shrinks and
      // can't be trusted after a trap). Pending requests are rejected.
      reset() {
         stateVersion++;
         abortHandoff();
         if (!worker) {
            return;
         }
         const error = new Error("Save worker was reset");
         for (const entry of pending.values()) {
            entry.reject(error);
         }
         pending.clear();
         activeProgress = null;
         worker.terminate();
         worker = null;
      },
   };
})();
