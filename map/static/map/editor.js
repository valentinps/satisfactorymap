// EditorTool: save-editing state and UI. The uploaded file is never
// modified; edits happen on the parsed body inside the WASM worker
// (SaveClient.applyEdits re-parses and returns a fresh payload) and
// "Download save" exports a new .sav (SaveClient.exportSave).
//
// Edits are kept as a list of "actions" (each action = the ops array of one
// applyEdits call). Undo pops the last action and replays the rest from the
// pristine body inside the worker (fromPristine=true), so undo is always
// byte-exact regardless of what the action touched.
var EditorTool = (function() {
  "use strict";

  // ---- Coordinate conversion ------------------------------------------------
  // Mirrors rust_parser/core/src/mapdata/geometry.rs (project_xy and its
  // constants): px = ((x/SCALE + OFF)/DESCALE - CROP_LO) * TO_HIGHRES, and
  // map Y is flipped (mapY = MAP_SIZE - py). Deltas therefore scale by
  // PIXELS_PER_WORLD_UNIT with a Y sign flip.
  var MAP_SIZE = 8192;
  var SCALE = 22.887;
  var OFF_X = 18282.5, OFF_Y = 20480.0;
  var DESCALE = 20;
  var CROP_LO = 4096 / DESCALE;               // 204.8
  var TO_HIGHRES = MAP_SIZE / (36864 / DESCALE - CROP_LO); // 5
  var PIXELS_PER_WORLD_UNIT = (1 / SCALE / DESCALE) * TO_HIGHRES;
  var CM_PER_PIXEL = 1 / PIXELS_PER_WORLD_UNIT; // ~91.55 cm per map pixel

  function mapPxToWorldXY(mapX, mapY) {
    var wx = ((mapX / TO_HIGHRES + CROP_LO) * DESCALE - OFF_X) * SCALE;
    var wy = (((MAP_SIZE - mapY) / TO_HIGHRES + CROP_LO) * DESCALE - OFF_Y) * SCALE;
    return [wx, wy];
  }

  function worldToMapPx(wx, wy) {
    var px = ((wx / SCALE + OFF_X) / DESCALE - CROP_LO) * TO_HIGHRES;
    var py = MAP_SIZE - ((wy / SCALE + OFF_Y) / DESCALE - CROP_LO) * TO_HIGHRES;
    return [px, py];
  }

  // Map-pixel delta -> world-cm delta (Y flipped).
  function mapDeltaToWorld(dPxX, dPxY) {
    return [dPxX * CM_PER_PIXEL, -dPxY * CM_PER_PIXEL];
  }

  // ---- State ------------------------------------------------------------------
  var actions = [];      // committed actions, each an array of edit ops
  var redoStack = [];    // actions undone, most recent last
  var applyInFlight = false;
  var currentFileName = null;

  var downloadBtn, toolbar, editCountEl, undoBtn, redoBtn;
  var hintBar;
  // Ghost = an L.rectangle in Leaflet's overlay pane rather than a
  // container-positioned div: the pane transform carries it through pans AND
  // the wheel-zoom CSS animation, so it never desyncs mid-animation.
  var ghostRect = null;
  var offsetOverlay, offsetDx, offsetDy, offsetDz, offsetRot, offsetApply, offsetCancel;
  var pastePanel, pastePanelTitle, pastePosOriginal, pastePosCustom;
  var pasteX, pasteY, pasteZ, pasteDx, pasteDy, pasteDz, pasteRot, pasteResult;
  var pastePanelApplyBtn, pastePanelCancelBtn;

  // Active ghost placement, or null: { mode: "move"|"paste"|"external",
  // targets, anchor: {x, y} (map px), bbox: {minX..maxY} (map px), rotSteps }.
  // Paste modes add the paste-panel state: anchorWorld {x,y} (cm, where the
  // copy came from), posMode "original"|"custom", customWorld {x,y} (cm).
  var placement = null;
  var offsetTargets = null; // targets of the open offset dialog
  var clipboard = null;     // editTargets captured by Copy

  // ---- Targets ---------------------------------------------------------------

  var LIGHTWEIGHT_ID_PREFIX = "LightweightBuildable:";

  function isEditableBucket(bucket) {
    return bucket.key.indexOf("building:") === 0
      || bucket.key.indexOf("line:belt:") === 0
      || bucket.key.indexOf("line:pipe:") === 0
      || bucket.key === "line:railroads"
      || bucket.key === "line:hypertubes"
      || bucket.key.indexOf("line:vehiclePath:") === 0;
  }

  function parseLightweightId(id) {
    var sep = id.lastIndexOf(":");
    return {
      typePath: id.slice(LIGHTWEIGHT_ID_PREFIX.length, sep),
      index: parseInt(id.slice(sep + 1), 10),
    };
  }

  // A single context-menu hit -> editTargets (same shape selection.js
  // builds), or null when the object isn't editable.
  function targetsFromHit(hit) {
    if (!hit || !hit.id || !isEditableBucket(hit.bucket)) {
      return null;
    }
    var x, y;
    if (hit.bucket.renderType === "line") {
      var line = hit.bucket.lines[hit.index];
      x = line[0];
      y = line[1];
    } else {
      var stride = hit.bucket.pointStride;
      x = hit.bucket.points[hit.index * stride];
      y = hit.bucket.points[hit.index * stride + 1];
    }
    // Altitude (meters) from the bucket's stride-1 slot, same convention as
    // selection.js's aggregate -- feeds the paste panel's Z field.
    var stride = hit.bucket.pointStride;
    var z;
    if (hit.bucket.renderType === "line") {
      var ln = hit.bucket.lines[hit.index];
      z = ln ? ln[stride - 1] : undefined;
    } else {
      z = hit.bucket.points[hit.index * stride + stride - 1];
    }
    var hasZ = typeof z === "number";
    var targets = {
      actorNames: [],
      lightweight: [],
      skipped: 0,
      bbox: { minX: x, minY: y, maxX: x, maxY: y,
              minZ: hasZ ? z : Infinity, maxZ: hasZ ? z : -Infinity },
    };
    if (hit.id.indexOf(LIGHTWEIGHT_ID_PREFIX) === 0) {
      targets.lightweight.push(parseLightweightId(hit.id));
    } else {
      targets.actorNames.push(hit.id);
    }
    return targets;
  }

  // ---- Building + applying ops -------------------------------------------------

  function buildMoveOps(targets, deltaWorld, rotDeg, pivotWorld) {
    var ops = [];
    if (targets.actorNames.length) {
      ops.push({
        op: "moveActors",
        names: targets.actorNames,
        delta: deltaWorld,
        rotateYawDeg: rotDeg,
        pivot: rotDeg ? pivotWorld : null,
      });
    }
    if (targets.lightweight.length) {
      ops.push({
        op: "moveLightweight",
        items: targets.lightweight,
        delta: deltaWorld,
        rotateYawDeg: rotDeg,
        pivot: rotDeg ? pivotWorld : null,
      });
    }
    return ops;
  }

  function buildDuplicateOps(targets, deltaWorld, rotDeg, pivotWorld) {
    // The seed is stored inside the op, so undo/redo replays regenerate the
    // exact same instance names.
    var seed = Math.floor(Math.random() * 0x1fffffffffffff);
    var ops = [];
    if (targets.actorNames.length) {
      ops.push({
        op: "duplicateActors",
        names: targets.actorNames,
        delta: deltaWorld,
        rotateYawDeg: rotDeg,
        pivot: rotDeg ? pivotWorld : null,
        seed: seed,
      });
    }
    if (targets.lightweight.length) {
      ops.push({
        op: "duplicateLightweight",
        items: targets.lightweight,
        delta: deltaWorld,
        rotateYawDeg: rotDeg,
        pivot: rotDeg ? pivotWorld : null,
      });
    }
    return ops;
  }

  // Busy-overlay label for an action, from the ops themselves: verb from
  // the op types (one action never mixes verbs), count from the target
  // lists. pasteExternal v2 blobs carry their objects compressed, so the
  // count can be 0 -- the label just drops it then.
  function describeOps(ops) {
    var verb = "Applying edit";
    var count = 0;
    ops.forEach(function(op) {
      var c = (op.names ? op.names.length : 0) + (op.items ? op.items.length : 0)
        + (op.actors ? op.actors.length : 0) + (op.lightweight ? op.lightweight.length : 0);
      // pasteExternal ops carry their objects compressed (or native-side);
      // their explicit count fills the label in.
      count += c > 0 ? c : (op.count || 0);
      if (op.op.indexOf("delete") === 0) {
        verb = "Deleting";
      } else if (op.op.indexOf("duplicate") === 0 || op.op === "pasteExternal") {
        verb = "Pasting";
      } else if (op.op.indexOf("move") === 0) {
        verb = "Moving";
      }
    });
    return count > 0
      ? verb + " " + count.toLocaleString() + " object" + (count === 1 ? "" : "s") + "…"
      : verb + "…";
  }

  // One progress callback shape for every applyEdits call: the thin top bar
  // (legacy) plus the modal busy overlay.
  function editProgress(phase, current, total) {
    var percent = total > 0 ? (current / total) * 100 : 0;
    SaveLoadFlow.showProgress(phase, percent);
    SaveLoadFlow.busyProgress(phase, percent);
  }

  function applyAction(ops) {
    if (applyInFlight || ops.length === 0) {
      return;
    }
    applyInFlight = true;
    SaveLoadFlow.showBusy(describeOps(ops));
    SaveClient.applyEdits(ops, false, editProgress)
      .then(function(payload) {
        actions.push(ops);
        redoStack = [];
        finishApply(payload, "Edit applied.");
      })
      .catch(failApply);
  }

  // Replace the whole edit list (undo replay from pristine).
  function replayActions(newActions, onSuccess) {
    if (applyInFlight) {
      return;
    }
    applyInFlight = true;
    SaveLoadFlow.showBusy("Undoing…");
    var flat = [];
    newActions.forEach(function(a) { flat.push.apply(flat, a); });
    SaveClient.applyEdits(flat, true, editProgress)
      .then(function(payload) {
        onSuccess();
        finishApply(payload, "Edit undone.");
      })
      .catch(failApply);
  }

  function finishApply(payload, statusText) {
    applyInFlight = false;
    SaveLoadFlow.hideProgress();
    SaveLoadFlow.hideBusy();
    SaveLoadFlow.applyPayload(payload);
    SaveLoadFlow.setStatus(statusText + " (" + actions.length + " edit" + (actions.length === 1 ? "" : "s") + " pending)");
    updateToolbar();
  }

  function failApply(error) {
    var message = "Edit failed: " + (error && error.message || error);
    // Semantic refusals (uneditable object, unknown name, ...) leave the
    // session intact: just report them.
    if (!error || !error.sessionLost) {
      applyInFlight = false;
      SaveLoadFlow.hideProgress();
      SaveLoadFlow.hideBusy();
      SaveLoadFlow.setStatus(message);
      updateToolbar();
      return;
    }
    // The wasm session is gone (out-of-memory trap on a huge save). Recover
    // with a fresh worker: reload the original file, then replay the
    // committed actions one at a time.
    recoverSession(message);
  }

  var recovering = false;

  function recoverSession(message) {
    if (recovering || !SaveLoadFlow.canReload()) {
      recovering = false;
      applyInFlight = false;
      SaveLoadFlow.hideProgress();
      SaveLoadFlow.hideBusy();
      SaveLoadFlow.setStatus(message + " — please re-load the save file (pending edits were lost).");
      actions = [];
      redoStack = [];
      updateToolbar();
      return;
    }
    recovering = true;
    applyInFlight = false;
    SaveLoadFlow.showBusy("Recovering — reloading save…");
    SaveLoadFlow.setStatus(message + " — recovering (reloading save)…");
    var backup = actions.slice();
    var savedClipboard = clipboard; // survives the reload: same save, same names
    SaveClient.reset();
    SaveLoadFlow.reloadCurrentFile() // resets EditorTool via onSaveLoaded
      .then(function() {
        clipboard = savedClipboard;
        return replaySequentially(backup, 0);
      })
      .then(function() {
        recovering = false;
        SaveLoadFlow.hideBusy();
        SaveLoadFlow.setStatus(message + " — recovered; your " + actions.length
          + " earlier edit" + (actions.length === 1 ? " was" : "s were") + " re-applied.");
        updateToolbar();
      })
      .catch(function(replayError) {
        recovering = false;
        applyInFlight = false;
        SaveLoadFlow.hideProgress();
        SaveLoadFlow.hideBusy();
        SaveLoadFlow.setStatus(message + " — recovery incomplete ("
          + (replayError && replayError.message || replayError)
          + "); " + actions.length + " of " + backup.length + " edits were restored.");
        updateToolbar();
      });
  }

  function replaySequentially(backup, i) {
    if (i >= backup.length) {
      return Promise.resolve();
    }
    SaveLoadFlow.showBusy("Recovering — re-applying edit " + (i + 1) + " of " + backup.length + "…");
    return SaveClient.applyEdits(backup[i], false, editProgress).then(function(payload) {
      SaveLoadFlow.hideProgress();
      SaveLoadFlow.applyPayload(payload);
      actions.push(backup[i]);
      updateToolbar();
      return replaySequentially(backup, i + 1);
    });
  }

  // ---- Toolbar (edit count / undo / redo) ---------------------------------------

  // The top bar centers the search box within a flex row whose sides are
  // unequal (menu+logo left, status buttons + altitude padding right), so
  // "centered" for top notifications means the SEARCH BOX's center, not the
  // viewport's -- align to it directly.
  function alignToolbar() {
    var searchBox = document.getElementById("searchBox");
    if (!searchBox || !toolbar) {
      return;
    }
    var box = searchBox.getBoundingClientRect();
    if (box.width > 0) {
      toolbar.style.left = (box.left + box.width / 2) + "px";
    }
  }

  function updateToolbar() {
    var any = actions.length > 0 || redoStack.length > 0;
    toolbar.style.display = any ? "flex" : "none";
    alignToolbar();
    editCountEl.textContent = actions.length + " edit" + (actions.length === 1 ? "" : "s");
    undoBtn.disabled = applyInFlight || actions.length === 0;
    redoBtn.disabled = applyInFlight || redoStack.length === 0;
    downloadBtn.classList.toggle("edited", actions.length > 0);
  }

  function undo() {
    if (actions.length === 0 || applyInFlight) {
      return;
    }
    var popped = actions[actions.length - 1];
    replayActions(actions.slice(0, -1), function() {
      actions.pop();
      redoStack.push(popped);
    });
  }

  function redo() {
    if (redoStack.length === 0 || applyInFlight) {
      return;
    }
    var ops = redoStack[redoStack.length - 1];
    applyInFlight = true;
    SaveLoadFlow.showBusy("Redoing…");
    SaveClient.applyEdits(ops, false, editProgress)
      .then(function(payload) {
        redoStack.pop();
        actions.push(ops);
        finishApply(payload, "Edit redone.");
      })
      .catch(failApply);
  }

  // ---- Ghost placement ------------------------------------------------------------

  function startPlacement(mode, targets) {
    var external = mode === "external";
    if (!targets || applyInFlight
        || (!external && (targets.actorNames.length + targets.lightweight.length) === 0)) {
      return;
    }
    cancelPlacement();
    var b = targets.bbox;
    placement = {
      mode: mode,
      targets: targets,
      anchor: { x: (b.minX + b.maxX) / 2, y: (b.minY + b.maxY) / 2 },
      bbox: b,
      rotSteps: 0,
      cursor: null,
    };
    document.getElementById("map").style.cursor = "crosshair";
    ghostRect = L.rectangle(
      [[placement.anchor.y - 4, placement.anchor.x - 4], [placement.anchor.y + 4, placement.anchor.x + 4]],
      { color: "#ffb454", weight: 2, dashArray: "6 4", fillColor: "#ffb454",
        fillOpacity: 0.12, interactive: false },
    ).addTo(MapApp.map);
    MapApp.map.on("click", onPlacementClick);
    if (mode === "move") {
      hintBar.textContent = "Click to place · R rotate 90° · Esc cancel";
      hintBar.style.display = "block";
      MapApp.map.on("mousemove", onPlacementMouseMove);
      return;
    }
    // Paste modes: the ghost is parked at the panel's effective position
    // (original location at first), not chasing the cursor; a map click just
    // fills the custom position, the panel's Paste button commits.
    // anchorWorld.z is the selection's altitude center in cm, or null when
    // unknown (old external blobs) -- the Z field disables then.
    var anchorWorld;
    if (external) {
      anchorWorld = {
        x: targets.external.anchor[0],
        y: targets.external.anchor[1],
        z: typeof targets.external.anchorZ === "number" ? targets.external.anchorZ : null,
      };
    } else {
      var aw = mapPxToWorldXY(placement.anchor.x, placement.anchor.y);
      var zKnown = isFinite(b.minZ) && isFinite(b.maxZ);
      anchorWorld = {
        x: aw[0],
        y: aw[1],
        // bbox z is altitude METERS (bucket convention); world cm is x100.
        z: zKnown ? ((b.minZ + b.maxZ) / 2) * 100 : null,
      };
    }
    placement.anchorWorld = anchorWorld;
    placement.posMode = "original";
    placement.customWorld = { x: anchorWorld.x, y: anchorWorld.y, z: anchorWorld.z };
    hintBar.textContent = "Click the map to pick a position · Enter or Paste applies · R rotate 90° · Esc cancel";
    hintBar.style.display = "block";
    openPastePanel();
    positionGhost();
  }

  function startMove(targets) {
    startPlacement("move", targets);
  }

  function copyTargets(targets) {
    if (!targets || (targets.actorNames.length + targets.lightweight.length) === 0) {
      return;
    }
    clipboard = targets;
    var n = targets.actorNames.length + targets.lightweight.length;
    SaveLoadFlow.setStatus("Copied " + n.toLocaleString() + " object" + (n === 1 ? "" : "s")
      + " — Ctrl+V or right-click to paste.");
    // Also put a portable blob (raw object bytes + version metadata) on the
    // OS clipboard so another tab -- even another save -- can paste it.
    // Extracting 100k+ objects takes a noticeable moment in the worker, so
    // it runs under the busy overlay -- otherwise a big Copy looks like
    // nothing happened until the status line quietly changes.
    if (window.__TAURI__ || (navigator.clipboard && navigator.clipboard.writeText)) {
      SaveLoadFlow.showBusy("Copying " + n.toLocaleString() + " object" + (n === 1 ? "" : "s") + "…");
      SaveClient.extractClipboard(targets.actorNames, targets.lightweight)
        .then(function(json) {
          // Same 200MB ceiling resolvePaste enforces: writing a blob the
          // paste side would refuse anyway just moves the confusion later.
          if (json.length > 200e6) {
            throw new Error("too many objects for the browser clipboard — use the desktop app");
          }
          // Desktop: write native-side, off WebView2's permission-gated
          // clipboard API.
          return window.__TAURI__
            ? SaveClient.writeClipboardText(json)
            : navigator.clipboard.writeText(json);
        })
        .then(function() {
          SaveLoadFlow.hideBusy();
          SaveLoadFlow.setStatus("Copied " + n.toLocaleString() + " object" + (n === 1 ? "" : "s")
            + " — paste with Ctrl+V here or in another tab.");
        })
        .catch(function(error) {
          SaveLoadFlow.hideBusy();
          console.warn("System-clipboard copy failed (same-tab paste still works):", error);
          SaveLoadFlow.setStatus("Copied " + n.toLocaleString() + " object" + (n === 1 ? "" : "s")
            + " — cross-tab copy failed (" + ((error && error.message) || error)
            + "); paste works in this tab only.");
        });
    }
  }

  // OS-clipboard text a paste could use, or null. Desktop reads native-side:
  // WebView2's navigator.clipboard.readText pops a permission prompt on
  // every Ctrl+V (and big cross-app blobs come back as a slot pointer).
  function readOsClipboardText() {
    if (window.__TAURI__) {
      return SaveClient.readPasteBlob().catch(function() { return null; });
    }
    if (!navigator.clipboard || !navigator.clipboard.readText) {
      return Promise.resolve(null);
    }
    return navigator.clipboard.readText().catch(function() {
      return null; // permission denied / unfocused: same-tab paste only
    });
  }

  // What a paste would use right now: the in-tab clipboard when set, else a
  // cross-tab blob from the OS clipboard (written by copyTargets in any tab).
  function resolvePaste() {
    if (clipboard) {
      return Promise.resolve({ mode: "internal" });
    }
    return readOsClipboardText().then(function(text) {
      if (!text || text.length > 200e6 || text.indexOf("\"smapPaste\"") === -1) {
        return null;
      }
      var blob;
      try {
        blob = JSON.parse(text);
      } catch (error) {
        return null;
      }
      if (!blob || (blob.smapPaste !== 1 && blob.smapPaste !== 2 && blob.smapPaste !== 3)
          || !blob.anchor || !blob.bboxWorld) {
        return null;
      }
      // smapPaste 3 is a desktop-app pointer: the object bytes live in the
      // desktop process, not on the OS clipboard, so only it can paste them.
      if (blob.smapPaste === 3 && !window.__TAURI__) {
        SaveLoadFlow.setStatus("These objects were copied in the desktop app — paste them there"
          + " (too many to travel through the browser clipboard).");
        return null;
      }
      return { mode: "external", blob: blob };
    });
  }

  function startPaste() {
    resolvePaste().then(function(src) {
      if (!src) {
        SaveLoadFlow.setStatus("Nothing to paste — copy something first.");
        return;
      }
      if (src.mode === "internal") {
        startPlacement("paste", clipboard);
        return;
      }
      startExternalPlacement(src.blob);
    });
  }

  // Ghost placement for a cross-tab blob: its bbox/anchor are in WORLD
  // coordinates; convert to map px for the ghost (Y flips, so normalize).
  function startExternalPlacement(blob) {
    var lo = worldToMapPx(blob.bboxWorld[0], blob.bboxWorld[1]);
    var hi = worldToMapPx(blob.bboxWorld[2], blob.bboxWorld[3]);
    var targets = {
      actorNames: [],
      lightweight: [],
      skipped: 0,
      external: blob,
      bbox: {
        minX: Math.min(lo[0], hi[0]),
        minY: Math.min(lo[1], hi[1]),
        maxX: Math.max(lo[0], hi[0]),
        maxY: Math.max(lo[1], hi[1]),
      },
    };
    startPlacement("external", targets);
  }

  function buildExternalOps(blob, targetWorld, rotDeg, dz) {
    var op = {
      op: "pasteExternal",
      anchor: blob.anchor,
      delta: [targetWorld[0] - blob.anchor[0], targetWorld[1] - blob.anchor[1], dz || 0],
      rotateYawDeg: rotDeg,
      seed: Math.floor(Math.random() * 0x1fffffffffffff),
      // Object count for busy/undo labels (the payload itself is opaque
      // here); the edit engine ignores it.
      count: blob.count,
    };
    if (blob.slot != null) {
      // Desktop pointer blob: the actual bytes never left the native side;
      // the Rust command layer splices slot N's stored blob into this op.
      op.slot = blob.slot;
    } else {
      op.saveVersion = blob.saveVersion;
      op.objectVersion = blob.objectVersion;
      op.lightweightVersion = blob.lightweightVersion;
      // v2 blobs carry the payload zlib-compressed; v1 fields pass through.
      op.z = blob.z;
      op.zLen = blob.zLen;
      op.actors = blob.actors || [];
      op.lightweight = blob.lightweight || [];
    }
    return [op];
  }

  // Delete applies immediately -- undo (full replay from pristine) is the
  // safety net, so no confirmation dialog.
  function deleteTargets(targets) {
    if (!targets || (targets.actorNames.length + targets.lightweight.length) === 0 || applyInFlight) {
      return;
    }
    var ops = [];
    if (targets.actorNames.length) {
      ops.push({ op: "deleteActors", names: targets.actorNames });
    }
    if (targets.lightweight.length) {
      ops.push({ op: "deleteLightweight", items: targets.lightweight });
    }
    applyAction(ops);
  }

  // Context menu "Paste here": paste immediately at the given map point
  // without the ghost flow.
  function pasteAt(mapX, mapY) {
    if (applyInFlight) {
      return;
    }
    resolvePaste().then(function(src) {
      if (!src) {
        SaveLoadFlow.setStatus("Nothing to paste — copy something first.");
        return;
      }
      if (src.mode === "internal") {
        var b = clipboard.bbox;
        var anchor = { x: (b.minX + b.maxX) / 2, y: (b.minY + b.maxY) / 2 };
        var deltaXY = mapDeltaToWorld(mapX - anchor.x, mapY - anchor.y);
        applyAction(buildDuplicateOps(clipboard, [deltaXY[0], deltaXY[1], 0], 0, null));
        return;
      }
      applyAction(buildExternalOps(src.blob, mapPxToWorldXY(mapX, mapY), 0));
    });
  }

  function cancelPlacement() {
    if (!placement) {
      return;
    }
    placement = null;
    if (ghostRect) {
      MapApp.map.removeLayer(ghostRect);
      ghostRect = null;
    }
    hintBar.style.display = "none";
    closePastePanel();
    document.getElementById("map").style.cursor = "";
    MapApp.map.off("click", onPlacementClick);
    MapApp.map.off("mousemove", onPlacementMouseMove);
  }

  function onPlacementMouseMove(e) {
    if (!placement) {
      return;
    }
    placement.cursor = e.latlng;
    positionGhost();
  }

  // Where the ghost's center sits right now: the cursor while moving, the
  // panel's effective paste position (base + offsets) while pasting.
  function ghostCenterLatLng() {
    if (placement.mode === "move") {
      return placement.cursor;
    }
    var w = effectivePasteWorld();
    var px = worldToMapPx(w.x, w.y);
    return { lat: px[1], lng: px[0] };
  }

  function positionGhost() {
    if (!placement || !ghostRect) {
      return;
    }
    var c = ghostCenterLatLng();
    if (!c) {
      return;
    }
    // Ghost = the selection bbox centered on the anchor point. Odd 90-degree
    // steps swap the box's width/height. Bounds are in map px ([lat, lng] =
    // [y, x]); Leaflet keeps the rectangle glued through pan/zoom itself.
    var b = placement.bbox;
    var halfW = Math.max((b.maxX - b.minX) / 2, 4);
    var halfH = Math.max((b.maxY - b.minY) / 2, 4);
    if (placement.rotSteps % 2 === 1) {
      var t = halfW; halfW = halfH; halfH = t;
    }
    ghostRect.setBounds([[c.lat - halfH, c.lng - halfW], [c.lat + halfH, c.lng + halfW]]);
  }

  function onPlacementClick(e) {
    if (!placement) {
      return;
    }
    var p = placement;
    if (p.mode !== "move") {
      // Paste modes: a click picks the XY; altitude keeps whatever the Z
      // field holds; the panel's Paste commits.
      var w = mapPxToWorldXY(e.latlng.lng, e.latlng.lat);
      p.posMode = "custom";
      p.customWorld = { x: w[0], y: w[1], z: p.customWorld.z };
      pastePosCustom.checked = true;
      setPasteXYFields(p.customWorld);
      positionGhost();
      refreshPasteResult();
      return;
    }
    var rotDeg = (p.rotSteps * 90) % 360;
    var dPxX = e.latlng.lng - p.anchor.x;
    var dPxY = e.latlng.lat - p.anchor.y;
    var deltaXY = mapDeltaToWorld(dPxX, dPxY);
    var pivot = mapPxToWorldXY(p.anchor.x, p.anchor.y);
    var ops = buildMoveOps(p.targets, [deltaXY[0], deltaXY[1], 0], rotDeg, pivot);
    cancelPlacement();
    applyAction(ops);
  }

  // ---- Paste panel ------------------------------------------------------------------

  // The paste position before offsets, in world cm.
  function pasteBaseWorld() {
    var p = placement;
    return p.posMode === "original" ? p.anchorWorld : p.customWorld;
  }

  // Base + offsets, in world cm (the point the selection's center lands on).
  function effectivePasteWorld() {
    var base = pasteBaseWorld();
    var dx = (parseFloat(pasteDx.value) || 0) * 100;
    var dy = (parseFloat(pasteDy.value) || 0) * 100;
    return { x: base.x + dx, y: base.y + dy };
  }

  function setPasteXYFields(world) {
    pasteX.value = (world.x / 100).toFixed(1);
    pasteY.value = (world.y / 100).toFixed(1);
    // Z stays editable only when the selection's altitude is known (old
    // external blobs don't carry it); the Up/Down offset always works.
    if (world.z != null) {
      pasteZ.disabled = false;
      pasteZ.value = (world.z / 100).toFixed(1);
    } else {
      pasteZ.disabled = true;
      pasteZ.value = "";
    }
  }

  // The altitude delta (cm) the current panel state produces: absolute-Z
  // move (custom mode with a known base) plus the Up/Down offset.
  function pasteZDeltaCm() {
    var p = placement;
    var dz = (parseFloat(pasteDz.value) || 0) * 100;
    if (p.posMode === "custom" && p.customWorld.z != null && p.anchorWorld.z != null) {
      dz += p.customWorld.z - p.anchorWorld.z;
    }
    return dz;
  }

  function refreshPasteResult() {
    if (!placement || placement.mode === "move") {
      return;
    }
    var w = effectivePasteWorld();
    var text = "Center lands at " + Math.round(w.x / 100) + ", " + Math.round(w.y / 100);
    var zDelta = pasteZDeltaCm();
    if (placement.anchorWorld.z != null) {
      text += " · alt " + Math.round((placement.anchorWorld.z + zDelta) / 100) + " m";
    } else if (zDelta) {
      text += " · " + (zDelta > 0 ? "+" : "") + Math.round(zDelta) / 100 + " m height";
    }
    pasteResult.textContent = text;
  }

  function openPastePanel() {
    var p = placement;
    var n = p.mode === "external"
      ? (p.targets.external.count || 0)
      : p.targets.actorNames.length + p.targets.lightweight.length;
    pastePanelTitle.textContent = n > 0
      ? "Paste " + n.toLocaleString() + " object" + (n === 1 ? "" : "s")
      : "Paste";
    pastePosOriginal.checked = true;
    pasteDx.value = "0";
    pasteDy.value = "0";
    pasteDz.value = "0";
    pasteRot.value = "0";
    setPasteXYFields(p.anchorWorld);
    refreshPasteResult();
    pastePanel.style.display = "block";
  }

  function closePastePanel() {
    pastePanel.style.display = "none";
  }

  // Typing X/Y switches to a custom position; the radio switch back to
  // "original" restores the copied coordinates.
  function onPasteXYInput() {
    var p = placement;
    if (!p || p.mode === "move") {
      return;
    }
    p.posMode = "custom";
    pastePosCustom.checked = true;
    p.customWorld = {
      x: (parseFloat(pasteX.value) || 0) * 100,
      y: (parseFloat(pasteY.value) || 0) * 100,
      z: !pasteZ.disabled && pasteZ.value !== "" ? (parseFloat(pasteZ.value) || 0) * 100 : null,
    };
    positionGhost();
    refreshPasteResult();
  }

  function onPastePosModeChange() {
    var p = placement;
    if (!p || p.mode === "move") {
      return;
    }
    p.posMode = pastePosOriginal.checked ? "original" : "custom";
    if (p.posMode === "original") {
      setPasteXYFields(p.anchorWorld);
    }
    positionGhost();
    refreshPasteResult();
  }

  function onPasteAdjustInput() {
    var p = placement;
    if (!p || p.mode === "move") {
      return;
    }
    p.rotSteps = ((parseInt(pasteRot.value, 10) || 0) / 90) % 4;
    positionGhost();
    refreshPasteResult();
  }

  function applyPastePanel() {
    var p = placement;
    if (!p || p.mode === "move" || applyInFlight) {
      return;
    }
    var w = effectivePasteWorld();
    var dz = pasteZDeltaCm();
    var rotDeg = (parseInt(pasteRot.value, 10) || 0) % 360;
    var ops;
    if (p.mode === "external") {
      ops = buildExternalOps(p.targets.external, [w.x, w.y], rotDeg, dz);
    } else {
      var delta = [w.x - p.anchorWorld.x, w.y - p.anchorWorld.y, dz];
      ops = buildDuplicateOps(p.targets, delta, rotDeg, [p.anchorWorld.x, p.anchorWorld.y]);
    }
    cancelPlacement();
    applyAction(ops);
  }

  // ---- Offset dialog (precise moves incl. Z) -----------------------------------------

  function openOffsetDialog(targets) {
    if (!targets || (targets.actorNames.length + targets.lightweight.length) === 0 || applyInFlight) {
      return;
    }
    cancelPlacement();
    offsetTargets = targets;
    offsetDx.value = "0";
    offsetDy.value = "0";
    offsetDz.value = "0";
    offsetRot.value = "0";
    offsetOverlay.style.display = "flex";
    offsetDx.focus();
  }

  function closeOffsetDialog() {
    offsetOverlay.style.display = "none";
    offsetTargets = null;
  }

  function applyOffsetDialog() {
    if (!offsetTargets) {
      return;
    }
    var METERS_TO_CM = 100;
    var dx = (parseFloat(offsetDx.value) || 0) * METERS_TO_CM;
    var dy = (parseFloat(offsetDy.value) || 0) * METERS_TO_CM;
    var dz = (parseFloat(offsetDz.value) || 0) * METERS_TO_CM;
    var rot = parseInt(offsetRot.value, 10) || 0;
    var b = offsetTargets.bbox;
    var pivot = mapPxToWorldXY((b.minX + b.maxX) / 2, (b.minY + b.maxY) / 2);
    var ops = buildMoveOps(offsetTargets, [dx, dy, dz], rot, pivot);
    closeOffsetDialog();
    applyAction(ops);
  }

  // ---- Download ---------------------------------------------------------------------

  var exportInFlight = false;

  function downloadName() {
    var base = currentFileName || "save.sav";
    return base.replace(/\.sav$/i, "") + "_edited.sav";
  }

  function exportSave() {
    if (exportInFlight || !MapApp.currentFile) {
      return;
    }
    exportInFlight = true;
    downloadBtn.disabled = true;
    downloadBtn.textContent = "Exporting…";
    SaveLoadFlow.showBusy("Exporting save…");
    // Desktop: native Save-as dialog, written to disk from the Rust side.
    // The browser-style anchor click below would hand WebView2 an invisible
    // silent download into the Downloads folder.
    var job = window.__TAURI__
      ? SaveClient.exportSaveDialog(downloadName()).then(function(path) {
          if (path) {
            SaveLoadFlow.setStatus("Saved " + path);
          }
        })
      : SaveClient.exportSave().then(function(bytes) {
          var blob = new Blob([bytes], { type: "application/octet-stream" });
          var url = URL.createObjectURL(blob);
          var a = document.createElement("a");
          a.href = url;
          a.download = downloadName();
          document.body.appendChild(a);
          a.click();
          a.remove();
          // Give the click a tick to start the download before revoking.
          setTimeout(function() { URL.revokeObjectURL(url); }, 5000);
        });
    job
      .catch(function(error) {
        SaveLoadFlow.setStatus("Failed to export save: " + (error && error.message || error));
      })
      .then(function() {
        exportInFlight = false;
        downloadBtn.disabled = false;
        downloadBtn.textContent = "Download save";
        SaveLoadFlow.hideBusy();
      });
  }

  // ---- Wiring -----------------------------------------------------------------------

  document.addEventListener("DOMContentLoaded", function() {
    downloadBtn = document.getElementById("downloadSaveBtn");
    toolbar = document.getElementById("editorToolbar");
    editCountEl = document.getElementById("editorEditCount");
    undoBtn = document.getElementById("editorUndoBtn");
    redoBtn = document.getElementById("editorRedoBtn");
    hintBar = document.getElementById("editorHint");
    offsetOverlay = document.getElementById("offsetDialogOverlay");
    offsetDx = document.getElementById("offsetDx");
    offsetDy = document.getElementById("offsetDy");
    offsetDz = document.getElementById("offsetDz");
    offsetRot = document.getElementById("offsetRot");
    offsetApply = document.getElementById("offsetApplyBtn");
    offsetCancel = document.getElementById("offsetCancelBtn");
    pastePanel = document.getElementById("pastePanel");
    pastePanelTitle = document.getElementById("pastePanelTitle");
    pastePosOriginal = document.getElementById("pastePosOriginal");
    pastePosCustom = document.getElementById("pastePosCustom");
    pasteX = document.getElementById("pasteX");
    pasteY = document.getElementById("pasteY");
    pasteZ = document.getElementById("pasteZ");
    pasteDx = document.getElementById("pasteDx");
    pasteDy = document.getElementById("pasteDy");
    pasteDz = document.getElementById("pasteDz");
    pasteRot = document.getElementById("pasteRot");
    pasteResult = document.getElementById("pasteResult");
    pastePanelApplyBtn = document.getElementById("pastePanelApplyBtn");
    pastePanelCancelBtn = document.getElementById("pastePanelCancelBtn");

    downloadBtn.addEventListener("click", exportSave);
    undoBtn.addEventListener("click", undo);
    redoBtn.addEventListener("click", redo);
    offsetApply.addEventListener("click", applyOffsetDialog);
    offsetCancel.addEventListener("click", closeOffsetDialog);
    pastePanelApplyBtn.addEventListener("click", applyPastePanel);
    pastePanelCancelBtn.addEventListener("click", cancelPlacement);
    pastePosOriginal.addEventListener("change", onPastePosModeChange);
    pastePosCustom.addEventListener("change", onPastePosModeChange);
    pasteX.addEventListener("input", onPasteXYInput);
    pasteY.addEventListener("input", onPasteXYInput);
    pasteZ.addEventListener("input", onPasteXYInput);
    pasteDx.addEventListener("input", onPasteAdjustInput);
    pasteDy.addEventListener("input", onPasteAdjustInput);
    pasteDz.addEventListener("input", onPasteAdjustInput);
    pasteRot.addEventListener("change", onPasteAdjustInput);
    pastePanel.addEventListener("keydown", function(e) {
      if (e.key === "Enter") {
        applyPastePanel();
        e.preventDefault();
      }
    });
    offsetOverlay.addEventListener("click", function(e) {
      if (e.target === offsetOverlay) {
        closeOffsetDialog();
      }
    });
    offsetOverlay.addEventListener("keydown", function(e) {
      if (e.key === "Enter") {
        applyOffsetDialog();
      }
    });

    document.addEventListener("keydown", function(e) {
      var inInput = e.target && (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA");
      if (e.key === "Escape") {
        cancelPlacement();
        closeOffsetDialog();
        return;
      }
      if (placement && !inInput && (e.key === "r" || e.key === "R")) {
        placement.rotSteps = (placement.rotSteps + 1) % 4;
        if (placement.mode !== "move") {
          pasteRot.value = String((placement.rotSteps * 90) % 360);
          refreshPasteResult();
        }
        positionGhost();
        e.preventDefault();
        return;
      }
      // Ctrl+Z / Ctrl+Y / Ctrl+V outside of text inputs.
      if (!inInput && (e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z") {
        undo();
        e.preventDefault();
      } else if (!inInput && (e.ctrlKey || e.metaKey)
                 && (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z"))) {
        redo();
        e.preventDefault();
      } else if (!inInput && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c") {
        // Don't hijack a real text copy.
        if (window.getSelection && String(window.getSelection())) {
          return;
        }
        var targets = window.SelectionTool && SelectionTool.currentEditTargets
          ? SelectionTool.currentEditTargets()
          : null;
        if (targets && (targets.actorNames.length + targets.lightweight.length) > 0) {
          copyTargets(targets);
          e.preventDefault();
        }
      } else if (!inInput && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        startPaste();
        e.preventDefault();
      }
    });

    window.addEventListener("beforeunload", function(e) {
      if (actions.length > 0) {
        e.preventDefault();
        e.returnValue = "";
      }
    });
    window.addEventListener("resize", alignToolbar);
  });

  return {
    // Called by data.js after every successful load: edits belong to the
    // previous save, so drop everything.
    onSaveLoaded(fileName) {
      currentFileName = fileName;
      actions = [];
      redoStack = [];
      clipboard = null;
      cancelPlacement();
      closeOffsetDialog();
      downloadBtn.style.display = "";
      updateToolbar();
    },
    startMove: startMove,
    openOffsetDialog: openOffsetDialog,
    targetsFromHit: targetsFromHit,
    copyTargets: copyTargets,
    startPaste: startPaste,
    pasteAt: pasteAt,
    deleteTargets: deleteTargets,
    hasClipboard: function() { return clipboard !== null; },
    undo: undo,
    redo: redo,
    opCount: function() { return actions.length; },
    isPlacing: function() { return placement !== null; },
    // filters.js derives tooltip world coordinates from bucket points with
    // this since the payload no longer ships worldPositions arrays (see
    // slim_payload_value in mapdata/mod.rs).
    mapPxToWorldXY: mapPxToWorldXY,
  };
})();
