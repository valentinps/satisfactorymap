// SaveClient: main-thread facade over the WASM parser worker (worker.js).
// Replaces the Flask endpoints: loadSave <- /api/map-data, describeInstance
// <- /api/instance, findItem <- /api/find-item, buildingInfo <-
// /api/building-info, vehicleInfo <- /api/vehicle-info (+ trainInfo for
// types=train), selectionInventory <- POST /api/selection-inventory.
// Every method returns a Promise resolving to an already-parsed object, so
// the former fetch(...).then(r => r.json()) call sites keep their bodies.
const SaveClient = (() => {
   // Desktop (Tauri) build: sav_core runs natively behind Tauri commands, no
   // wasm worker and no 4GB ceiling. The public API below is identical on both
   // transports; only request()/loadSave differ. window.__TAURI__ is injected
   // by the native shell (withGlobalTauri) and absent in the browser.
   const IS_TAURI = typeof window !== "undefined" && !!window.__TAURI__;
   // Numeric parse/build phases -> the same labels worker.js emits, so the
   // progress UI in data.js is identical on both transports.
   const PHASE_LABELS = ["Decompressing", "Parsing", "Building map data"];

   // The lean-worker handoff only pays off when there's actually a lot of
   // never-shrinking wasm memory to reclaim. Below this high-water mark there's
   // multi-GB of headroom to the 4GB ceiling, so the full-body recompress +
   // fresh-worker reparse would be pure overhead -- skip it. Small/medium saves
   // never hand off; only saves whose parse/edit peak crosses this do. (The
   // streaming payload builder lowered the peak that used to justify handing off
   // after every edit; this gate is what stops that from being wasted work.)
   const HANDOFF_MIN_MEM_BYTES = 1.5e9;

   let worker = null;
   let nextId = 1;
   const pending = new Map(); // id -> {resolve, reject}
   let activeProgress = null; // progress callback of the in-flight load
   // Latest wasm linear-memory high-water seen on a progress event; the handoff
   // gate reads it (the last event of a load/edit carries that op's peak).
   let lastMemBytes = 0;

   // Bumped by every state-changing request (load, edits); an in-flight
   // lean handoff refuses to swap if it changed under it.
   let stateVersion = 0;
   let handoffWorker = null; // lean worker being prepared in the background

   function attachHandlers(w) {
      w.onmessage = (event) => {
         const msg = event.data;
         if (msg.type === "progress") {
            if (msg.memBytes) {
               lastMemBytes = msg.memBytes; // peak high-water for the handoff gate
            }
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
      if (IS_TAURI) {
         return; // no worker, no lean handoff on the desktop transport
      }
      if (handoffWorker) {
         handoffWorker.terminate();
         handoffWorker = null;
      }
   }

   // Kick off a lean-worker swap unless the debug valve disables it or there's
   // little memory to reclaim (see HANDOFF_MIN_MEM_BYTES).
   function scheduleLeanHandoff() {
      if (IS_TAURI) {
         return; // native memory frees on its own; nothing to hand off
      }
      if (new URLSearchParams(location.search).get("noLean") === "1") {
         return;
      }
      if (lastMemBytes && lastMemBytes < HANDOFF_MIN_MEM_BYTES) {
         return; // plenty of headroom; the handoff would cost more than it saves
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

   // ---- Tauri transport -------------------------------------------------------
   // Maps the worker RPC ops to invoke("<command>", args). Load progress rides
   // a Tauri Channel that calls the same activeProgress the worker path uses.
   function forwardTauriProgress(m) {
      if (activeProgress) {
         activeProgress(PHASE_LABELS[m.phase] || "Loading", m.current, m.total, 0, 0);
      }
   }

   // Tauri's invoke() rejects with the Err string. Turn it back into an Error
   // and lift the SESSION_LOST: marker into error.sessionLost (editor.js reads
   // it to decide whether to recover by reloading).
   function rethrowTauriError(error) {
      const message = String((error && error.message) || error);
      const lost = message.indexOf("SESSION_LOST:") === 0;
      const err = new Error(lost ? message.slice("SESSION_LOST:".length) : message);
      err.sessionLost = lost;
      throw err;
   }

   function tauriDispatch(msg) {
      const core = window.__TAURI__.core;
      const invoke = core.invoke;
      switch (msg.op) {
         case "load": {
            const channel = new core.Channel();
            channel.onmessage = forwardTauriProgress;
            return invoke("load", { path: msg.path, onProgress: channel })
               .then((bytes) => new Uint8Array(bytes));
         }
         case "applyEdits": {
            const channel = new core.Channel();
            channel.onmessage = forwardTauriProgress;
            return invoke("apply_edits", {
               ops: JSON.stringify(msg.ops),
               fromPristine: !!msg.fromPristine,
               onProgress: channel,
            }).then((bytes) => new Uint8Array(bytes));
         }
         case "exportSave":
            return invoke("export_save").then((bytes) => new Uint8Array(bytes));
         case "extractClipboard":
            return invoke("extract_clipboard", {
               names: msg.names,
               lightweight: JSON.stringify(msg.lightweight),
            });
         case "describeInstance":
            return invoke("describe_instance", { name: msg.name }).then(JSON.parse);
         case "findItem":
            return invoke("find_item", { item: msg.item }).then(JSON.parse);
         case "buildingInfo":
            return invoke("building_info", { types: msg.types }).then(JSON.parse);
         case "vehicleInfo":
            return invoke("vehicle_info", { types: msg.types }).then(JSON.parse);
         case "trainInfo":
            return invoke("train_info").then(JSON.parse);
         case "selectionInventory":
            // The Flask endpoint wrapped the list as {items:[...]}; selection.js
            // still expects that shape (same as the worker path).
            return invoke("selection_inventory", { names: msg.names })
               .then((raw) => ({ items: JSON.parse(raw) }));
         case "memStats":
            return invoke("mem_stats");
         case "reset":
            return invoke("reset");
         default:
            return Promise.reject(new Error("Unknown op: " + msg.op));
      }
   }

   function tauriRequest(msg) {
      return tauriDispatch(msg).catch(rethrowTauriError);
   }

   // ---- Chunked payload (Tauri) ----------------------------------------------
   // A payload too big for one IPC response (WebView2 truncates very large
   // bodies, and V8 caps strings at ~512MB) arrives as a small marker object
   // listing byte ranges of the payload stashed on the Rust side. Each range
   // wraps to one parseable JSON object; pull its bytes in slices, parse, and
   // merge. Normal-size payloads never hit this path.
   const PAYLOAD_SLICE_BYTES = 96 * 1024 * 1024;

   async function resolveChunkedPayload(parsed) {
      const spec = parsed && parsed.__chunkedPayload;
      if (!spec) {
         return parsed;
      }
      const invoke = window.__TAURI__.core.invoke;
      const out = {};
      for (const range of spec.ranges) {
         const start = range[0];
         const len = range[1];
         const buf = new Uint8Array(len + 2);
         buf[0] = 0x7b; // '{'
         let got = 0;
         while (got < len) {
            const n = Math.min(PAYLOAD_SLICE_BYTES, len - got);
            const part = new Uint8Array(
               await invoke("payload_slice", { offset: start + got, len: n }),
            );
            if (part.length === 0) {
               throw new Error("Empty payload slice");
            }
            buf.set(part, 1 + got);
            got += part.length;
         }
         buf[len + 1] = 0x7d; // '}'
         Object.assign(out, JSON.parse(new TextDecoder().decode(buf)));
      }
      await invoke("payload_done").catch(() => {});
      return out;
   }

   // Payload bytes -> payload object, transport-agnostic: the worker path is a
   // plain parse; the Tauri path may indirect through the chunk protocol.
   function parsePayloadBytes(payloadBytes) {
      const parsed = JSON.parse(new TextDecoder().decode(payloadBytes));
      return IS_TAURI ? resolveChunkedPayload(parsed) : Promise.resolve(parsed);
   }

   function request(msg, transfer) {
      if (IS_TAURI) {
         return tauriRequest(msg);
      }
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
         // ?noLean=1 skips the lean-worker handoff (debug/A-B).
         return request({ op: "load", buffer }, [buffer]).then((payloadBytes) => {
            activeProgress = null;
            scheduleLeanHandoff();
            return parsePayloadBytes(payloadBytes);
         }, (error) => {
            activeProgress = null;
            throw error;
         });
      },
      // loadSavePath(path, onProgress) -> Promise<payload object>. Desktop
      // (Tauri) only: sav_core reads the .sav natively from the path, so a
      // 200MB file is never marshaled through the IPC boundary as a buffer.
      loadSavePath(path, onProgress) {
         activeProgress = onProgress || null;
         stateVersion++;
         return request({ op: "load", path }).then((payloadBytes) => {
            activeProgress = null;
            return parsePayloadBytes(payloadBytes);
         }, (error) => {
            activeProgress = null;
            throw error;
         });
      },
      // True when running inside the native desktop shell (path-based load,
      // native file dialog) rather than the browser (File/ArrayBuffer).
      isTauri() {
         return IS_TAURI;
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
            return parsePayloadBytes(payloadBytes);
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
      // extractClipboard(names, lightweight) -> Promise<string>: the
      // cross-save clipboard blob JSON for the given edit targets.
      extractClipboard(names, lightweight) {
         return request({ op: "extractClipboard", names, lightweight });
      },
      dispose() {
         abortHandoff();
         if (IS_TAURI) {
            return; // session lives for the app's lifetime; nothing to free
         }
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
         if (IS_TAURI) {
            request({ op: "reset" }).catch(function() {});
            return;
         }
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
