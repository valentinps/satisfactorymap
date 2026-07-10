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
  var startClient = null;
  var lastSelection = null; // { total, byLabel, labelOrder, ids, editTargets }

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

  function computeSelection(minX, maxX, minY, maxY) {
    var altMin = MapApp.altitudeRange ? MapApp.altitudeRange.min : -Infinity;
    var altMax = MapApp.altitudeRange ? MapApp.altitudeRange.max : Infinity;
    var byLabel = {};
    var labelOrder = [];
    var ids = [];
    var counters = { total: 0 };
    // What the save editor can act on, plus the map-pixel bbox of the
    // editable objects (its center anchors ghost placement / rotation).
    var editTargets = {
      actorNames: [],
      lightweight: [],
      skipped: 0,
      bbox: { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity },
    };

    function record(bucket, id, x, y) {
      counters.total++;
      if (!byLabel.hasOwnProperty(bucket.label)) {
        byLabel[bucket.label] = 0;
        labelOrder.push(bucket.label);
      }
      byLabel[bucket.label]++;
      // Lightweight buildables (foundations/walls/...) have synthetic ids and
      // never hold inventory, so they're left out of the inventory query --
      // both to keep the POST small on big selections and because the backend
      // would skip them anyway. They still count above.
      if (id && id.indexOf(LIGHTWEIGHT_ID_PREFIX) !== 0) {
        ids.push(id);
      }
      if (id && isEditableBucket(bucket)) {
        if (id.indexOf(LIGHTWEIGHT_ID_PREFIX) === 0) {
          editTargets.lightweight.push(parseLightweightId(id));
        } else {
          editTargets.actorNames.push(id);
        }
        var b = editTargets.bbox;
        b.minX = Math.min(b.minX, x);
        b.minY = Math.min(b.minY, y);
        b.maxX = Math.max(b.maxX, x);
        b.maxY = Math.max(b.maxY, y);
      } else {
        editTargets.skipped++;
      }
    }

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
            record(bucket, bucket.ids ? bucket.ids[li] : null, line[0], line[1]);
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
        record(bucket, bucket.ids ? bucket.ids[i / stride] : null, x, y);
      }
    });
    return { total: counters.total, byLabel: byLabel, labelOrder: labelOrder, ids: ids, editTargets: editTargets };
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
    if (hit) {
      ContextMenu.show(clientX, clientY, hit);
    }
  }

  function finishSelection(start, end) {
    if (Math.abs(end.x - start.x) < MIN_DRAG_PX && Math.abs(end.y - start.y) < MIN_DRAG_PX) {
      clearSelection();
      maybeShowContextMenu(end.x, end.y);
      return;
    }
    var a = clientToMapXY(start.x, start.y);
    var b = clientToMapXY(end.x, end.y);
    var selection = computeSelection(
      Math.min(a.x, b.x), Math.max(a.x, b.x), Math.min(a.y, b.y), Math.max(a.y, b.y));
    lastSelection = selection;

    countEl.textContent = selection.total.toLocaleString() + " object" + (selection.total === 1 ? "" : "s") + " selected";
    var hasAny = selection.total > 0;
    objectsBtn.disabled = !hasAny;
    inventoryBtn.disabled = selection.ids.length === 0;
    var editable = selection.editTargets.actorNames.length + selection.editTargets.lightweight.length;
    moveBtn.disabled = editable === 0;
    offsetBtn.disabled = editable === 0;
    moveBtn.title = editable === 0
      ? "Nothing editable selected"
      : "Move " + editable.toLocaleString() + " object" + (editable === 1 ? "" : "s")
        + (selection.editTargets.skipped ? " (" + selection.editTargets.skipped + " not editable, left behind)" : "");
    offsetBtn.title = moveBtn.title;
    panel.style.display = "flex";
  }

  function clearSelection() {
    panel.style.display = "none";
    rect.style.display = "none";
    lastSelection = null;
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
    finishSelection(startClient, { x: e.clientX, y: e.clientY });
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

  clearBtn.addEventListener("click", clearSelection);

  // Called on every save (re)load (see data.js) -- the previous selection's
  // ids belong to the old save's buckets, so drop it.
  SelectionTool.reset = function() {
    clearSelection();
    closeModal();
  };
})();
