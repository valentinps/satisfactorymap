// WebGL2 bulk renderer for the bucket layer -- replaces BucketedCanvasLayer's
// Canvas-2D rect/line/circle passes with instanced GPU draws so that pan/zoom
// stays interactive on 500k+ object saves. On the biggest test save the 2D
// path spends ~1.5s per redraw just BUILDING the canvas path (one path verb
// per building corner), before ~0.4s of raster; here the per-frame CPU work
// is a handful of draw calls plus one uniform transform, with all vertex data
// uploaded once per save load.
//
// Scope: rects, lines, and circles move to the GPU. Icon pins and the
// hover/pin highlight stay on 2D canvases layered ABOVE the GL canvas (2.6k
// pins and one highlight box are nowhere near the bottleneck, and pin/glyph
// drawing wants canvas anyway), and hitTest stays on the CPU untouched -- it
// reads bucket.points/lines directly, which this file never modifies.
//
// This class EXTENDS BucketedCanvasLayer (exported as
// MapApp.BucketedCanvasLayer) rather than reimplementing it: hitTest,
// _redrawHighlight, _drawSinglePin, _drawIconBucket, addBucket's grid
// building and _computeAffine are inherited byte-for-byte -- they close over
// map.js's private helpers (_collectGridIndices, _getIcon, _isHidden, ...),
// so a standalone copy would have to duplicate ~600 lines of them. The
// parent's buffered-canvas machinery (margin, zoom preview, redraw debounce)
// never runs because onAdd/onRemove are fully overridden and its event
// handlers are never bound: with per-frame rendering there is nothing to
// buffer or debounce.
//
// MapApp.init picks this layer when WebGL2 is available (see isSupported)
// and re-enables Leaflet's zoomAnimation/inertia for it; a lost GL context
// swaps the plain 2D layer back in (MapApp.fallbackToCanvasLayer).
(function() {
  "use strict";

  if (!window.MapApp || !MapApp.BucketedCanvasLayer || !window.L) {
    return; // Loaded out of order (or map.js failed) -- MapApp.init will use the 2D layer.
  }

  // Rect fill alpha -- keep in sync with the 2D paths' _withAlpha(color, 0.55).
  var RECT_ALPHA = 0.55;
  // Altitude quantization bins for the once-per-load bake of rect draw order
  // (see _buildRectStream). The 2D path re-sorts the visible subset every
  // redraw with 2048 bins over the visible z spread; baking over the GLOBAL
  // spread (~2km worst case) needs more bins for the same worst-case ~0.5m
  // resolution -- well under one 4m floor, and a sorted stream stays sorted
  // under any per-frame filtering (visibility/altitude/hidden are shader
  // gates, so they only ever drop instances, never reorder them).
  var Z_SORT_BINS = 4096;
  // Flattening curved stride-7 spans happens once at load, so the segment
  // count must be good enough for EVERY zoom: the target is the residual
  // chord-vs-curve error staying under ~half a screen px at maxZoom 7,
  // where 1 map px = 2^7 screen px. (The old fixed 0.5-map-px flatness
  // cutoff drew tight pipe elbows as one straight chord -- a visible ~45
  // degree chamfer at high zoom that snapped to the true curve on hover,
  // because the 2D highlight path draws real beziers.)
  var CURVE_ERR_MAP_PX = 0.5 / 128;
  // Cap on sub-chords per span. Straight runs -- the overwhelming majority
  // of belt/pipe spans -- still cost 1 segment (their control points sit on
  // the chord, so the measured deviation is ~0); only genuinely curved
  // spans pay for more, proportional to sqrt(curvature).
  var CURVE_SEGMENTS_MAX = 16;
  // Screen-space size parity with the 2D rect paths (see map.js's
  // SMALL_RECT_SCREEN_PX and the 0.75px floor in _drawRectBucketsAllSmall):
  // below 4px on both axes a rect draws axis-aligned with its half extents
  // floored at 0.75px, so a subpixel building stays a visible ~1.5px dot.
  var SMALL_RECT_SCREEN_PX = 4.0;
  var MIN_HALF_PX = 0.75;
  // Minimum on-screen rect size (px, both axes) at which each box gets a
  // ~1px darkened inner border (see RECT_FS) -- keep in sync with map.js's
  // RECT_OUTLINE_MIN_PX. Without it, buildings placed edge-to-edge on the
  // foundation grid fill into one indistinguishable slab; below this size
  // the border would swallow the fill.
  var RECT_OUTLINE_MIN_PX = 6.0;
  // v_halfPx sentinel meaning "no outline": far enough that the edge-distance
  // test in RECT_FS can never fire.
  var OUTLINE_OFF = "1.0e6";
  // Width of the R8 texture holding one visibility byte per rect bucket
  // (indexed by baked ordinal). ~400 buckets exist in practice; if a save
  // ever exceeds this the extras are clamped to slot VIS_TEX_WIDTH-1 (drawn
  // whenever that slot's bucket is visible -- degraded, not broken).
  var VIS_TEX_WIDTH = 1024;
  // MapApp.altitudeRange uses +/-Infinity for "no filter"; GLSL uniforms
  // can't portably carry Infinity, so the shaders get this sentinel instead.
  var ALT_SENTINEL = 1e30;
  // Individually hidden lines/circles are culled by patching their stored
  // altitude to +/-HIDDEN_Z in the GPU buffer -- it must beat ALT_SENTINEL so
  // the shader's altitude gate rejects them even with the filter wide open.
  // (Rects carry an explicit hidden byte instead; their instance slot is
  // easy to patch because the build keeps a slot->point map anyway.)
  var HIDDEN_Z = 3e38;

  function _queryParam(name) {
    var m = new RegExp("[?&]" + name + "=([^&]*)").exec(window.location.search);
    return m ? decodeURIComponent(m[1]) : null;
  }

  // True when WebGL2 is software-rendered (SwiftShader/llvmpipe -- e.g.
  // Chrome with hardware acceleration off or --disable-gpu). Software GL
  // still beats the 2D fallback by an order of magnitude here, but MSAA
  // costs it ~2x per frame (measured 400ms -> 150ms on the big save), so
  // _initGL skips antialiasing for it. Memoized; probed on a scratch canvas.
  var _softwareGLCache = null;
  function _isSoftwareGL() {
    if (_softwareGLCache === null) {
      _softwareGLCache = false;
      try {
        var gl = document.createElement("canvas").getContext("webgl2");
        if (gl) {
          var ext = gl.getExtension("WEBGL_debug_renderer_info");
          var renderer = String(ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER));
          _softwareGLCache = /swiftshader|llvmpipe|softpipe|software/i.test(renderer);
        }
      } catch (e) { /* keep false */ }
    }
    return _softwareGLCache;
  }

  // ---------------------------------------------------------------------
  // Shaders (GLSL ES 3.00). All positions are map-pixel space; the one view
  // transform is anchor-relative to dodge f32 cancellation: at zoom 7 raw
  // screen offsets reach ~1M px and originX + x*scaleX in f32 jitters
  // visibly, while (x - anchor)*scale keeps every operand small. The anchor
  // uniforms are computed per frame in JS doubles from the same affine the
  // 2D paths use (_computeAffine). u_viewport is CSS px (the GL canvas
  // backing store is CSS*devicePixelRatio, but clip space is resolution
  // independent, so all screen-px math -- line width, circle radius -- stays
  // in CSS px and scales correctly on any dpr).
  //
  // Per-instance gating (bucket visibility / altitude filter / hidden flag)
  // happens in the VERTEX shader by collapsing all four strip corners onto
  // one off-screen point -- zero fragments, no buffer traffic when a sidebar
  // checkbox flips. Bucket visibility for rects AND lines rides a 1-row R8
  // texture refreshed per frame (~400 bytes), because their merged z-sorted
  // streams interleave all buckets (see _buildRectStream/_buildLineStream).
  // ---------------------------------------------------------------------

  var COMMON_UNIFORMS = [
    "uniform vec2 u_anchorMap;",
    "uniform vec2 u_anchorScreen;",
    "uniform vec2 u_scale;",    // screen px per map px; y negative (flipped axis)
    "uniform vec2 u_viewport;", // CSS px
    "uniform vec2 u_altRange;", // meters, +/-ALT_SENTINEL when unfiltered
    // (zMin, 2/spread) mapping altitude onto NDC z. Rects and lines write
    // their altitude into the depth buffer (test ALWAYS -- draw order stays
    // pure painter's algorithm); the outline pass then depth-tests GEQUAL so
    // a lower building's border never shows through anything drawn above it.
    "uniform vec2 u_zRange;",
  ].join("\n");

  // screen -> clip. The 0.5px half-texel offset business isn't needed here:
  // MSAA + fractional positions come out the same as canvas AA in practice.
  var PROJECT_FN = [
    "vec4 project(vec2 screen) {",
    "  return vec4(screen.x / u_viewport.x * 2.0 - 1.0,",
    "              1.0 - screen.y / u_viewport.y * 2.0, 0.0, 1.0);",
    "}",
    "const vec4 CULLED = vec4(2.0, 2.0, 2.0, 1.0);", // all 4 verts collapse -> zero area
  ].join("\n");

  // All three primitive classes render as plain INDEXED TRIANGLES: 4 fully
  // expanded vertex records per quad plus a shared 6-indices-per-quad index
  // buffer -- NOT instanced quads. Instancing was measured at ~5-7us of
  // fixed overhead PER INSTANCE on SwiftShader (--disable-gpu): 465k rect
  // instances cost ~2.5s/frame even with every fragment culled, while the
  // same vertices as one non-instanced drawElements run in the tens of
  // milliseconds. Real GPUs don't care either way. The 4x vertex data
  // (~95MB total for the big test save) stays well inside the 200MB budget.
  //
  // Rect-stream quads carry 4 EXPLICIT corner offsets from their center.
  // Plain buildings bake their yaw rotation into the corners at build time;
  // a tilted-footprint polygon (see map.js _footprintForPoint -- this save
  // has 35k of them, mostly pillars and beams, NOT the rarity the name
  // suggests) is decomposed into 1-3 quads (convex fan quads plus a
  // degenerate-corner triangle when odd) so it z-sorts INSIDE the same
  // stream. Drawing tilted silhouettes in a separate pass painted 35k
  // pillars over every floor above them -- the exact 0.55-alpha layering
  // artifact the z-merge exists to prevent.
  var RECT_VS = [
    "#version 300 es",
    "precision highp float;",
    "layout(location=0) in vec2 a_corner;",  // this vertex's offset from center, map px
    "layout(location=1) in vec3 a_pos;",     // center x, y (map px), z (altitude m)
    "layout(location=2) in vec2 a_halfBox;", // LOCAL footprint half extents / 64 (u16-quantized map px)
    "layout(location=3) in vec4 a_color;",   // normalized rgba8 (a unused)
    "layout(location=4) in float a_bucketIdx;",
    "layout(location=5) in float a_hidden;",
    "layout(location=6) in float a_flags;",  // bit0 noClamp, bit1 unitX>0, bit2 unitY>0
    COMMON_UNIFORMS,
    "uniform sampler2D u_visibility;",
    "uniform float u_outlinePass;", // 0: fills; 1: band-only re-draw (see _renderFrame)
    "flat out vec4 v_color;",
    // Outline plumbing (see RECT_FS): the fragment's position and the rect's
    // half extents in the rect's OWN frame, screen px. Interpolating the
    // per-corner values gives exact local coordinates (the quad is an affine
    // image of local space, and |u_scale.x| == |u_scale.y| keeps rotated
    // lengths true). v_halfPx = OUTLINE_OFF disables the border.
    "out vec2 v_localPx;",
    "flat out vec2 v_halfPx;",
    PROJECT_FN,
    "void main() {",
    "  float vis = texelFetch(u_visibility, ivec2(int(a_bucketIdx + 0.5), 0), 0).r;",
    "  if (a_hidden > 0.5 || vis < 0.5 || a_pos.z < u_altRange.x || a_pos.z > u_altRange.y) {",
    "    gl_Position = CULLED; v_color = vec4(0.0); v_localPx = vec2(0.0); v_halfPx = vec2(" + OUTLINE_OFF + "); return;",
    "  }",
    "  vec2 corner = a_corner;",
    "  int flags = int(a_flags + 0.5);",
    "  v_localPx = vec2(0.0);",
    "  v_halfPx = vec2(" + OUTLINE_OFF + ");",
    "  bool outlined = false;",
    "  if ((flags & 1) == 0) {",
    // 2D parity (map.js SMALL_RECT_SCREEN_PX / the 0.75px floor): a rect
    // under 4 screen px on both axes draws as an axis-aligned dot floored
    // at 0.75px half extent, in this vertex's unit-corner direction.
    // Same-source parity: the 2D paths test footprintPixels (local extents)
    // too, which is why a_halfBox carries local rather than bounding extents.
    "    vec2 absScale = abs(u_scale);",
    "    vec2 halfScreen = a_halfBox * (1.0 / 64.0) * absScale;",
    "    vec2 unit = vec2(float((flags >> 1) & 1) * 2.0 - 1.0, float((flags >> 2) & 1) * 2.0 - 1.0);",
    "    if (max(halfScreen.x, halfScreen.y) * 2.0 < " + SMALL_RECT_SCREEN_PX.toFixed(1) + ") {",
    "      corner = unit * (max(halfScreen, vec2(" + MIN_HALF_PX + ")) / absScale);",
    "    } else if (min(halfScreen.x, halfScreen.y) * 2.0 >= " + RECT_OUTLINE_MIN_PX.toFixed(1) + ") {",
    "      outlined = true;",
    "      v_localPx = unit * halfScreen;",
    "      v_halfPx = halfScreen;",
    "    }",
    "  }",
    // The outline pass collapses band-less quads (small/tilted/too-zoomed-
    // out) to zero area up front -- they cost no fragments at all.
    "  if (u_outlinePass > 0.5 && !outlined) {",
    "    gl_Position = CULLED; v_color = vec4(0.0); return;",
    "  }",
    "  gl_Position = project(u_anchorScreen + (a_pos.xy + corner - u_anchorMap) * u_scale);",
    "  gl_Position.z = clamp((a_pos.z - u_zRange.x) * u_zRange.y - 1.0, -1.0, 1.0);",
    "  v_color = vec4(a_color.rgb, " + RECT_ALPHA + ");",
    "}",
  ].join("\n");

  // Fill fragments are the plain flat color; with u_outlinePass set the
  // same program instead draws ONLY a ~1px darkened band along each box
  // edge -- the same "slight outline" the 2D paths stroke (map.js
  // _outlineColor), so grid-adjacent same-color buildings read as
  // individual tiles instead of one merged slab. The band runs as a
  // separate depth-tested pass AFTER everything else (see _renderFrame):
  // drawn inline with the fills, a lower floor's border stayed visible
  // through the translucent fills of every floor above it, turning
  // multi-floor bases into a mess of bleed-through grids. highp (not the
  // usual mediump): v_localPx reaches thousands of px at high zoom, where
  // fp16 ulp (~4 at 4096) would visibly wobble the border.
  var RECT_FS = [
    "#version 300 es",
    "precision highp float;",
    "flat in vec4 v_color;",
    "in vec2 v_localPx;",
    "flat in vec2 v_halfPx;",
    "uniform float u_outlinePass;",
    "out vec4 outColor;",
    "void main() {",
    "  if (u_outlinePass > 0.5) {",
    "    vec2 edge = v_halfPx - abs(v_localPx);",     // px to the two nearest edges
    "    float t = 1.0 - smoothstep(0.5, 1.5, min(edge.x, edge.y));",
    // Strong stroke on purpose: the depth test already guarantees a band
    // only ever draws on the TOPMOST building, so it can't stack or bleed
    // -- and a building drawn over a foundation needs the extra contrast
    // against its own fill-over-fill background to read at all.
    "    if (t <= 0.004) { discard; }",               // interior: keep the fill as-is
    "    float a = 0.7 * t;",
    "    outColor = vec4(v_color.rgb * 0.65 * a, a);", // premultiplied
    "  } else {",
    "    outColor = vec4(v_color.rgb * v_color.a, v_color.a);", // premultiplied
    "  }",
    "}",
  ].join("\n");

  // Lines draw from ONE merged z-sorted stream interleaved with the rect
  // stream (see _buildLineStream), so color, width, and bucket visibility
  // all ride per-vertex attributes instead of per-run uniforms.
  var LINE_VS = [
    "#version 300 es",
    "precision highp float;",
    "layout(location=1) in vec2 a_a;",      // segment start, map px
    "layout(location=2) in vec2 a_b;",      // segment end, map px
    "layout(location=3) in vec2 a_zrange;", // the whole POLYLINE's (minZ,maxZ) -- 2D altitude-gates per line, not per vertex
    "layout(location=4) in float a_flags;", // bit0 t (0/1 along), bit1 side (+1 when set)
    "layout(location=5) in vec4 a_color;",  // normalized rgba8 (a unused)
    "layout(location=6) in float a_halfWidth;", // half line width, px * 8
    "layout(location=7) in float a_bucketIdx;",
    COMMON_UNIFORMS,
    "uniform sampler2D u_visibility;",
    "flat out vec4 v_color;",
    PROJECT_FN,
    "void main() {",
    "  float vis = texelFetch(u_visibility, ivec2(int(a_bucketIdx + 0.5), 0), 0).r;",
    "  if (vis < 0.5 || a_zrange.x > u_altRange.y || a_zrange.y < u_altRange.x) {",
    "    gl_Position = CULLED; v_color = vec4(0.0); return;",
    "  }",
    "  int flags = int(a_flags + 0.5);",
    "  float t = float(flags & 1);",
    "  float side = float((flags >> 1) & 1) * 2.0 - 1.0;",
    "  float halfWidthPx = a_halfWidth * 0.125;",
    "  vec2 sA = u_anchorScreen + (a_a - u_anchorMap) * u_scale;",
    "  vec2 sB = u_anchorScreen + (a_b - u_anchorMap) * u_scale;",
    "  vec2 dir = sB - sA;",
    "  float len = length(dir);",
    "  dir = len > 1e-6 ? dir / len : vec2(1.0, 0.0);",
    "  vec2 n = vec2(-dir.y, dir.x);",
    // Expand sideways by the half width AND extend both ends by it (square
    // caps): consecutive segments then overlap at joints instead of leaving
    // notches on curves, and the overlap is invisible because line colors
    // are opaque.
    "  vec2 screen = mix(sA, sB, t)",
    "              + dir * (t * 2.0 - 1.0) * halfWidthPx",
    "              + n * (side * halfWidthPx);",
    "  gl_Position = project(screen);",
    // Depth = the polyline's top z (its draw-order anchor), so the outline
    // pass can't paint a floor's border across the belts sitting on it.
    "  gl_Position.z = clamp((a_zrange.y - u_zRange.x) * u_zRange.y - 1.0, -1.0, 1.0);",
    "  v_color = vec4(a_color.rgb, 1.0);",
    "}",
  ].join("\n");

  var FLAT_FS = [
    "#version 300 es",
    "precision mediump float;",
    "flat in vec4 v_color;",
    "out vec4 outColor;",
    "void main() { outColor = vec4(v_color.rgb * v_color.a, v_color.a); }", // premultiplied source-over
  ].join("\n");

  var CIRCLE_VS = [
    "#version 300 es",
    "precision highp float;",
    "layout(location=1) in vec2 a_center;", // map px
    "layout(location=2) in float a_z;",
    "layout(location=3) in float a_flags;", // bit0 unitX>0, bit1 unitY>0
    COMMON_UNIFORMS,
    "uniform float u_radiusPx;",
    "out vec2 v_local;", // NOT flat -- interpolated for the disc test
    PROJECT_FN,
    "void main() {",
    "  if (a_z < u_altRange.x || a_z > u_altRange.y) {",
    "    gl_Position = CULLED; v_local = vec2(0.0); return;",
    "  }",
    "  int flags = int(a_flags + 0.5);",
    "  vec2 unit = vec2(float(flags & 1) * 2.0 - 1.0, float((flags >> 1) & 1) * 2.0 - 1.0);",
    "  v_local = unit * (u_radiusPx + 0.5);", // +0.5px margin for the AA rim
    "  gl_Position = project(u_anchorScreen + (a_center - u_anchorMap) * u_scale + v_local);",
    "}",
  ].join("\n");

  var CIRCLE_FS = [
    "#version 300 es",
    "precision mediump float;",
    "in vec2 v_local;",
    // highp to match the vertex stage's declaration -- uniform precisions
    // must agree across stages or the program fails to link.
    "uniform highp float u_radiusPx;",
    "uniform vec4 u_color;",
    "out vec4 outColor;",
    "void main() {",
    "  float alpha = 1.0 - smoothstep(u_radiusPx - 0.5, u_radiusPx + 0.5, length(v_local));",
    "  if (alpha <= 0.0) { discard; }",
    "  outColor = vec4(u_color.rgb * alpha, alpha);", // premultiplied; opaque body, half-px AA rim
    "}",
  ].join("\n");

  // ---------------------------------------------------------------------
  // Small helpers
  // ---------------------------------------------------------------------

  function _compileProgram(gl, vsSrc, fsSrc, name) {
    function shader(type, src) {
      var s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS) && !gl.isContextLost()) {
        throw new Error("webgl_layer " + name + " shader: " + gl.getShaderInfoLog(s));
      }
      return s;
    }
    var prog = gl.createProgram();
    gl.attachShader(prog, shader(gl.VERTEX_SHADER, vsSrc));
    gl.attachShader(prog, shader(gl.FRAGMENT_SHADER, fsSrc));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS) && !gl.isContextLost()) {
      throw new Error("webgl_layer " + name + " link: " + gl.getProgramInfoLog(prog));
    }
    // Cache every uniform location up front; gl.uniform* on a null location
    // is a spec'd no-op, so programs can share the common-uniform setter.
    var uniforms = {};
    var names = ["u_anchorMap", "u_anchorScreen", "u_scale", "u_viewport",
                 "u_altRange", "u_zRange", "u_visibility", "u_color", "u_radiusPx",
                 "u_outlinePass"];
    for (var i = 0; i < names.length; i++) {
      uniforms[names[i]] = gl.getUniformLocation(prog, names[i]);
    }
    return { prog: prog, u: uniforms };
  }

  // bucket.color is a "#rrggbb" hex string (filters.js color tables) --
  // same assumption map.js's _withAlpha makes.
  function _hexToRgb(hexColor) {
    if (typeof hexColor === "string" && hexColor.charAt(0) === "#" && hexColor.length >= 7) {
      return [
        parseInt(hexColor.slice(1, 3), 16),
        parseInt(hexColor.slice(3, 5), 16),
        parseInt(hexColor.slice(5, 7), 16),
      ];
    }
    return [153, 153, 153]; // the same "#999999" default the pin outline uses
  }

  function _distPointToSegment(px, py, ax, ay, bx, by) {
    var abx = bx - ax, aby = by - ay;
    var lengthSq = abx * abx + aby * aby;
    var t = lengthSq > 0 ? ((px - ax) * abx + (py - ay) * aby) / lengthSq : 0;
    t = Math.max(0, Math.min(1, t));
    var dx = px - (ax + t * abx), dy = py - (ay + t * aby);
    return Math.sqrt(dx * dx + dy * dy);
  }

  // Sub-chord count (1..CURVE_SEGMENTS_MAX) for the stride-7 span starting
  // at offset i in pts. The Bezier's deviation from its chord is at most
  // 3/4 of the control points' (same bound _pointToBezierDistance's comment
  // in map.js relies on), and flattening a curve of deviation d into n
  // equal-t sub-chords leaves a residual error of about d/n^2 -- so
  // n = sqrt(d / CURVE_ERR_MAP_PX) keeps the drawn polyline within
  // CURVE_ERR_MAP_PX of the true curve. Must be deterministic: the counting
  // pass, _writeLineSegments, and _patchHiddenLine all rely on it returning
  // the same answer for the same span so buffer offsets line up.
  function _spanSegments(pts, i, stride) {
    var ax = pts[i], ay = pts[i + 1];
    var bx = pts[i + stride], by = pts[i + stride + 1];
    var c1x = ax + pts[i + 4] / 3, c1y = ay + pts[i + 5] / 3;               // prev leaveTangent
    var c2x = bx - pts[i + stride + 2] / 3, c2y = by - pts[i + stride + 3] / 3; // cur arriveTangent
    var d = 0.75 * Math.max(_distPointToSegment(c1x, c1y, ax, ay, bx, by),
                            _distPointToSegment(c2x, c2y, ax, ay, bx, by));
    if (d <= CURVE_ERR_MAP_PX) {
      return 1;
    }
    var n = Math.ceil(Math.sqrt(d / CURVE_ERR_MAP_PX));
    return n < CURVE_SEGMENTS_MAX ? n : CURVE_SEGMENTS_MAX;
  }

  var WebGLBucketedLayer = MapApp.BucketedCanvasLayer.extend({
    _isWebGL: true, // Read by MapApp.fallbackToCanvasLayer's idempotency guard.

    initialize: function() {
      MapApp.BucketedCanvasLayer.prototype.initialize.call(this);
      this._gl = null;
      this._glCanvas = null;
      // this._canvas doubles as the 2D PIN canvas -- the inherited icon pass
      // (_drawIconBucket -> _occEnsure) sizes its dedup buffer from
      // this._canvas, so pointing the parent's field at the pin canvas keeps
      // that code working unmodified. The GL canvas lives in _glCanvas.
      this._canvas = null;
      this._highlightCanvas = null;
      this._contextLost = false;
      this._renderScheduled = false;
      this._renderFrameHandle = null;
      this._suspendRender = false; // set while a CSS zoom animation owns the canvases
      this._streamsDirty = true;
      this._rect = null;       // { buffer, vao, count, binStart, buckets, slotBucket, slotPoint, hiddenShadow }
      this._rectBucketList = null;
      this._line = null;       // { buffer, vao, count, binStart, buckets } -- merged z-sorted segment stream
      this._lineBucketList = null;
      this._circles = null;    // { buffer, runs: [...] }
      this._programs = null;
      this._indexBuffer = null;       // shared 6-indices-per-quad pattern
      this._indexQuadCapacity = 0;
      this._visTexture = null;
      this._visTexData = new Uint8Array(VIS_TEX_WIDTH);
      this._hiddenEpoch = 0;
      this._dpr = 1;
    },

    onAdd: function(map) {
      this._map = map;
      // Three sibling canvases in the overlay pane; DOM order is z-order:
      // GL bulk canvas at the bottom, icon pins above it, highlight on top.
      // All three carry leaflet-zoom-animated so the CSS zoom transition
      // scales them in lockstep with the base image (see _onZoomAnim).
      this._glCanvas = L.DomUtil.create("canvas", "bucketed-canvas-layer leaflet-zoom-animated");
      this._canvas = L.DomUtil.create("canvas", "bucketed-canvas-layer leaflet-zoom-animated");
      this._highlightCanvas = L.DomUtil.create("canvas", "bucketed-canvas-layer leaflet-zoom-animated");
      this.getPane().appendChild(this._glCanvas);
      this.getPane().appendChild(this._canvas);
      this.getPane().appendChild(this._highlightCanvas);

      if (!this._initGL()) {
        // isSupported() probed WebGL2 successfully, but this specific
        // context failed (out of contexts / driver hiccup). MapApp.layer
        // isn't assigned yet while addTo(map) is still running, so the swap
        // has to wait a tick.
        this._contextLost = true;
        setTimeout(function() { MapApp.fallbackToCanvasLayer(); }, 0);
        return;
      }

      this._boundOnContextLost = L.Util.bind(this._onContextLost, this);
      this._glCanvas.addEventListener("webglcontextlost", this._boundOnContextLost, false);

      // Per-frame rendering: every view change schedules one rAF render.
      // No free-running loop -- an idle map costs zero GPU work.
      map.on("move", this._onViewChange, this);
      map.on("zoom", this._onViewChange, this);
      map.on("viewreset", this._onViewChange, this);
      map.on("moveend", this._onViewChange, this);
      map.on("zoomend", this._onZoomEnd, this);
      map.on("zoomanim", this._onZoomAnim, this);
      map.on("resize", this._onResize, this);

      this._onResize();
    },

    onRemove: function(map) {
      map.off("move", this._onViewChange, this);
      map.off("zoom", this._onViewChange, this);
      map.off("viewreset", this._onViewChange, this);
      map.off("moveend", this._onViewChange, this);
      map.off("zoomend", this._onZoomEnd, this);
      map.off("zoomanim", this._onZoomAnim, this);
      map.off("resize", this._onResize, this);
      if (this._renderFrameHandle) {
        L.Util.cancelAnimFrame(this._renderFrameHandle);
        this._renderFrameHandle = null;
        this._renderScheduled = false;
      }
      if (this._glCanvas && this._boundOnContextLost) {
        this._glCanvas.removeEventListener("webglcontextlost", this._boundOnContextLost, false);
      }
      this._disposeGL();
      var pane = this.getPane();
      if (pane) {
        if (this._glCanvas && this._glCanvas.parentNode === pane) pane.removeChild(this._glCanvas);
        if (this._canvas && this._canvas.parentNode === pane) pane.removeChild(this._canvas);
        if (this._highlightCanvas && this._highlightCanvas.parentNode === pane) pane.removeChild(this._highlightCanvas);
      }
      this._map = null;
    },

    // ------------------------------------------------------------------
    // Public surface overrides. The parent implementations keep owning the
    // bucket bookkeeping (grid building, _sortedBuckets invalidation); the
    // overrides only add GPU-stream lifecycle on top.
    // ------------------------------------------------------------------

    addBucket: function(bucket) {
      var added = MapApp.BucketedCanvasLayer.prototype.addBucket.call(this, bucket);
      // Icon buckets never touch the GL streams (pins draw on the 2D
      // canvas), and they're the only kind added at runtime (finditem's
      // search highlight, bottleneck pins) -- skipping the dirty flag for
      // them is what keeps those flows from re-uploading 50MB of streams.
      if (bucket.renderType !== "icon") {
        this._streamsDirty = true;
      }
      this._scheduleRender();
      return added;
    },

    clearBuckets: function() {
      MapApp.BucketedCanvasLayer.prototype.clearBuckets.call(this);
      // Free GPU memory NOW rather than lazily at the next rendered frame:
      // a second save load calls clearBuckets then re-adds everything, and
      // deferring the delete would transiently hold both saves' streams.
      this._disposeStreams();
      this._streamsDirty = true;
      this._scheduleRender();
    },

    removeBucketByKey: function(key) {
      var hadGLBucket = false;
      for (var i = 0; i < this.buckets.length; i++) {
        if (this.buckets[i].key === key && this.buckets[i].renderType !== "icon") {
          hadGLBucket = true;
        }
      }
      MapApp.BucketedCanvasLayer.prototype.removeBucketByKey.call(this, key);
      if (hadGLBucket) {
        // Rebuilding from the surviving buckets both removes the bucket's
        // instances and releases its share of GPU memory.
        this._disposeStreams();
        this._streamsDirty = true;
      }
      this._scheduleRender();
    },

    requestRedraw: function() {
      if (this._map) {
        this._scheduleRender();
      }
    },

    // requestHighlightRedraw / hitTest: inherited unchanged. The highlight
    // path only needs _highlightCanvas + _viewTopLeft, both of which this
    // class maintains (see _renderFrame).

    // ------------------------------------------------------------------
    // GL lifecycle
    // ------------------------------------------------------------------

    _initGL: function() {
      var gl;
      // ?glaa=0/1 and ?glscale=0.5 are debug/tuning switches (see also
      // ?renderer=canvas in isSupported).
      var aaParam = _queryParam("glaa");
      var wantAA = aaParam !== null
        ? aaParam !== "0"
        // MSAA costs width*height*4*(samples+1) bytes of driver memory --
        // fine at 1080p (~40MB), the dominant GPU cost at 4K (~166MB), so
        // very large viewports run aliased instead. Software GL pays ~2x
        // frame time for it (see _isSoftwareGL) and skips it too.
        : !_isSoftwareGL() && (window.innerWidth * (window.devicePixelRatio || 1)) <= 2600;
      var scaleParam = parseFloat(_queryParam("glscale"));
      this._renderScale = scaleParam >= 0.25 && scaleParam <= 1 ? scaleParam : 1;
      try {
        gl = this._glCanvas.getContext("webgl2", {
          alpha: true,
          // Premultiplied alpha: shaders output rgb pre-scaled by a and the
          // blend state is (ONE, ONE_MINUS_SRC_ALPHA). The old straight-alpha
          // setup (premultipliedAlpha:false + SRC_ALPHA blending) applied
          // alpha twice wherever destination alpha was < 1 -- translucent
          // fills over bare map rendered at ~0.30 effective opacity instead
          // of RECT_ALPHA's 0.55, visibly darker than the 2D reference.
          premultipliedAlpha: true,
          antialias: wantAA,
          // Depth carries each primitive's ALTITUDE (see u_zRange): draw
          // order stays pure painter's algorithm (test ALWAYS), but the
          // deferred outline pass tests GEQUAL against it so a lower
          // building's border never shows through buildings above.
          depth: true,
          stencil: false,
          preserveDrawingBuffer: false,
          powerPreference: "high-performance",
        });
      } catch (e) {
        gl = null;
      }
      if (!gl) {
        return false;
      }
      this._gl = gl;
      try {
        this._programs = {
          rect: _compileProgram(gl, RECT_VS, RECT_FS, "rect"),
          line: _compileProgram(gl, LINE_VS, FLAT_FS, "line"),
          circle: _compileProgram(gl, CIRCLE_VS, CIRCLE_FS, "circle"),
        };
      } catch (e) {
        // A compile/link failure on some driver must degrade to the 2D
        // layer, not brick MapApp.init (this runs inside layer.addTo).
        console.error(e);
        this._programs = null;
        this._gl = null;
        return false;
      }
      this._visTexture = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, this._visTexture);
      gl.texStorage2D(gl.TEXTURE_2D, 1, gl.R8, VIS_TEX_WIDTH, 1);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1); // 1-byte rows for the R8 uploads

      gl.enable(gl.BLEND);
      // Premultiplied source-over: shaders emit rgb already scaled by alpha
      // (see RECT_FS/FLAT_FS/CIRCLE_FS), so one factor pair is exact for
      // both the color and alpha channels.
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      // DEPTH_TEST stays enabled so depth WRITES happen; ALWAYS keeps the
      // painter's-order passes unaffected by it (see the depth note above).
      gl.enable(gl.DEPTH_TEST);
      gl.depthFunc(gl.ALWAYS);
      gl.clearDepth(0); // "nothing drawn" = lowest altitude
      gl.disable(gl.CULL_FACE);
      gl.clearColor(0, 0, 0, 0);
      return true;
    },

    _onContextLost: function() {
      // No restore attempt (that would need every buffer/texture/program
      // rebuilt behind an event we can't fully trust) -- the 2D layer is a
      // complete, always-correct fallback and context loss is rare.
      this._contextLost = true;
      if (this._renderFrameHandle) {
        L.Util.cancelAnimFrame(this._renderFrameHandle);
        this._renderFrameHandle = null;
        this._renderScheduled = false;
      }
      MapApp.fallbackToCanvasLayer();
    },

    _disposeStreams: function() {
      var gl = this._gl;
      if (!gl) {
        return;
      }
      if (this._rect) {
        if (this._rect.vao) gl.deleteVertexArray(this._rect.vao);
        if (this._rect.buffer) gl.deleteBuffer(this._rect.buffer);
        this._rect = null;
      }
      if (this._line) {
        if (this._line.vao) gl.deleteVertexArray(this._line.vao);
        if (this._line.buffer) gl.deleteBuffer(this._line.buffer);
        this._line = null;
      }
      if (this._circles) {
        for (var i = 0; i < this._circles.runs.length; i++) {
          gl.deleteVertexArray(this._circles.runs[i].vao);
        }
        if (this._circles.buffer) gl.deleteBuffer(this._circles.buffer);
        this._circles = null;
      }
      this._rectBucketList = null;
      this._lineBucketList = null;
    },

    _disposeGL: function() {
      var gl = this._gl;
      if (!gl) {
        return;
      }
      this._disposeStreams();
      if (this._programs) {
        gl.deleteProgram(this._programs.rect.prog);
        gl.deleteProgram(this._programs.line.prog);
        gl.deleteProgram(this._programs.circle.prog);
        this._programs = null;
      }
      if (this._indexBuffer) { gl.deleteBuffer(this._indexBuffer); this._indexBuffer = null; this._indexQuadCapacity = 0; }
      if (this._visTexture) { gl.deleteTexture(this._visTexture); this._visTexture = null; }
      this._gl = null;
    },

    // Grows (never shrinks) the shared index buffer to cover `quads` quads:
    // the fixed pattern (4k, 4k+1, 4k+2, 4k+2, 4k+1, 4k+3) that turns four
    // corner-ordered vertex records into two triangles. One buffer serves
    // every stream -- indices are relative to each draw's attribute offsets.
    _ensureIndexCapacity: function(quads) {
      var gl = this._gl;
      if (quads <= this._indexQuadCapacity) {
        return;
      }
      if (!this._indexBuffer) {
        this._indexBuffer = gl.createBuffer();
      }
      var idx = new Uint32Array(quads * 6);
      for (var q = 0, o = 0; q < quads; q++) {
        var v = q * 4;
        idx[o++] = v; idx[o++] = v + 1; idx[o++] = v + 2;
        idx[o++] = v + 2; idx[o++] = v + 1; idx[o++] = v + 3;
      }
      gl.bindVertexArray(null); // don't capture this binding in someone's VAO
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, this._indexBuffer);
      gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, idx, gl.STATIC_DRAW);
      this._indexQuadCapacity = quads;
    },

    // ------------------------------------------------------------------
    // Events / scheduling
    // ------------------------------------------------------------------

    _onViewChange: function() {
      this._scheduleRender();
    },

    _scheduleRender: function() {
      if (this._renderScheduled || !this._map) {
        return;
      }
      this._renderScheduled = true;
      var self = this;
      this._renderFrameHandle = L.Util.requestAnimFrame(function() {
        self._renderScheduled = false;
        self._renderFrameHandle = null;
        self._renderFrame();
      });
    },

    // Animated (wheel) zoom: Leaflet CSS-transitions everything carrying
    // leaflet-zoom-animated over ~250ms and fires NO per-frame events, so a
    // true re-render mid-animation isn't possible -- instead hand the
    // canvases the same target transform L.ImageOverlay uses and let the
    // compositor scale them in lockstep with the base image (this is exactly
    // the 2D layer's zoom-preview behavior), then render crisp on zoomend.
    _onZoomAnim: function(e) {
      var map = this._map;
      if (!map || !this._viewTopLeft) {
        return;
      }
      this._suspendRender = true;
      var scale = map.getZoomScale(e.zoom);
      var anchorLatLng = map.layerPointToLatLng(this._viewTopLeft);
      var offset = map._latLngToNewLayerPoint(anchorLatLng, e.zoom, e.center);
      L.DomUtil.setTransform(this._glCanvas, offset, scale);
      L.DomUtil.setTransform(this._canvas, offset, scale);
      // The one highlight box would scale to the wrong spot; hide it until
      // the zoomend render repaints it (same as the 2D preview path).
      var hctx = this._highlightCanvas.getContext("2d");
      hctx.clearRect(0, 0, this._highlightCanvas.width, this._highlightCanvas.height);
      L.DomUtil.setTransform(this._highlightCanvas, offset, scale);
    },

    _onZoomEnd: function() {
      this._suspendRender = false;
      this._scheduleRender();
    },

    _onResize: function() {
      var map = this._map;
      if (!map) {
        return;
      }
      var size = map.getSize();
      var dpr = window.devicePixelRatio || 1;
      this._dpr = dpr;
      // GL backing store at device resolution for crisp output on hidpi
      // (times the ?glscale reduction, if any -- the CSS size below is what
      // the page sees either way); the pin/highlight canvases stay
      // CSS-resolution like the 2D layer's.
      var backing = dpr * (this._renderScale || 1);
      this._glCanvas.width = Math.max(1, Math.round(size.x * backing));
      this._glCanvas.height = Math.max(1, Math.round(size.y * backing));
      this._glCanvas.style.width = size.x + "px";
      this._glCanvas.style.height = size.y + "px";
      this._canvas.width = size.x;
      this._canvas.height = size.y;
      this._highlightCanvas.width = size.x;
      this._highlightCanvas.height = size.y;
      this._scheduleRender();
    },

    // ------------------------------------------------------------------
    // Stream builds -- once per save load (lazily, on the first rendered
    // frame after add/clear/remove), never per view change.
    // ------------------------------------------------------------------

    _rebuildStreams: function() {
      this._disposeStreams();
      if (!this._sortedBuckets) {
        // Same cached sort the parent's _redraw uses (drawPriority asc,
        // stable) -- the run list and pin pass both iterate it.
        this._sortedBuckets = this.buckets.slice().sort(function(a, b) { return (a.drawPriority || 0) - (b.drawPriority || 0); });
      }
      var zr = this._computeZRange();
      this._zRange = zr; // per-frame u_zRange (altitude -> depth) reads this
      this._buildRectStream(zr);
      this._buildLineStream(zr);
      this._buildCircleStream();
      this._hiddenEpoch = this._computeHiddenEpoch();
    },

    // Global altitude range across RECTS AND LINES: both streams quantize
    // into the same Z_SORT_BINS mapping so _renderFrame can merge-walk them
    // bin by bin (rects of bin <= b, then bin b's lines).
    _computeZRange: function() {
      var minZ = Infinity, maxZ = -Infinity;
      for (var b = 0; b < this.buckets.length; b++) {
        var bucket = this.buckets[b];
        if (bucket.renderType === "rect" && bucket.points) {
          var pts = bucket.points;
          for (var i = 3; i < pts.length; i += 4) {
            if (pts[i] < minZ) minZ = pts[i];
            if (pts[i] > maxZ) maxZ = pts[i];
          }
        } else if (bucket.renderType === "line" && bucket.lines) {
          var lineBounds = bucket._lineBounds;
          if (lineBounds) {
            for (var L2 = 0; L2 < lineBounds.length; L2++) {
              var lb = lineBounds[L2];
              if (lb) {
                if (lb.minZ < minZ) minZ = lb.minZ;
                if (lb.maxZ > maxZ) maxZ = lb.maxZ;
              }
            }
          } else {
            var stride = bucket.pointStride;
            for (var li = 0; li < bucket.lines.length; li++) {
              var lpts = bucket.lines[li];
              for (var k2 = stride - 1; k2 < lpts.length; k2 += stride) {
                if (lpts[k2] < minZ) minZ = lpts[k2];
                if (lpts[k2] > maxZ) maxZ = lpts[k2];
              }
            }
          }
        }
      }
      if (minZ > maxZ) {
        minZ = maxZ = 0;
      }
      var spread = maxZ - minZ;
      return { minZ: minZ, spread: spread, zScale: spread > 0 ? (Z_SORT_BINS - 1) / spread : 0 };
    },

    // One merged instance stream for ALL rect buckets, pre-sorted by
    // (quantized z, bucket ordinal) with the same stable counting sort the
    // 2D sorted path runs per redraw (map.js _drawRectBucketsSorted) --
    // altitude order is what makes 0.55-alpha overdraw look right on
    // multi-floor bases (and what hitTest's "highest z wins" expects to see
    // painted on top). Baking it once is only possible because z never
    // changes after load, and per-frame gating (visibility / altitude /
    // hidden) only DROPS instances from the sorted order, never reorders it.
    // Per-bucket draws sorted by bucket z-range were rejected: foundation
    // and machine buckets both span every floor of a real base, so their
    // z-ranges fully interleave and bucket granularity degenerates to
    // exactly the category-order artifact the 2D sort exists to fix.
    //
    // A point with a tiltedFootprints override contributes its convex
    // polygon as 1-3 corner-explicit instances AT ITS OWN SORTED SLOTS (see
    // RECT_VS's comment) -- same key, so pillars and beams interleave with
    // plain buildings exactly like the 2D sorted path's polygon items.
    _buildRectStream: function(zr) {
      var gl = this._gl;
      var sorted = this._sortedBuckets;
      var rectBuckets = [];
      var b, bucket, i, n, p, pts, tf, verts, nv;
      for (b = 0; b < sorted.length; b++) {
        bucket = sorted[b];
        if (bucket.renderType === "rect" && bucket.points && bucket.points.length > 0) {
          bucket._glRectOrdinal = rectBuckets.length;
          rectBuckets.push(bucket);
        }
      }
      // The visibility texture keys off this list (kept even when the
      // stream is empty so the texture upload stays trivially correct).
      this._rectBucketList = rectBuckets;
      if (rectBuckets.length === 0) {
        return;
      }
      if (rectBuckets.length > VIS_TEX_WIDTH) {
        console.warn("webgl_layer: " + rectBuckets.length + " rect buckets exceed the visibility texture (" + VIS_TEX_WIDTH + "); extras share the last slot.");
      }

      // Instances per point: 1 for a plain rect; a tilted polygon of nv
      // verts becomes floor((nv-2)/2) quads plus one triangle if odd.
      function instancesFor(verts) {
        return verts ? Math.max(1, Math.ceil((verts.length / 2 - 2) / 2)) : 1;
      }

      // Largest min-axis footprint (map px) across rect buckets: lets
      // _renderFrame skip the outline pass entirely when no bucket can
      // reach RECT_OUTLINE_MIN_PX at the current scale (every zoomed-out
      // view), so the second stream draw costs nothing there.
      var maxMinHalf = 0;
      for (b = 0; b < rectBuckets.length; b++) {
        var fpB = rectBuckets[b].footprintPixels;
        if (fpB) {
          var mm = Math.min(fpB[0], fpB[1]);
          if (mm > maxMinHalf) maxMinHalf = mm;
        }
      }

      // Pass 1: count instances (the z spread comes shared from
      // _computeZRange so line bins line up with rect bins).
      var total = 0;
      var minZ = zr.minZ, zScale = zr.zScale;
      for (b = 0; b < rectBuckets.length; b++) {
        bucket = rectBuckets[b];
        pts = bucket.points;
        tf = bucket.tiltedFootprints;
        n = pts.length / 4;
        for (i = 0; i < n; i++) {
          total += instancesFor(tf && tf[i]);
        }
      }

      // Counting sort over the composite key (zBin, ordinal): stable, O(n),
      // and same-bin instances stay grouped by bucket so flat single-floor
      // areas draw in the exact order the 2D flat path uses.
      var nBuckets = rectBuckets.length;
      var slots = Z_SORT_BINS * nBuckets + 1;
      var counts = new Uint32Array(slots);
      for (b = 0; b < rectBuckets.length; b++) {
        bucket = rectBuckets[b];
        pts = bucket.points;
        tf = bucket.tiltedFootprints;
        n = pts.length / 4;
        for (i = 0; i < n; i++) {
          counts[1 + (((pts[i * 4 + 3] - minZ) * zScale) | 0) * nBuckets + b] += instancesFor(tf && tf[i]);
        }
      }
      for (var s = 1; s < slots; s++) {
        counts[s] += counts[s - 1];
      }
      // First instance slot of each z bin (bin b's slots are
      // [binStart[b], binStart[b+1])) -- the per-frame line/rect merge walk
      // in _renderFrame splits the one big rect draw at these offsets.
      // Captured now, BEFORE pass 2 advances counts in place.
      var binStart = new Uint32Array(Z_SORT_BINS + 1);
      for (var bn = 0; bn <= Z_SORT_BINS; bn++) {
        binStart[bn] = counts[bn * nBuckets];
      }

      // Pass 2: write each quad's FOUR vertex records at its sorted slot
      // (see the non-instanced rationale above RECT_VS). 32 bytes/vertex:
      // corner offset f32 x2 | center f32 x2 | z f32 | halfBox u16 x2
      // (map px * 64) | rgba8 | bucketIdx u16 | hidden u8 | flags u8.
      var STRIDE = 32;
      var buf = new ArrayBuffer(total * 4 * STRIDE);
      var f32 = new Float32Array(buf);
      var u8 = new Uint8Array(buf);
      var u16 = new Uint16Array(buf);
      var slotBucket = new Uint16Array(total);
      var slotPoint = new Uint32Array(total);
      var hiddenShadow = new Uint8Array(total);

      function writeSlot(slot, cx, cy, z, c0x, c0y, c1x, c1y, c2x, c2y, c3x, c3y,
                         rgb, ordinal, hid, noClamp, b, pointIdx, halfLocalX, halfLocalY) {
        // LOCAL footprint half extents for the dot-clamp and the outline's
        // edge-distance math (same source the 2D paths' size tests read:
        // footprintPixels); quantized to u16 in 1/64 map px. Unused when
        // noClamp is set (tilted polygon pieces).
        var hbx = Math.min(65535, (halfLocalX * 64) | 0);
        var hby = Math.min(65535, (halfLocalY * 64) | 0);
        for (var k = 0; k < 4; k++) {
          var vi = slot * 4 + k;
          var fo = vi * 8;
          f32[fo] = k === 0 ? c0x : k === 1 ? c1x : k === 2 ? c2x : c3x;
          f32[fo + 1] = k === 0 ? c0y : k === 1 ? c1y : k === 2 ? c2y : c3y;
          f32[fo + 2] = cx;
          f32[fo + 3] = cy;
          f32[fo + 4] = z;
          var so = vi * 16;
          u16[so + 10] = hbx;
          u16[so + 11] = hby;
          var bo = vi * STRIDE;
          u8[bo + 24] = rgb[0];
          u8[bo + 25] = rgb[1];
          u8[bo + 26] = rgb[2];
          u8[bo + 27] = 255;
          u16[so + 14] = ordinal;
          u8[bo + 30] = hid;
          // bit0 noClamp; bits 1-2 this corner's unit direction (the corner
          // order is (-,-),(+,-),(-,+),(+,+), matching the index pattern).
          u8[bo + 31] = noClamp | ((k & 1) << 1) | (((k >> 1) & 1) << 2);
        }
        slotBucket[slot] = b;
        slotPoint[slot] = pointIdx;
        hiddenShadow[slot] = hid;
      }

      for (b = 0; b < rectBuckets.length; b++) {
        bucket = rectBuckets[b];
        pts = bucket.points;
        tf = bucket.tiltedFootprints;
        n = pts.length / 4;
        var rgb = _hexToRgb(bucket.color);
        var halfW = bucket.footprintPixels ? bucket.footprintPixels[0] : 1;
        var halfD = bucket.footprintPixels ? bucket.footprintPixels[1] : 1;
        var ordinal = Math.min(b, VIS_TEX_WIDTH - 1);
        var hiddenSet = bucket.hiddenIndices || null;
        for (i = 0; i < n; i++) {
          p = i * 4;
          var x = pts[p], y = pts[p + 1], z = pts[p + 3];
          var key = (((z - minZ) * zScale) | 0) * nBuckets + b;
          var hid = hiddenSet && hiddenSet.has(i) ? 1 : 0;
          verts = tf && tf[i];
          if (verts && verts.length >= 6) {
            // Convex polygon (pre-rotated offsets) -> strip quads
            // (v0, v_{2j+1}, v_{2j+3}, v_{2j+2}), triangle tail via a
            // duplicated corner. Convexity comes from the silhouette being
            // a projected box hull (sav_map_data._tiltedFootprintPolygon).
            nv = verts.length / 2;
            for (var j = 1; j < nv - 1; j += 2) {
              var slot = counts[key]++;
              var kB = j, kC = Math.min(j + 1, nv - 1), kD = Math.min(j + 2, nv - 1);
              writeSlot(slot, x, y, z,
                verts[0], verts[1],
                verts[kB * 2], verts[kB * 2 + 1],
                verts[kD * 2], verts[kD * 2 + 1],   // strip order: (v0, vB, vD, vC)
                verts[kC * 2], verts[kC * 2 + 1],
                rgb, ordinal, hid, 1, b, i, 0, 0);
            }
          } else {
            // Plain rect: bake the yaw rotation into the corners. Yaw
            // negated exactly like the 2D _traceRect (map-pixel space has a
            // flipped Y axis vs world space, which mirrors handedness).
            var cos = Math.cos(-pts[p + 2]);
            var sin = Math.sin(-pts[p + 2]);
            var cW = cos * halfW, sW = sin * halfW;
            var cD = cos * halfD, sD = sin * halfD;
            // Strip corners R(-w,-d), R(w,-d), R(-w,d), R(w,d) -- corner k's
            // unit-direction flag bits must match this order (RECT_VS's
            // dot-clamp and outline both reconstruct local coords from them).
            writeSlot(counts[key]++, x, y, z,
              -cW + sD, -sW - cD,
              cW + sD, sW - cD,
              -cW - sD, -sW + cD,
              cW - sD, sW + cD,
              rgb, ordinal, hid, 0, b, i, halfW, halfD);
          }
        }
      }

      var buffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.bufferData(gl.ARRAY_BUFFER, buf, gl.STATIC_DRAW);
      this._ensureIndexCapacity(total);

      var vao = gl.createVertexArray();
      gl.bindVertexArray(vao);
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, this._indexBuffer);
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.enableVertexAttribArray(0);
      gl.vertexAttribPointer(0, 2, gl.FLOAT, false, STRIDE, 0);  // a_corner
      gl.enableVertexAttribArray(1);
      gl.vertexAttribPointer(1, 3, gl.FLOAT, false, STRIDE, 8);  // a_pos (center.xy, z)
      gl.enableVertexAttribArray(2);
      gl.vertexAttribPointer(2, 2, gl.UNSIGNED_SHORT, false, STRIDE, 20); // a_halfBox (x64)
      gl.enableVertexAttribArray(3);
      gl.vertexAttribPointer(3, 4, gl.UNSIGNED_BYTE, true, STRIDE, 24);   // a_color
      gl.enableVertexAttribArray(4);
      gl.vertexAttribPointer(4, 1, gl.UNSIGNED_SHORT, false, STRIDE, 28); // a_bucketIdx (exact as float up to 2^24)
      gl.enableVertexAttribArray(5);
      gl.vertexAttribPointer(5, 1, gl.UNSIGNED_BYTE, false, STRIDE, 30);  // a_hidden
      gl.enableVertexAttribArray(6);
      gl.vertexAttribPointer(6, 1, gl.UNSIGNED_BYTE, false, STRIDE, 31);  // a_flags
      gl.bindVertexArray(null);

      // The CPU copy of the stream is dropped; only the slot->instance map
      // (needed to patch hidden bytes in place) survives, ~3MB per 480k slots.
      this._rect = {
        buffer: buffer,
        vao: vao,
        count: total,
        binStart: binStart,
        maxMinHalf: maxMinHalf,
        buckets: rectBuckets,
        slotBucket: slotBucket,
        slotPoint: slotPoint,
        hiddenShadow: hiddenShadow,
      };
    },

    // One merged segment stream for ALL line buckets, z-sorted with the same
    // (quantized z, bucket order) counting sort as the rect stream. Lines
    // used to draw as per-bucket runs BEFORE every rect, which buried a
    // top-floor belt under the 0.55-alpha fill of every foundation drawn
    // after it -- stack a few floors and the belt disappeared entirely even
    // though it was the highest thing in view. Sorting whole POLYLINES into
    // the shared bin mapping lets _renderFrame interleave line and rect
    // draws bin by bin, so a line dims only under floors genuinely above it.
    //
    // A polyline sorts by its TOP z (lifts and ramps stay visible over the
    // floors they climb past) and its segments stay contiguous at their
    // sorted slots, so hidden-patching remains a per-line slot range (see
    // _patchHiddenLine). Color, half width, and bucket index ride each
    // vertex -- with buckets interleaved inside one draw, per-run uniforms
    // can't carry them anymore; visibility rides the same R8 texture as
    // rects, at ordinals AFTER the rect buckets'.
    _buildLineStream: function(zr) {
      var gl = this._gl;
      var sorted = this._sortedBuckets;
      var lineBuckets = [];
      var b, bucket, i, k, pts, stride, lineBounds;
      for (b = 0; b < sorted.length; b++) {
        bucket = sorted[b];
        if (bucket.renderType === "line" && bucket.lines && bucket.lines.length > 0) {
          lineBuckets.push(bucket);
        }
      }
      this._lineBucketList = lineBuckets;
      if (lineBuckets.length === 0) {
        return;
      }
      var nLB = lineBuckets.length;
      var bins = Z_SORT_BINS;

      // Pass 1: per line, segment count (flattening decisions -- must match
      // _writeLineSegments, both call _spanSegments) and sort key.
      var totalSegs = 0;
      var keyCounts = new Uint32Array(bins * nLB + 1);
      for (b = 0; b < nLB; b++) {
        bucket = lineBuckets[b];
        stride = bucket.pointStride;
        lineBounds = bucket._lineBounds;
        var segCounts = new Int32Array(bucket.lines.length);
        var zKeys = new Int32Array(bucket.lines.length);
        for (i = 0; i < bucket.lines.length; i++) {
          pts = bucket.lines[i];
          if (pts.length < stride * 2) {
            continue; // single-vertex line: nothing to draw
          }
          var segs = 0;
          for (k = 0; k + stride < pts.length; k += stride) {
            segs += (stride >= 7) ? _spanSegments(pts, k, stride) : 1;
          }
          segCounts[i] = segs;
          var zTop;
          var lb = lineBounds && lineBounds[i];
          if (lb) {
            zTop = lb.maxZ;
          } else {
            zTop = -Infinity;
            for (k = stride - 1; k < pts.length; k += stride) {
              if (pts[k] > zTop) zTop = pts[k];
            }
          }
          var bin = ((zTop - zr.minZ) * zr.zScale) | 0;
          if (bin < 0) bin = 0; else if (bin > bins - 1) bin = bins - 1;
          zKeys[i] = bin * nLB + b;
          keyCounts[1 + zKeys[i]] += segs;
          totalSegs += segs;
        }
        bucket._glSegCounts = segCounts;
        bucket._glZKeys = zKeys;
      }
      if (totalSegs === 0) {
        return;
      }
      for (var s = 1; s < keyCounts.length; s++) {
        keyCounts[s] += keyCounts[s - 1];
      }
      var binStart = new Uint32Array(bins + 1);
      for (var bn = 0; bn <= bins; bn++) {
        binStart[bn] = keyCounts[bn * nLB];
      }

      // Pass 2: write each line's segments at its sorted slots. Line vertex,
      // 32 bytes: ax,ay,bx,by f32 | zmin,zmax f32 (the whole POLYLINE's z
      // range, copied per segment -- the 2D path altitude-gates per LINE via
      // _lineBounds, so per-segment z would visibly differ) | rgba8 |
      // flags u8 | halfWidth u8 (px * 8) | bucketIdx u16. Individually
      // hidden lines are written with the HIDDEN_Z sentinels up front (see
      // _refreshHiddenPatches for the incremental patching).
      var lineData = new ArrayBuffer(totalSegs * 4 * 32);
      var lineF32 = new Float32Array(lineData);
      var lineU8 = new Uint8Array(lineData);
      var lineU16 = new Uint16Array(lineData);
      var nRect = this._rectBucketList ? this._rectBucketList.length : 0;
      for (b = 0; b < nLB; b++) {
        bucket = lineBuckets[b];
        stride = bucket.pointStride;
        lineBounds = bucket._lineBounds;
        var rgb = _hexToRgb(bucket.color);
        var halfWidthQ = Math.min(255, Math.round(((bucket.lineWidth || 2.5) / 2) * 8));
        var ordinal = Math.min(nRect + b, VIS_TEX_WIDTH - 1);
        var segCounts2 = bucket._glSegCounts;
        var zKeys2 = bucket._glZKeys;
        var firstSlot = new Int32Array(bucket.lines.length).fill(-1);
        bucket._glBuiltHidden = bucket.hiddenIndices ? new Set(bucket.hiddenIndices) : new Set();
        for (i = 0; i < bucket.lines.length; i++) {
          if (segCounts2[i] === 0) {
            continue;
          }
          pts = bucket.lines[i];
          var slot = keyCounts[zKeys2[i]];
          keyCounts[zKeys2[i]] += segCounts2[i];
          firstSlot[i] = slot;
          var hidden = bucket.hiddenIndices && bucket.hiddenIndices.has(i);
          var lb2 = lineBounds && lineBounds[i];
          var zmin = hidden ? HIDDEN_Z : (lb2 ? lb2.minZ : -ALT_SENTINEL);
          var zmax = hidden ? -HIDDEN_Z : (lb2 ? lb2.maxZ : ALT_SENTINEL);
          this._writeLineSegments(lineF32, lineU8, lineU16, slot, pts, stride, zmin, zmax, rgb, halfWidthQ, ordinal);
        }
        bucket._glLineFirstSlot = firstSlot;
      }

      var lineBuffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, lineBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, lineData, gl.STATIC_DRAW);
      this._ensureIndexCapacity(totalSegs);
      var vao = gl.createVertexArray();
      gl.bindVertexArray(vao);
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, this._indexBuffer);
      gl.bindBuffer(gl.ARRAY_BUFFER, lineBuffer);
      gl.enableVertexAttribArray(1);
      gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 32, 0);   // a_a
      gl.enableVertexAttribArray(2);
      gl.vertexAttribPointer(2, 2, gl.FLOAT, false, 32, 8);   // a_b
      gl.enableVertexAttribArray(3);
      gl.vertexAttribPointer(3, 2, gl.FLOAT, false, 32, 16);  // a_zrange
      gl.enableVertexAttribArray(4);
      gl.vertexAttribPointer(4, 1, gl.UNSIGNED_BYTE, false, 32, 28); // a_flags
      gl.enableVertexAttribArray(5);
      gl.vertexAttribPointer(5, 4, gl.UNSIGNED_BYTE, true, 32, 24);  // a_color
      gl.enableVertexAttribArray(6);
      gl.vertexAttribPointer(6, 1, gl.UNSIGNED_BYTE, false, 32, 29); // a_halfWidth (px * 8)
      gl.enableVertexAttribArray(7);
      gl.vertexAttribPointer(7, 1, gl.UNSIGNED_SHORT, false, 32, 30); // a_bucketIdx
      gl.bindVertexArray(null);

      this._line = { buffer: lineBuffer, vao: vao, count: totalSegs, binStart: binStart, buckets: lineBuckets };
    },

    // Circles keep the old per-bucket run structure (one shared vertex
    // buffer, a tiny VAO per run with attributes bound at
    // byteOffset = firstQuad * 4 * stride -- WebGL2 has no baseVertex).
    // Circle vertex, 16 bytes: x,y,z f32 | flags u8 | pad x3. They're
    // terrain markers (resource nodes/wells), so they draw before the
    // rect/line merge walk rather than inside it.
    _buildCircleStream: function() {
      var gl = this._gl;
      var sorted = this._sortedBuckets;
      var runs = [];
      var b, bucket, i;
      var totalCircles = 0;
      for (b = 0; b < sorted.length; b++) {
        bucket = sorted[b];
        if (bucket.renderType === "circle" && bucket.points && bucket.points.length > 0) {
          var nPts = bucket.points.length / bucket.pointStride;
          runs.push({ bucket: bucket, first: totalCircles, count: nPts, vao: null,
                      rgb: _hexToRgb(bucket.color) });
          totalCircles += nPts;
        }
      }
      if (totalCircles === 0) {
        return;
      }
      var circleData = new ArrayBuffer(totalCircles * 4 * 16);
      var circF32 = new Float32Array(circleData);
      var circU8 = new Uint8Array(circleData);
      for (var r = 0; r < runs.length; r++) {
        var run = runs[r];
        bucket = run.bucket;
        var pts = bucket.points;
        var stride = bucket.pointStride;
        var altIdx = stride - 1;
        bucket._glBuiltHidden = bucket.hiddenIndices ? new Set(bucket.hiddenIndices) : new Set();
        for (i = 0; i < run.count; i++) {
          var cp = i * stride;
          var cz = (bucket.hiddenIndices && bucket.hiddenIndices.has(i)) ? HIDDEN_Z : pts[cp + altIdx];
          for (var ck = 0; ck < 4; ck++) {
            var cvi = (run.first + i) * 4 + ck;
            var cfo = cvi * 4;
            circF32[cfo] = pts[cp];
            circF32[cfo + 1] = pts[cp + 1];
            circF32[cfo + 2] = cz;
            circU8[cvi * 16 + 12] = ck; // bit0 unitX>0, bit1 unitY>0
          }
        }
      }
      var circleBuffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, circleBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, circleData, gl.STATIC_DRAW);
      this._ensureIndexCapacity(totalCircles);
      for (r = 0; r < runs.length; r++) {
        var crun = runs[r];
        var vao2 = gl.createVertexArray();
        gl.bindVertexArray(vao2);
        gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, this._indexBuffer);
        gl.bindBuffer(gl.ARRAY_BUFFER, circleBuffer);
        var cbase = crun.first * 4 * 16;
        gl.enableVertexAttribArray(1);
        gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 16, cbase);     // a_center
        gl.enableVertexAttribArray(2);
        gl.vertexAttribPointer(2, 1, gl.FLOAT, false, 16, cbase + 8); // a_z
        gl.enableVertexAttribArray(3);
        gl.vertexAttribPointer(3, 1, gl.UNSIGNED_BYTE, false, 16, cbase + 12); // a_flags
        gl.bindVertexArray(null);
        crun.vao = vao2;
      }
      this._circles = { buffer: circleBuffer, runs: runs };
    },

    // Writes one polyline's segments (4 expanded vertex records each) into
    // the line stream starting at segment slot `seg`; returns the next free
    // slot. Must make the same segment-count decision as the counting pass
    // (both call _spanSegments) so offsets line up. Curved spans get
    // sub-chords of the same cubic Bezier the 2D path hands to
    // bezierCurveTo (Hermite tangents / 3).
    _writeLineSegments: function(f32, u8, u16, seg, pts, stride, zmin, zmax, rgb, halfWidthQ, ordinal) {
      function emit(ax, ay, bx, by) {
        for (var k = 0; k < 4; k++) {
          var vi = seg * 4 + k;
          var fo = vi * 8;
          f32[fo] = ax; f32[fo + 1] = ay;
          f32[fo + 2] = bx; f32[fo + 3] = by;
          f32[fo + 4] = zmin; f32[fo + 5] = zmax;
          var bo = vi * 32;
          u8[bo + 24] = rgb[0]; u8[bo + 25] = rgb[1]; u8[bo + 26] = rgb[2]; u8[bo + 27] = 255;
          u8[bo + 28] = k; // bit0 t (along), bit1 side
          u8[bo + 29] = halfWidthQ;
          u16[vi * 16 + 15] = ordinal;
        }
        seg++;
      }
      var prevX = pts[0], prevY = pts[1];
      for (var k = 0; k + stride < pts.length; k += stride) {
        var bx = pts[k + stride], by = pts[k + stride + 1];
        var n = (stride >= 7) ? _spanSegments(pts, k, stride) : 1;
        if (n > 1) {
          var ax = pts[k], ay = pts[k + 1];
          var c1x = ax + pts[k + 4] / 3, c1y = ay + pts[k + 5] / 3;
          var c2x = bx - pts[k + stride + 2] / 3, c2y = by - pts[k + stride + 3] / 3;
          for (var s = 1; s <= n; s++) {
            var t = s / n;
            var mt = 1 - t;
            var w0 = mt * mt * mt, w1 = 3 * mt * mt * t, w2 = 3 * mt * t * t, w3 = t * t * t;
            var cx = w0 * ax + w1 * c1x + w2 * c2x + w3 * bx;
            var cy = w0 * ay + w1 * c1y + w2 * c2y + w3 * by;
            emit(prevX, prevY, cx, cy);
            prevX = cx; prevY = cy;
          }
        } else {
          emit(prevX, prevY, bx, by);
          prevX = bx; prevY = by;
        }
      }
      return seg;
    },

    // ------------------------------------------------------------------
    // Hidden-object patching. MapApp.hideObject / resetHiddenObjects mutate
    // bucket.hiddenIndices then requestRedraw; the next frame notices the
    // total count changed and patches ONLY the affected instances in place
    // -- no stream rebuild. (A hide+unhide pair that nets to the same count
    // within one frame is impossible through the UI: hide and reset are
    // separate clicks, each scheduling its own frame.)
    // ------------------------------------------------------------------

    _computeHiddenEpoch: function() {
      var sum = 0;
      for (var i = 0; i < this.buckets.length; i++) {
        var h = this.buckets[i].hiddenIndices;
        if (h) {
          sum += h.size;
        }
      }
      return sum;
    },

    _refreshHiddenPatches: function() {
      var gl = this._gl;
      var i, bucket;
      // Rects: diff every slot's desired hidden byte against the CPU shadow;
      // a single right-click hide is four 1-byte bufferSubData writes (one
      // per expanded vertex; a tilted instance has 1-3 slots to flip).
      var rect = this._rect;
      if (rect) {
        gl.bindBuffer(gl.ARRAY_BUFFER, rect.buffer);
        var one = new Uint8Array(1);
        for (i = 0; i < rect.count; i++) {
          bucket = rect.buckets[rect.slotBucket[i]];
          var want = bucket.hiddenIndices && bucket.hiddenIndices.has(rect.slotPoint[i]) ? 1 : 0;
          if (want !== rect.hiddenShadow[i]) {
            rect.hiddenShadow[i] = want;
            one[0] = want;
            for (var k = 0; k < 4; k++) {
              gl.bufferSubData(gl.ARRAY_BUFFER, (i * 4 + k) * 32 + 30, one);
            }
          }
        }
      }
      // Lines/circles: per bucket, diff hiddenIndices against the set the
      // stream was built with, then patch just the changed entries.
      var self = this;
      function diffHidden(bkt, apply) {
        var built = bkt._glBuiltHidden;
        var now = bkt.hiddenIndices;
        if (!built || (built.size === 0 && (!now || now.size === 0))) {
          return;
        }
        var changed = [];
        built.forEach(function(idx) {
          if (!now || !now.has(idx)) {
            changed.push(idx);
          }
        });
        if (now) {
          now.forEach(function(idx) {
            if (!built.has(idx)) {
              changed.push(idx);
            }
          });
        }
        if (changed.length === 0) {
          return;
        }
        for (var ci = 0; ci < changed.length; ci++) {
          apply(changed[ci], !!(now && now.has(changed[ci])));
        }
        bkt._glBuiltHidden = now ? new Set(now) : new Set();
      }
      if (this._line) {
        this._line.buckets.forEach(function(lineBucket) {
          diffHidden(lineBucket, function(idx, nowHidden) {
            self._patchHiddenLine(lineBucket, idx, nowHidden);
          });
        });
      }
      if (this._circles) {
        this._circles.runs.forEach(function(run) {
          diffHidden(run.bucket, function(idx, nowHidden) {
            self._patchHiddenCircle(run, idx, nowHidden);
          });
        });
      }
      this._hiddenEpoch = this._computeHiddenEpoch();
    },

    // Rewrites one polyline's per-segment z range with either the real
    // bounds (restore) or the HIDDEN_Z sentinels (hide -- the shader's
    // altitude gate then rejects every segment regardless of the active
    // filter). Only the two z floats are touched; the line's segments sit
    // contiguously at _glLineFirstSlot (see _buildLineStream).
    _patchHiddenLine: function(bucket, lineIdx, hidden) {
      var gl = this._gl;
      var firstSlot = bucket._glLineFirstSlot;
      var segCounts = bucket._glSegCounts;
      if (!this._line || !firstSlot || lineIdx >= firstSlot.length || firstSlot[lineIdx] < 0) {
        return;
      }
      var lb = bucket._lineBounds && bucket._lineBounds[lineIdx];
      var zmin = hidden ? HIDDEN_Z : (lb ? lb.minZ : -ALT_SENTINEL);
      var zmax = hidden ? -HIDDEN_Z : (lb ? lb.maxZ : ALT_SENTINEL);
      var two = new Float32Array([zmin, zmax]);
      gl.bindBuffer(gl.ARRAY_BUFFER, this._line.buffer);
      var first = firstSlot[lineIdx];
      for (var s = 0; s < segCounts[lineIdx]; s++) {
        for (var k = 0; k < 4; k++) {
          gl.bufferSubData(gl.ARRAY_BUFFER, ((first + s) * 4 + k) * 32 + 16, two);
        }
      }
    },

    _patchHiddenCircle: function(run, pointIdx, hidden) {
      var gl = this._gl;
      var bucket = run.bucket;
      var stride = bucket.pointStride;
      var z = hidden ? HIDDEN_Z : bucket.points[pointIdx * stride + (stride - 1)];
      var one = new Float32Array([z]);
      gl.bindBuffer(gl.ARRAY_BUFFER, this._circles.buffer);
      for (var k = 0; k < 4; k++) {
        gl.bufferSubData(gl.ARRAY_BUFFER, ((run.first + pointIdx) * 4 + k) * 16 + 8, one);
      }
    },

    // ------------------------------------------------------------------
    // Per-frame rendering
    // ------------------------------------------------------------------

    // Refreshed unconditionally every frame (~400 bytes) -- cheaper than
    // change-tracking checkbox toggles, and it's what makes a visibility
    // flip cost nothing beyond the frame it already scheduled.
    _updateVisibilityTexture: function() {
      var gl = this._gl;
      var data = this._visTexData;
      data.fill(0);
      var list = this._rectBucketList;
      if (list) {
        for (var b = 0; b < list.length; b++) {
          if (list[b].visible) {
            data[Math.min(b, VIS_TEX_WIDTH - 1)] = 255;
          }
        }
      }
      // Line buckets take the ordinals after the rect buckets' (matching
      // _buildLineStream's assignment).
      var lines = this._lineBucketList;
      if (lines) {
        var base = list ? list.length : 0;
        for (var lb = 0; lb < lines.length; lb++) {
          if (lines[lb].visible) {
            data[Math.min(base + lb, VIS_TEX_WIDTH - 1)] = 255;
          }
        }
      }
      gl.bindTexture(gl.TEXTURE_2D, this._visTexture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, VIS_TEX_WIDTH, 1, gl.RED, gl.UNSIGNED_BYTE, data);
    },

    _setCommonUniforms: function(pu, fr) {
      var gl = this._gl;
      gl.uniform2f(pu.u.u_anchorMap, fr.anchorMapX, fr.anchorMapY);
      gl.uniform2f(pu.u.u_anchorScreen, fr.anchorScreenX, fr.anchorScreenY);
      gl.uniform2f(pu.u.u_scale, fr.scaleX, fr.scaleY);
      gl.uniform2f(pu.u.u_viewport, fr.viewW, fr.viewH);
      gl.uniform2f(pu.u.u_altRange, fr.altMin, fr.altMax);
      gl.uniform2f(pu.u.u_zRange, fr.zMin, fr.zToNdc);
    },

    _renderFrame: function() {
      var map = this._map;
      var gl = this._gl;
      if (!map || !gl || this._contextLost || this._suspendRender) {
        return;
      }
      if ((window.devicePixelRatio || 1) !== this._dpr) {
        this._onResize(); // monitor move / browser zoom; _onResize re-schedules but rendering now is still correct
      }
      if (this._streamsDirty) {
        this._rebuildStreams();
        this._streamsDirty = false;
      }
      if (this._computeHiddenEpoch() !== this._hiddenEpoch) {
        this._refreshHiddenPatches();
      }

      // Re-glue all three canvases to the current viewport (setPosition also
      // clears any zoom-animation transform) and recompute the one affine.
      // _viewTopLeft must stay fresh: the inherited _redrawHighlight anchors
      // its affine to it.
      var size = map.getSize();
      var viewTopLeft = map.containerPointToLayerPoint([0, 0]);
      L.DomUtil.setPosition(this._glCanvas, viewTopLeft);
      L.DomUtil.setPosition(this._canvas, viewTopLeft);
      L.DomUtil.setPosition(this._highlightCanvas, viewTopLeft);
      this._viewTopLeft = viewTopLeft;

      var zoom = map.getZoom();
      var pixelOrigin = map.getPixelOrigin();
      var affine = this._computeAffine(zoom, pixelOrigin, viewTopLeft);
      // Anchor-relative transform (see the shader comment): anchor at the
      // view center keeps (mapPos - anchor) * scale small in f32.
      var center = map.getCenter();
      var ar = MapApp.altitudeRange;
      var fr = {
        anchorMapX: center.lng,
        anchorMapY: center.lat,
        anchorScreenX: affine.originX + center.lng * affine.scaleX,
        anchorScreenY: affine.originY + center.lat * affine.scaleY,
        scaleX: affine.scaleX,
        scaleY: affine.scaleY,
        viewW: size.x,
        viewH: size.y,
        altMin: ar && isFinite(ar.min) ? ar.min : -ALT_SENTINEL,
        altMax: ar && isFinite(ar.max) ? ar.max : ALT_SENTINEL,
        zMin: this._zRange ? this._zRange.minZ : 0,
        zToNdc: this._zRange && this._zRange.spread > 0 ? 2 / this._zRange.spread : 0,
      };

      this._updateVisibilityTexture();

      gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

      if (!this._sortedBuckets) {
        this._sortedBuckets = this.buckets.slice().sort(function(a, b) { return (a.drawPriority || 0) - (b.drawPriority || 0); });
      }

      // 1) Circles first: terrain markers (resource nodes/wells) sit under
      // the built world. They don't participate in altitude depth.
      gl.depthMask(false);
      var circleRadius = Math.min(3, 1 + Math.max(0, zoom) * 0.4);
      var progs = this._programs;
      if (this._circles) {
        var cp = progs.circle;
        gl.useProgram(cp.prog);
        this._setCommonUniforms(cp, fr);
        gl.uniform1f(cp.u.u_radiusPx, circleRadius);
        for (var r = 0; r < this._circles.runs.length; r++) {
          var run = this._circles.runs[r];
          if (!run.bucket.visible || run.count === 0) {
            continue;
          }
          gl.uniform4f(cp.u.u_color, run.rgb[0] / 255, run.rgb[1] / 255, run.rgb[2] / 255, 1);
          gl.bindVertexArray(run.vao);
          gl.drawElements(gl.TRIANGLES, run.count * 6, gl.UNSIGNED_INT, 0);
        }
      }

      // 2) Rects and lines interleaved by altitude bin: both streams are
      // baked lowest-z-first over the SAME bin mapping (_computeZRange), so
      // walking "every rect of bin <= b, then bin b's lines" ascending makes
      // blending follow true altitude across the two primitive classes. A
      // top-floor belt stays visible over the floors below it instead of
      // being buried under every rect's 0.55-alpha fill; lines of the same
      // bin draw after that bin's rects (a belt sits ON its floor). Each
      // contiguous slot range is one drawElements at an index-buffer offset
      // (6 u32 indices per quad -> slot * 24 bytes); on real saves lines
      // cluster into few bins, so this stays a handful of draws per frame.
      var rect = this._rect, line = this._line;
      var rp = progs.rect, lp = progs.line;
      gl.depthMask(true); // fills and lines record their altitude for the outline pass
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this._visTexture);
      if (rect && rect.count > 0) {
        gl.useProgram(rp.prog);
        this._setCommonUniforms(rp, fr);
        gl.uniform1i(rp.u.u_visibility, 0);
        gl.uniform1f(rp.u.u_outlinePass, 0);
      }
      if (line && line.count > 0) {
        gl.useProgram(lp.prog);
        this._setCommonUniforms(lp, fr);
        gl.uniform1i(lp.u.u_visibility, 0);
      }
      var rectCursor = 0;
      var curProg = null;
      var pendA = -1, pendB = -1; // pending line slot range, merged across bins with no rects in between
      function drawLines() {
        if (pendB <= pendA) {
          return;
        }
        if (curProg !== lp) {
          gl.useProgram(lp.prog);
          gl.bindVertexArray(line.vao);
          curProg = lp;
        }
        gl.drawElements(gl.TRIANGLES, (pendB - pendA) * 6, gl.UNSIGNED_INT, pendA * 24);
        pendA = pendB = -1;
      }
      if (line && line.count > 0) {
        var lbins = line.binStart;
        var rbins = rect ? rect.binStart : null;
        for (var bin = 0; bin < Z_SORT_BINS; bin++) {
          var s0 = lbins[bin], s1 = lbins[bin + 1];
          if (s1 <= s0) {
            continue;
          }
          if (rbins && rbins[bin + 1] > rectCursor) {
            drawLines();
            if (curProg !== rp) {
              gl.useProgram(rp.prog);
              gl.bindVertexArray(rect.vao);
              curProg = rp;
            }
            gl.drawElements(gl.TRIANGLES, (rbins[bin + 1] - rectCursor) * 6, gl.UNSIGNED_INT, rectCursor * 24);
            rectCursor = rbins[bin + 1];
          }
          // Contiguous with the pending range whenever no rect draw flushed
          // it (line slots are bin-ordered, so s0 == previous s1 then).
          if (pendA < 0) {
            pendA = s0;
          }
          pendB = s1;
        }
        drawLines();
      }
      if (rect && rect.count > rectCursor) {
        gl.useProgram(rp.prog);
        gl.bindVertexArray(rect.vao);
        gl.drawElements(gl.TRIANGLES, (rect.count - rectCursor) * 6, gl.UNSIGNED_INT, rectCursor * 24);
      }

      // 3) Deferred outline pass: re-draw the rect stream band-only (see
      // RECT_FS/u_outlinePass), depth-tested GEQUAL against the altitudes
      // every fill and line above just wrote -- a border survives only
      // where its own building is the topmost thing at that pixel, so
      // nothing bleeds through buildings (or belts) drawn above. Skipped
      // whenever no bucket can reach outline size at this scale.
      if (rect && rect.count > 0 &&
          rect.maxMinHalf * 2 * Math.abs(fr.scaleX) >= RECT_OUTLINE_MIN_PX) {
        gl.useProgram(rp.prog);
        gl.uniform1f(rp.u.u_outlinePass, 1);
        gl.depthFunc(gl.GEQUAL);
        gl.depthMask(false);
        gl.bindVertexArray(rect.vao);
        gl.drawElements(gl.TRIANGLES, rect.count * 6, gl.UNSIGNED_INT, 0);
        gl.depthFunc(gl.ALWAYS);
        // clear() honors the depth write mask -- without this restore the
        // next frame's depth clear is a silent no-op and stale altitudes
        // survive at uncovered pixels.
        gl.depthMask(true);
      }

      gl.bindVertexArray(null);

      // 3) Icon pins on the 2D canvas above the GL canvas, via the inherited
      // pin pass (occupancy dedup, icon cache and all -- this._canvas IS the
      // pin canvas, so _occEnsure sizes correctly). Same viewport cull
      // bounds the 2D _reset computes, minus the buffer margin.
      var pinCtx = this._canvas.getContext("2d");
      pinCtx.clearRect(0, 0, this._canvas.width, this._canvas.height);
      var nw = map.layerPointToLatLng(viewTopLeft);
      var se = map.layerPointToLatLng(L.point(viewTopLeft.x + size.x, viewTopLeft.y + size.y));
      var iconRadius = MapApp.iconRadiusForZoom(zoom);
      // Pad the cull bounds by a pin's full screen footprint (the circle
      // sits ~1.7 radii above its anchor): a pin anchored just outside the
      // viewport still pokes into it. The 2D path got this for free from
      // its buffered canvas margin.
      var pinPad = (iconRadius * 3) / Math.pow(2, zoom);
      var minX = Math.min(nw.lng, se.lng) - pinPad, maxX = Math.max(nw.lng, se.lng) + pinPad;
      var minY = Math.min(nw.lat, se.lat) - pinPad, maxY = Math.max(nw.lat, se.lat) + pinPad;
      var altMin2d = ar ? ar.min : -Infinity;
      var altMax2d = ar ? ar.max : Infinity;
      var sorted = this._sortedBuckets;
      for (var b = 0; b < sorted.length; b++) {
        var bucket = sorted[b];
        if (bucket.visible && bucket.renderType === "icon") {
          this._drawIconBucket(pinCtx, bucket, affine, minX, maxX, minY, maxY, iconRadius, altMin2d, altMax2d);
        }
      }

      // 4) The hover/pin highlight, inherited -- redrawn every frame so it
      // stays glued to its object during continuous pans.
      this._redrawHighlight();
    },
  });

  WebGLBucketedLayer.isSupported = function() {
    // ?renderer=canvas is the permanent escape hatch / A-B switch: it forces
    // the 2D layer for debugging and for visual-parity comparisons.
    if (/[?&]renderer=canvas\b/.test(window.location.search)) {
      return false;
    }
    try {
      var probe = document.createElement("canvas").getContext("webgl2");
      return !!probe;
    } catch (e) {
      return false;
    }
  };

  window.WebGLBucketedLayer = WebGLBucketedLayer;
})();
