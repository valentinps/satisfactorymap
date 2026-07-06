// Drives the top bar's item search and the Dimensional Depot icon button,
// both of which show their results in the same centered modal dialog (see
// #itemModalOverlay in index.html). The search looks up one item across
// every inventory in the save (see sav_map_data.findItemLocations /
// collectItemLocationIndex, queried via /api/find-item) and lists every
// building that holds it -- names and quantities, most first -- with an
// optional toggle to highlight (and hide everything else) just those
// buildings on the map. The Depot button shows sav_map_data.
// collectDimensionalDepotContents's contents the same way, minus the
// highlight toggle (the Depot is a single global inventory with no map
// position of its own).

var FindItem = {};

(function() {
  "use strict";

  var searchInput = document.getElementById("mainSearchInput");
  var searchBox = document.getElementById("searchBox");
  var suggestionsEl = document.getElementById("searchSuggestions");
  var depotButton = document.getElementById("depotIconButton");

  var MAX_SUGGESTIONS = 8;
  // Item icons are stored under readable name (see static/map/icons/items/,
  // e.g. "Iron Plate.png") -- the same catalog label the search matches on,
  // so the file name is just the label. Not every catalog item has one; a
  // missing image is hidden per-row (see onerror in renderSuggestions).
  var ITEM_ICON_BASE = "icons/items/";

  var overlay = document.getElementById("itemModalOverlay");
  var modalTitle = document.getElementById("itemModalTitle");
  var modalSummary = document.getElementById("itemModalSummary");
  var modalList = document.getElementById("itemModalList");
  var modalClose = document.getElementById("itemModalClose");
  var modalHighlightToggle = document.getElementById("itemModalHighlightToggle");

  var banner = document.getElementById("activeFilterBanner");
  var bannerLabel = document.getElementById("activeFilterLabel");
  var bannerDetails = document.getElementById("activeFilterDetails");
  var bannerClear = document.getElementById("activeFilterClear");

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

  var catalog = []; // [{label, itemPath}] -- the full searchable item list (see FindItem.build).
  var catalogByLabel = {}; // label -> itemPath, for an exact-match Enter on a fully typed name.
  var currentSuggestions = []; // The subset currently shown in the dropdown.
  var activeIndex = -1; // Highlighted suggestion row (keyboard/hover), -1 = none.
  var savedVisibility = null; // bucket.key -> visible, captured right before highlighting so "show all layers again" restores exactly what was on.
  var highlighting = false;
  var lastResult = null; // The currently-open modal's search result, if it's a searchable one (null for the Depot view).

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
    modalHighlightToggle.textContent = "Show only these on map";
    banner.style.display = "none";
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
    modalHighlightToggle.textContent = "Show all layers again";
    MapApp.layer.requestRedraw();
    // Intentionally does NOT re-fit the view to the results -- the current
    // zoom/pan is kept so the highlight is applied in place, wherever the
    // user was already looking.
  }

  // ---- Modal dialog (shared by search results and the Depot view) --------

  function openModal(title) {
    modalTitle.textContent = title;
    banner.style.display = "none"; // The modal and its map-view banner are never shown together.
    overlay.style.display = "flex";
  }

  // Closing the modal (X / backdrop / Escape) must NOT revert an active
  // find-item filter -- that's the banner's job. If a filter is live, closing
  // the modal just returns to the map view (banner reappears); otherwise
  // there's nothing to keep, so lastResult is dropped.
  function closeModal() {
    overlay.style.display = "none";
    if (highlighting) {
      banner.style.display = "flex";
    } else {
      lastResult = null;
    }
  }

  modalClose.addEventListener("click", closeModal);
  overlay.addEventListener("click", function(e) {
    if (e.target === overlay) {
      closeModal(); // Click on the backdrop, not the dialog itself.
    }
  });
  document.addEventListener("keydown", function(e) {
    if (e.key !== "Escape") {
      return;
    }
    if (overlay.style.display !== "none") {
      closeModal();
    } else if (highlighting) {
      clearHighlight(); // No modal open, but a filter is live on the map -- Esc reverts it.
    }
  });

  // Fills the modal's title/summary/list from a search result WITHOUT
  // touching the highlight -- so it's reusable both for a fresh search
  // (showResult) and for reopening the list from the banner's "Details"
  // button while a filter is still active.
  function fillModalFromResult(result) {
    modalTitle.textContent = result.label;
    var unit = result.isFluid ? " m³" : "";
    if (result.locations.length === 0) {
      modalSummary.textContent = "Not found in any inventory.";
      modalList.innerHTML = "";
      modalHighlightToggle.style.display = "none";
      return;
    }
    modalSummary.textContent = result.totalCount.toLocaleString() + unit +
      " across " + result.locations.length.toLocaleString() + " location" + (result.locations.length === 1 ? "" : "s") + ".";
    renderLocationList(modalList, groupLocationsForDisplay(result.locations).map(function(row) {
      return [row.label, row.count.toLocaleString() + unit];
    }));
    var hasPlottable = result.locations.some(function(loc) { return loc.position; });
    modalHighlightToggle.style.display = hasPlottable ? "block" : "none";
  }

  function showResult(result) {
    lastResult = result;
    clearHighlight();
    openModal(result.label);
    fillModalFromResult(result);
    modalHighlightToggle.textContent = "Show only these on map";
  }

  function runSearchFor(itemPath, label) {
    var filename = window.MapApp.currentFile;
    if (!filename) {
      return;
    }
    openModal(label);
    modalSummary.textContent = "Searching...";
    modalList.innerHTML = "";
    modalHighlightToggle.style.display = "none";
    fetch("/api/find-item?file=" + encodeURIComponent(filename) + "&item=" + encodeURIComponent(itemPath))
      .then(function(response) { return response.json(); })
      .then(function(result) {
        if (result.error) {
          modalSummary.textContent = result.error;
          return;
        }
        showResult(result);
      })
      .catch(function(error) {
        modalSummary.textContent = "Search failed: " + error;
      });
  }

  // ---- Spotlight-style suggestions dropdown -------------------------------

  function itemIconUrl(label) {
    return ITEM_ICON_BASE + encodeURIComponent(label + ".png");
  }

  function hideSuggestions() {
    suggestionsEl.style.display = "none";
    currentSuggestions = [];
    activeIndex = -1;
  }

  function setActive(index) {
    activeIndex = index;
    var rows = suggestionsEl.children;
    for (var i = 0; i < rows.length; i++) {
      var isActive = i === index;
      rows[i].classList.toggle("active", isActive);
      if (isActive && rows[i].scrollIntoView) {
        rows[i].scrollIntoView({ block: "nearest" });
      }
    }
  }

  function selectSuggestion(entry) {
    searchInput.value = entry.label;
    hideSuggestions();
    runSearchFor(entry.itemPath, entry.label);
  }

  // Substring match, case-insensitive, with prefix matches sorted first so
  // typing "iron" surfaces "Iron Plate"/"Iron Rod" ahead of "Reinforced Iron
  // Plate". The catalog is already alphabetical, so a stable sort keeps ties
  // in that order.
  function renderSuggestions(query) {
    var q = query.trim().toLowerCase();
    if (!q) {
      hideSuggestions();
      return;
    }
    var matches = catalog.filter(function(entry) {
      return entry.label.toLowerCase().indexOf(q) !== -1;
    });
    matches.sort(function(a, b) {
      var aPrefix = a.label.toLowerCase().indexOf(q) === 0 ? 0 : 1;
      var bPrefix = b.label.toLowerCase().indexOf(q) === 0 ? 0 : 1;
      return aPrefix - bPrefix;
    });
    currentSuggestions = matches.slice(0, MAX_SUGGESTIONS);

    suggestionsEl.innerHTML = "";
    if (currentSuggestions.length === 0) {
      suggestionsEl.appendChild(el("div", "searchSuggestionEmpty", "No matching item."));
      suggestionsEl.style.display = "block";
      activeIndex = -1;
      return;
    }
    currentSuggestions.forEach(function(entry, index) {
      var row = el("div", "searchSuggestionRow");
      var img = document.createElement("img");
      img.className = "searchSuggestionIcon";
      img.src = itemIconUrl(entry.label);
      img.alt = "";
      img.addEventListener("error", function() { img.style.visibility = "hidden"; });
      row.appendChild(img);
      row.appendChild(el("span", "searchSuggestionLabel", entry.label));
      // mousedown (not click) + preventDefault so selecting doesn't first
      // blur the input and let the document-level outside-click handler race
      // in and close the dropdown before the pick registers.
      row.addEventListener("mousedown", function(e) {
        e.preventDefault();
        selectSuggestion(entry);
      });
      row.addEventListener("mouseenter", function() { setActive(index); });
      suggestionsEl.appendChild(row);
    });
    activeIndex = 0;
    setActive(0);
    suggestionsEl.style.display = "block";
  }

  searchInput.addEventListener("input", function() {
    renderSuggestions(searchInput.value);
  });

  searchInput.addEventListener("focus", function() {
    if (searchInput.value.trim()) {
      renderSuggestions(searchInput.value);
    }
  });

  searchInput.addEventListener("keydown", function(e) {
    if (e.key === "ArrowDown" && currentSuggestions.length) {
      e.preventDefault();
      setActive((activeIndex + 1) % currentSuggestions.length);
    } else if (e.key === "ArrowUp" && currentSuggestions.length) {
      e.preventDefault();
      setActive((activeIndex - 1 + currentSuggestions.length) % currentSuggestions.length);
    } else if (e.key === "Enter") {
      if (activeIndex >= 0 && currentSuggestions[activeIndex]) {
        selectSuggestion(currentSuggestions[activeIndex]);
      } else if (catalogByLabel.hasOwnProperty(searchInput.value.trim())) {
        var label = searchInput.value.trim();
        hideSuggestions();
        runSearchFor(catalogByLabel[label], label);
      }
    } else if (e.key === "Escape") {
      hideSuggestions();
    }
  });

  // Clicking anywhere outside the search box dismisses the dropdown.
  document.addEventListener("mousedown", function(e) {
    if (!searchBox.contains(e.target)) {
      hideSuggestions();
    }
  });

  // "Show only these on map" -- apply the highlight, then get the modal out
  // of the way so the highlighted results are actually visible/navigable.
  // The floating banner (see below) takes over as the revert/reopen control.
  // (If the modal was reopened via the banner's "Details" while already
  // filtering, this button reads "Show all layers again" and reverts.)
  modalHighlightToggle.addEventListener("click", function() {
    if (highlighting) {
      clearHighlight();
    } else if (lastResult) {
      showHighlight(lastResult);
      modalHighlightToggle.textContent = "Show all layers again";
      overlay.style.display = "none";
      bannerLabel.textContent = "Showing only: " + lastResult.label;
      banner.style.display = "flex";
    }
  });

  // Banner "Show all" reverts the filter; "Details" reopens the full list
  // (keeping the filter active -- closing that list returns to the banner).
  bannerClear.addEventListener("click", clearHighlight);
  bannerDetails.addEventListener("click", function() {
    if (!lastResult) {
      return;
    }
    fillModalFromResult(lastResult);
    modalHighlightToggle.textContent = "Show all layers again";
    banner.style.display = "none";
    overlay.style.display = "flex";
  });

  depotButton.addEventListener("click", function() {
    var depotItems = window.MapApp.currentDepotItems || [];
    openModal("Dimensional Depot");
    modalHighlightToggle.style.display = "none";
    if (depotItems.length === 0) {
      modalSummary.textContent = "Empty (or no save loaded yet).";
      modalList.innerHTML = "";
      return;
    }
    var total = depotItems.reduce(function(s, entry) { return s + entry.count; }, 0);
    modalSummary.textContent = total.toLocaleString() + " items across " + depotItems.length + " types.";
    renderLocationList(modalList, depotItems.map(function(entry) {
      return [entry.label, entry.count.toLocaleString()];
    }));
  });

  // Rebuilds the (save-independent) item catalog and resets any in-progress
  // search/highlight -- called alongside Filters.build/Altitude.build on
  // every load (see data.js), since a reload's fresh buckets shouldn't be
  // silently hidden by a highlight the user set up against the old ones.
  FindItem.build = function(payload) {
    // Hard reset -- Filters.build already cleared/rebuilt every bucket (so the
    // old highlight bucket is gone and savedVisibility is stale), so just drop
    // all find-item state rather than trying to "revert" against buckets that
    // no longer exist.
    overlay.style.display = "none";
    banner.style.display = "none";
    highlighting = false;
    savedVisibility = null;
    lastResult = null;
    modalHighlightToggle.textContent = "Show only these on map";
    searchInput.value = "";
    hideSuggestions();

    catalog = (payload.itemCatalog || []).slice();
    catalogByLabel = {};
    catalog.forEach(function(entry) {
      catalogByLabel[entry.label] = entry.itemPath;
    });

    window.MapApp.currentDepotItems = payload.dimensionalDepot || [];
  };
})();
