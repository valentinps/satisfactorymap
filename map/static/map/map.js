// Leaflet map setup + a custom canvas-based layer for rendering large numbers
// of points/lines without per-marker DOM/SVG overhead (this is the main fix
// for the lag experienced with marker-heavy web maps).
//
// Coordinate convention: every point is [px, py] in map_highres.png pixel
// space (0..MAP_SIZE), already projected server-side by sav_map_data.py's
// projectXY(). We address Leaflet positions as L.latLng(py, px) (i.e. treat
// py as "lat", px as "lng"), matching the imageOverlay bounds set up below,
// since Leaflet's CRS.Simple is an identity projection over whatever 2-axis
// space you give it.
//
// Bucket shapes (see filters.js for construction):
//   { renderType: "circle", pointStride: 3, points: Float32Array [x,y,z,...], color, visible }
//   { renderType: "icon",   pointStride: 3, points: Float32Array [x,y,z,...], color, visible, iconUrl, iconOpacity }
//     Same point layout as "circle" -- just drawn as a centered image (see
//     _drawIconBucket) instead of a filled dot, for collectables where the
//     actual item icon reads far more clearly than an abstract colored dot
//     (slugs/somersloops/mercer spheres/hard drives).
//   { renderType: "rect",   pointStride: 4, points: Float32Array [x,y,yaw,z,...], color, visible, footprintPixels: [halfWidth,halfDepth] }
//   { renderType: "line",   pointStride: 3 or 7, lines: Float32Array[] (one polyline per entry), color, visible }
//     stride 3: [x,y,z,...] per vertex -- plain straight segments (power lines).
//     stride 7: [x,y,arriveTangentX,arriveTangentY,leaveTangentX,leaveTangentY,z,...]
//       per vertex -- belts/pipelines/railroads/hypertubes. The tangents let
//       consecutive vertices be drawn as a real curve (cubic Hermite, via a
//       canvas bezier -- see _drawLineBucket) instead of a straight chord, so
//       in-game S-bends/curves actually look curved on the map.
//
// "z" (altitude in meters, used by MapApp.altitudeRange filtering -- see
// setAltitudeRange below) is always the LAST value of each stride-sized
// group, for every bucket type including lines.

var MapApp = {};

