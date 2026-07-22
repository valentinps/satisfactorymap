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
  // Line layers whose segments are real editable actors: belts/pipes
  // (per-mark buckets "line:belt:Mk.6", ...), railroads, hypertubes, and
  // vehicle paths ("line:vehiclePath:<mark>"). Their spline data is
  // actor-local, so the edit engine's header-transform move/copy carries
  // the geometry. Power lines stay out: wires already travel with their
  // poles on copy/move, and selecting them directly would double-count.
  function isSelectableLineBucket(bucket) {
    return bucket.key.indexOf("line:belt:") === 0
      || bucket.key.indexOf("line:pipe:") === 0
      || bucket.key === "line:railroads"
      || bucket.key === "line:hypertubes"
      || bucket.key.indexOf("line:vehiclePath:") === 0;
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
      // minZ/maxZ are altitude METERS (bucket z convention, stride - 1), so
      // the paste panel can offer an absolute-altitude field; they stay
      // Infinity when no editable record carries a z.
      bbox: { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity,
              minZ: Infinity, maxZ: -Infinity },
    };
    // Altitude of one selected record, meters: same stride-1 slot the
    // altitude filter reads (first vertex for line buckets).
    function recordZ(r) {
      var stride = r.bucket.pointStride;
      if (r.bucket.lines) {
        var line = r.bucket.lines[r.index];
        return line ? line[stride - 1] : undefined;
      }
      return r.bucket.points ? r.bucket.points[r.index * stride + stride - 1] : undefined;
    }
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
        var z = recordZ(r);
        if (typeof z === "number") {
          b.minZ = Math.min(b.minZ, z);
          b.maxZ = Math.max(b.maxZ, z);
        }
      } else {
        editTargets.skipped++;
      }
    });
    return { total: total, byLabel: byLabel, labelOrder: labelOrder, ids: ids, editTargets: editTargets };
  }

  // ---- Highlight overlay ----------------------------------------------------
  // A pointer-transparent canvas that lives INSIDE Leaflet's overlay pane
  // (not over the map container): a drag-pan moves the pane with a CSS
  // transform, so the highlight moves in perfect lockstep with the base map
  // and the object layer -- no per-frame redraw, no one-frame-late desync.
  // The wheel-zoom CSS animation is matched the same way webgl_layer.js's
  // _onZoomAnim does it: hand the canvas the identical target transform and
  // let the compositor scale it with everything else, then repaint crisp on
  // zoomend.
  //
  // Selection geometry is cached as MAP-PIXEL-space Path2D objects (one per
  // bucket), rebuilt only when the selection itself changes. A repaint --
  // zoomend, a pan that escapes the buffered margin, selection change -- is
  // then a handful of native fill/stroke calls under a canvas transform
  // instead of a JS loop over every selected object, which is what made
  // 100k+ selections crawl.

  var highlightCanvas = null;
  var highlightCtx = null;
  var drawQueued = false;
  var mapEventsAttached = false;
  var canvasTopLeft = null; // layer point of the canvas's top-left corner
  var groupCache = null;    // per-bucket geometry cache, see rebuildGroupCache
  var zoomAnimating = false;

  // Buffered margin per side (fraction of the viewport): pans inside it show
  // fully-painted highlight with zero work; the canvas re-glues on moveend.
  // Total pixels are capped so a 4K window doesn't allocate absurd canvas
  // memory (same idea as map.js's BUFFER_MAX_PIXELS).
  var HIGHLIGHT_MARGIN = 0.5;
  var HIGHLIGHT_MAX_PIXELS = 16e6;
  // Below this on-screen half-extent a footprint is illegible; those records
  // draw as fixed-size dots instead (built per repaint, viewport-culled).
  var MIN_HALF_SCREEN_PX = 2.5;
  var DOT_HALF_SCREEN_PX = 3;
  // Above this many dots, dedupe them on a dot-sized grid: zoomed way out,
  // thousands of dots land on the same few pixels anyway.
  var DOT_DEDUP_THRESHOLD = 20000;

  function ensureHighlightCanvas() {
    if (!highlightCanvas && window.MapApp && MapApp.map) {
      highlightCanvas = document.createElement("canvas");
      highlightCanvas.id = "selectionHighlightCanvas";
      highlightCanvas.className = "leaflet-zoom-animated";
      // Explicit z-index: the bucket layer's canvases are unpositioned
      // siblings in the same pane, and a layer swap (WebGL -> 2D fallback)
      // re-appends them after this one.
      highlightCanvas.style.cssText = "position:absolute;z-index:200;pointer-events:none;";
      MapApp.map.getPanes().overlayPane.appendChild(highlightCanvas);
      highlightCtx = highlightCanvas.getContext("2d");
    }
    if (!mapEventsAttached && window.MapApp && MapApp.map) {
      // No "move" here on purpose: the pane transform already carries the
      // canvas during pans. "zoom" covers pinch/flyTo (fractional zoom with
      // no CSS animation); wheel zoom rides zoomanim/zoomend instead.
      MapApp.map.on("moveend viewreset resize", requestHighlightDraw);
      MapApp.map.on("zoomend", onZoomEnd);
      MapApp.map.on("zoom", onZoomEvent);
      MapApp.map.on("zoomanim", onZoomAnim);
      mapEventsAttached = true;
    }
  }

  // Wheel zoom: Leaflet CSS-transitions everything carrying
  // leaflet-zoom-animated over ~250ms and fires no per-frame events. Give
  // the canvas the same target transform L.ImageOverlay uses so it scales
  // in sync (slightly stretched until the zoomend repaint, exactly like the
  // base map image and the GL canvas).
  function onZoomAnim(e) {
    if (!highlightCanvas || !canvasTopLeft) {
      return;
    }
    zoomAnimating = true;
    var map = MapApp.map;
    var scale = map.getZoomScale(e.zoom);
    var anchorLatLng = map.layerPointToLatLng(canvasTopLeft);
    var offset = map._latLngToNewLayerPoint(anchorLatLng, e.zoom, e.center);
    L.DomUtil.setTransform(highlightCanvas, offset, scale);
  }

  // Continuous zoom (pinch, flyTo) fires "zoom" per frame with no CSS
  // animation -- repaint (cheap: cached paths). During an animated wheel
  // zoom "zoom" fires once with the TARGET zoom while the CSS transition
  // still runs; repainting then would double-transform, so skip until
  // zoomend clears the flag.
  function onZoomEvent() {
    if (!zoomAnimating) {
      requestHighlightDraw();
    }
  }

  function onZoomEnd() {
    zoomAnimating = false;
    requestHighlightDraw();
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

  // Selection changed: drop the cached geometry (rebuilt lazily on the next
  // repaint).
  function invalidateHighlightCache() {
    groupCache = null;
  }

  // Group the selection per bucket and bake each group's geometry into a
  // map-pixel-space Path2D: each building's real rotated footprint (same
  // corner math as map.js's _traceRect, including the genuinely-tilted
  // polygon overrides), the full polyline for belt/pipe segments. Centers
  // are kept alongside for the small-on-screen dot pass.
  function rebuildGroupCache() {
    var groups = new Map();
    selected.forEach(function(r) {
      var g = groups.get(r.bucket.key);
      if (!g) {
        g = {
          isLine: r.bucket.renderType === "line",
          half: r.bucket.footprintPixels || null,
          // Lines bake their polyline here once. Footprints do NOT: their
          // rotated geometry is built per-frame in drawHighlight, culled to
          // the viewport (see the note there) -- so a huge zoomed-out
          // selection that renders as dots builds no footprint path at all.
          path: r.bucket.renderType === "line" ? new Path2D() : null,
          recs: [], // footprint groups only: refs for the deferred build
          xs: [],
          ys: [],
        };
        groups.set(r.bucket.key, g);
      }
      if (g.isLine) {
        var line = r.bucket.lines[r.index];
        if (!line) {
          return;
        }
        var stride = r.bucket.pointStride;
        g.path.moveTo(line[0], line[1]);
        for (var vi = stride; vi < line.length; vi += stride) {
          g.path.lineTo(line[vi], line[vi + 1]);
        }
        return;
      }
      if (g.half) {
        g.recs.push(r);
      }
      g.xs.push(r.x);
      g.ys.push(r.y);
    });
    groupCache = [];
    groups.forEach(function(g) { groupCache.push(g); });
  }

  // One selected building's rotated footprint appended to `path`, in raw
  // map-pixel space. Same corner math as map.js's _traceRect (yaw negated for
  // the flipped-Y map space) with the tilted-polygon override. Uses an
  // explicit closing lineTo, NOT closePath(): closePath on a Path2D holding
  // many subpaths is O(n^2) in Chrome (50k footprints = ~4.5s), which is what
  // turned large selections into a multi-second freeze.
  function appendFootprint(path, r, half) {
    var tilted = r.bucket.tiltedFootprints && r.bucket.tiltedFootprints[r.index];
    if (tilted) {
      path.moveTo(r.x + tilted[0], r.y + tilted[1]);
      for (var ti = 2; ti < tilted.length; ti += 2) {
        path.lineTo(r.x + tilted[ti], r.y + tilted[ti + 1]);
      }
      path.lineTo(r.x + tilted[0], r.y + tilted[1]);
      return;
    }
    var yaw = r.bucket.pointStride === 4 ? r.bucket.points[r.index * 4 + 2] : 0;
    var cos = Math.cos(-yaw);
    var sin = Math.sin(-yaw);
    var cosW = cos * half[0];
    var sinW = sin * half[0];
    var cosD = cos * half[1];
    var sinD = sin * half[1];
    var x0 = r.x + cosW - sinD, y0 = r.y + sinW + cosD;
    path.moveTo(x0, y0);
    path.lineTo(r.x - cosW - sinD, r.y - sinW + cosD);
    path.lineTo(r.x - cosW + sinD, r.y - sinW - cosD);
    path.lineTo(r.x + cosW + sinD, r.y + sinW - cosD);
    path.lineTo(x0, y0);
  }

  function drawHighlight() {
    if (!highlightCanvas && selected.size === 0) {
      return;
    }
    if (!window.MapApp || !MapApp.map) {
      return;
    }
    ensureHighlightCanvas();
    var map = MapApp.map;

    // Size + position: viewport plus margin, glued to layer space so the
    // pane transform moves it with the map.
    var size = map.getSize();
    var factor = Math.min(
      HIGHLIGHT_MARGIN,
      Math.max(0, (Math.sqrt(HIGHLIGHT_MAX_PIXELS / Math.max(1, size.x * size.y)) - 1) / 2));
    var padX = Math.round(size.x * factor);
    var padY = Math.round(size.y * factor);
    var w = size.x + padX * 2;
    var h = size.y + padY * 2;
    var topLeft = map.containerPointToLayerPoint([0, 0]).subtract(L.point(padX, padY));
    canvasTopLeft = topLeft;
    // setPosition rewrites the whole transform, which also clears any
    // zoom-preview scale left by onZoomAnim.
    L.DomUtil.setPosition(highlightCanvas, topLeft);
    if (highlightCanvas.width !== w || highlightCanvas.height !== h) {
      highlightCanvas.width = w;
      highlightCanvas.height = h;
    }
    var ctx = highlightCtx;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (selected.size === 0) {
      return;
    }
    if (!groupCache) {
      rebuildGroupCache();
    }

    // Map px -> canvas px is affine under CRS.Simple; derive it from two
    // unrounded projections ([lat, lng] = [y, x]) and hand it to the canvas,
    // so the cached map-space paths render directly.
    var BASE = 4096;
    var zoom = map.getZoom();
    var origin = map.getPixelOrigin();
    var p0 = map.project(L.latLng(0, 0), zoom);
    var pu = map.project(L.latLng(BASE, BASE), zoom);
    var sx = (pu.x - p0.x) / BASE;
    var sy = (pu.y - p0.y) / BASE;
    var tx = p0.x - origin.x - topLeft.x;
    var ty = p0.y - origin.y - topLeft.y;
    var scale = Math.abs(sx); // |sx| == |sy| under CRS.Simple

    // Canvas bounds in map px, for culling the dot pass.
    var bx1 = (0 - tx) / sx, bx2 = (w - tx) / sx;
    var by1 = (0 - ty) / sy, by2 = (h - ty) / sy;
    var minX = Math.min(bx1, bx2), maxX = Math.max(bx1, bx2);
    var minY = Math.min(by1, by2), maxY = Math.max(by1, by2);

    ctx.setTransform(sx, 0, 0, sy, tx, ty);
    ctx.fillStyle = "rgba(91, 163, 224, 0.25)";
    ctx.strokeStyle = "#5ba3e0";

    var dotPath = null;
    var dotHalf = DOT_HALF_SCREEN_PX / scale;
    var dotCount = 0;
    groupCache.forEach(function(g) {
      if (g.isLine) {
        return; // Lines stroke last, over the fills.
      }
      if (g.half && Math.max(g.half[0], g.half[1]) * scale >= MIN_HALF_SCREEN_PX) {
        // Build the rotated footprints now, for viewport-visible objects
        // only. When zoomed in far enough to show real footprints, only a
        // small slice of a large selection is on screen, so this stays cheap
        // even for a 300k selection (and is skipped whenever the group is
        // small enough to draw as dots below).
        var fpPath = new Path2D();
        var fpCount = 0;
        for (var fi = 0; fi < g.recs.length; fi++) {
          var fr = g.recs[fi];
          if (fr.x < minX || fr.x > maxX || fr.y < minY || fr.y > maxY) {
            continue;
          }
          appendFootprint(fpPath, fr, g.half);
          fpCount++;
        }
        if (fpCount > 0) {
          ctx.fill(fpPath);
          ctx.lineWidth = 1.5 / scale;
          ctx.stroke(fpPath);
        }
        return;
      }
      // Too small on screen for its real footprint: fixed-size dots.
      if (!dotPath) {
        dotPath = new Path2D();
      }
      var seen = null, cell = 0;
      if (g.xs.length > DOT_DEDUP_THRESHOLD) {
        seen = new Set();
        cell = dotHalf;
      }
      for (var i = 0; i < g.xs.length; i++) {
        var x = g.xs[i], y = g.ys[i];
        if (x < minX || x > maxX || y < minY || y > maxY) {
          continue;
        }
        if (seen) {
          var key = Math.round(x / cell) * 262144 + Math.round(y / cell);
          if (seen.has(key)) {
            continue;
          }
          seen.add(key);
        }
        dotPath.rect(x - dotHalf, y - dotHalf, dotHalf * 2, dotHalf * 2);
        dotCount++;
      }
    });
    if (dotPath && dotCount > 0) {
      // Dots are too small to read at fill-alpha 0.25; draw them solid.
      ctx.fillStyle = "rgba(91, 163, 224, 0.85)";
      ctx.fill(dotPath);
      ctx.fillStyle = "rgba(91, 163, 224, 0.25)";
    }
    groupCache.forEach(function(g) {
      if (g.isLine) {
        ctx.lineWidth = 3.5 / scale;
        ctx.stroke(g.path);
      }
    });
    ctx.setTransform(1, 0, 0, 1, 0, 0);
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
  // Only ever called right after `selected` changed, so the baked highlight
  // geometry is stale by definition.
  function refreshUI() {
    var selection = aggregate();
    lastSelection = selection.total > 0 ? selection : null;
    invalidateHighlightCache();
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
    invalidateHighlightCache();
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

  // ---- Ctrl+A: select everything ------------------------------------------
  // Same semantics as a right-drag box over the whole world: visible buckets
  // only, altitude filter respected. Selections this big are legitimate
  // (megabase-wide move/delete) but take a noticeable moment to aggregate and
  // highlight, so past this threshold ask before committing.
  var SELECT_ALL_CONFIRM_THRESHOLD = 500000;

  // Upper bound on what a select-all would grab (per-bucket object counts;
  // cheap, no per-point walk) -- just for the confirmation gate.
  function selectableObjectCount() {
    var total = 0;
    MapApp.layer.buckets.forEach(function(bucket) {
      if (!bucket.visible || bucket.excludeFromSelection) {
        return;
      }
      if (bucket.renderType === "line") {
        if (isSelectableLineBucket(bucket) && bucket.lines) {
          total += bucket.lines.length;
        }
      } else if (bucket.points) {
        total += bucket.points.length / bucket.pointStride;
      }
    });
    return total;
  }

  document.addEventListener("keydown", function(e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "a") {
      return;
    }
    var target = e.target;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) {
      return; // Ctrl+A in a text field keeps its select-the-text meaning.
    }
    if (!window.MapApp || !MapApp.layer) {
      return;
    }
    var count = selectableObjectCount();
    if (count === 0) {
      return;
    }
    e.preventDefault(); // Ours even if the user cancels -- don't also select the page text.
    if (count >= SELECT_ALL_CONFIRM_THRESHOLD
        && !window.confirm("Are you sure you want to select all objects?")) {
      return;
    }
    var records = collectInBox(-Infinity, Infinity, -Infinity, Infinity);
    selected.clear();
    records.forEach(function(r) {
      selected.set(recordKey(r), r);
    });
    refreshUI();
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
    if (e.key === "Escape" && !e.defaultPrevented && overlay.style.display !== "none") {
      closeModal();
      e.preventDefault(); // One layer per press -- see finditem.js.
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

  // The current selection's edit targets, for Ctrl+C (see editor.js).
  SelectionTool.currentEditTargets = function() {
    return lastSelection ? lastSelection.editTargets : null;
  };

  // Called on every save (re)load (see data.js) -- the previous selection's
  // ids belong to the old save's buckets, so drop it.
  SelectionTool.reset = function() {
    clearSelection();
    closeModal();
  };
})();
