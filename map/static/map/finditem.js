// "Find Item" search: looks up one item across every inventory in the save
// (see sav_map_data.findItemLocations / collectItemLocationIndex, queried
// via /api/find-item) and lists every building that holds it -- names and
// quantities, most first -- with an optional toggle to highlight (and hide
// everything else) just those buildings on the map. Also exposes the
// Dimensional Depot's contents (see sav_map_data.collectDimensionalDepotContents),
// a single global shared inventory with no map position of its own, both as
// its own standalone list and folded into the search results above wherever
// the searched item is sitting in the Depot too.

var FindItem = {};

(function() {
  "use strict";

  var panel = document.getElementById("findItemPanel");
  var input = document.getElementById("findItemInput");
  var catalogList = document.getElementById("findItemCatalog");
  var findButton = document.getElementById("findItemButton");
  var resultBox = document.getElementById("findItemResult");
  var summaryEl = document.getElementById("findItemSummary");
  var listEl = document.getElementById("findItemList");
  var highlightToggle = document.getElementById("findItemHighlightToggle");

  var depotPanel = document.getElementById("dimensionalDepotPanel");
  var depotButton = document.getElementById("dimensionalDepotButton");
  var depotList = document.getElementById("dimensionalDepotList");

  var HIGHLIGHT_BUCKET_KEY = "find-item-highlight";
  var HIGHLIGHT_COLOR = "#ff3b81";
  // Simple inline SVG magnifying-glass silhouette -- same reasoning as the
  // player/HUB/creature icons in filters.js: a one-off marker doesn't
  // warrant a real image asset.
  var HIGHLIGHT_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<circle cx="13" cy="13" r="9" fill="none" stroke="' + HIGHLIGHT_COLOR + '" stroke-width="4"/>' +
    '<line x1="20" y1="20" x2="28" y2="28" stroke="' + HIGHLIGHT_COLOR + '" stroke-width="4" stroke-linecap="round"/>' +
    '</svg>'
  );

  var catalogByLabel = {}; // Typed/selected label -> itemPath (short class name), for resolving the datalist's free-text input.
  var savedVisibility = null; // bucket.key -> visible, captured right before highlighting so "show all layers again" restores exactly what was on.
  var highlighting = false;
  var lastResult = null;

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function renderLocationList(container, rows) {
    container.innerHTML = "";
    rows.forEach(function(pair) {
      var row = el("div", "itemLocationRow");
      row.appendChild(el("span", "itemLocationLabel", pair[0]));
      row.appendChild(el("span", "itemLocationCount", pair[1]));
      container.appendChild(row);
    });
  }

  // Real buildings each get their own row (their individual quantity/
  // location is the useful part). Power Slugs/Somersloops/Mercer Spheres/
  // Hard Drives still waiting to be collected, and the Dimensional Depot,
  // are a different case: sav_map_data.findItemLocations returns one entry
  // *per pickup* (each with count 1, so the map highlight can plot every
  // one), which reads as hundreds of near-identical "Blue Power Slug: 1"
  // rows here -- collapsed into a single summed row per label instead.
  // Distinguished from real buildings by typePath being null (see
  // findItemLocations -- only static/pseudo locations lack one); grouping
  // the Dimensional Depot's own single row by label is a harmless no-op
  // since there's only ever one of it.
  function groupLocationsForDisplay(locations) {
    var buildingRows = [];
    var groupedTotals = {};
    var groupedOrder = [];
    locations.forEach(function(loc) {
      if (loc.typePath) {
        buildingRows.push({ label: loc.label, count: loc.count });
        return;
      }
      if (!groupedTotals.hasOwnProperty(loc.label)) {
        groupedTotals[loc.label] = 0;
        groupedOrder.push(loc.label);
      }
      groupedTotals[loc.label] += loc.count;
    });
    var rows = buildingRows.concat(groupedOrder.map(function(label) {
      return { label: label, count: groupedTotals[label] };
    }));
    rows.sort(function(a, b) { return b.count - a.count; });
    return rows;
  }

  // Removes the temporary highlight bucket (if any) and restores whatever
  // every other bucket's visibility was right before highlighting started --
  // matched by bucket.key, which stays valid even across a reload in between
  // (see filters.js's savedVisibility comment: keys are stable identifiers
  // for a *kind* of thing, not tied to one specific save's data).
  function clearHighlight() {
    if (!highlighting) {
      return;
    }
    highlighting = false;
    MapApp.layer.buckets = MapApp.layer.buckets.filter(function(b) { return b.key !== HIGHLIGHT_BUCKET_KEY; });
    if (savedVisibility) {
      MapApp.layer.buckets.forEach(function(b) {
        if (savedVisibility.hasOwnProperty(b.key)) {
          b.visible = savedVisibility[b.key];
        }
      });
      savedVisibility = null;
    }
    highlightToggle.textContent = "Show only these on map";
    MapApp.layer.requestRedraw();
  }

  // Hides every existing layer and shows one new bucket containing just the
  // locations that hold the searched item -- reusing the normal bucket/
  // canvas pipeline (see map.js) instead of teaching the (already heavily
  // optimized) bulk renderer a new per-point filtering mode. The Dimensional
  // Depot's pseudo-location (see sav_map_data.findItemLocations) has no
  // position -- it's excluded here (nothing to plot) but still shown in the
  // text list above.
  function showHighlight(result) {
    var plottable = result.locations.filter(function(loc) { return loc.position; });
    if (plottable.length === 0) {
      return;
    }

    savedVisibility = {};
    MapApp.layer.buckets.forEach(function(b) {
      savedVisibility[b.key] = b.visible;
      b.visible = false;
    });

    var points = [];
    var ids = [];
    var byInstance = {};
    plottable.forEach(function(loc) {
      points.push(loc.position[0], loc.position[1], loc.position[2]);
      ids.push(loc.instanceName);
      byInstance[loc.instanceName] = loc;
    });

    MapApp.layer.addBucket({
      key: HIGHLIGHT_BUCKET_KEY,
      label: "Find Item Results",
      color: HIGHLIGHT_COLOR,
      visible: true,
      renderType: "icon",
      pointStride: 3,
      points: new Float32Array(points),
      ids: ids,
      tooltipKind: "static",
      tooltipInfo: function(index) {
        var loc = byInstance[ids[index]];
        var unit = result.isFluid ? " m³" : "";
        return {
          title: loc.label,
          rows: [["Item", result.label], ["Quantity here", loc.count + unit]],
          position: loc.worldPosition,
        };
      },
      iconUrl: HIGHLIGHT_ICON_URL,
      iconOpacity: 1,
    });

    highlighting = true;
    highlightToggle.textContent = "Show all layers again";
    MapApp.layer.requestRedraw();

    // Jump the view to the results instead of leaving the user to hunt for
    // them at whatever pan/zoom they happened to be at -- the whole point of
    // this feature is making them easy to find.
    var latLngs = plottable.map(function(loc) { return [loc.position[1], loc.position[0]]; });
    MapApp.map.fitBounds(L.latLngBounds(latLngs), { padding: [40, 40], maxZoom: 4 });
  }

  function showResult(result) {
    lastResult = result;
    clearHighlight();
    resultBox.style.display = "block";
    summaryEl.textContent = "";
    listEl.innerHTML = "";
    var unit = result.isFluid ? " m³" : "";
    if (result.locations.length === 0) {
      summaryEl.textContent = "No " + result.label + " found in any inventory.";
      highlightToggle.style.display = "none";
      return;
    }
    summaryEl.textContent = result.totalCount.toLocaleString() + unit + " " + result.label +
      " across " + result.locations.length.toLocaleString() + " location" + (result.locations.length === 1 ? "" : "s") + ".";
    renderLocationList(listEl, groupLocationsForDisplay(result.locations).map(function(row) {
      return [row.label, row.count.toLocaleString() + unit];
    }));
    var hasPlottable = result.locations.some(function(loc) { return loc.position; });
    highlightToggle.style.display = hasPlottable ? "inline-block" : "none";
  }

  function runSearch() {
    var typed = input.value.trim();
    var itemPath = catalogByLabel[typed];
    if (!itemPath) {
      clearHighlight();
      resultBox.style.display = "block";
      summaryEl.textContent = typed ? "Pick an item from the list." : "";
      listEl.innerHTML = "";
      highlightToggle.style.display = "none";
      lastResult = null;
      return;
    }
    var filename = window.MapApp.currentFile;
    if (!filename) {
      return;
    }
    summaryEl.textContent = "Searching...";
    listEl.innerHTML = "";
    resultBox.style.display = "block";
    highlightToggle.style.display = "none";
    fetch("/api/find-item?file=" + encodeURIComponent(filename) + "&item=" + encodeURIComponent(itemPath))
      .then(function(response) { return response.json(); })
      .then(function(result) {
        if (result.error) {
          summaryEl.textContent = result.error;
          return;
        }
        showResult(result);
      })
      .catch(function(error) {
        summaryEl.textContent = "Search failed: " + error;
      });
  }

  findButton.addEventListener("click", runSearch);
  input.addEventListener("keydown", function(e) {
    if (e.key === "Enter") {
      runSearch();
    }
  });
  highlightToggle.addEventListener("click", function() {
    if (highlighting) {
      clearHighlight();
    } else if (lastResult) {
      showHighlight(lastResult);
    }
  });

  depotButton.addEventListener("click", function() {
    var showing = depotList.style.display !== "none";
    depotList.style.display = showing ? "none" : "block";
  });

  // Rebuilds the (save-independent) catalog, the Dimensional Depot's
  // contents list, and resets any in-progress search/highlight -- called
  // alongside Filters.build/Altitude.build on every load (see data.js),
  // since a reload's fresh buckets shouldn't be silently hidden by a
  // highlight the user set up against the old ones.
  FindItem.build = function(payload) {
    clearHighlight();
    resultBox.style.display = "none";
    input.value = "";
    lastResult = null;

    catalogByLabel = {};
    catalogList.innerHTML = "";
    (payload.itemCatalog || []).forEach(function(entry) {
      catalogByLabel[entry.label] = entry.itemPath;
      var option = document.createElement("option");
      option.value = entry.label;
      catalogList.appendChild(option);
    });
    panel.style.display = (payload.itemCatalog || []).length > 0 ? "block" : "none";

    var depotItems = payload.dimensionalDepot || [];
    depotList.style.display = "none";
    renderLocationList(depotList, depotItems.map(function(entry) {
      return [entry.label, entry.count.toLocaleString()];
    }));
    depotPanel.style.display = depotItems.length > 0 ? "block" : "none";
  };
})();
