// Map markers for a conveyor line's or pipe network's speed bottleneck.
// When a hovered belt/lift/pipe/pump's /api/instance detail reports a
// mixed-mark line or network (see sav_map_data._flowBottleneck), the
// tooltip's warning section
// (tooltip.js's bottleneckSection) offers a "Show on map" button that lands
// here: a temporary icon bucket drops a warning pin on every limiting
// segment and the view jumps to frame them, so the one slow belt hiding in
// a long line is actually findable. Reuses the normal bucket/canvas
// pipeline the same way finditem.js's highlight does -- but WITHOUT hiding
// every other layer, since the surrounding factory is exactly the context
// needed to recognize which belt that is.
//
// Only one bottleneck's markers exist at a time (BUCKET_KEY); showing
// another line's replaces them. A save reload drops the bucket wholesale
// along with every other one (Filters.build clears and rebuilds all
// buckets), which is why isShowing() asks the live bucket list instead of
// keeping a flag that a reload would silently invalidate.

var Bottleneck = {};

(function() {
  "use strict";

  var BUCKET_KEY = "belt-bottleneck-highlight";
  var WARNING_COLOR = "#ffb020";
  // Inline SVG warning-triangle silhouette -- same reasoning as finditem.js's
  // magnifying glass: a one-off marker doesn't warrant a real image asset.
  var WARNING_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<path d="M16 4 L29 27 L3 27 Z" fill="none" stroke="' + WARNING_COLOR + '" stroke-width="3.5" stroke-linejoin="round"/>' +
    '<line x1="16" y1="12" x2="16" y2="20" stroke="' + WARNING_COLOR + '" stroke-width="3.5" stroke-linecap="round"/>' +
    '<circle cx="16" cy="24" r="1.8" fill="' + WARNING_COLOR + '"/>' +
    '</svg>'
  );

  // Identity of the bottleneck currently marked, so a pinned tooltip's
  // toggle button can tell "my markers are up" from "some other line's
  // markers are up" (a stable per-line stand-in: the first limiting
  // segment's instanceName).
  var shownKey = null;

  function bottleneckKey(bottleneck) {
    return bottleneck.limitingSegments && bottleneck.limitingSegments.length > 0
      ? bottleneck.limitingSegments[0].instanceName : null;
  }

  function bucketExists() {
    return !!(window.MapApp && MapApp.layer && MapApp.layer.buckets.some(function(b) {
      return b.key === BUCKET_KEY;
    }));
  }

  Bottleneck.isShowing = function(bottleneck) {
    return bucketExists() && shownKey !== null && shownKey === bottleneckKey(bottleneck);
  };

  Bottleneck.clear = function() {
    shownKey = null;
    if (bucketExists()) {
      MapApp.layer.removeBucketByKey(BUCKET_KEY);
      MapApp.layer.requestRedraw();
    }
  };

  // bottleneck is detail.lineBottleneck from /api/instance -- see
  // sav_map_data._conveyorChainBottleneck for the fields.
  Bottleneck.show = function(bottleneck) {
    Bottleneck.clear(); // At most one line's markers at a time.
    var segments = bottleneck.limitingSegments || [];
    if (segments.length === 0) {
      return;
    }

    var points = [];
    var ids = [];
    var byInstance = {};
    segments.forEach(function(seg) {
      points.push(seg.position[0], seg.position[1], seg.position[2]);
      ids.push(seg.instanceName);
      byInstance[seg.instanceName] = seg;
    });

    MapApp.layer.addBucket({
      key: BUCKET_KEY,
      label: "Line Bottleneck",
      color: WARNING_COLOR,
      visible: true,
      renderType: "icon",
      pointStride: 3,
      points: new Float32Array(points),
      ids: ids,
      // Hovering/clicking a warning pin shows the limiting segment's own
      // full /api/instance tooltip (whose warning section will read "this
      // segment slows the whole line/network"), with the capped rate as the
      // highlighted callout -- same static+server mix as finditem.js's
      // highlight bucket.
      tooltipKind: "static",
      tooltipInfo: function(index) {
        var seg = byInstance[ids[index]];
        return { title: seg.label, rows: [], position: seg.worldPosition };
      },
      tooltipServerId: function(index) {
        return ids[index];
      },
      tooltipExtraRows: function() {
        var scopeWord = bottleneck.scope === "network" ? "network" : "line";
        return [["Limits " + scopeWord + " to", bottleneck.limitPerMinute + " " + (bottleneck.unit || "items/min")]];
      },
      iconUrl: WARNING_ICON_URL,
      iconOpacity: 1,
    });
    shownKey = bottleneckKey(bottleneck);

    // Jump the view to frame every marker. maxZoom keeps a single-segment
    // fit from diving to full zoom where the surrounding factory (the
    // context that makes the belt recognizable) would be cropped away.
    // bucket points are map-pixel [x, y]; Leaflet CRS.Simple wants (lat=y,
    // lng=x) -- same axis mapping as map.js's hitTest calls.
    var latLngs = segments.map(function(seg) {
      return [seg.position[1], seg.position[0]];
    });
    MapApp.map.fitBounds(L.latLngBounds(latLngs), { padding: [80, 80], maxZoom: 4 });
    MapApp.layer.requestRedraw();
  };
})();
