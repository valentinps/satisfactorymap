// Altitude (Z, meters) range filter. Scans the loaded payload once for the
// actual min/max altitude present, then drives MapApp.setAltitudeRange()
// (see map.js) via two overlapping <input type="range"> sliders.

var Altitude = {};

(function() {
  "use strict";

  var panel = document.getElementById("altitudePanel");
  var title = document.querySelector("#altitudePanel .altitudeTitle");
  var maxLabel = document.getElementById("altitudeMaxLabel");
  var minLabel = document.getElementById("altitudeMinLabel");
  var sliderRow = document.querySelector("#altitudePanel .altitudeSliders");
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
    sliderRow.style.height = available + "px";
    minSlider.style.height = available + "px";
    maxSlider.style.height = available + "px";
  }
  window.addEventListener("resize", layoutSliders);

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
    if (current.min > current.max) {
      current = { min: 0, max: 0 };
    }
    return current;
  }

  function updateLabel(min, max) {
    maxLabel.textContent = Math.round(max) + " m";
    minLabel.textContent = Math.round(min) + " m";
  }

  function applyRange() {
    var min = parseFloat(minSlider.value);
    var max = parseFloat(maxSlider.value);
    if (min > max) {
      // Keep the two handles from crossing -- push the other one along instead.
      if (this === minSlider) {
        maxSlider.value = min;
        max = min;
      } else {
        minSlider.value = max;
        min = max;
      }
    }
    updateLabel(min, max);
    savedRange = { min: min, max: max };
    MapApp.setAltitudeRange(min, max);
  }

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
    var isFullRange = initMin === lo && initMax === hi;
    MapApp.setAltitudeRange(isFullRange ? -Infinity : initMin, isFullRange ? Infinity : initMax);

    panel.style.display = "flex";
    layoutSliders();
  };

  minSlider.addEventListener("input", applyRange);
  maxSlider.addEventListener("input", applyRange);
  resetButton.addEventListener("click", function() {
    minSlider.value = minSlider.min;
    maxSlider.value = maxSlider.max;
    updateLabel(parseFloat(minSlider.min), parseFloat(maxSlider.max));
    savedRange = { min: parseFloat(minSlider.min), max: parseFloat(maxSlider.max) };
    MapApp.setAltitudeRange(-Infinity, Infinity);
  });
})();
