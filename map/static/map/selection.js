// Right-click-drag rectangle selection over the map. While the right button
// is held, a selection rectangle is drawn; on release, every visible object
// whose plotted point falls inside it (respecting the altitude filter, same
// as what's drawn -- see map.js) is collected. A floating panel then shows
// the count with two actions: "List objects" (a per-type breakdown, computed
// entirely client-side from the buckets' own labels) and "Total inventory"
// (the summed contents of the selected buildings' inventories, via
// /api/selection-inventory -> sav_map_data.aggregateSelectionInventory).

var SelectionTool = {};

(function() {
  "use strict";

  var rect = document.getElementById("selectionRect");
  var panel = document.getElementById("selectionPanel");
  var countEl = document.getElementById("selectionCount");
  var objectsBtn = document.getElementById("selectionObjectsBtn");
  var inventoryBtn = document.getElementById("selectionInventoryBtn");
  var moveBtn = document.getElementById("selectionMoveBtn");
  var offsetBtn = document.getElementById("selectionOffsetBtn");
  var copyBtn = document.getElementById("selectionCopyBtn");
  var deleteBtn = document.getElementById("selectionDeleteBtn");
  var clearBtn = document.getElementById("selectionClearBtn");

  var overlay = document.getElementById("selectionModalOverlay");
  var modalTitle = document.getElementById("selectionModalTitle");
  var modalSummary = document.getElementById("selectionModalSummary");
  var modalList = document.getElementById("selectionModalList");
  var modalClose = document.getElementById("selectionModalClose");

  var MIN_DRAG_PX = 4; // Below this the gesture is a stray right-click, not a drag.
  // Only belts and pipes are selectable among the line layers -- their
  // segments carry content (in-transit items / fluid). Their buckets are now
  // per-mark ("line:belt:Mk.6", "line:pipe:Mk.1", ...), matched by prefix.
  // The other line layers (railroads, hypertubes, power lines, vehicle
  // paths) hold nothing, and power lines in particular also exist as point
  // actors (which would double-count), so they're left out.
  function isSelectableLineBucket(bucket) {
    return bucket.key.indexOf("line:belt:") === 0 || bucket.key.indexOf("line:pipe:") === 0;
  }

  var selecting = false;
  var selectingAdditive = false; // Ctrl held at right-drag start: add to selection
  var startClient = null;
  var lastSelection = null; // aggregates of `selected`: { total, byLabel, labelOrder, ids, editTargets }

  // The persistent selection: "<bucket.key>#<index>" -> record. Box select
  // replaces it (or extends it with Ctrl); Ctrl+click toggles single
  // objects; every edit/load clears it via SelectionTool.reset (buckets are
  // rebuilt then, so records would dangle).
  var selected = new Map();

  function recordOf(bucket, index, id, x, y) {
    return { bucket: bucket, index: index, id: id, x: x, y: y };
  }

  function recordKey(record) {
    return record.bucket.key + "#" + record.index;
  }

  // Buckets whose objects the save editor can transform: buildings (normal
  // actors + lightweight foundations/walls) and belt/pipe segments. Vehicle,
  // train, creature, and collectable buckets are viewer-only -- their
  // objects are counted into editTargets.skipped instead, and the Rust edit
  // engine independently refuses anything unsupported that slips through.
  function isEditableBucket(bucket) {
    return bucket.key.indexOf("building:") === 0 || isSelectableLineBucket(bucket);
  }

  var LIGHTWEIGHT_ID_PREFIX = "LightweightBuildable:";

  // "LightweightBuildable:<typePath>:<idx>" -> {typePath, index} (type paths
  // never contain ':', so the last colon splits off the index).
  function parseLightweightId(id) {
    var sep = id.lastIndexOf(":");
    return {
      typePath: id.slice(LIGHTWEIGHT_ID_PREFIX.length, sep),
      index: parseInt(id.slice(sep + 1), 10),
    };
  }

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function renderList(rows) {
    modalList.innerHTML = "";
    rows.forEach(function(pair) {
      var row = el("div", "itemLocationRow");
      row.appendChild(el("span", "itemLocationLabel", pair[0]));
      row.appendChild(el("span", "itemLocationCount", pair[1]));
      modalList.appendChild(row);
    });
  }

  // ---- The drag rectangle -------------------------------------------------

  function positionRect(x1, y1, x2, y2) {
    var left = Math.min(x1, x2);
    var top = Math.min(y1, y2);
    rect.style.left = left + "px";
    rect.style.top = top + "px";
    rect.style.width = Math.abs(x2 - x1) + "px";
    rect.style.height = Math.abs(y2 - y1) + "px";
  }

  // Leaflet maps a plotted point [x, y] to L.latLng(y, x) (see map.js), so a
  // screen point converts back with lng = mapX, lat = mapY.
  function clientToMapXY(clientX, clientY) {
    var latLng = MapApp.map.mouseEventToLatLng({ clientX: clientX, clientY: clientY });
    return { x: latLng.lng, y: latLng.lat };
  }

  // Everything visible inside the box, as records (no aggregation here --
  // records feed the persistent `selected` set).
  function collectInBox(minX, maxX, minY, maxY) {
    var altMin = MapApp.altitudeRange ? MapApp.altitudeRange.min : -Infinity;
    var altMax = MapApp.altitudeRange ? MapApp.altitudeRange.max : Infinity;
    var records = [];

    MapApp.layer.buckets.forEach(function(bucket) {
      if (!bucket.visible) {
        return;
      }
      // Vehicle pins and their box buckets share ids (and a train's pin is
      // just its cars' abstract consist actor) -- one of the pair opts out
      // here so nothing is counted twice. See filters.js's
      // buildVehiclesSection/buildTrainRow.
      if (bucket.excludeFromSelection) {
        return;
      }
      if (bucket.renderType === "line") {
        // Belts/pipes -- a segment is selected if any of its vertices falls
        // in the box; its id (the segment instanceName) drives the backend's
        // belt-item / pipe-fluid tally.
        if (!isSelectableLineBucket(bucket) || !bucket.lines) {
          return;
        }
        var lineStride = bucket.pointStride;
        var lineZ = lineStride - 1;
        for (var li = 0; li < bucket.lines.length; li++) {
          var line = bucket.lines[li];
          var hit = false;
          for (var vi = 0; vi < line.length; vi += lineStride) {
            var lx = line[vi], ly = line[vi + 1], lz = line[vi + lineZ];
            if (lx >= minX && lx <= maxX && ly >= minY && ly <= maxY && lz >= altMin && lz <= altMax) {
              hit = true;
              break;
            }
          }
          if (hit) {
            records.push(recordOf(bucket, li, bucket.ids ? bucket.ids[li] : null, line[0], line[1]));
          }
        }
        return;
      }
      if (!bucket.points) {
        return;
      }
      var stride = bucket.pointStride;
      var zIndex = stride - 1;
      var pts = bucket.points;
      for (var i = 0; i < pts.length; i += stride) {
        var x = pts[i], y = pts[i + 1], z = pts[i + zIndex];
        if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
          continue;
        }
        records.push(recordOf(bucket, i / stride, bucket.ids ? bucket.ids[i / stride] : null, x, y));
      }
    });
    return records;
  }

  // Aggregate `selected` into what the panel/modal/editor consume.
  function aggregate() {
    var byLabel = {};
    var labelOrder = [];
    var ids = [];
    var total = 0;
    // What the save editor can act on, plus the map-pixel bbox of the
    // editable objects (its center anchors ghost placement / rotation).
    var editTargets = {
      actorNames: [],
      lightweight: [],
      skipped: 0,
      bbox: { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity },
    };
    selected.forEach(function(r) {
      total++;
      if (!byLabel.hasOwnProperty(r.bucket.label)) {
        byLabel[r.bucket.label] = 0;
        labelOrder.push(r.bucket.label);
      }
      byLabel[r.bucket.label]++;
      // Lightweight buildables (foundations/walls/...) have synthetic ids and
      // never hold inventory, so they're left out of the inventory query --
      // both to keep the POST small on big selections and because the backend
      // would skip them anyway. They still count above.
      if (r.id && r.id.indexOf(LIGHTWEIGHT_ID_PREFIX) !== 0) {
        ids.push(r.id);
      }
      if (r.id && isEditableBucket(r.bucket)) {
        if (r.id.indexOf(LIGHTWEIGHT_ID_PREFIX) === 0) {
          editTargets.lightweight.push(parseLightweightId(r.id));
        } else {
          editTargets.actorNames.push(r.id);
        }
        var b = editTargets.bbox;
        b.minX = Math.min(b.minX, r.x);
        b.minY = Math.min(b.minY, r.y);
        b.maxX = Math.max(b.maxX, r.x);
        b.maxY = Math.max(b.maxY, r.y);
      } else {
        editTargets.skipped++;
      }
    });
    return { total: total, byLabel: byLabel, labelOrder: labelOrder, ids: ids, editTargets: editTargets };
  }

  // ---- Highlight overlay ----------------------------------------------------
  // A pointer-transparent canvas over the map that outlines every selected
  // object, redrawn on selection changes and map pan/zoom.

  var highlightCanvas = null;
  var highlightCtx = null;
  var drawQueued = false;
  var mapEventsAttached = false;

  function ensureHighlightCanvas() {
    if (!highlightCanvas) {
      highlightCanvas = document.createElement("canvas");
      highlightCanvas.id = "selectionHighlightCanvas";
      highlightCanvas.style.cssText =
        "position:absolute;left:0;top:0;z-index:640;pointer-events:none;";
      mapContainer.appendChild(highlightCanvas);
      highlightCtx = highlightCanvas.getContext("2d");
    }
    if (!mapEventsAttached && window.MapApp && MapApp.map) {
      MapApp.map.on("move zoom zoomend moveend resize", requestHighlightDraw);
      mapEventsAttached = true;
    }
  }

  function requestHighlightDraw() {
    if (drawQueued) {
      return;
    }
    drawQueued = true;
    requestAnimationFrame(function() {
      drawQueued = false;
      drawHighlight();
    });
  }

  function drawHighlight() {
    if (!highlightCanvas && selected.size === 0) {
      return;
    }
    ensureHighlightCanvas();
    var w = mapContainer.clientWidth;
    var h = mapContainer.clientHeight;
    if (highlightCanvas.width !== w || highlightCanvas.height !== h) {
      highlightCanvas.width = w;
      highlightCanvas.height = h;
    }
    var ctx = highlightCtx;
    ctx.clearRect(0, 0, w, h);
    if (selected.size === 0 || !window.MapApp || !MapApp.map) {
      return;
    }
    // Map px -> container px is affine under CRS.Simple; derive it from two
    // reference conversions instead of converting every point through
    // Leaflet ([lat, lng] = [y, x]). Leaflet rounds container points to
    // whole pixels, so use a large baseline (a 1px one degenerates to a
    // scale of 0 when zoomed out).
    var BASE = 4096;
    var p0 = MapApp.map.latLngToContainerPoint([0, 0]);
    var px = MapApp.map.latLngToContainerPoint([0, BASE]);
    var py = MapApp.map.latLngToContainerPoint([BASE, 0]);
    var sx = (px.x - p0.x) / BASE;
    var sy = (py.y - p0.y) / BASE;

    ctx.strokeStyle = "#5ba3e0";
    ctx.fillStyle = "rgba(91, 163, 224, 0.25)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    selected.forEach(function(r) {
      if (r.bucket.renderType === "line") {
        // Belts/pipes: trace the whole selected segment.
        var line = r.bucket.lines[r.index];
        if (!line) {
          return;
        }
        var stride = r.bucket.pointStride;
        ctx.moveTo(p0.x + line[0] * sx, p0.y + line[1] * sy);
        for (var vi = stride; vi < line.length; vi += stride) {
          ctx.lineTo(p0.x + line[vi] * sx, p0.y + line[vi + 1] * sy);
        }
        return;
      }
      var cx = p0.x + r.x * sx;
      var cy = p0.y + r.y * sy;
      if (cx < -50 || cx > w + 50 || cy < -50 || cy > h + 50) {
        return;
      }
      // Buildings: an axis-aligned box at the footprint's extent (yaw
      // ignored -- this is a "what's selected" cue, not geometry); other
      // points a small fixed marker.
      var half = 4;
      if (r.bucket.footprintPixels) {
        half = Math.max(3, Math.max(r.bucket.footprintPixels[0], r.bucket.footprintPixels[1]) * Math.abs(sx));
      }
      ctx.rect(cx - half, cy - half, half * 2, half * 2);
    });
    ctx.fill();
    ctx.stroke();
  }

  // A plain right-click (no drag) opens the context menu for whatever's
  // under the cursor (see ContextMenu/contextmenu.js) instead of starting a
  // rectangle selection -- same hitTest/tolerance convention map.js's own
  // hover/click handlers already use.
  function maybeShowContextMenu(clientX, clientY) {
    if (!window.ContextMenu || !window.MapApp || !MapApp.layer) {
      return;
    }
    var latLng = MapApp.map.mouseEventToLatLng({ clientX: clientX, clientY: clientY });
    var toleranceScreenPx = 8;
    var toleranceMapUnits = toleranceScreenPx / Math.pow(2, MapApp.map.getZoom());
    var hit = MapApp.layer.hitTest(latLng.lng, latLng.lat, toleranceMapUnits);
    // hit may be null: the empty-map menu offers "Paste here" (or a hint
    // that the clipboard is empty) -- never silently nothing.
    ContextMenu.show(clientX, clientY, hit);
  }

  // Recompute aggregates from `selected` and sync the panel + highlight.
  function refreshUI() {
    var selection = aggregate();
    lastSelection = selection.total > 0 ? selection : null;
    requestHighlightDraw();
    if (selection.total === 0) {
      panel.style.display = "none";
      return;
    }
    countEl.textContent = selection.total.toLocaleString() + " object" + (selection.total === 1 ? "" : "s") + " selected";
    objectsBtn.disabled = false;
    inventoryBtn.disabled = selection.ids.length === 0;
    var editable = selection.editTargets.actorNames.length + selection.editTargets.lightweight.length;
    moveBtn.disabled = editable === 0;
    offsetBtn.disabled = editable === 0;
    copyBtn.disabled = editable === 0;
    deleteBtn.disabled = editable === 0;
    moveBtn.title = editable === 0
      ? "Nothing editable selected"
      : "Move " + editable.toLocaleString() + " object" + (editable === 1 ? "" : "s")
        + (selection.editTargets.skipped ? " (" + selection.editTargets.skipped + " not editable, left behind)" : "");
    offsetBtn.title = moveBtn.title;
    panel.style.display = "flex";
  }

  function finishSelection(start, end, additive) {
    if (Math.abs(end.x - start.x) < MIN_DRAG_PX && Math.abs(end.y - start.y) < MIN_DRAG_PX) {
      if (!additive) {
        clearSelection();
        maybeShowContextMenu(end.x, end.y);
      }
      return;
    }
    var a = clientToMapXY(start.x, start.y);
    var b = clientToMapXY(end.x, end.y);
    var records = collectInBox(
      Math.min(a.x, b.x), Math.max(a.x, b.x), Math.min(a.y, b.y), Math.max(a.y, b.y));
    if (!additive) {
      selected.clear();
    }
    records.forEach(function(r) {
      selected.set(recordKey(r), r);
    });
    refreshUI();
  }

  // Ctrl+left-click (see map.js's click handler): toggle the object under
  // the cursor in/out of the selection.
  SelectionTool.toggleAtEvent = function(e) {
    if (!MapApp.layer) {
      return;
    }
    var toleranceMapUnits = 8 / Math.pow(2, MapApp.map.getZoom());
    var hit = MapApp.layer.hitTest(e.latlng.lng, e.latlng.lat, toleranceMapUnits);
    if (!hit || hit.bucket.excludeFromSelection) {
      return;
    }
    var x, y;
    if (hit.bucket.renderType === "line") {
      if (!isSelectableLineBucket(hit.bucket)) {
        return;
      }
      var line = hit.bucket.lines[hit.index];
      x = line[0];
      y = line[1];
    } else {
      var stride = hit.bucket.pointStride;
      x = hit.bucket.points[hit.index * stride];
      y = hit.bucket.points[hit.index * stride + 1];
    }
    var r = recordOf(hit.bucket, hit.index, hit.id, x, y);
    var key = recordKey(r);
    if (selected.has(key)) {
      selected.delete(key);
    } else {
      selected.set(key, r);
    }
    refreshUI();
  };

  function clearSelection() {
    panel.style.display = "none";
    rect.style.display = "none";
    lastSelection = null;
    selected.clear();
    requestHighlightDraw();
  }

  // ---- Pointer handling (right button) ------------------------------------

  var mapContainer = document.getElementById("map");

  mapContainer.addEventListener("contextmenu", function(e) {
    e.preventDefault(); // Suppress the browser menu so right-drag is ours.
  });

  mapContainer.addEventListener("mousedown", function(e) {
    if (e.button !== 2) {
      return;
    }
    selecting = true;
    selectingAdditive = e.ctrlKey || e.metaKey; // Ctrl+right-drag extends the selection
    startClient = { x: e.clientX, y: e.clientY };
    positionRect(e.clientX, e.clientY, e.clientX, e.clientY);
    rect.style.display = "block";
    e.preventDefault();
  });

  document.addEventListener("mousemove", function(e) {
    if (!selecting) {
      return;
    }
    positionRect(startClient.x, startClient.y, e.clientX, e.clientY);
  });

  document.addEventListener("mouseup", function(e) {
    if (!selecting || e.button !== 2) {
      return;
    }
    selecting = false;
    rect.style.display = "none";
    finishSelection(startClient, { x: e.clientX, y: e.clientY }, selectingAdditive);
  });

  // ---- Selection modal (List objects / Total inventory) -------------------

  function openModal(title, summary) {
    modalTitle.textContent = title;
    modalSummary.textContent = summary;
    overlay.style.display = "flex";
  }

  function closeModal() {
    overlay.style.display = "none";
  }

  modalClose.addEventListener("click", closeModal);
  overlay.addEventListener("click", function(e) {
    if (e.target === overlay) {
      closeModal();
    }
  });
  document.addEventListener("keydown", function(e) {
    if (e.key === "Escape" && overlay.style.display !== "none") {
      closeModal();
    }
  });

  objectsBtn.addEventListener("click", function() {
    if (!lastSelection) {
      return;
    }
    var rows = lastSelection.labelOrder
      .map(function(label) { return { label: label, count: lastSelection.byLabel[label] }; })
      .sort(function(a, b) { return b.count - a.count; })
      .map(function(row) { return [row.label, row.count.toLocaleString()]; });
    openModal("Selected objects",
      lastSelection.total.toLocaleString() + " objects across " + lastSelection.labelOrder.length + " type" + (lastSelection.labelOrder.length === 1 ? "" : "s") + ".");
    renderList(rows);
  });

  inventoryBtn.addEventListener("click", function() {
    if (!lastSelection || lastSelection.ids.length === 0 || !window.MapApp.currentFile) {
      return;
    }
    openModal("Selection inventory", "Summing inventories…");
    modalList.innerHTML = "";
    SaveClient.selectionInventory(lastSelection.ids)
      .then(function(result) {
        if (result.error) {
          modalSummary.textContent = result.error;
          return;
        }
        var items = result.items || [];
        if (items.length === 0) {
          modalSummary.textContent = "The selected objects hold no items.";
          modalList.innerHTML = "";
          return;
        }
        var totalItems = items.reduce(function(s, e) { return s + (e.isFluid ? 0 : e.count); }, 0);
        modalSummary.textContent = totalItems.toLocaleString() + " items across " + items.length + " type" + (items.length === 1 ? "" : "s") + ".";
        renderList(items.map(function(entry) {
          return [entry.label, entry.count.toLocaleString() + (entry.isFluid ? " m³" : "")];
        }));
      })
      .catch(function(error) {
        modalSummary.textContent = "Failed to load inventory: " + error;
      });
  });

  moveBtn.addEventListener("click", function() {
    if (!lastSelection || moveBtn.disabled) {
      return;
    }
    var targets = lastSelection.editTargets;
    clearSelection();
    EditorTool.startMove(targets);
  });

  offsetBtn.addEventListener("click", function() {
    if (!lastSelection || offsetBtn.disabled) {
      return;
    }
    var targets = lastSelection.editTargets;
    clearSelection();
    EditorTool.openOffsetDialog(targets);
  });

  copyBtn.addEventListener("click", function() {
    if (!lastSelection || copyBtn.disabled) {
      return;
    }
    EditorTool.copyTargets(lastSelection.editTargets);
  });

  deleteBtn.addEventListener("click", function() {
    if (!lastSelection || deleteBtn.disabled) {
      return;
    }
    var targets = lastSelection.editTargets;
    clearSelection();
    EditorTool.deleteTargets(targets);
  });

  clearBtn.addEventListener("click", clearSelection);

  // Called on every save (re)load (see data.js) -- the previous selection's
  // ids belong to the old save's buckets, so drop it.
  SelectionTool.reset = function() {
    clearSelection();
    closeModal();
  };
})();
