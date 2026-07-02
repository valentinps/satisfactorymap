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
  // bucket skips rotated-quad rendering entirely in favor of plotting tiny
  // axis-aligned squares -- the case that matters at tens-of-thousands-of-
  // boxes scale (lightweight buildables), where this is the dominant cost.
  var SUBPIXEL_RECT_THRESHOLD = 2;

  // See _drawRectBuckets: max altitude spread (in meters) among currently
  // visible rects below which fill order is treated as not mattering, so
  // the per-point sort is skipped. Comfortably smaller than one floor's
  // height (a default foundation/wall is 4m), so any genuine multi-floor
  // overlap still takes the sorted path, while a single coplanar floor's
  // minor snapping jitter doesn't.
  var FLAT_PLATFORM_Z_EPSILON = 1.0;

  var BucketedCanvasLayer = L.Layer.extend({
    initialize: function() {
      this.buckets = []; // Public: filters.js pushes/reads bucket objects here directly.
      // Reused across redraws/hit-tests to avoid allocating a fresh array for
      // every bucket on every frame (see _collectGridIndices) -- safe since
      // everything here runs synchronously on the main thread, one bucket at
      // a time.
      this._scratchIndices = [];
    },

    clearBuckets: function() {
      this.buckets = [];
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

      map.on("moveend zoomend resize", this._reset, this);
      this._reset();
    },

    onRemove: function(map) {
      this.getPane().removeChild(this._canvas);
      this.getPane().removeChild(this._highlightCanvas);
      map.off("moveend zoomend resize", this._reset, this);
    },

    requestRedraw: function() {
      if (this._map) {
        this._redraw();
        this._redrawHighlight();
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
      var size = map.getSize();
      var topLeft = map.containerPointToLayerPoint([0, 0]);
      L.DomUtil.setPosition(this._canvas, topLeft);
      L.DomUtil.setPosition(this._highlightCanvas, topLeft);
      this._canvas.width = size.x;
      this._canvas.height = size.y;
      this._highlightCanvas.width = size.x;
      this._highlightCanvas.height = size.y;
      this._topLeft = topLeft;
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
      if (!bucket || bucket.renderType !== "rect" || !bucket.ids) {
        return;
      }
      var idx = bucket.ids.indexOf(MapApp.highlightedId);
      if (idx === -1) {
        return;
      }
      var p = idx * 4;
      var zoom = map.getZoom();
      var pixelOrigin = map.getPixelOrigin();
      var topLeft = this._topLeft;
      var affine = this._computeAffine(zoom, pixelOrigin, topLeft);

      ctx.beginPath();
      this._traceRect(ctx, bucket.points[p], bucket.points[p + 1], bucket.points[p + 2], bucket.footprintPixels[0], bucket.footprintPixels[1], affine);
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
      var bounds = map.getBounds();
      var minX = bounds.getWest();
      var maxX = bounds.getEast();
      var minY = bounds.getSouth();
      var maxY = bounds.getNorth();
      var circleRadius = Math.min(3, 1 + Math.max(0, zoom) * 0.4);
      var iconRadius = _iconRadiusForZoom(zoom);
      var affine = this._computeAffine(zoom, pixelOrigin, topLeft);
      var altMin = MapApp.altitudeRange ? MapApp.altitudeRange.min : -Infinity;
      var altMax = MapApp.altitudeRange ? MapApp.altitudeRange.max : Infinity;

      // Canvas painting is just layering -- whatever's drawn last sits on
      // top. Non-rect buckets (lines/circles/icons) still go by drawPriority
      // (see filters.js's makePointBucket) since they rarely visually
      // conflict with each other; this sorts the (few hundred) bucket
      // objects themselves, not their underlying point arrays, so it's
      // cheap even though it runs every redraw. Rect buckets (every
      // building/foundation, across every category) are pulled out and
      // drawn separately by _drawRectBuckets, which orders them by actual
      // altitude instead -- see that function for why drawPriority alone
      // isn't enough once buildings span multiple floors.
      var orderedBuckets = this.buckets.slice().sort(function(a, b) { return (a.drawPriority || 0) - (b.drawPriority || 0); });
      var rectBuckets = [];

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
          this._drawIconBucket(ctx, bucket, affine, minX, maxX, minY, maxY, iconRadius, altMin, altMax);
        } else {
          this._drawCircleBucket(ctx, bucket, affine, minX, maxX, minY, maxY, circleRadius, altMin, altMax);
        }
      }

      this._drawRectBuckets(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax);
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
      var img = _getIcon(bucket.iconUrl);
      if (!img.complete || img.naturalWidth === 0) {
        return; // Not loaded yet -- _getIcon's onload will trigger a redraw.
      }
      var stride = bucket.pointStride;
      var altIdx = stride - 1;
      var tailLength = radius * 0.7; // Circle-bottom-to-tip distance.
      var tailHalfWidth = radius * 0.5;
      var tailBaseInset = radius * 0.6; // Keeps the tail's base inside the circle so the two fills below overlap with no gap.
      var imageSize = radius * 1.3;
      var prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = bucket.iconOpacity !== undefined ? bucket.iconOpacity : 1;
      var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
      for (var ii = 0; ii < indices.length; ii++) {
        var i = indices[ii] * stride;
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

        // Tail and circle are filled as two SEPARATE fill() calls rather
        // than one combined path -- combining them into a single path and
        // relying on the nonzero winding rule to merge the overlap looked
        // right in theory, but the tail's winding direction ended up
        // opposite the circle's there, so the rule canceled the overlap out
        // to a hole instead of solid fill. Two plain opaque white fills of
        // the same color have no such winding interaction: painting white
        // over white in the overlap is still just white.
        var fillColor = bucket.pinFillColor || "#ffffff";
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
        ctx.strokeStyle = bucket.color || "#999999";
        ctx.lineWidth = 1.25;
        ctx.stroke();

        ctx.drawImage(img, circleX - imageSize / 2, circleY - imageSize / 2, imageSize, imageSize);
      }
      ctx.globalAlpha = prevAlpha;
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
      var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
      for (var ii = 0; ii < indices.length; ii++) {
        var i = indices[ii] * stride;
        var x = pts[i];
        var y = pts[i + 1];
        var z = pts[i + altIdx];
        if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
          continue;
        }
        var cx = affine.originX + x * affine.scaleX;
        var cy = affine.originY + y * affine.scaleY;
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
      var corners = [
        [cosW - sinD, sinW + cosD],
        [-cosW - sinD, -sinW + cosD],
        [-cosW + sinD, -sinW - cosD],
        [cosW + sinD, sinW - cosD],
      ];
      for (var k = 0; k < 4; k++) {
        var sx = cx + corners[k][0] * affine.scaleX;
        var sy = cy + corners[k][1] * affine.scaleY;
        if (k === 0) {
          ctx.moveTo(sx, sy);
        } else {
          ctx.lineTo(sx, sy);
        }
      }
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
      var globalMinZ = Infinity, globalMaxZ = -Infinity;
      for (var zb = 0; zb < rectBuckets.length; zb++) {
        var zBucket = rectBuckets[zb];
        var zPts = zBucket.points;
        if (zPts.length === 0) {
          continue;
        }
        var zIndices = _collectGridIndices(zBucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
        for (var zi = 0; zi < zIndices.length; zi++) {
          var zp = zIndices[zi] * 4;
          var zz = zPts[zp + 3];
          if (zPts[zp] < minX || zPts[zp] > maxX || zPts[zp + 1] < minY || zPts[zp + 1] > maxY || zz < altMin || zz > altMax) {
            continue;
          }
          if (zz < globalMinZ) globalMinZ = zz;
          if (zz > globalMaxZ) globalMaxZ = zz;
        }
      }
      if (globalMinZ > globalMaxZ) {
        return; // Nothing visible.
      }

      if (globalMaxZ - globalMinZ <= FLAT_PLATFORM_Z_EPSILON) {
        this._drawRectBucketsFlat(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax);
      } else {
        this._drawRectBucketsSorted(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax);
      }
    },

    // Fast path: no point in this view set is more than FLAT_PLATFORM_Z_EPSILON
    // away from any other, so fill order can't visibly matter -- draw each
    // bucket directly (one beginPath/fill per bucket, like the original
    // single-bucket version of this code) with no per-point allocation or sort.
    _drawRectBucketsFlat: function(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax) {
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        var bucket = rectBuckets[bi];
        var pts = bucket.points;
        if (pts.length === 0) {
          continue;
        }
        var halfWidth = bucket.footprintPixels[0];
        var halfDepth = bucket.footprintPixels[1];
        var tiny = halfWidth * 2 * affine.scaleX < SUBPIXEL_RECT_THRESHOLD && halfDepth * 2 * affine.scaleY < SUBPIXEL_RECT_THRESHOLD;
        ctx.fillStyle = _withAlpha(bucket.color, 0.55);
        ctx.beginPath();
        var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
        for (var ii = 0; ii < indices.length; ii++) {
          var i = indices[ii] * 4;
          var x = pts[i];
          var y = pts[i + 1];
          var yaw = pts[i + 2];
          var z = pts[i + 3];
          if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
            continue;
          }
          if (tiny) {
            var cx = affine.originX + x * affine.scaleX;
            var cy = affine.originY + y * affine.scaleY;
            ctx.rect(cx - 0.75, cy - 0.75, 1.5, 1.5);
          } else {
            this._traceRect(ctx, x, y, yaw, halfWidth, halfDepth, affine);
          }
        }
        ctx.fill();
      }
    },

    // Slow path: points genuinely span multiple floors, so fill order has to
    // follow actual altitude (lowest first) to match hitTest's "highest Z
    // wins" rule -- see the big comment on _drawRectBuckets above.
    _drawRectBucketsSorted: function(ctx, rectBuckets, affine, minX, maxX, minY, maxY, altMin, altMax) {
      var items = [];
      for (var bi = 0; bi < rectBuckets.length; bi++) {
        var bucket = rectBuckets[bi];
        var pts = bucket.points;
        if (pts.length === 0) {
          continue;
        }
        var halfWidth = bucket.footprintPixels[0];
        var halfDepth = bucket.footprintPixels[1];
        var tiny = halfWidth * 2 * affine.scaleX < SUBPIXEL_RECT_THRESHOLD && halfDepth * 2 * affine.scaleY < SUBPIXEL_RECT_THRESHOLD;
        var indices = _collectGridIndices(bucket._grid, minX, maxX, minY, maxY, this._scratchIndices);
        for (var ii = 0; ii < indices.length; ii++) {
          var i = indices[ii] * 4;
          var x = pts[i];
          var y = pts[i + 1];
          var yaw = pts[i + 2];
          var z = pts[i + 3];
          if (x < minX || x > maxX || y < minY || y > maxY || z < altMin || z > altMax) {
            continue;
          }
          items.push({ z: z, bucket: bucket, x: x, y: y, yaw: yaw, tiny: tiny });
        }
      }
      if (items.length === 0) {
        return;
      }
      items.sort(function(a, b) { return a.z - b.z; });

      // ctx.fill() only applies one fillStyle to the whole current path, so
      // a fresh path/fill is needed whenever the color changes -- runs of
      // same-bucket items (common, since nearby altitudes tend to share a
      // floor/category) still batch into a single fill() the same as before.
      var currentColor = null;
      var hasOpenPath = false;
      for (var k = 0; k < items.length; k++) {
        var item = items[k];
        var color = _withAlpha(item.bucket.color, 0.55);
        if (color !== currentColor) {
          if (hasOpenPath) {
            ctx.fill();
          }
          ctx.beginPath();
          ctx.fillStyle = color;
          currentColor = color;
          hasOpenPath = true;
        }
        if (item.tiny) {
          var cx = affine.originX + item.x * affine.scaleX;
          var cy = affine.originY + item.y * affine.scaleY;
          ctx.rect(cx - 0.75, cy - 0.75, 1.5, 1.5);
        } else {
          this._traceRect(ctx, item.x, item.y, item.yaw, item.bucket.footprintPixels[0], item.bucket.footprintPixels[1], affine);
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
      for (var L_ = 0; L_ < lines.length; L_++) {
        var pts = lines[L_];
        // Precomputed once when the bucket was added (see _buildLineBounds) --
        // a single bbox-overlap check replaces a full scan of every vertex
        // just to find out a line is entirely off-screen.
        var lb = lineBounds && lineBounds[L_];
        if (lb && (lb.minX > maxX || lb.maxX < minX || lb.minY > maxY || lb.maxY < minY || lb.minZ > altMax || lb.maxZ < altMin)) {
          continue;
        }
        var prevX = affine.originX + pts[0] * affine.scaleX;
        var prevY = affine.originY + pts[1] * affine.scaleY;
        ctx.moveTo(prevX, prevY);
        for (var i = stride; i < pts.length; i += stride) {
          var curX = affine.originX + pts[i] * affine.scaleX;
          var curY = affine.originY + pts[i + 1] * affine.scaleY;
          if (stride >= 7) {
            var cp1x = prevX + (pts[i - stride + 4] / 3) * affine.scaleX; // prev vertex's leaveTangentX
            var cp1y = prevY + (pts[i - stride + 5] / 3) * affine.scaleY; // prev vertex's leaveTangentY
            var cp2x = curX - (pts[i + 2] / 3) * affine.scaleX; // cur vertex's arriveTangentX
            var cp2y = curY - (pts[i + 3] / 3) * affine.scaleY; // cur vertex's arriveTangentY
            ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, curX, curY);
          } else {
            ctx.lineTo(curX, curY);
          }
          prevX = curX;
          prevY = curY;
        }
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
        for (var li = 0; li < lineBucket.lines.length; li++) {
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
          // Hit-tested against the straight chord between consecutive
          // spline points, not the rendered curve itself -- close enough
          // given the generous hover/click tolerance, and avoids needing
          // point-to-bezier distance math.
          for (var i = 0; i + lineStride + 1 < pts.length; i += lineStride) {
            var d = _pointToSegmentDistance(x, y, pts[i], pts[i + 1], pts[i + lineStride], pts[i + lineStride + 1]);
            var lineScore = d / toleranceMapUnits;
            if (lineScore < bestLineScore) {
              bestLineScore = lineScore;
              bestLineHit = { bucket: lineBucket, id: lineBucket.ids[li], z: pts[i + lineAltIdx] };
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
        // A point further than this from the box center can't possibly be
        // inside it regardless of rotation -- checking this cheap distance
        // first, before any trig, skips the expensive inverse-rotation for
        // the vast majority of points on every single hover tick (this runs
        // on every mousemove, not just on redraw, so it's the hottest path
        // of all once a bucket has tens of thousands of points).
        var maxRadius = Math.sqrt(halfWidth * halfWidth + halfDepth * halfDepth);
        // Only cells within maxRadius of the cursor can possibly contain a
        // box that reaches the cursor -- same grid used by _redraw, queried
        // with a cursor-centered box instead of the viewport (see
        // _buildPointGrid/_collectGridIndices). This is what keeps hover/
        // click responsive once a bucket has tens of thousands of boxes.
        var rectIndices = _collectGridIndices(rectBucket._grid, x - maxRadius, x + maxRadius, y - maxRadius, y + maxRadius, this._scratchIndices);
        for (var ri = 0; ri < rectIndices.length; ri++) {
          var rectIdx = rectIndices[ri];
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
          // Inverse-rotate the cursor offset into the building's local frame
          // (see _drawRectBuckets for the matching forward rotation + the
          // note on why yaw is negated here).
          var cos = Math.cos(byaw);
          var sin = Math.sin(byaw);
          var localX = ddx * cos - ddy * sin;
          var localY = ddx * sin + ddy * cos;
          if (Math.abs(localX) <= halfWidth && Math.abs(localY) <= halfDepth && bz > bestBoxZ) {
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
      if (bestLineHit && (!bestBoxHit || bestLineHit.z >= bestBoxHit.z)) {
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

  MapApp.init = function() {
    var map = L.map("map", {
      crs: L.CRS.Simple,
      minZoom: -3,
      maxZoom: 6, // map_highres.png is ~2.44x blank_map20.png's resolution; bumped from 4 so that extra detail is actually reachable.
      attributionControl: false,
      maxBoundsViscosity: 0.8,
      // The canvas overlay only redraws on moveend/zoomend (see BucketedCanvasLayer),
      // so Leaflet's animated zoom would visibly scale the image first and snap
      // the points into place a moment later. Disabling it keeps both in sync.
      zoomAnimation: false,
      fadeAnimation: false,
      // Same reasoning applies to drag inertia: it keeps the base map
      // gliding after mouseup while the canvas overlay sits frozen (it only
      // redraws on moveend) until the glide actually stops, which reads as
      // a stuck/laggy redraw. Disabling inertia makes panning stop exactly
      // when the mouse does, so the overlay's moveend redraw fires
      // immediately instead of after an extra coast-to-stop delay.
      inertia: false,
    });

    var mapSize = 5000;
    var margin = mapSize * 0.5;
    var bounds = [[0, 0], [mapSize, mapSize]];
    map.setMaxBounds([[-margin, -margin], [mapSize + margin, mapSize + margin]]);
    L.imageOverlay("map_highres.png", bounds).addTo(map);
    map.fitBounds(bounds);

    var layer = new BucketedCanvasLayer();
    layer.addTo(map);

    MapApp.map = map;
    MapApp.layer = layer;
    MapApp.mapSize = mapSize;

    // Hover-driven tooltips: throttled so hit-testing runs at most every
    // HOVER_THROTTLE_MS, not on every mousemove tick (which can fire far
    // more often than that while the mouse is actually moving).
    var HOVER_THROTTLE_MS = 40;
    var lastHoverTime = 0;
    map.on("mousemove", function(e) {
      if (!window.Tooltip || window.Tooltip.isPinned()) {
        return; // A pinned tooltip stays put until explicitly unpinned (see click handler below).
      }
      var now = Date.now();
      if (now - lastHoverTime < HOVER_THROTTLE_MS) {
        return;
      }
      lastHoverTime = now;
      var hoverToleranceScreenPx = 8;
      var toleranceMapUnits = hoverToleranceScreenPx / Math.pow(2, map.getZoom());
      var hit = layer.hitTest(e.latlng.lng, e.latlng.lat, toleranceMapUnits);
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
      var hit = layer.hitTest(e.latlng.lng, e.latlng.lat, toleranceMapUnits);
      if (hit) {
        window.Tooltip.pin(e.originalEvent.clientX, e.originalEvent.clientY, hit);
        MapApp.setHighlight(hit.bucket, hit.id);
      } else {
        window.Tooltip.unpin();
        MapApp.setHighlight(null, null);
      }
    });
    map.on("mouseout", function() {
      if (window.Tooltip && !window.Tooltip.isPinned()) {
        window.Tooltip.hide();
        MapApp.setHighlight(null, null);
      }
    });
  };
})();
