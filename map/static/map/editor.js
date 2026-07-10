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
  var ghost, hintBar;
  var offsetOverlay, offsetDx, offsetDy, offsetDz, offsetRot, offsetApply, offsetCancel;

  // Active ghost placement, or null: { mode: "move"|"paste", targets,
  // anchor: {x, y} (map px), bbox: {minX..maxY} (map px), rotSteps }
  var placement = null;
  var offsetTargets = null; // targets of the open offset dialog
  var clipboard = null;     // editTargets captured by Copy

  // ---- Targets ---------------------------------------------------------------

  var LIGHTWEIGHT_ID_PREFIX = "LightweightBuildable:";

  function isEditableBucket(bucket) {
    return bucket.key.indexOf("building:") === 0
      || bucket.key.indexOf("line:belt:") === 0
      || bucket.key.indexOf("line:pipe:") === 0;
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
    var targets = {
      actorNames: [],
      lightweight: [],
      skipped: 0,
      bbox: { minX: x, minY: y, maxX: x, maxY: y },
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

  function applyAction(ops) {
    if (applyInFlight || ops.length === 0) {
      return;
    }
    applyInFlight = true;
    SaveClient.applyEdits(ops, false, function(phase, current, total) {
      var percent = total > 0 ? (current / total) * 100 : 0;
      SaveLoadFlow.showProgress(phase, percent);
    })
      .then(function(payload) {
        actions.push(ops);
        redoStack = [];
        finishApply(payload, "Edit applied.");
      })
      .catch(failApply);
  }

  // Replace the whole edit list (undo/redo replay from pristine).
  function replayActions(newActions, onSuccess) {
    if (applyInFlight) {
      return;
    }
    applyInFlight = true;
    var flat = [];
    newActions.forEach(function(a) { flat.push.apply(flat, a); });
    SaveClient.applyEdits(flat, true, function(phase, current, total) {
      var percent = total > 0 ? (current / total) * 100 : 0;
      SaveLoadFlow.showProgress(phase, percent);
    })
      .then(function(payload) {
        onSuccess();
        finishApply(payload, "Edit undone.");
      })
      .catch(failApply);
  }

  function finishApply(payload, statusText) {
    applyInFlight = false;
    SaveLoadFlow.hideProgress();
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
      SaveLoadFlow.setStatus(message + " — please re-load the save file (pending edits were lost).");
      actions = [];
      redoStack = [];
      updateToolbar();
      return;
    }
    recovering = true;
    applyInFlight = false;
    SaveLoadFlow.setStatus(message + " — recovering (reloading save)…");
    var backup = actions.slice();
    SaveClient.reset();
    SaveLoadFlow.reloadCurrentFile() // resets EditorTool via onSaveLoaded
      .then(function() {
        return replaySequentially(backup, 0);
      })
      .then(function() {
        recovering = false;
        SaveLoadFlow.setStatus(message + " — recovered; your " + actions.length
          + " earlier edit" + (actions.length === 1 ? " was" : "s were") + " re-applied.");
        updateToolbar();
      })
      .catch(function(replayError) {
        recovering = false;
        applyInFlight = false;
        SaveLoadFlow.hideProgress();
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
    return SaveClient.applyEdits(backup[i], false, function(phase, current, total) {
      SaveLoadFlow.showProgress(phase, total > 0 ? (current / total) * 100 : 0);
    }).then(function(payload) {
      SaveLoadFlow.hideProgress();
      SaveLoadFlow.applyPayload(payload);
      actions.push(backup[i]);
      updateToolbar();
      return replaySequentially(backup, i + 1);
    });
  }

  // ---- Toolbar (edit count / undo / redo) ---------------------------------------

  function updateToolbar() {
    var any = actions.length > 0 || redoStack.length > 0;
    toolbar.style.display = any ? "flex" : "none";
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
    SaveClient.applyEdits(ops, false, function(phase, current, total) {
      SaveLoadFlow.showProgress(phase, total > 0 ? (current / total) * 100 : 0);
    })
      .then(function(payload) {
        redoStack.pop();
        actions.push(ops);
        finishApply(payload, "Edit redone.");
      })
      .catch(failApply);
  }

  // ---- Ghost placement ------------------------------------------------------------

  function startPlacement(mode, targets) {
    if (!targets || (targets.actorNames.length + targets.lightweight.length) === 0 || applyInFlight) {
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
    hintBar.textContent = (mode === "paste" ? "Click to paste" : "Click to place")
      + " · R rotate 90° · Esc cancel";
    hintBar.style.display = "block";
    ghost.style.display = "block";
    MapApp.map.on("click", onPlacementClick);
    MapApp.map.on("mousemove", onPlacementMouseMove);
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
  }

  function startPaste() {
    if (clipboard) {
      startPlacement("paste", clipboard);
    }
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
    if (!clipboard || applyInFlight) {
      return;
    }
    var b = clipboard.bbox;
    var anchor = { x: (b.minX + b.maxX) / 2, y: (b.minY + b.maxY) / 2 };
    var deltaXY = mapDeltaToWorld(mapX - anchor.x, mapY - anchor.y);
    applyAction(buildDuplicateOps(clipboard, [deltaXY[0], deltaXY[1], 0], 0, null));
  }

  function cancelPlacement() {
    if (!placement) {
      return;
    }
    placement = null;
    ghost.style.display = "none";
    hintBar.style.display = "none";
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

  function positionGhost() {
    if (!placement || !placement.cursor) {
      return;
    }
    // Ghost = the selection bbox centered on the cursor (the anchor is the
    // bbox center). Odd 90-degree steps swap the box's width/height.
    var b = placement.bbox;
    var halfW = Math.max((b.maxX - b.minX) / 2, 4);
    var halfH = Math.max((b.maxY - b.minY) / 2, 4);
    if (placement.rotSteps % 2 === 1) {
      var t = halfW; halfW = halfH; halfH = t;
    }
    var c = placement.cursor;
    // Map px -> screen px via Leaflet ([lat, lng] = [y, x]).
    var p1 = MapApp.map.latLngToContainerPoint([c.lat - halfH, c.lng - halfW]);
    var p2 = MapApp.map.latLngToContainerPoint([c.lat + halfH, c.lng + halfW]);
    ghost.style.left = Math.min(p1.x, p2.x) + "px";
    ghost.style.top = Math.min(p1.y, p2.y) + "px";
    ghost.style.width = Math.abs(p2.x - p1.x) + "px";
    ghost.style.height = Math.abs(p2.y - p1.y) + "px";
  }

  function onPlacementClick(e) {
    if (!placement) {
      return;
    }
    var p = placement;
    var dPxX = e.latlng.lng - p.anchor.x;
    var dPxY = e.latlng.lat - p.anchor.y;
    var deltaXY = mapDeltaToWorld(dPxX, dPxY);
    var rotDeg = (p.rotSteps * 90) % 360;
    var pivot = mapPxToWorldXY(p.anchor.x, p.anchor.y);
    var build = p.mode === "paste" ? buildDuplicateOps : buildMoveOps;
    var ops = build(p.targets, [deltaXY[0], deltaXY[1], 0], rotDeg, pivot);
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
    SaveClient.exportSave()
      .then(function(bytes) {
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
      })
      .catch(function(error) {
        SaveLoadFlow.setStatus("Failed to export save: " + (error && error.message || error));
      })
      .then(function() {
        exportInFlight = false;
        downloadBtn.disabled = false;
        downloadBtn.textContent = "Download save";
      });
  }

  // ---- Wiring -----------------------------------------------------------------------

  document.addEventListener("DOMContentLoaded", function() {
    downloadBtn = document.getElementById("downloadSaveBtn");
    toolbar = document.getElementById("editorToolbar");
    editCountEl = document.getElementById("editorEditCount");
    undoBtn = document.getElementById("editorUndoBtn");
    redoBtn = document.getElementById("editorRedoBtn");
    ghost = document.getElementById("editorGhost");
    hintBar = document.getElementById("editorHint");
    offsetOverlay = document.getElementById("offsetDialogOverlay");
    offsetDx = document.getElementById("offsetDx");
    offsetDy = document.getElementById("offsetDy");
    offsetDz = document.getElementById("offsetDz");
    offsetRot = document.getElementById("offsetRot");
    offsetApply = document.getElementById("offsetApplyBtn");
    offsetCancel = document.getElementById("offsetCancelBtn");

    // The ghost is positioned in map-container coordinates
    // (latLngToContainerPoint), so it must live inside the map container.
    document.getElementById("map").appendChild(ghost);

    downloadBtn.addEventListener("click", exportSave);
    undoBtn.addEventListener("click", undo);
    redoBtn.addEventListener("click", redo);
    offsetApply.addEventListener("click", applyOffsetDialog);
    offsetCancel.addEventListener("click", closeOffsetDialog);
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
      if (e.key === "Escape") {
        cancelPlacement();
        closeOffsetDialog();
        return;
      }
      if (placement && (e.key === "r" || e.key === "R")) {
        placement.rotSteps = (placement.rotSteps + 1) % 4;
        positionGhost();
        e.preventDefault();
        return;
      }
      // Ctrl+Z / Ctrl+Y / Ctrl+V outside of text inputs.
      var inInput = e.target && (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA");
      if (!inInput && (e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z") {
        undo();
        e.preventDefault();
      } else if (!inInput && (e.ctrlKey || e.metaKey)
                 && (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z"))) {
        redo();
        e.preventDefault();
      } else if (!inInput && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        if (clipboard) {
          startPaste();
          e.preventDefault();
        }
      }
    });

    window.addEventListener("beforeunload", function(e) {
      if (actions.length > 0) {
        e.preventDefault();
        e.returnValue = "";
      }
    });
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
  };
})();
