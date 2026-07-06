// Altitude (Z, meters) range filter. Scans the loaded payload once for the
// actual min/max altitude present, then drives MapApp.setAltitudeRange()
// (see map.js) via two <input type="range"> sliders stacked on one shared
// visual track (see map.css's .altitudeTrackWrap).

var Altitude = {};

(function() {
  "use strict";

  var panel = document.getElementById("altitudePanel");
  var title = document.querySelector("#altitudePanel .altitudeTitle");
  var maxLabel = document.getElementById("altitudeMaxLabel");
  var minLabel = document.getElementById("altitudeMinLabel");
  var trackWrap = document.querySelector("#altitudePanel .altitudeTrackWrap");
  var trackFill = document.getElementById("altitudeTrackFill");
  var minSlider = document.getElementById("altitudeMinSlider");
  var maxSlider = document.getElementById("altitudeMaxSlider");
  var resetButton = document.getElementById("altitudeResetButton");

  // Native <input type="range"> + writing-mode:vertical-lr doesn't reliably
  // stretch to fill a flex parent's height across browsers (it caused the
  // panel to overflow past the viewport, clipping the reset button and the
  // low end of the track). Measuring the actually-available space and
  // setting an explicit pixel height sidesteps that entirely.
  function layoutSliders() {
    var available = panel.clientHeight - title.offsetHeight - maxLabel.offsetHeight - minLabel.offsetHeight - resetButton.offsetHeight;
    available -= 16; // panel padding/margins between elements
    available = Math.max(40, available);
    trackWrap.style.height = available + "px";
  }
  window.addEventListener("resize", layoutSliders);

  // Positions the highlighted segment of the shared track between the two
  // handles. direction:rtl on the inputs (see map.css) puts the maximum at
  // the top, so the fraction of the track "used up" from the top is how far
  // *down* from hi the current max is, and the fill spans from there down to
  // the equivalent point for min.
  function updateTrackFill(min, max, lo, hi) {
    var span = hi - lo;
    if (span <= 0) {
      trackFill.style.top = "0%";
      trackFill.style.height = "100%";
      return;
    }
    var topPercent = ((hi - max) / span) * 100;
    var bottomPercent = ((hi - min) / span) * 100;
    trackFill.style.top = topPercent + "%";
    trackFill.style.height = Math.max(0, bottomPercent - topPercent) + "%";
  }

  // Remembers the user's last chosen min/max (in absolute meters) across
  // rebuilds -- e.g. an auto-refresh reload (see data.js's checkForNewerSave)
  // shouldn't snap a deliberately narrowed altitude filter back open just
  // because a newer save was parsed. Left null until the user actually moves
  // a handle (or clicks Reset), so the very first build for a save still
  // gets the plain full-range default.
  var savedRange = null;

  function scanRange(points, stride, current) {
    var altIndex = stride - 1;
    for (var i = altIndex; i < points.length; i += stride) {
      var z = points[i];
      if (z < current.min) current.min = z;
      if (z > current.max) current.max = z;
    }
  }

  function computeAltitudeRange(payload) {
    var current = { min: Infinity, max: -Infinity };
    payload.buildingCategories.forEach(function(cat) {
      cat.types.forEach(function(t) { scanRange(t.points, 4, current); });
    });
    payload.resourceNodes.byResourceType.forEach(function(r) {
      ["mined", "unmined"].forEach(function(state) {
        Object.values(r[state].byPurity).forEach(function(p) { scanRange(p.points, 3, current); });
      });
    });
    Object.keys(payload.collectables).forEach(function(key) {
      var c = payload.collectables[key];
      scanRange(c.remaining, 3, current);
      scanRange(c.collected, 3, current);
    });
    ["hasDrive", "empty", "dismantled"].forEach(function(key) {
      scanRange(payload.hardDrives[key], 3, current);
    });
    Object.keys(payload.lines).forEach(function(key) {
      var stride = payload.lines[key].pointStride || 3;
      payload.lines[key].polylines.forEach(function(line) { scanRange(line, stride, current); });
    });
    (payload.belts || []).concat(payload.pipes || []).forEach(function(group) {
      var stride = group.pointStride || 7;
      group.polylines.forEach(function(line) { scanRange(line, stride, current); });
    });
    if (current.min > current.max) {
      current = { min: 0, max: 0 };
    }
    return current;
  }

  function updateLabel(min, max) {
    maxLabel.textContent = Math.round(max) + " m";
    minLabel.textContent = Math.round(min) + " m";
  }

  // Shared by both handle-dragging (applyRange) and whole-window-dragging
  // (the fill-bar pointer handlers below) -- applies a min/max pair
  // everywhere it needs to land: the two <input>s' own values (so their
  // thumbs redraw in the right spot), the labels, the visual fill, the
  // persisted savedRange, and the live map filter.
  function setRange(min, max) {
    minSlider.value = min;
    maxSlider.value = max;
    updateLabel(min, max);
    updateTrackFill(min, max, parseFloat(minSlider.min), parseFloat(maxSlider.max));
    savedRange = { min: min, max: max };
    MapApp.setAltitudeRange(min, max);
  }

  function applyRange() {
    var min = parseFloat(minSlider.value);
    var max = parseFloat(maxSlider.value);
    if (min > max) {
      // Keep the two handles from crossing -- push the other one along instead.
      if (this === minSlider) {
        max = min;
      } else {
        min = max;
      }
    }
    setRange(min, max);
  }

  // Dragging the fill bar shifts the whole [min, max] window by the same
  // amount, keeping its span fixed, instead of only moving one handle at a
  // time -- clamped so neither bound crosses the track's lo/hi. Pointer
  // events (not native HTML5 drag) so touch works too, with pointer capture
  // so the drag keeps tracking even once the cursor strays off the fill
  // strip mid-gesture.
  var fillDrag = null;

  trackFill.addEventListener("pointerdown", function(e) {
    fillDrag = {
      pointerId: e.pointerId,
      startClientY: e.clientY,
      startMin: parseFloat(minSlider.value),
      startMax: parseFloat(maxSlider.value),
      lo: parseFloat(minSlider.min),
      hi: parseFloat(maxSlider.max),
      trackHeightPx: trackWrap.getBoundingClientRect().height,
    };
    trackFill.setPointerCapture(e.pointerId);
    e.preventDefault();
  });

  trackFill.addEventListener("pointermove", function(e) {
    if (!fillDrag || e.pointerId !== fillDrag.pointerId) {
      return;
    }
    var boundsSpan = fillDrag.hi - fillDrag.lo;
    if (boundsSpan <= 0 || fillDrag.trackHeightPx <= 0) {
      return;
    }
    // direction:rtl puts the maximum at the top of the track (see
    // updateTrackFill's doc comment above), so moving the pointer down
    // (increasing clientY) means decreasing altitude -- the pixel delta's
    // sign is flipped relative to the value delta it produces.
    var deltaValue = -((e.clientY - fillDrag.startClientY) / fillDrag.trackHeightPx) * boundsSpan;
    var rangeSpan = fillDrag.startMax - fillDrag.startMin;
    var newMin = fillDrag.startMin + deltaValue;
    var newMax = fillDrag.startMax + deltaValue;
    if (newMin < fillDrag.lo) {
      newMin = fillDrag.lo;
      newMax = newMin + rangeSpan;
    } else if (newMax > fillDrag.hi) {
      newMax = fillDrag.hi;
      newMin = newMax - rangeSpan;
    }
    setRange(Math.round(newMin), Math.round(newMax));
  });

  function endFillDrag(e) {
    if (fillDrag && (!e || e.pointerId === fillDrag.pointerId)) {
      fillDrag = null;
    }
  }
  trackFill.addEventListener("pointerup", endFillDrag);
  trackFill.addEventListener("pointercancel", endFillDrag);

  Altitude.build = function(payload) {
    var range = computeAltitudeRange(payload);
    var lo = Math.floor(range.min) - 1;
    var hi = Math.ceil(range.max) + 1;

    [minSlider, maxSlider].forEach(function(slider) {
      slider.min = lo;
      slider.max = hi;
      slider.step = 1;
    });

    // Restore a previously chosen range (clamped to this build's actual
    // altitude span, which can shift slightly build-to-build) instead of
    // always reopening to the full range -- see savedRange above.
    var initMin = lo, initMax = hi;
    if (savedRange) {
      initMin = Math.min(Math.max(savedRange.min, lo), hi);
      initMax = Math.min(Math.max(savedRange.max, lo), hi);
      if (initMin > initMax) {
        initMin = lo;
        initMax = hi;
      }
    }
    minSlider.value = initMin;
    maxSlider.value = initMax;
    updateLabel(initMin, initMax);
    updateTrackFill(initMin, initMax, lo, hi);
    var isFullRange = initMin === lo && initMax === hi;
    MapApp.setAltitudeRange(isFullRange ? -Infinity : initMin, isFullRange ? Infinity : initMax);

    panel.style.display = "flex";
    layoutSliders();
  };

  minSlider.addEventListener("input", applyRange);
  maxSlider.addEventListener("input", applyRange);
  resetButton.addEventListener("click", function() {
    var lo = parseFloat(minSlider.min);
    var hi = parseFloat(maxSlider.max);
    minSlider.value = lo;
    maxSlider.value = hi;
    updateLabel(lo, hi);
    updateTrackFill(lo, hi, lo, hi);
    savedRange = { min: lo, max: hi };
    MapApp.setAltitudeRange(-Infinity, Infinity);
  });
})();
