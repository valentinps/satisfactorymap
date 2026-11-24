// NetworkTool -- "Optimal network finder (EMST)": the shortest possible set of
// links joining a set of places on the map, for planning where belts, trains
// or power lines should actually run.
//
// It has no permanent button anywhere: the only way in is to type its name in
// the search bar (see finditem.js's TOOL_ENTRIES), which keeps a planning
// tool out of the way of the map's normal job of showing a save.
//
// Points come from four places -- clicking empty map, clicking a building
// (which takes that building's exact coordinates and name), the current
// rectangle selection in bulk, or typed X/Y metres -- and all of them land in
// the same flat list, so the tree never cares where a point came from.
//
// The maths lives in emst.js (exact, O(n log n), gated by tools/check_emst.js)
// and is fed map-pixel coordinates: the world -> map projection is one uniform
// scale with no rotation (see rust_parser/core/src/mapdata/geometry.rs), so an
// optimal tree in map pixels is an optimal tree in metres, and "along X" on
// screen is "along X" in the world.
//
// Two link styles, which are genuinely different networks and not the same
// tree drawn twice:
//   Straight   -- Euclidean MST, links run point to point.
//   X / Y only -- rectilinear (L1) MST, every link an L of two axis-aligned
//                 legs. Minimising Manhattan length changes which points the
//                 optimal tree even connects, so this is recomputed from
//                 scratch, not the straight tree with corners drawn on.

var NetworkTool = {};