(function() {
  "use strict";

  // See _drawRectBuckets: full screen-space box size (in px) below which a
  // bucket skips rotated-quad rendering in favor of an axis-aligned rect of
  // the same screen dimensions. Rotation of a sub-4px blob is imperceptible,
  // while the rotated path costs sin/cos plus four corner computations per
  // building -- at the zoom levels where a whole megabase is in view (every
  // building 1-4px), that difference is the redraw. Axis-aligned rects also
  // stay in the canvas's fast-rect path representation and are eligible for
  // the per-pixel dedup (see _occEnsure).
  var SMALL_RECT_SCREEN_PX = 4;

  // See _drawRectBuckets: max altitude spread (in meters) among currently
  // visible rects below which fill order is treated as not mattering, so
  // the per-point sort is skipped. Comfortably smaller than one floor's
  // height (a default foundation/wall is 4m), so any genuine multi-floor
  // overlap still takes the sorted path, while a single coplanar floor's
  // minor snapping jitter doesn't.
  var FLAT_PLATFORM_Z_EPSILON = 1.0;

  // See _drawRectBucketsSorted: number of altitude quantization bins for
  // the O(n) counting sort that replaces a comparator sort over every
  // visible rect. 2048 bins over the visible z spread resolves draw order
  // to ~1m even across the full ~2km world altitude range (finer than one
  // 4m floor; a typical view spans far less, so bins are usually a few
  // cm). The sort key is (zBin, bucket), not raw z -- items within the
  // same bin draw grouped by bucket, so each (floor, category) pair
  // becomes ONE batched fill instead of a beginPath/fill per color change
  // as z-interleaved categories alternate.
  var Z_SORT_BINS = 2048;

  // See _reset: the bulk canvas is rendered with a margin around the
  // viewport (up to 2x viewport per axis) so that panning within the margin
  // needs NO redraw at all -- Leaflet's pane transform already moves the
  // canvas the right amount during/after a drag. Capped in total pixels so
  // a 4K fullscreen window doesn't allocate absurd canvas memory (20M px
  // RGBA == 80MB); the margin just shrinks on huge viewports.
  var BUFFER_MAX_PIXELS = 20e6;

  // See _onZoomEnd: how long after the last zoom step the real redraw runs.
  // In between, the existing buffer is scaled into place with a CSS
  // transform (compositor-only, no canvas work), so wheel-zooming across
  // several levels costs ONE full redraw at the end instead of one freeze
  // per notch.
  var ZOOM_REDRAW_DEBOUNCE_MS = 180;

  var BucketedCanvasLayer = L.Layer.extend({
    initialize: function() {
      this.buckets = []; // Public: filters.js pushes/reads bucket objects here directly.
      // Reused across redraws/hit-tests to avoid allocating a fresh array for
      // every bucket on every frame (see _collectGridIndices) -- safe since
      // everything here runs synchronously on the main thread, one bucket at
      // a time.
      this._scratchIndices = [];
      // Reusable visible-point snapshot filled by _drawRectBuckets each
      // redraw (see there) -- persisted across frames so a steady view
      // allocates nothing per redraw.
      this._cullCapacity = 0;
      this._cullZ = null;
      this._cullPointIdx = null;
      this._cullBucketIdx = null;
      this._orderArr = null;
      this._binCounts = null;
      // Screen-pixel occupancy buffer for subpixel-rect dedup (see
      // _occEnsure) -- lazily sized to the canvas.
      this._occ = null;
      this._occW = 0;
      this._occH = 0;
      this._occStamp = 0;
      this._resetFrame = null;
      // Buffered-rendering state (see _reset/_onMoveEnd/_onZoomEnd).
      this._renderedZoom = null;
      this._renderedPixelOrigin = null;
      this._viewTopLeft = null;
      this._bufferW = 0;
      this._bufferH = 0;
      this._cullBounds = null;
      this._zoomRedrawTimer = null;
    },

    clearBuckets: function() {
      this.buckets = [];
      this._sortedBuckets = null;
    },

    // The one supported way to remove a single bucket (finditem.js's
    // temporary highlight bucket). Reassigning `buckets` from outside
    // instead would leave the removed bucket alive in the _sortedBuckets
    // draw cache below -- it kept being drawn every frame (ghost pins that
    // didn't even hit-test, since hitTest walks the live array) until
    // something else happened to invalidate the cache.
    removeBucketByKey: function(key) {
      this.buckets = this.buckets.filter(function(bucket) { return bucket.key !== key; });
      this._sortedBuckets = null;
    },

    // Point-based buckets (circle/icon/rect) can hold tens to hundreds of
    // thousands of points; without an index, every redraw/hit-test has to
    // walk the *entire* points array just to reject the points outside the
    // current viewport, even when zoomed into a tiny fraction of the map --
    // that full-array walk is the actual cost behind "zoom freezes for a
    // few seconds". Building a coarse uniform grid once (here, when the
    // bucket is added) lets redraws/hit-tests touch only the cells that
    // overlap the current view. Line buckets get a cheap per-line bounding
    // box instead (see _drawLineBucket) since there are far fewer lines.
    addBucket: function(bucket) {
      if (bucket.points && bucket.points.length > 0 && (bucket.renderType === "circle" || bucket.renderType === "icon" || bucket.renderType === "rect")) {
        bucket._grid = _buildPointGrid(bucket.points, bucket.pointStride);
      } else if (bucket.lines && bucket.lines.length > 0) {
        bucket._lineBounds = _buildLineBounds(bucket.lines, bucket.pointStride);
      }
      this.buckets.push(bucket);
      this._sortedBuckets = null;
      return bucket;
    },

    onAdd: function(map) {
      this._map = map;
      this._canvas = L.DomUtil.create("canvas", "bucketed-canvas-layer");
      // A second, separate canvas just for the hovered/pinned highlight (see
      // _redrawHighlight): hovering changes far more often than the bulk
      // data does, and redrawing thousands of buildings on every hover tick
      // was the main source of the lag this was added to fix. Appended
      // after the main canvas so it paints on top in DOM order.
      this._highlightCanvas = L.DomUtil.create("canvas", "bucketed-canvas-layer");
      var size = map.getSize();
      this._canvas.width = size.x;
      this._canvas.height = size.y;
      this._highlightCanvas.width = size.x;
      this._highlightCanvas.height = size.y;
      this.getPane().appendChild(this._canvas);
      this.getPane().appendChild(this._highlightCanvas);

      map.on("zoomend", this._onZoomEnd, this);
      map.on("moveend", this._onMoveEnd, this);
      map.on("resize", this._requestReset, this);
      this._reset();
    },

    onRemove: function(map) {
      this.getPane().removeChild(this._canvas);
      this.getPane().removeChild(this._highlightCanvas);
      map.off("zoomend", this._onZoomEnd, this);
      map.off("moveend", this._onMoveEnd, this);
      map.off("resize", this._requestReset, this);
      if (this._resetFrame) {
        L.Util.cancelAnimFrame(this._resetFrame);
        this._resetFrame = null;
      }
      if (this._zoomRedrawTimer) {
        clearTimeout(this._zoomRedrawTimer);
        this._zoomRedrawTimer = null;
      }
      this._map = null;
    },

    // Coalesces the reset/redraw work to one per animation frame -- used
    // for anything that genuinely needs a full redraw right away (buffer
    // exceeded, resize, data/visibility changes).
    _requestReset: function() {
      if (this._resetFrame) {
        return;
      }
      var self = this;
      this._resetFrame = L.Util.requestAnimFrame(function() {
        self._resetFrame = null;
        self._reset();
      });
    },

    // Zoom changed: don't redraw hundreds of thousands of points per wheel
    // notch. Scale the already-rendered buffer into place as a preview
    // (cheap CSS transform) and debounce the one real redraw to after the
    // zoom gesture settles.
    _onZoomEnd: function() {
      var map = this._map;
      if (!map) {
        return;
      }
      if (this._renderedZoom === null) {
        this._requestReset();
        return;
      }
      if (map.getZoom() === this._renderedZoom) {
        return;
      }
      this._applyZoomPreview();
      if (this._zoomRedrawTimer) {
        clearTimeout(this._zoomRedrawTimer);
      }
      var self = this;
      this._zoomRedrawTimer = setTimeout(function() {
        self._zoomRedrawTimer = null;
        self._reset();
      }, ZOOM_REDRAW_DEBOUNCE_MS);
    },

    _onMoveEnd: function() {
      var map = this._map;
      if (!map) {
        return;
      }
      if (map.getZoom() !== this._renderedZoom) {
        return; // Mid-zoom flow: _onZoomEnd already previewed and scheduled the real redraw.
      }
      // The viewport-sized highlight canvas is screen-anchored, so a pan
      // has to re-glue it (cheap -- it holds at most one box).
      var viewTopLeft = map.containerPointToLayerPoint([0, 0]);
      this._viewTopLeft = viewTopLeft;
      L.DomUtil.setPosition(this._highlightCanvas, viewTopLeft);
      this._redrawHighlight();
      // Pan still inside the buffered margin: the bulk canvas is positioned
      // in layer space, and Leaflet's pane transform already moved it with
      // the drag -- nothing to redraw at all. Only when the viewport
      // escapes the rendered margin is a real redraw needed. Fully zoomed
      // out the buffer covers the whole map, so every pan is free.
      var size = map.getSize();
      if (this._topLeft &&
          viewTopLeft.x >= this._topLeft.x && viewTopLeft.y >= this._topLeft.y &&
          viewTopLeft.x + size.x <= this._topLeft.x + this._bufferW &&
          viewTopLeft.y + size.y <= this._topLeft.y + this._bufferH) {
        return;
      }
      this._requestReset();
    },

    // Places the last-rendered buffer where it belongs in the NEW zoom's
    // layer space: for CRS.Simple, layerNew = k*(layerOld + pixelOriginOld)
    // - pixelOriginNew with the bitmap scaled by k around that corner.
    // Pure compositor work -- no canvas commands at all.
    _applyZoomPreview: function() {
      var map = this._map;
      var k = map.getZoomScale(map.getZoom(), this._renderedZoom);
      var pixelOrigin = map.getPixelOrigin();
      var t = this._topLeft.add(this._renderedPixelOrigin).multiplyBy(k).subtract(pixelOrigin);
      this._canvas.style.transformOrigin = "0 0";
      this._canvas.style.transform = "translate3d(" + t.x + "px, " + t.y + "px, 0) scale(" + k + ")";
      // The highlight box would sit at the wrong spot mid-preview; hide it
      // until the real redraw repaints it (it holds at most one box).
      var hctx = this._highlightCanvas.getContext("2d");
      hctx.clearRect(0, 0, this._highlightCanvas.width, this._highlightCanvas.height);
    },

    requestRedraw: function() {
      if (this._map) {
        this._requestReset();
      }
    },

    // Cheap path used on every hover/pin change -- only touches the small
    // overlay canvas, never the full bucket redraw.
    requestHighlightRedraw: function() {
      if (this._map) {
        this._redrawHighlight();
      }
    },

    // Projects a map-pixel-space [x,y] to this frame's on-screen canvas coordinates.
    _toCanvas: function(x, y, zoom, pixelOrigin, topLeft) {
      var worldPoint = this._map.project([y, x], zoom);
      return [worldPoint.x - pixelOrigin.x - topLeft.x, worldPoint.y - pixelOrigin.y - topLeft.y];
    },

    _reset: function() {
      var map = this._map;
      if (!map) {
        return; // Layer was removed while a coalesced reset was pending.
      }
      if (this._zoomRedrawTimer) {
        clearTimeout(this._zoomRedrawTimer);
        this._zoomRedrawTimer = null;
      }
      var size = map.getSize();
      // Bulk canvas covers viewport + margin (see BUFFER_MAX_PIXELS); the
      // highlight canvas stays viewport-sized -- it redraws for free on
      // every hover anyway and doesn't need buffer-sized memory.
      var factor = Math.max(1, Math.min(2, Math.sqrt(BUFFER_MAX_PIXELS / Math.max(1, size.x * size.y))));
      var bufW = Math.round(size.x * factor);
      var bufH = Math.round(size.y * factor);
      var padX = Math.round((bufW - size.x) / 2);
      var padY = Math.round((bufH - size.y) / 2);
      var viewTopLeft = map.containerPointToLayerPoint([0, 0]);
      var bufferTopLeft = viewTopLeft.subtract(L.point(padX, padY));
      // setPosition rewrites the element's whole transform, which also
      // clears any zoom-preview scale left by _applyZoomPreview.
      L.DomUtil.setPosition(this._canvas, bufferTopLeft);
      L.DomUtil.setPosition(this._highlightCanvas, viewTopLeft);
      this._canvas.width = bufW;
      this._canvas.height = bufH;
      this._highlightCanvas.width = size.x;
      this._highlightCanvas.height = size.y;
      this._topLeft = bufferTopLeft;
      this._viewTopLeft = viewTopLeft;
      this._bufferW = bufW;
      this._bufferH = bufH;
      this._renderedZoom = map.getZoom();
      this._renderedPixelOrigin = map.getPixelOrigin();
      // Map-space bounds of the whole buffered canvas, for culling in
      // _redraw -- the plain viewport bounds would cull away the margin.
      var nw = map.layerPointToLatLng(bufferTopLeft);
      var se = map.layerPointToLatLng(L.point(bufferTopLeft.x + bufW, bufferTopLeft.y + bufH));
      this._cullBounds = {
        minX: Math.min(nw.lng, se.lng),
        maxX: Math.max(nw.lng, se.lng),
        minY: Math.min(nw.lat, se.lat),
        maxY: Math.max(nw.lat, se.lat),
      };
      this._redraw();
      this._redrawHighlight();
    },

    // CRS.Simple's projection is a pure uniform scale (no distortion/
    // rotation), so the whole map-pixel-space -> canvas-pixel-space mapping
    // for one frame is a single affine transform: canvasX = originX +
    // x*scaleX, canvasY = originY + y*scaleY. Computing that once per frame
    // here and applying it as plain arithmetic at every point (see the draw
    // functions below) replaces a map.project() call -- which allocates a
    // Leaflet Point object -- for every single point on every redraw. With
    // hundreds of thousands of points in view, that per-point allocation
    // was the dominant cost behind pan/zoom lag, well above the cost of the
    // grid lookups or the actual canvas drawing.
    _computeAffine: function(zoom, pixelOrigin, topLeft) {
      var origin = this._toCanvas(0, 0, zoom, pixelOrigin, topLeft);
      var unit = this._toCanvas(1, 1, zoom, pixelOrigin, topLeft);
      return { originX: origin[0], originY: origin[1], scaleX: unit[0] - origin[0], scaleY: unit[1] - origin[1] };
    },

    // Redraws just the currently hovered/pinned box (see MapApp.setHighlight)
    // at full opacity with a bright outline, on its own small canvas -- the
    // bulk canvas underneath is untouched, so this stays cheap regardless of
    // how many buildings are loaded.
    _redrawHighlight: function() {
      var map = this._map;
      if (!map) {
        return;
      }
      var ctx = this._highlightCanvas.getContext("2d");
      ctx.clearRect(0, 0, this._highlightCanvas.width, this._highlightCanvas.height);

      var bucket = MapApp.highlightedBucket;
      if (!bucket || !bucket.ids) {
        return;
      }
      var idx = bucket.ids.indexOf(MapApp.highlightedId);
      if (idx === -1) {
        return;
      }
      var zoom = map.getZoom();
      var pixelOrigin = map.getPixelOrigin();
      // The highlight canvas is viewport-anchored (see _reset/_onMoveEnd),
      // unlike the buffered bulk canvas -- so its affine uses _viewTopLeft.
      var affine = this._computeAffine(zoom, pixelOrigin, this._viewTopLeft);

      if (bucket.renderType === "rect") {
        this._strokeRectHighlight(ctx, bucket, idx, affine);
        // A vehicle's box highlight is a full-opacity fill drawn on top of
        // the bulk canvas -- zoomed in, the box visually contains its own
        // pin, so without this the pin vanished the moment the box lit up.
        // Redraw the companion pin over the fill (see filters.js's
        // buildVehiclesSection/buildTrainRow for which pin belongs to which
        // box; a train's pin only exists at its lead car).
        var companionPin = bucket.companionPinBucket;
        if (companionPin && companionPin.visible) {
          var pinIdx = bucket.companionPinIndexByPoint ? bucket.companionPinIndexByPoint[idx] : idx;
          if (pinIdx !== undefined && pinIdx !== null) {
            this._drawSinglePin(ctx, companionPin, pinIdx, affine, _iconRadiusForZoom(zoom), "#ffffff");
          }
        }
      } else if (bucket.renderType === "line") {
        // Same idea as the rect treatment above, adapted to a stroke: retrace
        // just the one hovered polyline at full detail, wearing a white halo
        // (a wider stroke underneath) plus its own color slightly thicker
        // than the bulk pass, so the exact belt/pipe/wire the tooltip is
        // describing pops out of a dense bundle of same-colored neighbors.
        var pts = bucket.lines[idx];
        if (!pts || pts.length < bucket.pointStride * 2) {
          return;
        }
        var baseWidth = bucket.lineWidth || 2.5;
        ctx.lineJoin = "round";
        ctx.lineCap = "round";
        ctx.beginPath();
        _tracePolylinePath(ctx, pts, bucket.pointStride, affine);
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = baseWidth + 3.5;
        ctx.stroke();
        ctx.strokeStyle = bucket.color;
        ctx.lineWidth = baseWidth + 1;
        ctx.stroke(); // The traced path survives the first stroke -- no need to rebuild it.
      } else if (bucket.renderType === "circle") {
        var cp = idx * bucket.pointStride;
        var cx = affine.originX + bucket.points[cp] * affine.scaleX;
        var cy = affine.originY + bucket.points[cp + 1] * affine.scaleY;
        // Redrawn slightly larger than _redraw's zoom-dependent dot radius
        // with a white ring, mirroring the rect treatment.
        var dotRadius = Math.min(3, 1 + Math.max(0, zoom) * 0.4) + 1.5;
        ctx.beginPath();
        ctx.arc(cx, cy, dotRadius, 0, Math.PI * 2);
        ctx.fillStyle = bucket.color;
        ctx.fill();
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2;
        ctx.stroke();
      } else if (bucket.renderType === "icon") {
        // Boxes first, pin last, so the box fills can't swallow the pin:
        // a train's pin lights up every one of its cars' boxes (see
        // filters.js's buildTrainRow, which maps each train id to its cars'
        // indices in the shared cars bucket), a vehicle's pin lights up its
        // own box -- and the lead car/vehicle box sits directly under the
        // pin itself.
        var group = bucket.trainCarHighlights;
        var memberIndices = group && group.indicesById[MapApp.highlightedId];
        if (memberIndices) {
          for (var mi = 0; mi < memberIndices.length; mi++) {
            this._strokeRectHighlight(ctx, group.bucket, memberIndices[mi], affine);
          }
        }
        if (bucket.companionBoxBucket && bucket.companionBoxBucket.visible) {
          this._strokeRectHighlight(ctx, bucket.companionBoxBucket, idx, affine);
        }
        // The pin itself, redrawn over the fills, wearing a white ring
        // around its circle (whose center sits above the real coordinate by
        // the tail length -- see _drawSinglePin's pin geometry).
        var ip = idx * bucket.pointStride;
        var pinRadius = _iconRadiusForZoom(zoom);
        var tipX = affine.originX + bucket.points[ip] * affine.scaleX;
        var tipY = affine.originY + bucket.points[ip + 1] * affine.scaleY;
        if (memberIndices || bucket.companionBoxBucket) {
          this._drawSinglePin(ctx, bucket, idx, affine, pinRadius, "#ffffff");
        }
        ctx.beginPath();
        ctx.arc(tipX, tipY - pinRadius - pinRadius * 0.7, pinRadius + 2, 0, Math.PI * 2);
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2.5;
        ctx.stroke();
      }
    },

    // One rect-bucket instance's highlight treatment (full-opacity fill plus
    // a bright outline) -- shared by _redrawHighlight's plain hovered-box
    // case and the whole-consist group a train pin lights up.
    _strokeRectHighlight: function(ctx, bucket, idx, affine) {
      var p = idx * 4;
      var footprint = _footprintForPoint(bucket, idx, bucket.points[p + 2]);
      ctx.beginPath();
      if (footprint.verts) {
        _tracePolygon(ctx, bucket.points[p], bucket.points[p + 1], footprint.verts, affine);
      } else {
        this._traceRect(ctx, bucket.points[p], bucket.points[p + 1], footprint.yaw, footprint.halfWidth, footprint.halfDepth, affine);
      }
      ctx.fillStyle = bucket.color;
      ctx.fill();
      ctx.strokeStyle = "#ffffff";
      ctx.lineWidth = 2;
      ctx.stroke();
    },

    _redraw: function() {
      var map = this._map;
      if (!map) {
        return;
      }
      var ctx = this._canvas.getContext("2d");
      ctx.clearRect(0, 0, this._canvas.width, this._canvas.height);

      var zoom = map.getZoom();
      var pixelOrigin = map.getPixelOrigin();
      var topLeft = this._topLeft;
      // Cull to the whole buffered canvas, not just the viewport -- the
      // margin is the whole point (see _reset/_onMoveEnd).
      var cullBounds = this._cullBounds;
      var minX = cullBounds.minX;
      var maxX = cullBounds.maxX;
      var minY = cullBounds.minY;
      var maxY = cullBounds.maxY;
      var circleRadius = Math.min(3, 1 + Math.max(0, zoom) * 0.4);
      var iconRadius = _iconRadiusForZoom(zoom);
      var affine = this._computeAffine(zoom, pixelOrigin, topLeft);
      var altMin = MapApp.altitudeRange ? MapApp.altitudeRange.min : -Infinity;
      var altMax = MapApp.altitudeRange ? MapApp.altitudeRange.max : Infinity;

      // Canvas painting is just layering -- whatever's drawn last sits on
      // top. Non-rect buckets (lines/circles/icons) still go by drawPriority
      // (see filters.js's makePointBucket) since they rarely visually
      // conflict with each other. Rect buckets (every building/foundation,
      // across every category) are pulled out and drawn separately by
      // _drawRectBuckets, which orders them by actual altitude instead --
      // see that function for why drawPriority alone isn't enough once
      // buildings span multiple floors.
      //
      // The sort itself is cached (_sortedBuckets, invalidated only by
      // addBucket/clearBuckets -- see above) rather than redone here on
      // every redraw: bucket membership and drawPriority never change
      // between a save load and the next, only `visible` flags do, so
      // resorting on every single pan/zoom/checkbox-toggle was pure waste.
      if (!this._sortedBuckets) {
        this._sortedBuckets = this.buckets.slice().sort(function(a, b) { return (a.drawPriority || 0) - (b.drawPriority || 0); });
      }
      var orderedBuckets = this._sortedBuckets;
      var rectBuckets = [];
      var iconBuckets = [];

      for (var b = 0; b < orderedBuckets.length; b++) {
        var bucket = orderedBuckets[b];
        if (!bucket.visible) {
          continue;
        }
        if (bucket.renderType === "line") {
          this._drawLineBucket(ctx, bucket, affine, minX, maxX, minY, maxY, altMin, altMax);
        } else if (bucket.renderType === "rect") {
          rectBuckets.push(bucket);
        } else if (bucket.renderType === "icon") {
          // Deferred below rather than drawn in place: icon pins are point
          // MARKERS (vehicles/collectables/players/HUB), not scenery -- a
          // pin whose anchor happens to sit inside a building's box (a
          // truck parked at its Truck Station, a player standing in a
          // factory) must paint over that box, not be washed out under its
          // translucent fill, so pins get their own pass after
          // _drawRectBuckets. hitTest's pin phase relies on this layering.
          iconBuckets.push(bucket);
        } else {
          this._drawCircleBucket(ctx, bucket, affine, minX, maxX, minY, maxY, circleRadius, altMin, altMax);
        }
      }

      this._drawRectBuckets(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax);

      for (var ic = 0; ic < iconBuckets.length; ic++) {
        this._drawIconBucket(ctx, iconBuckets[ic], affine, minX, maxX, minY, maxY, iconRadius, altMin, altMax);
      }
    },

    // Draws each point as a "pin" -- a white circle (so the icon inside it
    // reads clearly against any background) with a narrow triangular tail
    // hanging from its bottom, the tail's tip landing exactly on the real
    // coordinate. The circle itself sits entirely above that coordinate, so
    // it never covers up the precise spot it's pointing at (unlike the old
    // plain centered icon). Images load asynchronously (browser Image()) --
    // a shared cache keyed by URL avoids re-requesting/re-decoding the same
    // icon for every point, and a not-yet-loaded icon just triggers one
    // redraw once it arrives instead of blocking the rest of the canvas.
    // bucket.iconOpacity (e.g. "remaining" vs "collected") is applied via
    // globalAlpha around the whole pin -- circle, tail, and icon together --
    // not just the icon image.
    _drawIconBucket: function(ctx, bucket, affine, minX, maxX, minY, maxY, radius, altMin, altMax) {
      var pts = bucket.points;
      if (pts.length === 0) {
        return;
      }
      var sprite = _getPinSprite(bucket, radius);
      if (!sprite) {
        return; // Icon not loaded yet -- _getIcon's onload will trigger a redraw.
      }
      var stride = bucket.pointStride;
      var altIdx = stride - 1;
      var tailLength = radius * 0.7; // Circle-bottom-to-tip distance -- keep in sync with _paintPin, whose circleY the occ dedup below has to match.
      var prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = bucket.iconOpacity !== undefined ? bucket.iconOpacity : 1;
      // Pins whose centers land within the same 2x2px cell are visually one
      // pin (the circle alone is 2*radius >= 32px across) -- drawing the
      // pile costs a sprite blit per pin for zero visible change.
      // Dense clusters (e.g. a field of uncollected pickups viewed zoomed
      // out) collapse to one draw per occupied cell. Same stamp mechanism
      // as the rect paths -- see _occEnsure.
      var occ = this._occEnsure();
      var occW = this._occW, occH = this._occH, stamp = this._occStamp;
      var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
      for (var ii = 0; ii < indices.length; ii++) {
        var pointIdx = indices[ii];
        if (_isHidden(bucket, pointIdx)) {
          continue; // Individually hidden via a right-click "Hide this object" (see MapApp.hideObject).
        }
        var i = pointIdx * stride;
        var x = pts[i];
        var y = pts[i + 1];
        var z = pts[i + altIdx];
        if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
          continue;
        }
        var tipX = affine.originX + x * affine.scaleX;
        var tipY = affine.originY + y * affine.scaleY;
        var circleX = tipX;
        var circleY = tipY - radius - tailLength;
        var pxi = circleX | 0, pyi = circleY | 0;
        if (pxi >= 0 && pyi >= 0 && pxi < occW && pyi < occH) {
          var oi = (pyi >> 1) * occW + (pxi >> 1); // 2px cells; row stride occW keeps cell keys unique.
          if (occ[oi] === stamp) {
            continue;
          }
          occ[oi] = stamp;
        }

        ctx.drawImage(sprite.canvas, tipX - sprite.anchorX, tipY - sprite.anchorY, sprite.width, sprite.height);
      }
      ctx.globalAlpha = prevAlpha;
    },

    // One pin, painted directly (no sprite) -- used by _redrawHighlight,
    // which redraws a vehicle/train pin on top of its own highlighted box
    // fill, where the bulk-canvas pin would otherwise be completely covered.
    // The bulk pass in _drawIconBucket blits pre-baked sprites instead --
    // see _getPinSprite.
    // outlineColor overrides the circle's usual bucket-color stroke -- the
    // highlight path passes white, since a vehicle pin's circle is the same
    // orange as the highlighted box fill it's being redrawn onto and would
    // otherwise melt into it, leaving just the floating glyph.
    _drawSinglePin: function(ctx, bucket, idx, affine, radius, outlineColor) {
      var p = idx * bucket.pointStride;
      var tipX = affine.originX + bucket.points[p] * affine.scaleX;
      var tipY = affine.originY + bucket.points[p + 1] * affine.scaleY;
      _paintPin(ctx, tipX, tipY, radius,
                bucket.pinFillColor || "#ffffff",
                outlineColor || bucket.color || "#999999",
                outlineColor ? 2 : 1.25,
                _getIcon(bucket.iconUrl));
    },

    _drawCircleBucket: function(ctx, bucket, affine, minX, maxX, minY, maxY, radius, altMin, altMax) {
      var pts = bucket.points;
      if (pts.length === 0) {
        return;
      }
      var stride = bucket.pointStride;
      var altIdx = stride - 1;
      ctx.fillStyle = bucket.color;
      ctx.beginPath();
      // At <=1.5px radius (fully zoomed out) coincident dots are pure path
      // bloat -- dedup per screen pixel, same as the tiny-rect paths.
      var dedup = radius <= 1.5;
      var occ = dedup ? this._occEnsure() : null;
      var occW = this._occW, occH = this._occH, stamp = this._occStamp;
      var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
      for (var ii = 0; ii < indices.length; ii++) {
        var pointIdx = indices[ii];
        if (_isHidden(bucket, pointIdx)) {
          continue;
        }
        var i = pointIdx * stride;
        var x = pts[i];
        var y = pts[i + 1];
        var z = pts[i + altIdx];
        if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
          continue;
        }
        var cx = affine.originX + x * affine.scaleX;
        var cy = affine.originY + y * affine.scaleY;
        if (occ) {
          var pxi = cx | 0, pyi = cy | 0;
          if (pxi >= 0 && pyi >= 0 && pxi < occW && pyi < occH) {
            var oi = pyi * occW + pxi;
            if (occ[oi] === stamp) {
              continue;
            }
            occ[oi] = stamp;
          }
        }
        ctx.moveTo(cx + radius, cy);
        ctx.arc(cx, cy, radius, 0, Math.PI * 2);
      }
      ctx.fill();
    },

    // Traces one rotated rect's outline into ctx's current path (caller does
    // beginPath/fill/stroke). Shared between the bulk batch and the single
    // highlighted box in _drawRectBuckets so the geometry only lives in one place.
    _traceRect: function(ctx, x, y, yaw, halfWidth, halfDepth, affine) {
      var cx = affine.originX + x * affine.scaleX;
      var cy = affine.originY + y * affine.scaleY;
      // Yaw is computed from the world-space quaternion, but map-pixel-space
      // has a flipped Y axis relative to world space (see projectXY in
      // sav_map_data.py) -- a single-axis mirror reverses rotation
      // handedness, so the angle must be negated here to render off-angle
      // buildings with the correct (not mirrored) orientation.
      var cos = Math.cos(-yaw);
      var sin = Math.sin(-yaw);
      var cosW = cos * halfWidth;
      var sinW = sin * halfWidth;
      var cosD = cos * halfDepth;
      var sinD = sin * halfDepth;
      // Corners unrolled with no intermediate arrays -- this runs once per
      // visible building per redraw, and the array-of-arrays version was
      // measurable pure GC churn at megabase scale.
      var sX = affine.scaleX;
      var sY = affine.scaleY;
      ctx.moveTo(cx + (cosW - sinD) * sX, cy + (sinW + cosD) * sY);
      ctx.lineTo(cx + (-cosW - sinD) * sX, cy + (-sinW + cosD) * sY);
      ctx.lineTo(cx + (-cosW + sinD) * sX, cy + (-sinW - cosD) * sY);
      ctx.lineTo(cx + (cosW + sinD) * sX, cy + (sinW - cosD) * sY);
      ctx.closePath();
    },

    // Draws every building at its real-world footprint size (see
    // sav_map_data.footprintPixels), rotated to match its actual facing.
    // Fills only, with no outline -- adjacent/overlapping same-color
    // buildings (e.g. a large foundation platform) blend into one solid
    // shape instead of showing a grid of individual tile borders. A single
    // building's outline is only ever shown via the separate highlight
    // canvas, on hover/click (see _redrawHighlight), which draws just the
    // one hovered/pinned rect at full opacity with a bright border on top
    // of everything drawn here.
    //
    // All rect buckets are drawn together here, sorted by actual altitude
    // (lowest first) when buildings actually span multiple floors, rather
    // than each bucket being drawn in its own pass ordered by category
    // drawPriority. A fixed per-category order (e.g. "all foundations
    // before all machines") only matches reality for a single ground floor
    // -- on a multi-floor base, a foundation built on an upper floor should
    // visually cover whatever's underneath it, but with category-only
    // ordering it always painted before (i.e. under) every machine
    // regardless of which floor either one was actually on. That mismatched
    // what hitTest already does for tooltips (highest Z wins -- see hitTest
    // phase 1), so the tooltip would name a building the view appeared to
    // draw underneath everything.
    //
    // Most views (a flat platform, a single-story factory floor) have every
    // visible rect on the same floor, where altitude order doesn't change
    // what's visually on top of what -- so before paying for a per-point
    // sort, this cheaply checks the actual Z spread among currently visible
    // points (reusing the same grid-culling either path needs anyway) and
    // only takes the sorted path once buildings genuinely overlap across
    // floors. This matters because with hundreds of thousands of points,
    // doing this sort/allocation every redraw even when it's a no-op was
    // adding multi-second pan/zoom lag for exactly the foundation-heavy
    // views this Z-ordering fix was meant to help.
    _drawRectBuckets: function(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax) {
      if (rectBuckets.length === 0) {
        return;
      }
      // When every bucket in view renders below SMALL_RECT_SCREEN_PX,
      // altitude layering of few-px blobs is imperceptible, so the whole
      // cull-snapshot + z-sort machinery below is pure overhead -- and
      // z-interleaved draw order would force a beginPath/fill per color
      // change anyway (thousands of tiny fills on exactly the full-map
      // view). Decidable up front from footprint * scale alone, so the
      // fused fast path can skip the snapshot entirely.
      var absScaleX = Math.abs(affine.scaleX);
      var absScaleY = Math.abs(affine.scaleY);
      var allSmall = true;
      for (var tb = 0; tb < rectBuckets.length; tb++) {
        var tfp = rectBuckets[tb].footprintPixels;
        if (!tfp || tfp[0] * 2 * absScaleX >= SMALL_RECT_SCREEN_PX || tfp[1] * 2 * absScaleY >= SMALL_RECT_SCREEN_PX) {
          allSmall = false;
          break;
        }
      }
      if (allSmall) {
        this._drawRectBucketsAllSmall(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax);
        return;
      }
      // Single cull pass: every visible point is recorded once into the
      // flat reusable _cull* arrays (grouped by bucket via bucketStarts),
      // with the global z spread computed along the way. The flat and
      // sorted paths below both draw straight from this snapshot -- the
      // previous version re-walked the grid up to three times per redraw
      // (a z-spread probe, then one or two more passes inside whichever
      // path ran), which at hundreds of thousands of visible points
      // tripled the most expensive part of the frame.
      var total = 0;
      var globalMinZ = Infinity, globalMaxZ = -Infinity;
      var bucketStarts = this._bucketStarts || (this._bucketStarts = []);
      bucketStarts.length = rectBuckets.length + 1;
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        bucketStarts[bi] = total;
        var bucket = rectBuckets[bi];
        var pts = bucket.points;
        if (pts.length === 0) {
          continue;
        }
        var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
        if (total + indices.length > this._cullCapacity) {
          this._growCull(total + indices.length);
        }
        var zArr = this._cullZ;
        var pointIdxArr = this._cullPointIdx;
        var bucketIdxArr = this._cullBucketIdx;
        for (var ii = 0; ii < indices.length; ii++) {
          var idx = indices[ii];
          if (_isHidden(bucket, idx)) {
            continue;
          }
          var p = idx * 4;
          var x = pts[p], y = pts[p + 1], z = pts[p + 3];
          if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
            continue;
          }
          zArr[total] = z;
          pointIdxArr[total] = idx;
          bucketIdxArr[total] = bi;
          total++;
          if (z < globalMinZ) globalMinZ = z;
          if (z > globalMaxZ) globalMaxZ = z;
        }
      }
      bucketStarts[rectBuckets.length] = total;
      if (total === 0) {
        return;
      }

      if (globalMaxZ - globalMinZ <= FLAT_PLATFORM_Z_EPSILON) {
        this._drawRectBucketsFlat(ctx, rectBuckets, affine, bucketStarts);
      } else {
        this._drawRectBucketsSorted(ctx, rectBuckets, affine, total, globalMinZ, globalMaxZ);
      }
    },

    // Fused fast path for zoomed-out views: every bucket renders below
    // SMALL_RECT_SCREEN_PX, so cull + dedup + draw happen in ONE direct
    // walk of each bucket's grid cells -- no scratch index array, no cull
    // snapshot, no sort, one batched fill per bucket, and axis-aligned
    // rects at each bucket's true screen dimensions instead of rotated
    // quads. This is what a sidebar checkbox toggle or a pan/zoom settle
    // costs on a 500k-object save viewed whole, so it's the hottest loop
    // in the renderer; keep it allocation-free.
    _drawRectBucketsAllSmall: function(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax) {
      var absScaleX = Math.abs(affine.scaleX);
      var absScaleY = Math.abs(affine.scaleY);
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        var bucket = rectBuckets[bi];
        var pts = bucket.points;
        var grid = bucket._grid;
        if (pts.length === 0 || !grid) {
          continue;
        }
        // Half extents in screen px, floored at 0.75 so even a subpixel
        // building stays a visible ~1.5px dot (the old fixed size).
        var halfW = Math.max(0.75, bucket.footprintPixels[0] * absScaleX);
        var halfH = Math.max(0.75, bucket.footprintPixels[1] * absScaleY);
        var fullW = halfW * 2, fullH = halfH * 2;
        var hidden = bucket.hiddenIndices || null;
        var hasOverrides = !!bucket.tiltedFootprints;
        var fillColor = _withAlpha(bucket.color, 0.55);
        ctx.fillStyle = fillColor;
        ctx.beginPath();
        var occ = this._occEnsure();
        var occW = this._occW, occH = this._occH, stamp = this._occStamp;
        var deferredPolygons = null;
        // Inline _collectGridIndices: iterate the overlapping cells
        // directly instead of copying half a million indices into the
        // scratch array first.
        var gridSize = grid.gridSize;
        var cx0 = Math.max(0, Math.floor((minX - grid.minX) / grid.cellW));
        var cx1 = Math.min(gridSize - 1, Math.floor((maxX - grid.minX) / grid.cellW));
        var cy0 = Math.max(0, Math.floor((minY - grid.minY) / grid.cellH));
        var cy1 = Math.min(gridSize - 1, Math.floor((maxY - grid.minY) / grid.cellH));
        if (cx1 < 0 || cy1 < 0 || cx0 >= gridSize || cy0 >= gridSize || cx0 > cx1 || cy0 > cy1) {
          continue;
        }
        for (var cy = cy0; cy <= cy1; cy++) {
          var rowBase = cy * gridSize;
          for (var cx = cx0; cx <= cx1; cx++) {
            var cell = grid.cells[rowBase + cx];
            for (var k = 0; k < cell.length; k++) {
              var idx = cell[k];
              if (hidden !== null && hidden.has(idx)) {
                continue;
              }
              var p = idx * 4;
              var x = pts[p], y = pts[p + 1], z = pts[p + 3];
              if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
                continue;
              }
              if (hasOverrides) {
                var verts = bucket.tiltedFootprints[idx];
                if (verts) {
                  (deferredPolygons || (deferredPolygons = [])).push(x, y, verts);
                  continue;
                }
              }
              var sx = affine.originX + x * affine.scaleX;
              var sy = affine.originY + y * affine.scaleY;
              var pxi = sx | 0, pyi = sy | 0;
              if (pxi >= 0 && pyi >= 0 && pxi < occW && pyi < occH) {
                var oi = pyi * occW + pxi;
                if (occ[oi] === stamp) {
                  continue; // A same-sized rect already covers this pixel this fill.
                }
                occ[oi] = stamp;
              }
              ctx.rect(sx - halfW, sy - halfH, fullW, fullH);
            }
          }
        }
        ctx.fill();
        if (deferredPolygons) {
          ctx.fillStyle = fillColor;
          for (var dp = 0; dp < deferredPolygons.length; dp += 3) {
            ctx.beginPath();
            _tracePolygon(ctx, deferredPolygons[dp], deferredPolygons[dp + 1], deferredPolygons[dp + 2], affine);
            ctx.fill();
          }
        }
      }
    },

    // Doubles the _cull* snapshot arrays to at least `needed` entries,
    // preserving what's already been written this frame. Sized to the
    // high-water mark and kept across frames, so after the first zoomed-out
    // redraw this never runs again.
    _growCull: function(needed) {
      var cap = Math.max(4096, this._cullCapacity);
      while (cap < needed) {
        cap *= 2;
      }
      var z = new Float32Array(cap);
      var pointIdx = new Uint32Array(cap);
      var bucketIdx = new Uint16Array(cap);
      if (this._cullZ) {
        z.set(this._cullZ);
        pointIdx.set(this._cullPointIdx);
        bucketIdx.set(this._cullBucketIdx);
      }
      this._cullZ = z;
      this._cullPointIdx = pointIdx;
      this._cullBucketIdx = bucketIdx;
      this._cullCapacity = cap;
    },

    // Starts a new subpixel-dedup batch (see the tiny-rect branches in the
    // draw paths below) and returns the occupancy buffer: one stamp byte
    // per canvas pixel. A point whose integer pixel already carries the
    // current stamp is skipped -- zoomed out, huge foundation fields
    // collapse to a handful of screen pixels, and building a path with
    // hundreds of thousands of coincident 1.5px squares costs real time
    // while changing nothing visually (within one fill() overlapping
    // subpaths don't even double-blend). Bumping the stamp makes
    // "clearing" the buffer between batches free; Uint8 (vs Int32) keeps
    // the random-access working set 4x smaller -- this buffer is hit once
    // per point in the hottest loops, so cache footprint matters. The
    // stamp wraps at 255, at which point the buffer is actually zeroed
    // (a rare, cheap fill) so stale stamps can never false-positive.
    _occEnsure: function() {
      var w = this._canvas.width, h = this._canvas.height;
      if (!this._occ || this._occW !== w || this._occH !== h) {
        this._occ = new Uint8Array(w * h);
        this._occStamp = 0;
        this._occW = w;
        this._occH = h;
      }
      this._occStamp++;
      if (this._occStamp > 255) {
        this._occ.fill(0);
        this._occStamp = 1;
      }
      return this._occ;
    },

    // Fast path: no point in this view set is more than FLAT_PLATFORM_Z_EPSILON
    // away from any other, so fill order can't visibly matter -- draw each
    // bucket directly (one beginPath/fill per bucket, like the original
    // single-bucket version of this code) straight from the _cull* snapshot
    // _drawRectBuckets already built (bucketStarts[bi]..bucketStarts[bi+1]
    // is bucket bi's slice of it), with no re-culling and no sort.
    _drawRectBucketsFlat: function(ctx, rectBuckets, affine, bucketStarts) {
      var pointIdxArr = this._cullPointIdx;
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        var start = bucketStarts[bi];
        var end = bucketStarts[bi + 1];
        if (start === end) {
          continue;
        }
        var bucket = rectBuckets[bi];
        var pts = bucket.points;
        var halfWidth = bucket.footprintPixels[0];
        var halfDepth = bucket.footprintPixels[1];
        var screenW = halfWidth * 2 * Math.abs(affine.scaleX);
        var screenH = halfDepth * 2 * Math.abs(affine.scaleY);
        // Below SMALL_RECT_SCREEN_PX rotation is invisible -- draw an
        // axis-aligned rect at true screen size (see the constant's comment).
        var small = screenW < SMALL_RECT_SCREEN_PX && screenH < SMALL_RECT_SCREEN_PX;
        var halfWPx = Math.max(0.75, screenW / 2);
        var halfHPx = Math.max(0.75, screenH / 2);
        var hasOverrides = !!bucket.tiltedFootprints; // See _footprintForPoint -- false for almost every bucket.
        var fillColor = _withAlpha(bucket.color, 0.55);
        ctx.fillStyle = fillColor;
        ctx.beginPath();
        var occ = small ? this._occEnsure() : null;
        var occW = this._occW, occH = this._occH, stamp = this._occStamp;
        // Deferred [x,y,verts, x,y,verts, ...] for this bucket's tilted
        // instances -- only ever allocated if the bucket actually has any
        // (see the comment below on why they can't just join the loop above).
        var deferredPolygons = null;
        for (var k = start; k < end; k++) {
          var idx = pointIdxArr[k];
          var i = idx * 4;
          var x = pts[i];
          var y = pts[i + 1];
          var verts = hasOverrides && bucket.tiltedFootprints[idx];
          if (verts) {
            // Measured, not guessed: adding even a few thousand closePath()'d
            // polygon subpaths into the SAME accumulating path as tens of
            // thousands of ctx.rect() calls below took a shared path from
            // ~5ms to ~300ms to fill in a synthetic benchmark at this scale
            // -- apparently mixing the two path-command styles knocks the
            // whole path out of the browser's specialized fast-rect
            // representation. Tilted instances are rare enough that
            // deferring them to their own individual beginPath/fill each
            // (below, after this bucket's plain rects are filled) sidesteps
            // that entirely, and was faster still than batching them
            // together in a second shared path.
            (deferredPolygons || (deferredPolygons = [])).push(x, y, verts);
            continue;
          }
          if (small) {
            var cx = affine.originX + x * affine.scaleX;
            var cy = affine.originY + y * affine.scaleY;
            var pxi = cx | 0, pyi = cy | 0;
            if (pxi >= 0 && pyi >= 0 && pxi < occW && pyi < occH) {
              var oi = pyi * occW + pxi;
              if (occ[oi] === stamp) {
                continue; // A same-sized rect already covers this pixel this fill.
              }
              occ[oi] = stamp;
            }
            ctx.rect(cx - halfWPx, cy - halfHPx, halfWPx * 2, halfHPx * 2);
          } else {
            this._traceRect(ctx, x, y, pts[i + 2], halfWidth, halfDepth, affine);
          }
        }
        ctx.fill();
        if (deferredPolygons) {
          ctx.fillStyle = fillColor;
          for (var dp = 0; dp < deferredPolygons.length; dp += 3) {
            ctx.beginPath();
            _tracePolygon(ctx, deferredPolygons[dp], deferredPolygons[dp + 1], deferredPolygons[dp + 2], affine);
            ctx.fill();
          }
        }
      }
    },

    // Slow path: points genuinely span multiple floors, so fill order has to
    // follow actual altitude (lowest first) to match hitTest's "highest Z
    // wins" rule -- see the big comment on _drawRectBuckets above.
    //
    // Draws from the _cull* snapshot _drawRectBuckets already built, ordered
    // by a counting sort over quantized z instead of a comparator sort: at
    // hundreds of thousands of visible points, Array#sort with a JS
    // comparator (O(n log n) with a call per comparison) was the single
    // biggest chunk of the redraw. Counting into Z_SORT_BINS bins is O(n),
    // allocation-free after the first frame, and stable, so same-bin points
    // keep bucket order -- exactly what the flat path does for views whose
    // whole z spread fits FLAT_PLATFORM_Z_EPSILON anyway. Bin granularity is
    // spread/Z_SORT_BINS: even a full-map view spanning ~2000m of altitude
    // still resolves draw order to ~0.5m, well under one floor's height.
    _drawRectBucketsSorted: function(ctx, rectBuckets, affine, total, globalMinZ, globalMaxZ) {
      var zArr = this._cullZ;
      var pointIdxArr = this._cullPointIdx;
      var bucketIdxArr = this._cullBucketIdx;

      // Precomputed once per bucket (constant across all of its points) so
      // the fill loop below doesn't redo this arithmetic -- or, for the
      // color, a string-keyed cache lookup -- once per point.
      var absScaleX = Math.abs(affine.scaleX);
      var absScaleY = Math.abs(affine.scaleY);
      var smallByBucket = new Uint8Array(rectBuckets.length);
      var halfWByBucket = new Float32Array(rectBuckets.length);
      var halfHByBucket = new Float32Array(rectBuckets.length);
      var colorByBucket = new Array(rectBuckets.length);
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        var fp = rectBuckets[bi].footprintPixels;
        var screenW = fp ? fp[0] * 2 * absScaleX : 0;
        var screenH = fp ? fp[1] * 2 * absScaleY : 0;
        smallByBucket[bi] = (screenW < SMALL_RECT_SCREEN_PX && screenH < SMALL_RECT_SCREEN_PX) ? 1 : 0;
        halfWByBucket[bi] = Math.max(0.75, screenW / 2);
        halfHByBucket[bi] = Math.max(0.75, screenH / 2);
        colorByBucket[bi] = _withAlpha(rectBuckets[bi].color, 0.55);
      }

      // Counting sort over the composite key (zBin, bucket) -- see
      // Z_SORT_BINS for why the bucket is folded into the key. Stable, so
      // same-key points keep collection order.
      var bins = Z_SORT_BINS;
      var nBuckets = rectBuckets.length;
      var slots = bins * nBuckets + 1;
      var counts = this._binCounts;
      if (!counts || counts.length < slots) {
        counts = this._binCounts = new Uint32Array(slots);
      } else {
        counts.fill(0, 0, slots);
      }
      var zScale = (bins - 1) / (globalMaxZ - globalMinZ); // Spread is > FLAT_PLATFORM_Z_EPSILON here, so never divides by zero.
      var i;
      for (i = 0; i < total; i++) {
        counts[1 + (((zArr[i] - globalMinZ) * zScale) | 0) * nBuckets + bucketIdxArr[i]]++;
      }
      for (var b = 1; b < slots; b++) {
        counts[b] += counts[b - 1];
      }
      var order = this._orderArr;
      if (!order || order.length < total) {
        order = this._orderArr = new Uint32Array(this._cullCapacity);
      }
      for (i = 0; i < total; i++) {
        order[counts[(((zArr[i] - globalMinZ) * zScale) | 0) * nBuckets + bucketIdxArr[i]]++] = i;
      }

      // ctx.fill() only applies one fillStyle to the whole current path, so
      // a fresh path/fill is needed whenever the color changes -- runs of
      // same-bucket items (common, since nearby altitudes tend to share a
      // floor/category) still batch into a single fill() the same as before.
      // A polygon item (a tilted instance -- see _footprintForPoint) also
      // forces a fresh path even when the color hasn't changed: mixing its
      // closePath()'d subpath into the same accumulating path as plain
      // ctx.rect() calls measurably tanks fill() performance at scale (see
      // _drawRectBucketsFlat's comment for the benchmark) by apparently
      // knocking the whole path out of the browser's specialized fast-rect
      // representation -- keeping polygon and non-polygon runs in separate
      // paths avoids that regardless of how they interleave in Z-order.
      var currentColor = null;
      var currentIsPolygon = null;
      var hasOpenPath = false;
      // Subpixel dedup batch state (see _occEnsure) -- restarted on every
      // beginPath below, so dedup never suppresses a square that a
      // different-colored fill layered on top would have shown through.
      var occ = null, occW = 0, occH = 0, stamp = 0;
      for (var k = 0; k < total; k++) {
        var w2 = order[k];
        var bIdx = bucketIdxArr[w2];
        var itemBucket = rectBuckets[bIdx];
        var pIdx = pointIdxArr[w2];
        var ip = pIdx * 4;
        var ipts = itemBucket.points;
        var ix = ipts[ip], iy = ipts[ip + 1], iyaw = ipts[ip + 2];
        var verts = itemBucket.tiltedFootprints && itemBucket.tiltedFootprints[pIdx];
        var color = colorByBucket[bIdx];
        var isPolygon = !!verts;
        if (color !== currentColor || isPolygon !== currentIsPolygon) {
          if (hasOpenPath) {
            ctx.fill();
          }
          ctx.beginPath();
          ctx.fillStyle = color;
          currentColor = color;
          currentIsPolygon = isPolygon;
          hasOpenPath = true;
          occ = null;
        }
        if (isPolygon) {
          _tracePolygon(ctx, ix, iy, verts, affine);
        } else if (smallByBucket[bIdx]) {
          var cx = affine.originX + ix * affine.scaleX;
          var cy = affine.originY + iy * affine.scaleY;
          if (!occ) {
            occ = this._occEnsure();
            occW = this._occW;
            occH = this._occH;
            stamp = this._occStamp;
          }
          var pxi = cx | 0, pyi = cy | 0;
          if (pxi >= 0 && pyi >= 0 && pxi < occW && pyi < occH) {
            var oi = pyi * occW + pxi;
            if (occ[oi] === stamp) {
              continue; // A same-sized rect already covers this pixel this fill.
            }
            occ[oi] = stamp;
          }
          var hw = halfWByBucket[bIdx], hh = halfHByBucket[bIdx];
          ctx.rect(cx - hw, cy - hh, hw * 2, hh * 2);
        } else {
          this._traceRect(ctx, ix, iy, iyaw, itemBucket.footprintPixels[0], itemBucket.footprintPixels[1], affine);
        }
      }
      if (hasOpenPath) {
        ctx.fill();
      }
    },

    // stride 3 vertices ([x,y,z]) are connected with straight segments.
    // stride 7 vertices ([x,y,arriveX,arriveY,leaveX,leaveY,z]) are connected
    // with a cubic bezier, converted from the pair of Hermite tangents via
    // the standard identity: bezier control point 1 = P0 + leaveTangent(P0)/3,
    // control point 2 = P1 - arriveTangent(P1)/3. Tangents are stored in
    // map-pixel-space units (see sav_map_data.projectVectorXY), so they need
    // the same screen-pixels-per-map-pixel scale used for rotated building
    // boxes (_traceRect) to become screen-space control point offsets.
    _drawLineBucket: function(ctx, bucket, affine, minX, maxX, minY, maxY, altMin, altMax) {
      var lines = bucket.lines;
      if (!lines || lines.length === 0) {
        return;
      }
      var stride = bucket.pointStride;
      var altIdx = stride - 1;
      ctx.strokeStyle = bucket.color;
      ctx.lineWidth = bucket.lineWidth || 2.5;
      ctx.beginPath();
      var lineBounds = bucket._lineBounds;
      var absScaleX = Math.abs(affine.scaleX);
      var absScaleY = Math.abs(affine.scaleY);
      for (var L_ = 0; L_ < lines.length; L_++) {
        var pts = lines[L_];
        // Precomputed once when the bucket was added (see _buildLineBounds) --
        // a single bbox-overlap check replaces a full scan of every vertex
        // just to find out a line is entirely off-screen.
        var lb = lineBounds && lineBounds[L_];
        if (lb && (lb.minX > maxX || lb.maxX < minX || lb.minY > maxY || lb.maxY < minY || lb.minZ > altMax || lb.maxZ < altMin)) {
          continue;
        }
        if (_isHidden(bucket, L_)) {
          continue; // Individually hidden via a right-click "Hide this object".
        }
        var prevX = affine.originX + pts[0] * affine.scaleX;
        var prevY = affine.originY + pts[1] * affine.scaleY;
        // Zoomed out far enough that this whole polyline covers under ~3
        // screen px, its curve/vertex detail is invisible -- one straight
        // first-to-last segment is indistinguishable and turns a belt-heavy
        // save's tens of thousands of multi-vertex beziers into two path
        // verbs each. This is what keeps full-map views usable with huge
        // conveyor/pipe networks.
        if (lb && (lb.maxX - lb.minX) * absScaleX + (lb.maxY - lb.minY) * absScaleY < 3) {
          var lastI = pts.length - stride;
          ctx.moveTo(prevX, prevY);
          ctx.lineTo(affine.originX + pts[lastI] * affine.scaleX, affine.originY + pts[lastI + 1] * affine.scaleY);
          continue;
        }
        _tracePolylinePath(ctx, pts, stride, affine);
      }
      ctx.stroke();
    },

    // Finds the closest clickable point/line/box within toleranceMapUnits of
    // (x,y), both in map-pixel space (same space as bucket.points/lines --
    // no projection needed since the click handler converts e.latlng the
    // same way buckets are already stored). Respects the altitude filter, so
    // filtered-out objects aren't hoverable either. Returns {bucket, id} or null.
    hitTest: function(x, y, toleranceMapUnits) {
      var altMin = MapApp.altitudeRange ? MapApp.altitudeRange.min : -Infinity;
      var altMax = MapApp.altitudeRange ? MapApp.altitudeRange.max : Infinity;
      // CRS.Simple's affine scale is 2^zoom screen-px per map-unit (same
      // approximation the caller already uses to turn toleranceMapUnits'
      // screen-px tolerance into map units) -- reused below to place an
      // icon bucket's hit area where its pin is actually drawn, not at the
      // bare coordinate the tail's tip touches.
      var zoom = this._map ? this._map.getZoom() : 0;
      var scaleFactor = Math.pow(2, zoom);

      // Phase 0a: icon pins. Pins are drawn on top of everything, including
      // building boxes (see _redraw's deferred icon pass), so the cursor
      // sitting inside a pin's visible circle unambiguously means that pin
      // -- checked first and returned outright. Without this, phase 1's
      // box-containment always won wherever a pin overlaps a building
      // (e.g. a vehicle parked on a foundation road resolved to
      // "Foundation" no matter where on the truck's pin you clicked). Same
      // circle-center geometry as phase 2's icon branch below (which still
      // covers icons for symmetry, but can never beat this earlier return).
      var pinRadiusPx = _iconRadiusForZoom(zoom);
      var pinTolerance = pinRadiusPx / scaleFactor;
      var pinYOffset = (pinRadiusPx + pinRadiusPx * 0.7) / scaleFactor;
      var bestPin = null;
      var bestPinScore = 1;
      for (var ib = 0; ib < this.buckets.length; ib++) {
        var pinBucket = this.buckets[ib];
        if (!pinBucket.visible || !pinBucket.ids || pinBucket.tooltipKind === "none" || pinBucket.renderType !== "icon") {
          continue;
        }
        var pinStride = pinBucket.pointStride;
        var pinAltIdx = pinStride - 1;
        var pinQueryY = y - pinYOffset;
        var pinIndices = _collectGridIndices(pinBucket._grid, x - pinTolerance, x + pinTolerance, pinQueryY - pinTolerance, pinQueryY + pinTolerance, this._scratchIndices);
        for (var pj = 0; pj < pinIndices.length; pj++) {
          var pinIdx = pinIndices[pj];
          if (_isHidden(pinBucket, pinIdx)) {
            continue;
          }
          var pp = pinIdx * pinStride;
          var pinZ = pinBucket.points[pp + pinAltIdx];
          if (pinZ < altMin || pinZ > altMax) {
            continue;
          }
          var pinDx = pinBucket.points[pp] - x;
          var pinDy = (pinBucket.points[pp + 1] + pinYOffset) - y;
          var pinScore = Math.sqrt(pinDx * pinDx + pinDy * pinDy) / pinTolerance;
          if (pinScore < bestPinScore) {
            bestPinScore = pinScore;
            bestPin = { bucket: pinBucket, id: pinBucket.ids[pinIdx], index: pinIdx, z: pinZ };
          }
        }
      }
      if (bestPin) {
        return bestPin;
      }

      // Phase 0: lines (belts/pipelines/railroads/hypertubes/power lines)
      // are always drawn UNDER every building (see _redraw -- rect buckets
      // are painted last, after every line/icon/circle bucket, regardless
      // of relative altitude), so a belt running near or through a
      // building is very often entirely hidden inside that building's box.
      // Without this dedicated check, hovering directly over a visible bit
      // of line would still always resolve to whatever box happens to
      // contain that point, making many lines impossible to ever select.
      // This only finds the best candidate -- it does NOT return yet (see
      // the topmost-wins comparison against phase 1's box below), since a
      // line found here might genuinely be on a lower floor, truly covered
      // by an unrelated building one or more stories above it, not just
      // incidentally near it.
      var bestLineHit = null;
      var bestLineScore = 1;
      for (var bl = 0; bl < this.buckets.length; bl++) {
        var lineBucket = this.buckets[bl];
        if (!lineBucket.visible || !lineBucket.ids || lineBucket.tooltipKind === "none" || lineBucket.renderType !== "line") {
          continue;
        }
        var lineStride = lineBucket.pointStride;
        var lineAltIdx = lineStride - 1;
        var lineBoundsArr = lineBucket._lineBounds;
        for (var li = 0; li < lineBucket.lines.length; li++) {
          // Bounding-box reject before touching any vertex: without this,
          // every hover tick walked every vertex of every belt/pipe on the
          // map (millions, on a big save) just to find the one line near
          // the cursor -- a constant background freeze while zoomed out.
          var hlb = lineBoundsArr && lineBoundsArr[li];
          if (hlb && (hlb.minX > x + toleranceMapUnits || hlb.maxX < x - toleranceMapUnits ||
                      hlb.minY > y + toleranceMapUnits || hlb.maxY < y - toleranceMapUnits ||
                      hlb.minZ > altMax || hlb.maxZ < altMin)) {
            continue;
          }
          if (_isHidden(lineBucket, li)) {
            continue;
          }
          var pts = lineBucket.lines[li];
          var lineInAltitudeRange = false;
          for (var zk = lineAltIdx; zk < pts.length; zk += lineStride) {
            if (pts[zk] >= altMin && pts[zk] <= altMax) {
              lineInAltitudeRange = true;
              break;
            }
          }
          if (!lineInAltitudeRange) {
            continue;
          }
          // Stride-7 segments are hit-tested against the same cubic bezier
          // the renderer draws (see _tracePolylinePath) -- the straight
          // chord alone can sit many screen px away from the visible curve
          // on sharp bends, making the drawn line unhoverable there while
          // empty space along the invisible chord lit up instead.
          for (var i = 0; i + lineStride + 1 < pts.length; i += lineStride) {
            var segAx = pts[i], segAy = pts[i + 1];
            var segBx = pts[i + lineStride], segBy = pts[i + lineStride + 1];
            var d;
            if (lineStride >= 7) {
              d = _pointToBezierDistance(x, y, segAx, segAy,
                segAx + pts[i + 4] / 3, segAy + pts[i + 5] / 3, // prev vertex's leave tangent -> control point 1
                segBx - pts[i + lineStride + 2] / 3, segBy - pts[i + lineStride + 3] / 3, // cur vertex's arrive tangent -> control point 2
                segBx, segBy, toleranceMapUnits);
            } else {
              d = _pointToSegmentDistance(x, y, segAx, segAy, segBx, segBy);
            }
            var lineScore = d / toleranceMapUnits;
            if (lineScore < bestLineScore) {
              bestLineScore = lineScore;
              bestLineHit = { bucket: lineBucket, id: lineBucket.ids[li], index: li, z: pts[i + lineAltIdx] };
            }
          }
        }
      }

      // Phase 1: box buildings are large enough that "anywhere inside the
      // box" should register, not just within toleranceMapUnits of its
      // center -- so this is checked as unambiguous containment, separately
      // from (and with priority over) the nearest-point search in phase 2.
      // Multiple boxes can contain the same (x,y) on multi-story factories
      // (e.g. a building on a floor directly above another), so every
      // containing box is collected and the highest-altitude one wins,
      // rather than just whichever bucket/order happened to be checked first.
      var bestBoxHit = null;
      var bestBoxZ = -Infinity;
      for (var b = 0; b < this.buckets.length; b++) {
        var rectBucket = this.buckets[b];
        if (!rectBucket.visible || !rectBucket.ids || rectBucket.tooltipKind === "none" || rectBucket.renderType !== "rect") {
          continue;
        }
        var halfWidth = rectBucket.footprintPixels[0];
        var halfDepth = rectBucket.footprintPixels[1];
        var hasOverrides = !!rectBucket.tiltedFootprints; // See _footprintForPoint -- false for almost every bucket.
        // maxFootprintRadius (== this rect's own corner distance unless some
        // instance in this bucket needed a bigger tilted polygon, see
        // collectBuildings) is what the *query radius* below has to use --
        // the plain rect size alone would make an enlarged tilted polygon's
        // cells fall outside the search and become unclickable. The
        // per-instance containment check further down still uses each
        // point's own (possibly overridden) shape.
        var maxRadius = rectBucket.maxFootprintRadius || Math.sqrt(halfWidth * halfWidth + halfDepth * halfDepth);
        // Only cells within maxRadius of the cursor can possibly contain a
        // box that reaches the cursor -- same grid used by _redraw, queried
        // with a cursor-centered box instead of the viewport (see
        // _buildPointGrid/_collectGridIndices). This is what keeps hover/
        // click responsive once a bucket has tens of thousands of boxes.
        var rectIndices = _collectGridIndices(rectBucket._grid, x - maxRadius, x + maxRadius, y - maxRadius, y + maxRadius, this._scratchIndices);
        for (var ri = 0; ri < rectIndices.length; ri++) {
          var rectIdx = rectIndices[ri];
          if (_isHidden(rectBucket, rectIdx)) {
            continue;
          }
          var rp = rectIdx * 4;
          var bx = rectBucket.points[rp];
          var by = rectBucket.points[rp + 1];
          var byaw = rectBucket.points[rp + 2];
          var bz = rectBucket.points[rp + 3];
          if (bz < altMin || bz > altMax) {
            continue;
          }
          var ddx = x - bx;
          var ddy = y - by;
          if (Math.abs(ddx) > maxRadius || Math.abs(ddy) > maxRadius) {
            continue;
          }
          var verts = hasOverrides && rectBucket.tiltedFootprints[rectIdx];
          var isHit;
          if (verts) {
            // verts is already in final rotated orientation (see
            // sav_map_data.collectBuildings), so the cursor offset is tested
            // against it directly -- no inverse-rotation needed.
            isHit = _pointInPolygon(ddx, ddy, verts);
          } else {
            // Inverse-rotate the cursor offset into the building's local frame
            // (see _drawRectBuckets for the matching forward rotation + the
            // note on why yaw is negated here).
            var cos = Math.cos(byaw);
            var sin = Math.sin(byaw);
            var localX = ddx * cos - ddy * sin;
            var localY = ddx * sin + ddy * cos;
            isHit = Math.abs(localX) <= halfWidth && Math.abs(localY) <= halfDepth;
          }
          if (isHit && bz > bestBoxZ) {
            bestBoxZ = bz;
            bestBoxHit = { bucket: rectBucket, id: rectBucket.ids[rectIdx], index: rectIdx, z: bz };
          }
        }
      }

      // Topmost wins between a line and an overlapping box -- the same
      // "highest altitude wins" rule phase 1 already uses to resolve
      // box-vs-box ties. A box only beats a line if it's genuinely higher
      // (a different floor truly covering the line from above); at the
      // same or a lower altitude than the box, the line wins, since
      // buildings are drawn with partial transparency (see
      // _drawRectBucketsFlat's alpha-0.55 fill) -- a line at that level
      // actually shows through rather than being truly hidden.
      // lineHitClearanceM (vehicle/train-car boxes, see filters.js) raises
      // the bar: a vehicle physically sits ON its road/rail, so the line it
      // drives on registers at essentially the vehicle's own altitude, and
      // the plain same-altitude rule made the vehicle's box unhoverable
      // along its whole midline. The line must clear the box by more than a
      // vehicle's height to win there -- an actual bridge crossing overhead
      // still does, the vehicle's own path never.
      if (bestLineHit && (!bestBoxHit ||
          bestLineHit.z >= bestBoxHit.z + (bestBoxHit.bucket.lineHitClearanceM || 0))) {
        return bestLineHit;
      }
      if (bestBoxHit) {
        return bestBoxHit;
      }

      // Phase 2: nearest-point/icon fallback (also covers rect buckets by
      // center distance, in case the cursor is just outside the box).
      // Lines are handled entirely by phase 0 above, not here. Icon buckets
      // get their own, larger tolerance (see below), so candidates are
      // ranked by dist/tolerance ("how far into its own allowed radius")
      // rather than raw distance -- otherwise a valid icon hit using its
      // bigger radius could lose out to a closer-but-still-out-of-range
      // plain point just because its raw distance looks bigger.
      var best = null;
      var bestScore = 1;
      for (var b2 = 0; b2 < this.buckets.length; b2++) {
        var bucket = this.buckets[b2];
        if (!bucket.visible || !bucket.ids || bucket.tooltipKind === "none" || bucket.renderType === "line") {
          continue;
        }
        var stride = bucket.pointStride;
        var altIdx = stride - 1;
        // Icon buckets (see _drawIconBucket) draw their pin's circle well
        // above the actual point, not centered on it -- without this, the
        // hover/click area would only cover the tail's tip, missing the
        // much larger visible circle+icon that's where the cursor
        // actually ends up. yOffsetMapUnits is the same upward screen-px
        // shift _drawIconBucket uses (radius + tailLength), converted to
        // map units; effectiveTolerance grows to the pin's own on-screen
        // radius so anywhere within the visible circle counts as a hit.
        var effectiveTolerance = toleranceMapUnits;
        var yOffsetMapUnits = 0;
        if (bucket.renderType === "icon") {
          var iconRadiusPx = _iconRadiusForZoom(zoom);
          yOffsetMapUnits = (iconRadiusPx + iconRadiusPx * 0.7) / scaleFactor;
          effectiveTolerance = iconRadiusPx / scaleFactor;
        }
        // effectiveTolerance bounds how far a match can be, so only cells
        // within that radius of the (possibly shifted) query center need
        // to be visited (see phase 1).
        var queryY = y - yOffsetMapUnits;
        var pointIndices = _collectGridIndices(bucket._grid, x - effectiveTolerance, x + effectiveTolerance, queryY - effectiveTolerance, queryY + effectiveTolerance, this._scratchIndices);
        for (var pi = 0; pi < pointIndices.length; pi++) {
          var idx = pointIndices[pi];
          if (_isHidden(bucket, idx)) {
            continue;
          }
          var p = idx * stride;
          var z = bucket.points[p + altIdx];
          if (z < altMin || z > altMax) {
            continue;
          }
          var dx = bucket.points[p] - x;
          var dy = (bucket.points[p + 1] + yOffsetMapUnits) - y;
          var dist = Math.sqrt(dx * dx + dy * dy);
          var pointScore = dist / effectiveTolerance;
          if (pointScore < bestScore) {
            bestScore = pointScore;
            best = { bucket: bucket, id: bucket.ids[idx], index: idx, z: z };
          }
        }
      }
      return best;
    },
  });

  // Builds a uniform grid index over a point-bucket's Float32Array so redraws
  // and hit-tests can skip straight to the cells overlapping the current
  // viewport instead of scanning every point (see addBucket). Grid bounds
  // and cell size are derived from the actual point spread, not the whole
  // map, so sparse/clustered buckets still get a useful index. Cell count
  // is capped at 128 per axis -- beyond that the per-cell array overhead
  // stops paying for itself relative to the point count it's indexing.
  function _buildPointGrid(points, stride) {
    var n = points.length / stride;
    var minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (var p = 0; p < points.length; p += stride) {
      var x = points[p];
      var y = points[p + 1];
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
    }
    var gridSize = Math.max(1, Math.min(128, Math.round(Math.sqrt(n) / 2)));
    var cellW = Math.max((maxX - minX) / gridSize, 1e-6);
    var cellH = Math.max((maxY - minY) / gridSize, 1e-6);
    var cells = new Array(gridSize * gridSize);
    for (var c = 0; c < cells.length; c++) {
      cells[c] = [];
    }
    var idx = 0;
    for (var i = 0; i < points.length; i += stride, idx++) {
      var cx = Math.min(gridSize - 1, Math.floor((points[i] - minX) / cellW));
      var cy = Math.min(gridSize - 1, Math.floor((points[i + 1] - minY) / cellH));
      cells[cy * gridSize + cx].push(idx);
    }
    return { minX: minX, minY: minY, cellW: cellW, cellH: cellH, gridSize: gridSize, cells: cells };
  }

  // Fills `out` (a reused scratch array -- see initialize) with the point
  // indices (not byte offsets -- multiply by stride at the call site) whose
  // cell overlaps [minX,maxX]x[minY,maxY], and returns it. The bounds check
  // is still re-applied per point at the call site since cells are coarse
  // rectangles that can straddle the viewport edge.
  function _collectGridIndices(grid, minX, maxX, minY, maxY, out) {
    out.length = 0;
    if (!grid) {
      return out;
    }
    var gridSize = grid.gridSize;
    var cx0 = Math.max(0, Math.floor((minX - grid.minX) / grid.cellW));
    var cx1 = Math.min(gridSize - 1, Math.floor((maxX - grid.minX) / grid.cellW));
    var cy0 = Math.max(0, Math.floor((minY - grid.minY) / grid.cellH));
    var cy1 = Math.min(gridSize - 1, Math.floor((maxY - grid.minY) / grid.cellH));
    if (cx1 < 0 || cy1 < 0 || cx0 >= gridSize || cy0 >= gridSize || cx0 > cx1 || cy0 > cy1) {
      return out;
    }
    for (var cy = cy0; cy <= cy1; cy++) {
      var rowBase = cy * gridSize;
      for (var cx = cx0; cx <= cx1; cx++) {
        var cell = grid.cells[rowBase + cx];
        for (var k = 0; k < cell.length; k++) {
          out.push(cell[k]);
        }
      }
    }
    return out;
  }

  // Per-line bounding box (x/y/z), computed once when a line bucket is
  // added -- see _drawLineBucket, which uses this for an O(1) off-screen
  // rejection instead of scanning every vertex of every line on every redraw.
  function _buildLineBounds(lines, stride) {
    var altIdx = stride - 1;
    var bounds = new Array(lines.length);
    for (var L_ = 0; L_ < lines.length; L_++) {
      var pts = lines[L_];
      var minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity, minZ = Infinity, maxZ = -Infinity;
      for (var k = 0; k < pts.length; k += stride) {
        var x = pts[k], y = pts[k + 1], z = pts[k + altIdx];
        if (x < minX) minX = x;
        if (x > maxX) maxX = x;
        if (y < minY) minY = y;
        if (y > maxY) maxY = y;
        if (z < minZ) minZ = z;
        if (z > maxZ) maxZ = z;
        if (stride >= 7) {
          // The drawn bezier stays inside its control points' hull, which
          // bulges outside the vertex-only bbox on curved segments -- fold
          // both of this vertex's control points in so a curve's bulge is
          // neither culled from drawing at the viewport edge nor rejected
          // by hitTest's bbox check. (Slightly conservative: the first
          // vertex's arrive / last vertex's leave control points are never
          // drawn, but including them only over-inflates, never clips.)
          var apx = x - pts[k + 2] / 3, apy = y - pts[k + 3] / 3;
          var lpx = x + pts[k + 4] / 3, lpy = y + pts[k + 5] / 3;
          if (apx < minX) minX = apx;
          if (apx > maxX) maxX = apx;
          if (apy < minY) minY = apy;
          if (apy > maxY) maxY = apy;
          if (lpx < minX) minX = lpx;
          if (lpx > maxX) maxX = lpx;
          if (lpy < minY) minY = lpy;
          if (lpy > maxY) maxY = lpy;
        }
      }
      bounds[L_] = { minX: minX, maxX: maxX, minY: minY, maxY: maxY, minZ: minZ, maxZ: maxZ };
    }
    return bounds;
  }

  // Shared Image() cache for _drawIconBucket -- a fresh `new Image()` per
  // draw call would re-request/re-decode the same file every redraw (every
  // pan/zoom). Triggers one redraw once a not-yet-seen icon finishes loading.
  var _iconCache = {};
  function _getIcon(url) {
    var cached = _iconCache[url];
    if (cached) {
      return cached;
    }
    var img = new Image();
    img.onload = function() {
      if (MapApp.layer) {
        MapApp.layer.requestRedraw();
      }
    };
    img.src = url;
    _iconCache[url] = img;
    return img;
  }

  // One pin's full geometry -- tail, circle, glyph -- with the tip landing
  // exactly on (tipX, tipY). Shared between the sprite bake below and
  // _drawSinglePin (the hover/highlight path, which draws at most a handful
  // of pins per frame and wants a custom outline color).
  // Tail and circle are filled as two SEPARATE fill() calls rather
  // than one combined path -- combining them into a single path and
  // relying on the nonzero winding rule to merge the overlap looked
  // right in theory, but the tail's winding direction ended up
  // opposite the circle's there, so the rule canceled the overlap out
  // to a hole instead of solid fill. Two plain opaque white fills of
  // the same color have no such winding interaction: painting white
  // over white in the overlap is still just white.
  function _paintPin(ctx, tipX, tipY, radius, fillColor, strokeColor, lineWidth, img) {
    var tailLength = radius * 0.7;
    var tailHalfWidth = radius * 0.5;
    var tailBaseInset = radius * 0.6;
    var imageSize = radius * 1.3;
    var circleX = tipX;
    var circleY = tipY - radius - tailLength;

    ctx.beginPath();
    ctx.moveTo(circleX - tailHalfWidth, circleY + tailBaseInset);
    ctx.lineTo(tipX, tipY);
    ctx.lineTo(circleX + tailHalfWidth, circleY + tailBaseInset);
    ctx.closePath();
    ctx.fillStyle = fillColor;
    ctx.fill();

    ctx.beginPath();
    ctx.arc(circleX, circleY, radius, 0, Math.PI * 2);
    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = lineWidth;
    ctx.stroke();

    if (img.complete && img.naturalWidth > 0) {
      ctx.drawImage(img, circleX - imageSize / 2, circleY - imageSize / 2, imageSize, imageSize);
    }
  }

  // Pre-rendered pin sprites for the bulk icon pass. Painting a pin costs
  // two path fills, an arc stroke, and a drawImage that downscales a 256px
  // source PNG to ~20px -- fine for one highlight pin, but the WebGL layer
  // repaints EVERY visible pin on EVERY frame of a pan (see its step 3), so
  // a couple thousand collectable/resource pins did all of that per pin per
  // frame and pans dropped to ~10fps (worse yet software-rendered, where
  // the downscale and the antialiased arc are pure CPU). Baking each
  // distinct (icon, radius, colors) combination once -- radius takes one
  // discrete value per zoom level (_iconRadiusForZoom) -- turns the
  // per-frame cost into a single small canvas-to-canvas blit per pin.
  // Sprites bake at devicePixelRatio resolution so hidpi screens keep crisp
  // pins even though the pin canvas itself is CSS-resolution.
  var _pinSpriteCache = {};
  function _getPinSprite(bucket, radius) {
    var img = _getIcon(bucket.iconUrl);
    if (!img.complete || img.naturalWidth === 0) {
      return null; // Not loaded yet -- _getIcon's onload will trigger a redraw.
    }
    var fillColor = bucket.pinFillColor || "#ffffff";
    var strokeColor = bucket.color || "#999999";
    var key = bucket.iconUrl + "|" + radius + "|" + fillColor + "|" + strokeColor;
    var sprite = _pinSpriteCache[key];
    if (sprite) {
      return sprite;
    }
    var pad = 2; // room for the stroke (lineWidth 1.25) plus its antialiasing fringe
    var width = Math.ceil(radius * 2 + pad * 2);
    var height = Math.ceil(radius * 2 + radius * 0.7 + pad * 2); // circle + tail
    var dpr = window.devicePixelRatio || 1;
    var canvas = document.createElement("canvas");
    canvas.width = Math.ceil(width * dpr);
    canvas.height = Math.ceil(height * dpr);
    var ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    _paintPin(ctx, width / 2, height - pad, radius, fillColor, strokeColor, 1.25, img);
    sprite = { canvas: canvas, width: width, height: height, anchorX: width / 2, anchorY: height - pad };
    _pinSpriteCache[key] = sprite;
    return sprite;
  }

  // True if this specific point/line index was individually hidden via a
  // right-click "Hide this object" (see MapApp.hideObject/ContextMenu).
  // bucket.hiddenIndices is left undefined for the overwhelming majority of
  // buckets (nothing in them has ever been individually hidden), so this is
  // a single false-y check with no Set allocated or touched in the common case.
  function _isHidden(bucket, idx) {
    return !!bucket.hiddenIndices && bucket.hiddenIndices.has(idx);
  }

  // Per-point footprint override for a rect bucket -- see
  // sav_map_data.collectBuildings' tiltedFootprints. Most buildings only
  // ever rotate around the vertical axis (yaw), so one shared
  // bucket.footprintPixels covers every instance; a Pillar/Beam bracing a
  // diagonal run between two out-of-line snap points genuinely has pitch/
  // roll baked into its rotation too, which can make its true top-down
  // extent bigger than the bucket's default -- tiltedFootprints carries a
  // precomputed, correct override for just those (rare) instances, keyed by
  // point index, so the overwhelming majority of points here take the cheap
  // path (a single object-property check that's immediately false).
  function _footprintForPoint(bucket, idx, yaw) {
    var verts = bucket.tiltedFootprints && bucket.tiltedFootprints[idx];
    if (verts) {
      return { verts: verts };
    }
    return { halfWidth: bucket.footprintPixels[0], halfDepth: bucket.footprintPixels[1], yaw: yaw };
  }

  // Traces one already-rotated polygon (a tilted instance's true top-down
  // silhouette -- see sav_map_data._tiltedFootprintPolygon) into ctx's
  // current path. verts is a flat [x1,y1,x2,y2,...] list of pixel offsets
  // from (x,y), already in final orientation -- unlike _traceRect, no
  // rotation is applied here, just translation + the same screen scale.
  // Traces one full-detail polyline into ctx's current path -- the shared
  // vertex walk behind both _drawLineBucket's bulk pass and the hovered-line
  // highlight (_redrawHighlight), so the curve geometry only lives in one
  // place. stride semantics match the bucket shapes described at the top of
  // this file: stride 3 vertices connect with straight segments, stride 7
  // vertices with a cubic bezier built from the stored Hermite tangents.
  function _tracePolylinePath(ctx, pts, stride, affine) {
    var prevX = affine.originX + pts[0] * affine.scaleX;
    var prevY = affine.originY + pts[1] * affine.scaleY;
    ctx.moveTo(prevX, prevY);
    for (var i = stride; i < pts.length; i += stride) {
      var curX = affine.originX + pts[i] * affine.scaleX;
      var curY = affine.originY + pts[i + 1] * affine.scaleY;
      if (stride >= 7) {
        // A bezier whose chord is under ~4px can't visibly deviate from
        // a straight segment (tangent magnitudes are on the order of the
        // chord, so the bulge is a fraction of it) -- lineTo is much
        // cheaper to build and rasterize at path sizes this large.
        var sdx = curX - prevX, sdy = curY - prevY;
        if (sdx * sdx + sdy * sdy < 16) {
          ctx.lineTo(curX, curY);
        } else {
          var cp1x = prevX + (pts[i - stride + 4] / 3) * affine.scaleX; // prev vertex's leaveTangentX
          var cp1y = prevY + (pts[i - stride + 5] / 3) * affine.scaleY; // prev vertex's leaveTangentY
          var cp2x = curX - (pts[i + 2] / 3) * affine.scaleX; // cur vertex's arriveTangentX
          var cp2y = curY - (pts[i + 3] / 3) * affine.scaleY; // cur vertex's arriveTangentY
          ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, curX, curY);
        }
      } else {
        ctx.lineTo(curX, curY);
      }
      prevX = curX;
      prevY = curY;
    }
  }

  function _tracePolygon(ctx, x, y, verts, affine) {
    var cx = affine.originX + x * affine.scaleX;
    var cy = affine.originY + y * affine.scaleY;
    for (var k = 0; k < verts.length; k += 2) {
      var sx = cx + verts[k] * affine.scaleX;
      var sy = cy + verts[k + 1] * affine.scaleY;
      if (k === 0) {
        ctx.moveTo(sx, sy);
      } else {
        ctx.lineTo(sx, sy);
      }
    }
    ctx.closePath();
  }

  // Standard ray-casting point-in-polygon test (even-odd rule) -- (localX,
  // localY) and verts must already be in the same (untransformed map-pixel)
  // space, which is exactly how hitTest phase 1 uses this: the cursor's
  // offset from the instance's center against verts as sav_map_data
  // computed them (see _tracePolygon's doc comment on why no rotation is
  // needed here either).
  function _pointInPolygon(localX, localY, verts) {
    var inside = false;
    var n = verts.length / 2;
    for (var i = 0, j = n - 1; i < n; j = i++) {
      var xi = verts[i * 2], yi = verts[i * 2 + 1];
      var xj = verts[j * 2], yj = verts[j * 2 + 1];
      var intersect = ((yi > localY) !== (yj > localY)) &&
        (localX < (xj - xi) * (localY - yi) / (yj - yi) + xi);
      if (intersect) {
        inside = !inside;
      }
    }
    return inside;
  }

  // bucket.color is always a "#rrggbb" hex string (see filters.js's color
  // tables) -- this is a small cache since it's called every redraw for
  // every visible rect bucket, not just once.
  var _alphaColorCache = {};
  function _withAlpha(hexColor, alpha) {
    var cacheKey = hexColor + "|" + alpha;
    var cached = _alphaColorCache[cacheKey];
    if (cached) {
      return cached;
    }
    var r = parseInt(hexColor.slice(1, 3), 16);
    var g = parseInt(hexColor.slice(3, 5), 16);
    var b = parseInt(hexColor.slice(5, 7), 16);
    var result = "rgba(" + r + "," + g + "," + b + "," + alpha + ")";
    _alphaColorCache[cacheKey] = result;
    return result;
  }

  // Shared by _redraw (drawing) and hitTest (so the hover/click area matches
  // where the pin is actually drawn) -- see _drawIconBucket's "O with a V"
  // pin shape.
  function _iconRadiusForZoom(zoom) {
    return Math.min(32, 16 + Math.max(0, zoom) * 4);
  }

  // Distance from (px,py) to the cubic bezier drawn for one stride-7
  // segment (control points already converted from Hermite tangents by the
  // caller -- keep in sync with _tracePolylinePath). A bezier never leaves
  // its control points' convex hull, and its deviation from the chord is at
  // most 3/4 of the control points' -- so when both control points sit
  // within flatTolerance of the chord, the plain chord distance is already
  // accurate to well within the hover tolerance and the flattening below is
  // skipped. Curvier segments get flattened into 8 sub-chords, whose
  // residual error (deviation/64) is negligible at any curvature belts or
  // rails actually produce.
  function _pointToBezierDistance(px, py, ax, ay, c1x, c1y, c2x, c2y, bx, by, flatTolerance) {
    if (_pointToSegmentDistance(c1x, c1y, ax, ay, bx, by) < flatTolerance &&
        _pointToSegmentDistance(c2x, c2y, ax, ay, bx, by) < flatTolerance) {
      return _pointToSegmentDistance(px, py, ax, ay, bx, by);
    }
    var best = Infinity;
    var prevX = ax, prevY = ay;
    for (var s = 1; s <= 8; s++) {
      var t = s / 8;
      var mt = 1 - t;
      var w0 = mt * mt * mt, w1 = 3 * mt * mt * t, w2 = 3 * mt * t * t, w3 = t * t * t;
      var curX = w0 * ax + w1 * c1x + w2 * c2x + w3 * bx;
      var curY = w0 * ay + w1 * c1y + w2 * c2y + w3 * by;
      var d = _pointToSegmentDistance(px, py, prevX, prevY, curX, curY);
      if (d < best) {
        best = d;
      }
      prevX = curX;
      prevY = curY;
    }
    return best;
  }

  function _pointToSegmentDistance(px, py, ax, ay, bx, by) {
    var abx = bx - ax;
    var aby = by - ay;
    var lengthSq = abx * abx + aby * aby;
    var t = lengthSq > 0 ? ((px - ax) * abx + (py - ay) * aby) / lengthSq : 0;
    t = Math.max(0, Math.min(1, t));
    var cx = ax + t * abx;
    var cy = ay + t * aby;
    var dx = px - cx;
    var dy = py - cy;
    return Math.sqrt(dx * dx + dy * dy);
  }

  // Global altitude (Z, meters) filter applied across all point buckets.
  MapApp.altitudeRange = { min: -Infinity, max: Infinity };
  MapApp.setAltitudeRange = function(min, max) {
    MapApp.altitudeRange = { min: min, max: max };
    if (MapApp.layer) {
      MapApp.layer.requestRedraw();
    }
  };

  // Currently hovered/pinned box (see _redrawHighlight), drawn at full
  // opacity with a bright outline so it's obvious which one the tooltip is
  // describing. Uses the cheap highlight-only redraw path -- this fires on
  // every hover change, so it must not touch the full bucket redraw.
  MapApp.highlightedBucket = null;
  MapApp.highlightedId = null;
  MapApp.setHighlight = function(bucket, id) {
    MapApp.highlightedBucket = bucket || null;
    MapApp.highlightedId = bucket ? id : null;
    if (MapApp.layer) {
      MapApp.layer.requestHighlightRedraw();
    }
  };

  // Hides one specific instance within a bucket (a right-click "Hide this
  // object" -- see ContextMenu) rather than the whole bucket/layer. `index`
  // is a point index for circle/icon/rect buckets or a line index for line
  // buckets -- whichever hitTest returned as `hit.index`. Not persisted
  // across a save reload: buckets (and their indices) are rebuilt fresh by
  // Filters.build every load, same as everything else keyed by bucket index.
  MapApp.hideObject = function(bucket, index) {
    if (index === undefined || index === null) {
      return;
    }
    if (!bucket.hiddenIndices) {
      bucket.hiddenIndices = new Set();
    }
    bucket.hiddenIndices.add(index);
    if (MapApp.highlightedBucket === bucket && bucket.ids && bucket.ids[index] === MapApp.highlightedId) {
      MapApp.setHighlight(null, null);
      if (window.Tooltip) {
        window.Tooltip.hide();
      }
    }
    if (MapApp.layer) {
      MapApp.layer.requestRedraw();
    }
  };

  // Total count across every bucket's hiddenIndices -- drives the sidebar's
  // "Reset hidden objects" button (see filters.js), which only makes sense
  // to show at all once this is greater than zero.
  MapApp.countHiddenObjects = function() {
    if (!MapApp.layer) {
      return 0;
    }
    var total = 0;
    MapApp.layer.buckets.forEach(function(bucket) {
      if (bucket.hiddenIndices) {
        total += bucket.hiddenIndices.size;
      }
    });
    return total;
  };

  // Un-hides every individually-hidden object across every bucket -- the
  // only way to undo MapApp.hideObject short of reloading the save, since
  // (unlike a layer/category hide) there's no sidebar checkbox tracking
  // these to just re-check.
  MapApp.resetHiddenObjects = function() {
    if (!MapApp.layer) {
      return;
    }
    MapApp.layer.buckets.forEach(function(bucket) {
      bucket.hiddenIndices = null;
    });
    MapApp.layer.requestRedraw();
  };

  // Exposed for webgl_layer.js: the WebGL layer extends BucketedCanvasLayer
  // (inheriting hitTest, the highlight/pin drawing paths, and addBucket's
  // grid building, all of which close over this file's private helpers), and
  // its 2D pin pass needs the same pin radius the inherited hit-testing uses.
  MapApp.BucketedCanvasLayer = BucketedCanvasLayer;
  MapApp.iconRadiusForZoom = _iconRadiusForZoom;

  // Swaps the WebGL layer out for the plain 2D canvas layer -- called when
  // the GL context is lost (driver reset, GPU removed) or fails to create.
  // The bucket objects (points/lines/_grid/_lineBounds and all) are shared
  // by reference, so no data rebuild is needed; the animation options the
  // GL path enabled go back off because the 2D layer only redraws on
  // moveend/zoomend (see MapApp.init's option comments).
  MapApp.fallbackToCanvasLayer = function() {
    var old = MapApp.layer;
    var map = MapApp.map;
    if (!map || !old || !old._isWebGL) {
      return; // Already on the 2D layer (or nothing to swap).
    }
    map.removeLayer(old);
    map.options.zoomAnimation = false;
    map.options.fadeAnimation = false;
    map.options.inertia = false;
    // Leaflet caches options.zoomAnimation into _zoomAnimated at map
    // construction and only ever reads the cache -- without flipping it too,
    // the first zoom after fallback CSS-scales a canvas that never repaints
    // until the debounced 2D redraw lands.
    map._zoomAnimated = false;
    var layer = new BucketedCanvasLayer();
    layer.buckets = old.buckets;
    layer._sortedBuckets = null;
    layer.addTo(map);
    MapApp.layer = layer;
    layer.requestRedraw();
  };

  MapApp.init = function() {
    // The WebGL layer (webgl_layer.js, loaded right after this file) renders
    // every frame during interaction, so Leaflet's animated zoom and drag
    // inertia -- disabled below for the 2D layer, which only redraws on
    // moveend/zoomend -- work fine with it and are turned back on.
    var useWebGL = !!(window.WebGLBucketedLayer && window.WebGLBucketedLayer.isSupported());
    var map = L.map("map", {
      crs: L.CRS.Simple,
      minZoom: -3,
      maxZoom: 7, // map_highres.png is 8192px (game-native fused map, see game_data/extract_map_image.py), ~1.64x the old 5000px upscale; bumped from 6 so the extra detail is actually reachable.
      attributionControl: false,
      // The map now spans the full viewport with the sidebar and top
      // controls floating over it -- Leaflet's default top-left slot sits
      // underneath both, so the zoom control lives bottom-right instead
      // (next to the altitude rail, over nothing but map).
      zoomControl: false,
      maxBoundsViscosity: 0.8,
      // The 2D canvas overlay only redraws on moveend/zoomend (see
      // BucketedCanvasLayer), so Leaflet's animated zoom would visibly scale
      // the image first and snap the points into place a moment later.
      // Disabling it keeps both in sync.
      zoomAnimation: useWebGL,
      fadeAnimation: useWebGL,
      // Same reasoning applies to drag inertia: it keeps the base map
      // gliding after mouseup while the 2D canvas overlay sits frozen (it
      // only redraws on moveend) until the glide actually stops, which reads
      // as a stuck/laggy redraw. Disabling inertia makes panning stop
      // exactly when the mouse does, so the overlay's moveend redraw fires
      // immediately instead of after an extra coast-to-stop delay.
      inertia: useWebGL,
    });
    L.control.zoom({ position: "bottomright" }).addTo(map);

    var mapSize = 8192;
    var margin = mapSize * 0.5;
    var bounds = [[0, 0], [mapSize, mapSize]];
    map.setMaxBounds([[-margin, -margin], [mapSize + margin, mapSize + margin]]);
    L.imageOverlay("map_highres.png", bounds).addTo(map);
    map.fitBounds(bounds);

    var layer = useWebGL ? new window.WebGLBucketedLayer() : new BucketedCanvasLayer();
    layer.addTo(map);

    MapApp.map = map;
    MapApp.layer = layer;
    MapApp.mapSize = mapSize;

    // Hover-driven tooltips: throttled so hit-testing runs at most every
    // HOVER_THROTTLE_MS, not on every mousemove tick (which can fire far
    // more often than that while the mouse is actually moving).
    var HOVER_THROTTLE_MS = 40;
    var lastHoverTime = 0;
    // No hover hit-testing while the map is moving: DOM mousemove keeps
    // firing during a drag, and running hitTest against a huge save every
    // 40ms while also panning is wasted work at the worst possible time.
    var isMoving = false;
    map.on("movestart", function() { isMoving = true; });
    map.on("moveend", function() { isMoving = false; });
    map.on("mousemove", function(e) {
      if (isMoving) {
        return;
      }
      if (!window.Tooltip || window.Tooltip.isPinned()) {
        return; // A pinned tooltip stays put until explicitly unpinned (see click handler below).
      }
      if (window.ContextMenu && ContextMenu.isOpen()) {
        return; // Leave the right-clicked object's tooltip/highlight alone while its menu is up (see contextmenu.js).
      }
      var now = Date.now();
      if (now - lastHoverTime < HOVER_THROTTLE_MS) {
        return;
      }
      lastHoverTime = now;
      var hoverToleranceScreenPx = 8;
      var toleranceMapUnits = hoverToleranceScreenPx / Math.pow(2, map.getZoom());
      // Via MapApp.layer, not the closed-over `layer`: a GL context loss can
      // swap the layer instance out from under this handler (see
      // MapApp.fallbackToCanvasLayer), and hit-testing the detached old
      // layer would silently misfire.
      var hit = MapApp.layer.hitTest(e.latlng.lng, e.latlng.lat, toleranceMapUnits);
      if (hit) {
        window.Tooltip.show(e.originalEvent.clientX, e.originalEvent.clientY, hit);
        MapApp.setHighlight(hit.bucket, hit.id);
      } else {
        window.Tooltip.hide();
        MapApp.setHighlight(null, null);
      }
    });
    map.on("click", function(e) {
      if (!window.Tooltip) {
        return;
      }
      var clickToleranceScreenPx = 8;
      var toleranceMapUnits = clickToleranceScreenPx / Math.pow(2, map.getZoom());
      var hit = MapApp.layer.hitTest(e.latlng.lng, e.latlng.lat, toleranceMapUnits); // MapApp.layer, not `layer` -- see the hover handler.
      if (hit) {
        window.Tooltip.pin(e.originalEvent.clientX, e.originalEvent.clientY, hit);
        MapApp.setHighlight(hit.bucket, hit.id);
      } else {
        window.Tooltip.unpin();
        MapApp.setHighlight(null, null);
      }
    });
    map.on("mouseout", function() {
      if (window.ContextMenu && ContextMenu.isOpen()) {
        return; // Keep the right-clicked object highlighted while its menu is up.
      }
      if (window.Tooltip && !window.Tooltip.isPinned()) {
        window.Tooltip.hide();
        MapApp.setHighlight(null, null);
      }
    });
  };
})();