(function() {
  "use strict";

  // ---- Coordinate conversion ------------------------------------------------
  // Same constants as editor.js and rust_parser/core/src/mapdata/geometry.rs:
  // px = ((x/SCALE + OFF)/DESCALE - CROP_LO) * TO_HIGHRES, with map Y flipped.
  var MAP_SIZE = 8192;
  var SCALE = 22.887;
  var OFF_X = 18282.5, OFF_Y = 20480.0;
  var DESCALE = 20;
  var CROP_LO = 4096 / DESCALE;                            // 204.8
  var TO_HIGHRES = MAP_SIZE / (36864 / DESCALE - CROP_LO); // 5
  var PIXELS_PER_WORLD_UNIT = (1 / SCALE / DESCALE) * TO_HIGHRES;
  var METERS_PER_PIXEL = 1 / (PIXELS_PER_WORLD_UNIT * 100); // ~0.9155 m per map pixel

  function mapPxToWorldMeters(mapX, mapY) {
    var wx = ((mapX / TO_HIGHRES + CROP_LO) * DESCALE - OFF_X) * SCALE;
    var wy = (((MAP_SIZE - mapY) / TO_HIGHRES + CROP_LO) * DESCALE - OFF_Y) * SCALE;
    return [wx / 100, wy / 100];
  }

  function worldMetersToMapPx(mx, my) {
    var wx = mx * 100, wy = my * 100;
    return [
      ((wx / SCALE + OFF_X) / DESCALE - CROP_LO) * TO_HIGHRES,
      MAP_SIZE - ((wy / SCALE + OFF_Y) / DESCALE - CROP_LO) * TO_HIGHRES,
    ];
  }

  // ---- Look ------------------------------------------------------------------
  // Cyan: nothing else on the map uses it (buildings are category-coloured,
  // find-item highlights pink, the editor ghost amber), so a computed network
  // reads as its own thing at a glance. Every link is drawn twice -- a dark
  // casing underneath, the bright line on top -- which is what keeps it
  // legible over pale foundations as well as over dark terrain.
  var LINK_COLOR = "#25e0ff";
  var LINK_CASING = "#04212b";
  var LINK_WEIGHT = 3;
  var LINK_CASING_WEIGHT = 6;
  var LINK_HOVER_WEIGHT = 6;
  var POINT_COLOR = "#25e0ff";
  var POINT_RADIUS = 5;
  // The destination is one point among hundreds, and every trip number in the
  // panel is measured from it -- so it gets its own mark: a filled core in
  // the tool's cyan inside a white ring, twice the size of an ordinary point.
  var DESTINATION_RADIUS = 9;
  // Above this many points the tree is recomputed when the slider is
  // RELEASED rather than on every drag frame. The maths stays in the tens of
  // milliseconds either way; it is redrawing tens of thousands of canvas
  // paths per frame that would make the drag feel like tar.
  var LIVE_RECOMPUTE_MAX_POINTS = 2000;
  var HIT_TOLERANCE_PX = 12; // Click-to-remove radius around a placed point.
  // Above this many links the dark casing is dropped: at that size the extra
  // path per link costs more than the legibility it buys, and the network is
  // dense enough to read on its own.
  var CASING_MAX_LINKS = 3000;
  // How many rows the point list renders. The tree itself is happily computed
  // over tens of thousands of points (adding a whole factory's selection is a
  // real use), but building a DOM row for each is what would actually freeze
  // the tab -- and nobody scrolls 29,000 rows. Everything past the cap is
  // still on the map and in "Copy points & links".
  var LIST_ROW_LIMIT = 200;

  // ---- State -----------------------------------------------------------------

  var points = [];      // {x, y, z, label, sourceKey} -- x/y map pixels, z metres or null
  var destination = -1; // Index into points, or -1 for "just minimise length".
  // 0 = shortest possible network (a minimum spanning tree, exactly as with
  // no destination), 1 = shortest possible trips (every point joined straight
  // to the destination). See emst.js's primDijkstra for what the values in
  // between mean -- and do not mean.
  var alpha = 0;
  // sourceKey of every point that came from a real object, so the same
  // object can never be added twice (see addObjectPoints). Free points have
  // no key and are not deduplicated -- two clicks on empty map a metre apart
  // are two deliberate points.
  var sourceKeys = Object.create(null);
  var flashIndex = -1;  // Point briefly called out after a duplicate click.
  var flashTimer = null;
  var mode = "euclidean";
  var result = null;    // {mode, edges: [{a, b, len}], total, ms}
  var picking = false;
  var open = false;
  var hoveringLink = false; // Suppresses the map's own hover tooltip (see map.js).

  var dom = null;       // Resolved on first open.
  var renderer = null;  // Shared canvas renderer for points + links.
  var pointLayer = null;
  var linkLayer = null;
  var linkPaths = [];   // Parallel to result.edges: {main, casing}

  // ---- DOM -------------------------------------------------------------------

  // Crosshair-in-a-circle: the "route everything here" button on a point row.
  // Inline (not an <img>) so its stroke follows the button's own colour and
  // lights up with the row's destination state for free.
  var TARGET_SVG =
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="15" height="15">' +
    '<circle cx="12" cy="12" r="7" fill="none" stroke="currentColor" stroke-width="1.8"/>' +
    '<circle cx="12" cy="12" r="2.2" fill="currentColor"/>' +
    '<path d="M12 2v3M12 19v3M2 12h3M19 12h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>' +
    '</svg>';

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function ensureDom() {
    if (dom) {
      return;
    }
    dom = {
      panel: document.getElementById("networkPanel"),
      close: document.getElementById("networkPanelClose"),
      back: document.getElementById("networkPanelBack"),
      modeStraight: document.getElementById("networkModeStraight"),
      modeAxis: document.getElementById("networkModeAxis"),
      modeHint: document.getElementById("networkModeHint"),
      pickBtn: document.getElementById("networkPickBtn"),
      selectionBtn: document.getElementById("networkSelectionBtn"),
      coordX: document.getElementById("networkCoordX"),
      coordY: document.getElementById("networkCoordY"),
      coordAdd: document.getElementById("networkCoordAdd"),
      pointList: document.getElementById("networkPointList"),
      pointCount: document.getElementById("networkPointCount"),
      destinationRow: document.getElementById("networkDestinationRow"),
      destinationNone: document.getElementById("networkDestinationNone"),
      destinationName: document.getElementById("networkDestinationName"),
      destinationClear: document.getElementById("networkDestinationClear"),
      priority: document.getElementById("networkPriority"),
      alphaSlider: document.getElementById("networkAlpha"),
      alphaNote: document.getElementById("networkAlphaNote"),
      computeBtn: document.getElementById("networkComputeBtn"),
      clearBtn: document.getElementById("networkClearBtn"),
      resultBox: document.getElementById("networkResult"),
      summary: document.getElementById("networkSummary"),
      copyBtn: document.getElementById("networkCopyBtn"),
      hint: document.getElementById("networkHint"),
    };

    dom.close.addEventListener("click", function() { NetworkTool.close(); });
    // The dock swapping another tool in (the Tools tab, paste) closes this
    // one properly -- map-click handler and drawn layers included.
    Panels.registerTool(dom.panel, function() { NetworkTool.close(); });
    dom.back.addEventListener("click", function() {
      NetworkTool.close();
      if (window.Tools) {
        Tools.openLauncher();
      }
    });
    dom.modeStraight.addEventListener("click", function() { setMode("euclidean"); });
    dom.modeAxis.addEventListener("click", function() { setMode("rectilinear"); });
    dom.pickBtn.addEventListener("click", function() { setPicking(!picking); });
    dom.selectionBtn.addEventListener("click", addFromSelection);
    dom.coordAdd.addEventListener("click", addFromCoordInputs);
    [dom.coordX, dom.coordY].forEach(function(input) {
      input.addEventListener("keydown", function(e) {
        if (e.key === "Enter") {
          addFromCoordInputs();
        }
      });
    });
    dom.computeBtn.addEventListener("click", compute);
    dom.clearBtn.addEventListener("click", clearAll);
    dom.copyBtn.addEventListener("click", copyToClipboard);
    dom.destinationClear.addEventListener("click", function() { setDestination(-1); });
    // "input" follows the thumb, "change" fires once on release: a big point
    // set only recomputes on release (see LIVE_RECOMPUTE_MAX_POINTS), but the
    // caption under the slider always tracks the thumb.
    dom.alphaSlider.addEventListener("input", function() {
      setAlpha(parseInt(dom.alphaSlider.value, 10) / 100, points.length <= LIVE_RECOMPUTE_MAX_POINTS);
    });
    dom.alphaSlider.addEventListener("change", function() {
      setAlpha(parseInt(dom.alphaSlider.value, 10) / 100, true);
    });
  }

  // ---- Map layers -------------------------------------------------------------

  function ensureLayers() {
    if (renderer) {
      return;
    }
    // One shared canvas renderer rather than Leaflet's default SVG: a network
    // over a few thousand points is a few thousand paths, which SVG turns
    // into a few thousand DOM nodes to re-layout on every pan.
    renderer = L.canvas({ padding: 0.5 });
    pointLayer = L.layerGroup().addTo(MapApp.map);
    linkLayer = L.layerGroup().addTo(MapApp.map);
  }

  function removeLayers() {
    if (!renderer) {
      return;
    }
    MapApp.map.removeLayer(pointLayer);
    MapApp.map.removeLayer(linkLayer);
    pointLayer = null;
    linkLayer = null;
    linkPaths = [];
    renderer = null;
  }

  // ---- Points ------------------------------------------------------------------

  function addPoint(x, y, z, label, sourceKey) {
    points.push({
      x: x,
      y: y,
      z: typeof z === "number" && isFinite(z) ? z : null,
      label: label || null,
      sourceKey: sourceKey || null,
    });
    if (sourceKey) {
      sourceKeys[sourceKey] = points.length - 1;
    }
  }

  // Adds every object that is not already a point, and returns how many were
  // dropped as duplicates plus where the first duplicate already sits (so a
  // click that adds nothing can still say so).
  //
  // Objects are keyed by their save id where they have one (stable across a
  // reload of the same save) and by bucket+index otherwise. Position is
  // deliberately NOT the key: clicking the far corner of a 100m foundation
  // lands nowhere near the point already placed at its centre, which is
  // exactly the case where the same object used to be added twice.
  function addObjectPoints(entries) {
    var added = 0;
    var duplicates = 0;
    var firstDuplicate = -1;
    entries.forEach(function(entry) {
      if (entry.key && sourceKeys[entry.key] !== undefined) {
        duplicates++;
        if (firstDuplicate === -1) {
          firstDuplicate = sourceKeys[entry.key];
        }
        return;
      }
      addPoint(entry.x, entry.y, entry.z, entry.label, entry.key);
      added++;
    });
    return { added: added, duplicates: duplicates, firstDuplicate: firstDuplicate };
  }

  // sourceKeys stores list positions, so it is rebuilt whenever those shift.
  function reindexSourceKeys() {
    sourceKeys = Object.create(null);
    points.forEach(function(point, index) {
      if (point.sourceKey) {
        sourceKeys[point.sourceKey] = index;
      }
    });
  }

  // ---- Destination ---------------------------------------------------------

  function setDestination(index) {
    destination = index === destination ? -1 : index; // The same point again unsets it.
    if (destination === -1) {
      alpha = 0; // Nothing left to trade against, so back to plain shortest network.
    }
    if (result) {
      compute();
    }
    refresh();
  }

  function setAlpha(next, recompute) {
    alpha = next;
    refreshAlphaNote();
    if (recompute && result) {
      compute();
    }
  }

  // Says what this slider position means in words. The ends are the two
  // provable optima; the middle is a trade, and is described as one rather
  // than as an "optimal" anything.
  function refreshAlphaNote() {
    if (alpha === 0) {
      dom.alphaNote.textContent = "Shortest possible network. Trips through it can be long.";
    } else if (alpha === 1) {
      dom.alphaNote.textContent = "Shortest possible trips: every point joins the destination directly.";
    } else {
      dom.alphaNote.textContent = Math.round(alpha * 100)
        + "% toward shorter trips — some network length traded for shorter trips.";
    }
  }

  function removePoint(index) {
    points.splice(index, 1);
    reindexSourceKeys();
    if (index === destination) {
      destination = -1;
      alpha = 0;
    } else if (index < destination) {
      destination--; // Everything after the hole shifted down by one.
    }
    // The tree described a set of points that no longer exists -- recompute it
    // so the map never shows links to a point that has been taken away.
    if (result) {
      if (points.length > 1) {
        compute();
      } else {
        clearResult();
      }
    }
    refresh();
  }

  function clearAll() {
    points = [];
    sourceKeys = Object.create(null);
    destination = -1;
    alpha = 0;
    clearResult();
    refresh();
  }

  function addFromSelection() {
    var selection = window.SelectionTool && SelectionTool.currentPoints
      ? SelectionTool.currentPoints() : [];
    if (selection.length === 0) {
      return;
    }
    // Adding the same selection twice (or a selection overlapping one
    // already added) tops the list up rather than doubling it.
    var outcome = addObjectPoints(selection);
    if (outcome.added === 0) {
      if (outcome.firstDuplicate !== -1) {
        flashPoint(outcome.firstDuplicate);
      }
      return;
    }
    afterPointsChanged();
  }

  function addFromCoordInputs() {
    var mx = parseFloat(dom.coordX.value);
    var my = parseFloat(dom.coordY.value);
    if (!isFinite(mx) || !isFinite(my)) {
      return;
    }
    var px = worldMetersToMapPx(mx, my);
    addPoint(px[0], px[1], null, null);
    dom.coordX.value = "";
    dom.coordY.value = "";
    dom.coordX.focus();
    afterPointsChanged();
  }

  // Every point-set change funnels through here: a tree already on screen is
  // recomputed rather than left describing a stale set of points.
  function afterPointsChanged() {
    if (result) {
      compute();
    }
    refresh();
  }

  // ---- Picking on the map --------------------------------------------------------

  function setPicking(on) {
    picking = on;
    if (open) {
      ensureDom();
      dom.pickBtn.classList.toggle("active", picking);
      dom.hint.style.display = picking ? "block" : "none";
      document.getElementById("map").style.cursor = picking ? "crosshair" : "";
      positionHint();
    }
  }

  // The hint bar and the selection panel both want the bottom-centre of the
  // map (so does the editor's placement hint -- but that one never coexists
  // with a selection, since placing consumes it). This one genuinely does
  // coexist: selecting objects to bulk-add is a normal thing to do while
  // still picking. So when the selection panel is up, the hint sits directly
  // above it instead of on top of it.
  var HINT_BOTTOM = 22; // Matches #networkHint's own bottom in map.css.

  function positionHint() {
    if (!dom || dom.hint.style.display === "none") {
      return;
    }
    var panel = document.getElementById("selectionPanel");
    var stacked = panel && panel.style.display !== "none" && panel.offsetHeight > 0;
    dom.hint.style.bottom = stacked ? (HINT_BOTTOM + panel.offsetHeight + 10) + "px" : "";
  }

  // The placed point under a screen position, or -1. Points are few and the
  // check is a handful of arithmetic each, so a plain scan beats any index.
  function pointAtContainerPoint(containerPoint) {
    var best = -1;
    var bestDistance = HIT_TOLERANCE_PX;
    for (var i = 0; i < points.length; i++) {
      var screen = MapApp.map.latLngToContainerPoint([points[i].y, points[i].x]);
      var distance = Math.hypot(screen.x - containerPoint.x, screen.y - containerPoint.y);
      if (distance <= bestDistance) {
        bestDistance = distance;
        best = i;
      }
    }
    return best;
  }

  function onMapClick(e) {
    if (!picking) {
      return;
    }
    // Clicking a point you already placed takes it back -- the same gesture
    // that put it there, which is what makes the tool feel like a sketch pad
    // rather than a form.
    var existing = pointAtContainerPoint(e.containerPoint);
    if (existing !== -1) {
      removePoint(existing);
      return;
    }
    // Otherwise: whatever object is under the cursor, and its real
    // coordinates -- or, over empty map, the clicked spot itself.
    var tolerance = 8 / Math.pow(2, MapApp.map.getZoom());
    var hit = MapApp.layer ? MapApp.layer.hitTest(e.latlng.lng, e.latlng.lat, tolerance) : null;
    if (hit) {
      var placed = objectPoint(hit);
      if (placed) {
        var outcome = addObjectPoints([placed]);
        if (outcome.added === 0) {
          // Already a point: clicking the object again takes it back, exactly
          // like clicking its dot does. Never a second copy of the same
          // object -- and clicking a 100m foundation's far corner counts as
          // the same object, which clicking its dot could not.
          removePoint(outcome.firstDuplicate);
          return;
        }
        afterPointsChanged();
        return;
      }
    }
    addPoint(e.latlng.lng, e.latlng.lat, null, null, null);
    afterPointsChanged();
  }

  // Briefly enlarges a point on the map and pulses its row in the list.
  function flashPoint(index) {
    if (index < 0 || index >= points.length) {
      return;
    }
    flashIndex = index;
    drawPoints();
    var row = index < LIST_ROW_LIMIT ? dom.pointList.children[index] : null;
    if (row) {
      row.classList.remove("flash");
      void row.offsetWidth; // Restart the animation even on a repeat click.
      row.classList.add("flash");
      if (row.scrollIntoView) {
        row.scrollIntoView({ block: "nearest" });
      }
    }
    if (flashTimer) {
      clearTimeout(flashTimer);
    }
    flashTimer = setTimeout(function() {
      flashTimer = null;
      flashIndex = -1;
      if (open) {
        drawPoints();
        if (row) {
          row.classList.remove("flash");
        }
      }
    }, 1000);
  }

  // A hit's own plotted position, straight out of the bucket it belongs to --
  // never the click position, so "add this miner" adds the miner's exact spot.
  // `key` identifies the object for the duplicate check: its save id when it
  // has one (which survives a reload of the same save), bucket + index
  // otherwise.
  function objectPoint(hit) {
    var bucket = hit.bucket;
    var stride = bucket.pointStride;
    var key = hit.id || (bucket.key + "#" + hit.index);
    if (bucket.renderType === "line") {
      var line = bucket.lines && bucket.lines[hit.index];
      if (!line) {
        return null;
      }
      return { x: line[0], y: line[1], z: line[stride - 1], label: bucket.label, key: key };
    }
    if (!bucket.points) {
      return null;
    }
    var base = hit.index * stride;
    return {
      x: bucket.points[base],
      y: bucket.points[base + 1],
      z: bucket.points[base + stride - 1],
      label: bucket.label,
      key: key,
    };
  }

  // ---- Computing -------------------------------------------------------------------

  function setMode(next) {
    if (mode === next) {
      return;
    }
    mode = next;
    ensureDom();
    dom.modeStraight.classList.toggle("active", mode === "euclidean");
    dom.modeAxis.classList.toggle("active", mode === "rectilinear");
    dom.modeHint.textContent = mode === "rectilinear"
      ? "Every link runs along X then Y. Shortest such network (rectilinear MST) — a different tree, not the straight one with corners."
      : "Shortest point-to-point network (Euclidean MST).";
    if (result) {
      compute(); // The other metric is a different tree, so re-solve rather than redraw.
    }
  }

  function clearResult() {
    result = null;
    linkPaths = [];
    if (linkLayer) {
      linkLayer.clearLayers();
    }
    if (dom) {
      dom.resultBox.style.display = "none";
      dom.copyBtn.style.display = "none"; // Lives in the pinned footer, not in the result box.
    }
  }

  function compute() {
    ensureDom();
    if (points.length < 2) {
      clearResult();
      refresh();
      return;
    }
    var xs = new Array(points.length);
    var ys = new Array(points.length);
    for (var i = 0; i < points.length; i++) {
      xs[i] = points[i].x;
      ys[i] = points[i].y;
    }
    var options = destination === -1 ? null : { root: destination, alpha: alpha };
    var started = performance.now();
    var tree = mode === "rectilinear"
      ? Emst.rectilinear(xs, ys, options)
      : Emst.euclidean(xs, ys, options);
    var elapsed = performance.now() - started;
    // Sorted longest first, which numbers the links by how much they matter:
    // "Link 1" in a hover tooltip (and the first rows of a copied export) is
    // the longest haul in the network, the one a factory plan lives or dies by.
    var edges = tree.edges.slice().sort(function(a, b) { return b.len - a.len; });
    result = {
      mode: mode,
      edges: edges,
      total: tree.total,
      ms: elapsed,
      destination: destination,
      alpha: alpha,
      paths: tree.paths,
      pathTotal: tree.pathTotal,
      directTotal: tree.directTotal,
    };
    drawResult();
    renderResultPanel();
  }

  // The two legs of a rectilinear link. Both possible L routings (across then
  // up, or up then across) have exactly the same length, so the corner is
  // picked deterministically -- horizontal leg first, from the left-hand end
  // -- and a recompute never reshuffles a network the user has been reading.
  function cornerFor(a, b) {
    var left = a.x <= b.x ? a : b;
    var right = left === a ? b : a;
    return { x: right.x, y: left.y, first: left, second: right };
  }

  function latLngsFor(edge) {
    var a = points[edge.a];
    var b = points[edge.b];
    if (result.mode !== "rectilinear") {
      return [[a.y, a.x], [b.y, b.x]];
    }
    var corner = cornerFor(a, b);
    return [[corner.first.y, corner.first.x], [corner.y, corner.x], [corner.second.y, corner.second.x]];
  }

  function drawResult() {
    ensureLayers();
    linkLayer.clearLayers();
    linkPaths = [];
    var withCasing = result.edges.length <= CASING_MAX_LINKS;
    result.edges.forEach(function(edge, index) {
      if (edge.len === 0) {
        linkPaths.push(null); // Two points on the same spot: nothing to draw.
        return;
      }
      var latLngs = latLngsFor(edge);
      var casing = null;
      if (withCasing) {
        casing = L.polyline(latLngs, {
          renderer: renderer, color: LINK_CASING, weight: LINK_CASING_WEIGHT,
          opacity: 0.85, interactive: false, lineJoin: "round", lineCap: "round",
        }).addTo(linkLayer);
      }
      var main = L.polyline(latLngs, {
        renderer: renderer, color: LINK_COLOR, weight: LINK_WEIGHT,
        opacity: 1, lineJoin: "round", lineCap: "round",
      }).addTo(linkLayer);
      main.bindTooltip(linkTooltipHtml(edge, index), { sticky: true, className: "networkLinkTooltip" });
      main.on("mouseover", function() {
        hoveringLink = true;
        if (window.Tooltip) {
          window.Tooltip.hide(); // Only one tooltip at a time (see map.js's hover guard).
        }
        highlightLink(index, true);
      });
      main.on("mouseout", function() {
        hoveringLink = false;
        highlightLink(index, false);
      });
      linkPaths.push({ main: main, casing: casing });
    });
  }

  // Pointer over a link: brighten it and lift it above its neighbours, so the
  // one the tooltip is describing is unmistakable in a dense network.
  function highlightLink(index, on) {
    var path = linkPaths[index];
    if (!path) {
      return;
    }
    path.main.setStyle({ weight: on ? LINK_HOVER_WEIGHT : LINK_WEIGHT, color: on ? "#ffffff" : LINK_COLOR });
    if (on) {
      path.main.bringToFront();
    }
  }

  function drawPoints() {
    ensureLayers();
    pointLayer.clearLayers();
    points.forEach(function(point, index) {
      var flashing = index === flashIndex;
      var isDestination = index === destination;
      L.circleMarker([point.y, point.x], {
        renderer: renderer,
        radius: flashing ? POINT_RADIUS * 2.2 : (isDestination ? DESTINATION_RADIUS : POINT_RADIUS),
        color: flashing || isDestination ? "#ffffff" : "#04212b",
        weight: flashing ? 3 : (isDestination ? 3 : 2),
        fillColor: POINT_COLOR,
        fillOpacity: 1,
        interactive: false, // Clicks are resolved by pointAtContainerPoint instead.
      }).addTo(pointLayer);
      if (isDestination) {
        // A dark core inside the cyan disc, so the destination reads as a
        // target rather than just "a bigger dot".
        L.circleMarker([point.y, point.x], {
          renderer: renderer,
          radius: POINT_RADIUS * 0.6,
          stroke: false,
          fillColor: "#04212b",
          fillOpacity: 1,
          interactive: false,
        }).addTo(pointLayer);
      }
    });
  }

  // ---- Panel rendering ---------------------------------------------------------------

  // Metres up to a kilometre-ish, kilometres past it. Summed trip distances
  // over a few thousand points run to millions of metres, where a raw metre
  // count is a wall of digits nobody reads.
  function formatMeters(meters) {
    var rounded = Math.round(meters);
    if (Math.abs(rounded) < 10000) {
      return rounded.toLocaleString() + " m";
    }
    var km = rounded / 1000;
    if (Math.abs(km) >= 1000) {
      return Math.round(km).toLocaleString() + " km";
    }
    return km.toFixed(1) + " km";
  }

  function pointCoordinates(point) {
    var world = mapPxToWorldMeters(point.x, point.y);
    return Math.round(world[0]).toLocaleString() + ", " + Math.round(world[1]).toLocaleString();
  }

  function pointLabel(point, index) {
    return point.label || "Point " + (index + 1);
  }

  // Says outright what the list is not showing, rather than quietly ending at
  // row 200 as if that were everything.
  function appendOverflowRow(list, total, noun) {
    if (total <= LIST_ROW_LIMIT) {
      return;
    }
    var hidden = total - LIST_ROW_LIMIT;
    list.appendChild(el("div", "networkOverflowRow",
      "… and " + hidden.toLocaleString() + " more " + noun + (hidden === 1 ? "" : "s")
      + " (on the map, and in Copy)"));
  }

  function refresh() {
    ensureDom();
    drawPoints();

    dom.pointCount.textContent = points.length === 0
      ? "none yet"
      : points.length.toLocaleString() + (points.length === 1 ? " point" : " points");

    dom.pointList.innerHTML = "";
    points.slice(0, LIST_ROW_LIMIT).forEach(function(point, index) {
      var isDestination = index === destination;
      var row = el("div", "row row-hover networkRow" + (isDestination ? " isDestination" : ""));
      row.appendChild(el("span", "networkRowIndex", String(index + 1)));
      var text = el("div", "networkRowText");
      text.appendChild(el("span", "row-label networkRowLabel", pointLabel(point, index)));
      var coordinates = pointCoordinates(point)
        + (point.z === null ? "" : " · " + Math.round(point.z).toLocaleString() + " m up");
      text.appendChild(el("span", "row-meta", coordinates));
      row.appendChild(text);
      var pick = el("button", "btn btn-ghost btn-sm btn-icon networkRowDestination");
      pick.innerHTML = TARGET_SVG;
      pick.title = isDestination ? "Stop routing to this point" : "Make this the destination";
      pick.addEventListener("click", function() { setDestination(index); });
      row.appendChild(pick);
      var remove = el("button", "btn btn-ghost btn-sm btn-icon networkRowRemove", "×");
      remove.title = "Remove this point";
      remove.addEventListener("click", function() { removePoint(index); });
      row.appendChild(remove);
      dom.pointList.appendChild(row);
    });
    appendOverflowRow(dom.pointList, points.length, "point");

    var hasDestination = destination >= 0 && destination < points.length;
    dom.destinationNone.style.display = hasDestination ? "none" : "block";
    dom.destinationName.style.display = hasDestination ? "flex" : "none";
    dom.destinationClear.style.display = hasDestination ? "block" : "none";
    dom.priority.style.display = hasDestination ? "block" : "none";
    if (hasDestination) {
      dom.destinationName.textContent =
        (destination + 1) + " · " + pointLabel(points[destination], destination);
      dom.alphaSlider.value = String(Math.round(alpha * 100));
      refreshAlphaNote();
    }

    dom.computeBtn.disabled = points.length < 2;
    dom.computeBtn.title = points.length < 2 ? "Add at least two points" : "";
    dom.clearBtn.disabled = points.length === 0;
    refreshSelectionButton();
  }

  function refreshSelectionButton() {
    if (!dom) {
      return;
    }
    var count = window.SelectionTool && SelectionTool.selectedCount
      ? SelectionTool.selectedCount() : 0;
    dom.selectionBtn.disabled = count === 0;
    dom.selectionBtn.textContent = count === 0
      ? "Add selection"
      : "Add selection (" + count.toLocaleString() + ")";
    dom.selectionBtn.title = count === 0
      ? "Right-drag the map to select objects first"
      : "Add every selected object as a point";
  }

  function linkTooltipHtml(edge, index) {
    var a = points[edge.a];
    var b = points[edge.b];
    var lines = [
      '<div class="networkTooltipTitle">Link ' + (index + 1) + ' · ' + formatMeters(edge.len * METERS_PER_PIXEL) + '</div>',
    ];
    if (result.mode === "rectilinear") {
      var alongX = Math.abs(a.x - b.x) * METERS_PER_PIXEL;
      var alongY = Math.abs(a.y - b.y) * METERS_PER_PIXEL;
      lines.push('<div class="networkTooltipLeg">' + formatMeters(alongX) + ' along X + '
        + formatMeters(alongY) + ' along Y</div>');
    }
    // Endpoints carry their point number, not just their name: a dozen ore
    // nodes are all called the same thing, and the number is what ties the
    // link back to the point list and to a copied export.
    lines.push('<div class="networkTooltipRow"><span>From</span>' + (edge.a + 1) + ' · '
      + escapeHtml(pointLabel(a, edge.a)) + ' (' + pointCoordinates(a) + ')</div>');
    lines.push('<div class="networkTooltipRow"><span>To</span>' + (edge.b + 1) + ' · '
      + escapeHtml(pointLabel(b, edge.b)) + ' (' + pointCoordinates(b) + ')</div>');
    return lines.join("");
  }

  function escapeHtml(text) {
    return String(text).replace(/[&<>"]/g, function(character) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[character];
    });
  }

  function renderResultPanel() {
    var totalMeters = result.total * METERS_PER_PIXEL;
    dom.summary.innerHTML = "";
    var headline = el("div", "networkSummaryTotal", formatMeters(totalMeters));
    dom.summary.appendChild(headline);
    dom.summary.appendChild(el("div", "networkSummaryDetail",
      result.edges.length.toLocaleString() + (result.edges.length === 1 ? " link over " : " links over ")
      + points.length.toLocaleString() + " points · "
      + (result.mode === "rectilinear" ? "X/Y only" : "straight")
      + " · solved in " + (result.ms < 1 ? "<1" : Math.round(result.ms)) + " ms"));

    // With a destination, the number being traded away is just as important
    // as the headline, so it gets its own line: what everyone's journeys add
    // up to, and how much of that is detour rather than distance.
    if (result.destination >= 0 && result.paths) {
      var others = Math.max(1, points.length - 1);
      var detour = result.directTotal > 0 ? result.pathTotal / result.directTotal - 1 : 0;
      var trips = el("div", "networkSummaryTrips");
      trips.appendChild(el("span", "networkTripsTotal",
        formatMeters(result.pathTotal * METERS_PER_PIXEL)));
      trips.appendChild(el("span", "networkTripsLabel",
        " of travel to " + pointLabel(points[result.destination], result.destination)));
      dom.summary.appendChild(trips);
      dom.summary.appendChild(el("div", "networkSummaryDetail",
        formatMeters(result.pathTotal / others * METERS_PER_PIXEL) + " average trip · "
        + (detour < 0.005
            ? "no detour — every point joins it directly"
            : "+" + Math.round(detour * 100) + "% over going direct")));
    }

    // No per-link list here on purpose: a network of any size is hundreds of
    // near-identical rows, which cost most of the panel's height and tell you
    // less than the links themselves do. Each link's length and endpoints are
    // on the map, in its hover tooltip; the full set is in Copy.
    dom.resultBox.style.display = "flex";
    dom.copyBtn.style.display = "block";
    dom.copyBtn.textContent = "Copy points & links";
  }

  // ---- Copy --------------------------------------------------------------------------

  function copyToClipboard() {
    var lines = [];
    var routed = result && result.destination >= 0 && result.paths;
    lines.push("Optimal network finder (EMST) — "
      + (result && result.mode === "rectilinear" ? "X/Y only (rectilinear MST)" : "straight (Euclidean MST)"));
    if (routed) {
      lines.push("Destination: " + pointLabel(points[result.destination], result.destination)
        + " (point " + (result.destination + 1) + ") — "
        + Math.round(result.alpha * 100) + "% toward shorter trips");
    }
    // Tab separated with a header row on each table, so it pastes straight
    // into a spreadsheet.
    lines.push("");
    lines.push("Points");
    lines.push(["#", "Name", "X (m)", "Y (m)", "Altitude (m)"]
      .concat(routed ? ["Trip to destination (m)"] : []).join("\t"));
    points.forEach(function(point, index) {
      var world = mapPxToWorldMeters(point.x, point.y);
      var row = [index + 1, pointLabel(point, index), Math.round(world[0]), Math.round(world[1]),
        point.z === null ? "" : Math.round(point.z)];
      if (routed) {
        row.push(Math.round(result.paths[index] * METERS_PER_PIXEL));
      }
      lines.push(row.join("\t"));
    });
    if (result) {
      lines.push("");
      // The prose lines read like the panel does (km once the numbers get
      // long); only the table cells stay raw metres, for pasting into a
      // spreadsheet and doing arithmetic on.
      if (routed) {
        lines.push("Total travel to destination: "
          + formatMeters(result.pathTotal * METERS_PER_PIXEL)
          + " (going direct would be " + formatMeters(result.directTotal * METERS_PER_PIXEL) + ")");
      }
      lines.push("Links — total " + formatMeters(result.total * METERS_PER_PIXEL));
      // Endpoints are the POINT NUMBERS from the table above, plus their
      // coordinates. Names alone are useless here: a network over sixteen
      // coal nodes is sixteen rows all reading "Coal Ore -> Coal Ore". The
      // leading # is the same link number its hover tooltip shows on the map.
      lines.push(["#", "From #", "To #", "Length (m)", "From X", "From Y", "To X", "To Y"].join("\t"));
      result.edges.forEach(function(edge, index) {
        var from = mapPxToWorldMeters(points[edge.a].x, points[edge.a].y);
        var to = mapPxToWorldMeters(points[edge.b].x, points[edge.b].y);
        lines.push([index + 1, edge.a + 1, edge.b + 1, Math.round(edge.len * METERS_PER_PIXEL),
          Math.round(from[0]), Math.round(from[1]),
          Math.round(to[0]), Math.round(to[1])].join("\t"));
      });
    }
    var text = lines.join("\n");

    function done(ok) {
      dom.copyBtn.textContent = ok ? "Copied" : "Copy failed";
      setTimeout(function() { dom.copyBtn.textContent = "Copy points & links"; }, 1600);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(function() { done(true); }, function() { done(fallbackCopy(text)); });
      return;
    }
    done(fallbackCopy(text));
  }

  // execCommand("copy") off a temporary textarea -- the only path left when
  // the async clipboard API is unavailable or refused (insecure context, no
  // permission).
  function fallbackCopy(text) {
    var area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    var ok = false;
    try {
      ok = document.execCommand("copy");
    } catch (e) {
      ok = false;
    }
    document.body.removeChild(area);
    return ok;
  }

  // ---- Open / close --------------------------------------------------------------------

  NetworkTool.open = function() {
    ensureDom();
    Panels.openTool(dom.panel); // Into the right tool dock -- see panels.js.
    if (open) {
      return;
    }
    open = true;
    ensureLayers();
    MapApp.map.on("click", onMapClick);
    // Picking starts OFF: opening a panel should not quietly take over what a
    // left-click on the map does. "Click map to add" arms it.
    setPicking(false);
    refresh();
    if (result) {
      drawResult();
      renderResultPanel();
    }
  };

  NetworkTool.close = function() {
    if (!open) {
      return;
    }
    setPicking(false); // Before open flips: setPicking only touches the UI while open.
    open = false;
    Panels.closeTool(dom.panel);
    MapApp.map.off("click", onMapClick);
    hoveringLink = false;
    // The points and the computed tree are kept in memory: reopening from the
    // search bar brings the same network back rather than starting over.
    removeLayers();
  };

  NetworkTool.isOpen = function() { return open; };
  NetworkTool.isPicking = function() { return picking; };
  // map.js checks this so its own hover tooltip does not fight the link
  // tooltip for the same cursor position.
  NetworkTool.isHoveringLink = function() { return hoveringLink; };
  // selection.js calls this whenever the rectangle selection changes, so the
  // "Add selection (N)" button always says what it will actually add.
  NetworkTool.onSelectionChanged = function() {
    if (open) {
      refreshSelectionButton();
      positionHint(); // The selection panel just appeared or went away underneath it.
    }
  };

  // Two layers, popped one press at a time (see ui.js's UI.onEscape): stop
  // placing points first, close the tool only on a second Escape.
  UI.onEscape(UI.LAYER.placement, function() {
    if (!open || !picking) {
      return false;
    }
    setPicking(false);
    return true;
  });

  UI.onEscape(UI.LAYER.tool, function() {
    if (!open) {
      return false;
    }
    NetworkTool.close();
    return true;
  });
})();
