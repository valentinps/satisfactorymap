// Drives the top bar's search (items, buildings AND vehicles) and the
// Dimensional Depot icon button. Item results and Depot contents show in the
// shared #itemModalOverlay dialog (see below); building results get their own
// richer #buildingModalOverlay (count/recipes/power/inventory -- see
// sav_map_data.collectBuildingInfo, queried via /api/building-info), which
// vehicle results reuse with fleet stats/consist-mix/fuel/cargo instead (see
// collectVehicleInfo/collectTrainInfo, queried via /api/vehicle-info --
// trains are searchable only as one whole-consist "Train" entry, matching
// the sidebar, never as individual locomotives/freight cars).
//
// Item search looks up one item across every inventory in the save (see
// sav_map_data.findItemLocations / collectItemLocationIndex, queried via
// /api/find-item) and lists everything that holds it, grouped by building
// type (one expandable summed row per type, most first -- see
// buildDisplayGroups/renderGroupedLocations), with an optional toggle to
// highlight (and hide everything else) just those buildings on the map.
// Hovering an individual location row -- or a highlighted pin on the map --
// shows the same full instance tooltip as hovering the machine normally,
// plus a highlighted "searched item quantity here" line. The Depot button shows
// sav_map_data.collectDimensionalDepotContents's contents the same way,
// minus the highlight toggle (the Depot is a single global inventory with no
// map position of its own).
//
// Building search instead lists every placed building type (from
// filters.js's buildingSearchEntries, one entry per sidebar row -- so
// same-shape/different-material skins are already merged together the same
// way the sidebar shows them). Each suggestion row carries its own show/hide
// toggle wired directly to that row's real sidebar checkbox, so toggling
// visibility from a suggestion and from the sidebar are the exact same
// action, never two states to keep in sync. Selecting a building (click or
// Enter) opens the building modal with its own "Show only this on map"
// isolate toggle, sharing the item search's highlight/banner machinery below
// (only one highlight -- item or building -- is ever active at a time).

var FindItem = {};

(function() {
  "use strict";

  var searchInput = document.getElementById("mainSearchInput");
  var searchBox = document.getElementById("searchBox");
  var suggestionsEl = document.getElementById("searchSuggestions");
  var depotButton = document.getElementById("depotIconButton");

  var MAX_SUGGESTIONS_PER_KIND = 5;
  // Item/building icons are stored under ClassName (see static/map/icons/items/,
  // e.g. "Desc_IronPlate_C.png", and icons/buildings/, e.g. "Build_WorkBench_C.png")
  // -- extracted straight from the game's own per-class icon (game_data/generated/
  // items.json|buildings.json's "icon" field, see game_data/copy_icons.py) rather
  // than a hand-picked file per readable label, so the lookup is always exact,
  // never a guess. Not every catalog entry has one (a couple of buildings, see
  // SCHEMA.md); a missing image just falls back to a generic glyph (see
  // attachIconWithFallback).
  var ITEM_ICON_BASE = "icons/items/";
  var BUILDING_ICON_BASE = "icons/buildings/";

  // typePath as carried on building catalog/location entries is the save's
  // full asset path (e.g. ".../Build_Foo.Build_Foo_C") -- the icon files are
  // keyed by just the trailing short ClassName, same convention used
  // everywhere else in the map (see sav_map_data._shortClassName).
  function shortClassName(path) {
    var pos = path.lastIndexOf(".");
    return pos === -1 ? path : path.slice(pos + 1);
  }

  var overlay = document.getElementById("itemModalOverlay");
  var modalIcon = document.getElementById("itemModalIcon");
  var modalTitle = document.getElementById("itemModalTitle");
  var modalSummary = document.getElementById("itemModalSummary");
  var modalList = document.getElementById("itemModalList");
  var modalClose = document.getElementById("itemModalClose");
  var modalHighlightToggle = document.getElementById("itemModalHighlightToggle");

  var buildingOverlay = document.getElementById("buildingModalOverlay");
  var buildingModalIcon = document.getElementById("buildingModalIcon");
  var buildingModalTitle = document.getElementById("buildingModalTitle");
  var buildingModalCategory = document.getElementById("buildingModalCategory");
  var buildingModalClose = document.getElementById("buildingModalClose");
  var buildingModalSummary = document.getElementById("buildingModalSummary");
  var buildingModalStats = document.getElementById("buildingModalStats");
  var buildingModalRecipes = document.getElementById("buildingModalRecipes");
  var buildingModalInventoryLabel = document.getElementById("buildingModalInventoryLabel");
  var buildingModalInventory = document.getElementById("buildingModalInventory");
  var buildingModalHighlightToggle = document.getElementById("buildingModalHighlightToggle");
  var buildingModalVisibilityToggle = document.getElementById("buildingModalVisibilityToggle");

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

  // Suggestion-row show/hide toggle glyphs (see makeVisibilityToggle) --
  // plain inline markup (not a data-URI <img> like HIGHLIGHT_ICON_URL above)
  // since this is a real <button> whose color CSS drives the stroke via
  // currentColor, letting :hover/.isShown recolor it for free.
  var EYE_OPEN_SVG =
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="16" height="16">' +
    '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>' +
    '<circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="1.8"/>' +
    '</svg>';
  var EYE_OFF_SVG =
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="16" height="16">' +
    '<path d="M2 12s3.6-7 10-7c1.5 0 2.9.3 4.1.8M22 12s-1.2 2.3-3.4 4.2M9.9 9.9a3 3 0 0 0 4.2 4.2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>' +
    '<line x1="3" y1="3" x2="21" y2="21" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>' +
    '</svg>';

  // Fallback glyphs for when neither an exact nor a fuzzy icon match exists
  // (see attachIconWithFallback) -- a plain gem for items, a plain building
  // silhouette for buildings, so the two kinds still read as distinct even
  // when generic. Hand-drawn shapes, not game assets, so there's always
  // something sane to fall back to no matter how the real icon set changes.
  var DEFAULT_ITEM_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="26" height="26">' +
    '<path d="M12 3 L20 9 L17 20 L7 20 L4 9 Z" fill="none" stroke="#7a8190" stroke-width="1.5" stroke-linejoin="round"/>' +
    '<path d="M4 9 L20 9 M8.5 9 L12 3 L15.5 9 M8.5 9 L7 20 M15.5 9 L17 20" fill="none" stroke="#7a8190" stroke-width="1" stroke-linejoin="round"/>' +
    '</svg>'
  );
  var DEFAULT_BUILDING_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="26" height="26">' +
    '<path d="M4 10 L12 4 L20 10 V20 H4 Z" fill="none" stroke="#7a8190" stroke-width="1.5" stroke-linejoin="round"/>' +
    '<rect x="10" y="14" width="4" height="6" fill="none" stroke="#7a8190" stroke-width="1.2"/>' +
    '</svg>'
  );

  // Same crate glyph the top bar's #depotIconButton uses -- shown as the
  // modal header icon for the Dimensional Depot view (and its row inside an
  // item's location list), which has no item/building icon of its own.
  var DEPOT_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="26" height="26">' +
    '<path d="M12 3 L20 8 L20 16 L12 21 L4 16 L4 8 Z" fill="none" stroke="#8ab4f8" stroke-width="1.6" stroke-linejoin="round"/>' +
    '<path d="M12 8 V16 M12 8 L8 6 M12 8 L16 6" stroke="#8ab4f8" stroke-width="1.4" fill="none" stroke-linecap="round"/>' +
    '</svg>'
  );

  // A tiny, hand-curated set of classes with no real per-class icon of their
  // own in Docs.json (see SCHEMA.md's buildings.json section) but a visually
  // close sibling that does -- redirected to that sibling's real icon rather
  // than dropping straight to the generic glyph. Deliberately short and
  // explicit, not a fuzzy/generic lookup: every entry is a specific, verified
  // "this class borrows that class's icon" fact, not a guess.
  var BUILDING_ICON_REDIRECTS = {
    // "Pillar Top" (the metal pillar family's capping piece) has no icon of
    // its own -- borrows the middle segment's, the closest visual match.
    Build_PillarTop_C: "Build_PillarMiddle_C",
  };

  // Sets img's icon from the item's/building's own ClassName (redirected
  // through BUILDING_ICON_REDIRECTS first, if that class is in it), falling
  // back to a generic glyph if there's still no icon to show -- shared by
  // every place an item/building icon shows up (suggestion rows, the
  // building modal header). Never leaves img visibly broken: the default
  // glyph is a data URI, so it can't itself 404.
  function attachIconWithFallback(img, kind, classNameOrPath) {
    var iconBase = kind === "building" ? BUILDING_ICON_BASE : ITEM_ICON_BASE;
    var defaultIconUrl = kind === "building" ? DEFAULT_BUILDING_ICON_URL : DEFAULT_ITEM_ICON_URL;
    img.style.visibility = "visible";
    var className = classNameOrPath && (kind === "building" ? shortClassName(classNameOrPath) : classNameOrPath);
    if (kind === "building" && className && BUILDING_ICON_REDIRECTS.hasOwnProperty(className)) {
      className = BUILDING_ICON_REDIRECTS[className];
    }
    img.onerror = function() {
      img.onerror = null; // The default glyph is a data URI -- it cannot itself fail.
      img.src = defaultIconUrl;
    };
    if (className) {
      img.src = iconBase + encodeURIComponent(className) + ".png";
    } else {
      img.onerror(); // No class-keyed icon to try at all -- go straight to the default.
    }
  }

  var catalog = []; // [{kind:"item", label, itemPath}, {kind:"building", label, typePaths, category, subcategory, row}, {kind:"vehicle", label, typePaths, isTrain, iconUrl, row}, ...]
  var itemCatalogByLabel = {}; // label -> itemPath, for an exact-match Enter on a fully typed item name.
  var buildingCatalogByLabel = {}; // label -> building catalog entry, same for a fully typed building name.
  var vehicleCatalogByLabel = {}; // label -> vehicle catalog entry, same for a fully typed vehicle name.
  var currentSuggestions = []; // The subset currently shown in the dropdown (flat, in display order, kinds mixed).
  var currentRowElements = []; // Row DOM elements parallel to currentSuggestions (group-label divs aren't part of this).
  var activeIndex = -1; // Highlighted suggestion row (keyboard/hover), -1 = none.
  var savedVisibility = null; // bucket.key -> visible, captured right before highlighting so "show all layers again" restores exactly what was on.
  var highlighting = false;
  var highlightedBuildingEntry = null; // Set by showBuildingHighlight, so clearHighlight can re-sync its checkbox.
  var lastKind = null; // "item" | "building" -- which of the two below is the live one.
  var lastResult = null; // The currently-open item modal's search result, if it's a searchable one (null for the Depot view).
  var lastBuilding = null; // {entry, info} for the currently-open building modal.

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // rows: [label, countText, iconKind?, classNameOrPath?] -- iconKind
  // ("item"/"building") prepends the matching icon (see attachIconWithFallback),
  // looked up by classNameOrPath (a short item ClassName, or a building's full
  // typePath). Omitted iconKind keeps the old plain text row.
  function renderLocationList(container, rows) {
    container.innerHTML = "";
    rows.forEach(function(pair) {
      var row = el("div", "itemLocationRow");
      if (pair[2]) {
        var img = document.createElement("img");
        img.className = "itemLocationIcon";
        img.alt = "";
        // Depot/location lists can run to hundreds of rows -- don't fetch/
        // decode offscreen icons up front (same reasoning as progression.js).
        img.loading = "lazy";
        img.decoding = "async";
        if (pair[0] === "Dimensional Depot") {
          img.src = DEPOT_ICON_URL; // Neither an item nor a building -- no catalog icon to look up.
        } else {
          attachIconWithFallback(img, pair[2], pair[3]);
        }
        row.appendChild(img);
      }
      row.appendChild(el("span", "itemLocationLabel", pair[0]));
      row.appendChild(el("span", "itemLocationCount", pair[1]));
      container.appendChild(row);
    });
  }

  // ---- Grouped item-location list (the item search modal's body) ----------
  //
  // One group per location label -- i.e. per building type ("Storage
  // Container", "Smelter", ...), and likewise per pickup kind ("Blue Power
  // Slug", "Dropped on the ground") and the Dimensional Depot. A save can
  // easily hold thousands of machines all containing the searched item;
  // one row per machine made the list unreadable AND slow (thousands of DOM
  // rows + icons built up front), so the list shows one summed row per type
  // and expands to the individual locations on demand -- and even then only
  // GROUP_CHUNK_SIZE children at a time, so expanding a 5000-machine group
  // never builds 5000 rows in one go.
  var GROUP_CHUNK_SIZE = 150;

  var CHEVRON_SVG =
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="14" height="14">' +
    '<polyline points="9 6 15 12 9 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>' +
    '</svg>';

  function buildDisplayGroups(locations, itemPath) {
    var groupsByLabel = {};
    var groups = [];
    locations.forEach(function(loc) {
      var group = groupsByLabel[loc.label];
      if (!group) {
        group = groupsByLabel[loc.label] = { label: loc.label, iconKind: "item", iconClassName: itemPath, totalCount: 0, locations: [] };
        groups.push(group);
      }
      // A group's icon: the building's own icon when this is a real placed
      // building; the searched item's icon otherwise -- uncollected pickups
      // and the Dimensional Depot aren't buildings (typePath null, see
      // findItemLocations), and "Dropped on the ground" stacks are the item
      // itself lying loose, even when a live pickup actor carries them.
      // Grouping purely by label also merges live drops (typePath set) and
      // catalog-only drops (typePath null) into the one "Dropped on the
      // ground" group instead of two identically-named rows.
      if (loc.typePath && group.iconKind !== "building" && loc.label !== "Dropped on the ground") {
        group.iconKind = "building";
        group.iconClassName = loc.typePath;
      }
      group.totalCount += loc.count;
      group.locations.push(loc);
    });
    // Each group's own location list arrives already sorted by count
    // descending (findItemLocations sorts server-side); only the groups
    // themselves need ordering here.
    groups.sort(function(a, b) { return b.totalCount - a.totalCount; });
    return groups;
  }

  // Hovering an individual location row shows the exact same full tooltip
  // as hovering that machine's pin on the map -- rich /api/instance detail
  // for real buildings, plain position info for static pickups (no live
  // actor to describe) -- with the searched item's quantity as the
  // highlighted callout line (see tooltip.js's showFloating/renderSpec).
  // The Dimensional Depot row has no position at all -- nothing to show.
  function attachLocationHover(row, loc, unit, itemLabel) {
    if (!loc.position) {
      return;
    }
    function show(e) {
      Tooltip.showFloating(e.clientX, e.clientY, {
        key: "find-row:" + loc.instanceName,
        title: loc.label,
        staticRows: [],
        z: loc.position[2],
        worldPosition: loc.worldPosition,
        extraRows: [[itemLabel + " here", loc.count.toLocaleString() + unit]],
        instanceName: loc.typePath ? loc.instanceName : null,
      });
    }
    row.addEventListener("mouseenter", show);
    row.addEventListener("mousemove", show); // Same key -- renderSpec just repositions, no re-render/refetch.
    row.addEventListener("mouseleave", function() { Tooltip.hide(); });
  }

  // In-game map coordinates (the same /100-scaled values the tooltip's copy
  // button produces) plus altitude -- every machine in an expanded group is
  // the same building type by construction, so its position is the only
  // thing that tells the rows apart.
  function locationChildLabel(loc) {
    if (!loc.worldPosition) {
      return loc.label;
    }
    var text = Math.round(loc.worldPosition[0] / 100).toLocaleString() + ", " + Math.round(loc.worldPosition[1] / 100).toLocaleString();
    if (loc.position) {
      text += "  ·  " + Math.round(loc.position[2]) + " m";
    }
    return text;
  }

  function groupIcon(group) {
    var img = document.createElement("img");
    img.className = "itemLocationIcon";
    img.alt = "";
    img.loading = "lazy";
    img.decoding = "async";
    if (group.label === "Dimensional Depot") {
      img.src = DEPOT_ICON_URL; // Neither an item nor a building -- no catalog icon to look up.
    } else {
      attachIconWithFallback(img, group.iconKind, group.iconClassName);
    }
    return img;
  }

  function childLocationRow(loc, unit, itemLabel) {
    var row = el("div", "itemLocationRow itemLocationChildRow");
    row.appendChild(el("span", "itemLocationLabel", locationChildLabel(loc)));
    row.appendChild(el("span", "itemLocationCount", loc.count.toLocaleString() + unit));
    attachLocationHover(row, loc, unit, itemLabel);
    return row;
  }

  function renderGroupedLocations(container, groups, unit, itemLabel) {
    container.innerHTML = "";
    groups.forEach(function(group) {
      if (group.locations.length === 1) {
        var row = el("div", "itemLocationRow");
        row.appendChild(groupIcon(group));
        row.appendChild(el("span", "itemLocationLabel", group.label));
        row.appendChild(el("span", "itemLocationCount", group.totalCount.toLocaleString() + unit));
        attachLocationHover(row, group.locations[0], unit, itemLabel);
        container.appendChild(row);
        return;
      }

      var header = el("div", "itemLocationRow itemLocationGroupHeader");
      var chevron = el("span", "itemLocationChevron");
      chevron.innerHTML = CHEVRON_SVG;
      header.appendChild(chevron);
      header.appendChild(groupIcon(group));
      header.appendChild(el("span", "itemLocationLabel", group.label));
      header.appendChild(el("span", "itemLocationGroupBadge", "× " + group.locations.length.toLocaleString()));
      header.appendChild(el("span", "itemLocationCount", group.totalCount.toLocaleString() + unit));
      container.appendChild(header);

      var childrenWrap = el("div", "itemLocationChildren");
      childrenWrap.style.display = "none";
      container.appendChild(childrenWrap);

      // Children are built lazily on first expand, GROUP_CHUNK_SIZE at a
      // time -- a collapsed group costs two DOM nodes no matter whether it
      // holds 2 machines or 5000.
      var rendered = 0;
      var showMore = null;
      function renderChunk() {
        if (showMore) {
          childrenWrap.removeChild(showMore);
          showMore = null;
        }
        var end = Math.min(rendered + GROUP_CHUNK_SIZE, group.locations.length);
        for (; rendered < end; rendered++) {
          childrenWrap.appendChild(childLocationRow(group.locations[rendered], unit, itemLabel));
        }
        var remaining = group.locations.length - rendered;
        if (remaining > 0) {
          showMore = el("button", "itemLocationShowMore",
            "Show " + Math.min(GROUP_CHUNK_SIZE, remaining).toLocaleString() + " more (" + remaining.toLocaleString() + " remaining)");
          showMore.type = "button";
          showMore.addEventListener("click", renderChunk);
          childrenWrap.appendChild(showMore);
        }
      }

      header.addEventListener("click", function() {
        var expand = childrenWrap.style.display === "none";
        if (expand && rendered === 0) {
          renderChunk();
        }
        childrenWrap.style.display = expand ? "block" : "none";
        header.classList.toggle("expanded", expand);
      });
    });
  }

  // Removes the temporary highlight bucket (if any -- only the item-search
  // kind creates one, see showHighlight) and restores whatever every other
  // bucket's visibility was right before highlighting started -- matched by
  // bucket.key, which stays valid even across a reload in between (see
  // filters.js's savedVisibility comment: keys are stable identifiers for a
  // *kind* of thing, not tied to one specific save's data). Shared by both
  // highlight kinds (item search's synthetic bucket, building search's
  // isolate-in-place -- see showBuildingHighlight) since only one is ever
  // active at a time.
  // Extra synthetic buckets created alongside the main highlight: the item
  // search's per-type building boxes and the building search's lens pins
  // (see showHighlight/showBuildingHighlight). Tracked by key so
  // clearHighlight can drop them all through the layer's own removal.
  var extraHighlightKeys = [];
  function removeExtraHighlightBuckets() {
    extraHighlightKeys.forEach(function(key) { MapApp.layer.removeBucketByKey(key); });
    extraHighlightKeys = [];
  }

  function clearHighlight() {
    if (!highlighting) {
      return;
    }
    highlighting = false;
    removeExtraHighlightBuckets();
    if (lastKind === "item") {
      // Must go through the layer's own removal (not a bare `buckets =
      // buckets.filter(...)` reassignment) so the _sortedBuckets draw cache
      // is invalidated too -- see map.js's removeBucketByKey; the old
      // reassignment left the highlight's lens pins ghost-drawn on the map.
      MapApp.layer.removeBucketByKey(HIGHLIGHT_BUCKET_KEY);
    }
    if (savedVisibility) {
      MapApp.layer.buckets.forEach(function(b) {
        if (savedVisibility.hasOwnProperty(b.key)) {
          b.visible = savedVisibility[b.key];
        }
      });
      savedVisibility = null;
    }
    // showBuildingHighlight forced this row's checkbox to "on" without a real
    // change event (see there) -- put it back in sync with the bucket state
    // just restored above, so the sidebar doesn't show "visible" for a
    // building that's actually gone back to hidden.
    if (highlightedBuildingEntry && highlightedBuildingEntry.row.checkbox) {
      highlightedBuildingEntry.row.checkbox.checked = highlightedBuildingEntry.row.buckets[0].visible;
    }
    highlightedBuildingEntry = null;
    refreshBuildingModalEye(); // The checkbox resync above may have flipped the modal's eye state.
    modalHighlightToggle.textContent = "Show only these on map";
    buildingModalHighlightToggle.textContent = "Show only this on map";
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
      // Real buildings get the complete /api/instance detail tooltip
      // (recipe, inventory, power, ... -- exactly what hovering the same
      // building outside of a search shows) via tooltipServerId; static
      // pickups (uncollected slugs, catalog-only drops -- no live actor in
      // the save to describe, see sav_map_data._collectStaticItemLocations)
      // keep the plain static tooltip. Both carry the searched item's
      // quantity as the highlighted callout line (tooltipExtraRows), shown
      // even while the rich detail is still loading.
      tooltipKind: "static",
      tooltipInfo: function(index) {
        var loc = byInstance[ids[index]];
        return {
          title: loc.label,
          rows: [],
          position: loc.worldPosition,
        };
      },
      tooltipServerId: function(index) {
        var loc = byInstance[ids[index]];
        return loc.typePath ? loc.instanceName : null;
      },
      tooltipExtraRows: function(index) {
        var loc = byInstance[ids[index]];
        var unit = result.isFluid ? " m³" : "";
        return [[result.label + " here", loc.count.toLocaleString() + unit]];
      },
      iconUrl: HIGHLIGHT_ICON_URL,
      iconOpacity: 1,
    });

    // The lens pins alone were hard to relate back to actual structures at a
    // distance -- so every located instance that lives in a real building
    // bucket also gets its bounding box drawn (same silhouette the normal
    // map view shows), in the highlight color, beneath the pins (icons paint
    // in a later pass, so the pin keeps hover priority). One synthetic box
    // bucket per source bucket, since the footprint is a per-type property.
    var boxSpecs = [];
    MapApp.layer.buckets.forEach(function(src) {
      if (src.renderType !== "rect" || !src.ids || !src.footprintPixels) {
        return;
      }
      var boxPoints = [], boxIds = [];
      for (var i = 0; i < src.ids.length; i++) {
        if (byInstance.hasOwnProperty(src.ids[i])) {
          boxPoints.push(src.points[i * 4], src.points[i * 4 + 1],
                         src.points[i * 4 + 2], src.points[i * 4 + 3]);
          boxIds.push(src.ids[i]);
        }
      }
      if (boxPoints.length > 0) {
        boxSpecs.push({ src: src, points: boxPoints, ids: boxIds });
      }
    });
    boxSpecs.forEach(function(spec) {
      var key = HIGHLIGHT_BUCKET_KEY + ":box:" + spec.src.key;
      var bucket = MapApp.layer.addBucket({
        key: key,
        label: spec.src.label,
        color: HIGHLIGHT_COLOR,
        visible: true,
        renderType: "rect",
        pointStride: 4,
        points: new Float32Array(spec.points),
        ids: spec.ids,
        tooltipKind: "server",
        footprintPixels: spec.src.footprintPixels,
        maxFootprintRadius: spec.src.maxFootprintRadius,
      });
      // The pins are the countable search results; these boxes are the same
      // objects drawn a second way -- selection must not count them twice.
      bucket.excludeFromSelection = true;
      extraHighlightKeys.push(key);
    });

    highlighting = true;
    modalHighlightToggle.textContent = "Show all layers again";
    MapApp.layer.requestRedraw();
    // Intentionally does NOT re-fit the view to the results -- the current
    // zoom/pan is kept so the highlight is applied in place, wherever the
    // user was already looking.
  }

  // Building search's equivalent of showHighlight -- but a building's own
  // buckets already exist and are real, permanent sidebar buckets (see
  // filters.js's buildingSearchEntries), so this just hides every other
  // bucket and forces this one's own bucket(s) visible in place, rather than
  // building a synthetic one-off bucket the way item search does. Also
  // forces the building's own sidebar checkbox to reflect "shown" so the two
  // controls don't end up disagreeing once the isolate is cleared.
  // Above how many instances the building isolate skips its lens pins: past
  // this the pins are pure noise (imagine one per foundation), and the boxes
  // alone already carry the "where" at that density.
  var BUILDING_PIN_LIMIT = 1500;

  function showBuildingHighlight(entry) {
    savedVisibility = {};
    MapApp.layer.buckets.forEach(function(b) {
      savedVisibility[b.key] = b.visible;
      b.visible = false;
    });
    entry.row.buckets.forEach(function(b) { b.visible = true; });
    // The isolated buildings' boxes are hard to spot at map-wide zoom, so
    // each instance also gets the same lens pin the item search uses --
    // sharing the box buckets' [x, y, yaw, z] stride-4 points (icon
    // consumers read x/y and z at stride-1, same trick as the vehicles' pin
    // buckets), and their ids so hovering a pin serves the building's own
    // rich tooltip. Icons paint above the boxes, so pins keep hover priority.
    var pinPoints = [], pinIds = [];
    entry.row.buckets.forEach(function(b) {
      if (!b.ids) {
        return;
      }
      for (var i = 0; i < b.ids.length; i++) {
        pinPoints.push(b.points[i * 4], b.points[i * 4 + 1],
                       b.points[i * 4 + 2], b.points[i * 4 + 3]);
        pinIds.push(b.ids[i]);
      }
    });
    if (pinPoints.length > 0 && pinIds.length <= BUILDING_PIN_LIMIT) {
      var pinKey = HIGHLIGHT_BUCKET_KEY + ":pins";
      var pinBucket = MapApp.layer.addBucket({
        key: pinKey,
        label: entry.row.label,
        color: HIGHLIGHT_COLOR,
        visible: true,
        renderType: "icon",
        pointStride: 4,
        points: new Float32Array(pinPoints),
        ids: pinIds,
        tooltipKind: "server",
        iconUrl: HIGHLIGHT_ICON_URL,
        iconOpacity: 1,
      });
      // The visible box buckets are the real objects -- the pins are just
      // markers over them, so selection must not count them twice.
      pinBucket.excludeFromSelection = true;
      extraHighlightKeys.push(pinKey);
    }
    if (entry.row.checkbox) {
      entry.row.checkbox.checked = true;
    }
    highlightedBuildingEntry = entry;
    highlighting = true;
    buildingModalHighlightToggle.textContent = "Show all layers again";
    refreshBuildingModalEye(); // The isolate just forced this row's checkbox on.
    MapApp.layer.requestRedraw();
  }

  // ---- Item/Depot modal dialog ---------------------------------------------

  function openModal(title) {
    modalTitle.textContent = title;
    banner.style.display = "none"; // The modal and its map-view banner are never shown together.
    overlay.style.display = "flex";
  }

  // Closing the modal (X / backdrop / Escape) must NOT revert an active
  // find-item filter -- that's the banner's job. If a filter is live, closing
  // the modal just returns to the map view (banner reappears); otherwise
  // there's nothing to keep, so lastResult is dropped.
  function closeItemModal() {
    overlay.style.display = "none";
    Tooltip.hide(); // A row-hover tooltip (see attachLocationHover) shouldn't outlive the list it belongs to.
    if (highlighting) {
      banner.style.display = "flex";
    } else {
      lastResult = null;
      lastKind = null;
    }
  }

  modalClose.addEventListener("click", closeItemModal);
  overlay.addEventListener("click", function(e) {
    if (e.target === overlay) {
      closeItemModal(); // Click on the backdrop, not the dialog itself.
    }
  });
  // Wheel-scrolling the list slides a different row under the (unmoved)
  // cursor without any mouseleave/mouseenter firing -- drop the tooltip
  // rather than leave it showing a row that's no longer under the pointer;
  // the next real mousemove re-shows the right one.
  modalList.addEventListener("scroll", function() { Tooltip.hide(); }, { passive: true });

  // Fills the modal's title/summary/list from a search result WITHOUT
  // touching the highlight -- so it's reusable both for a fresh search
  // (showResult) and for reopening the list from the banner's "Details"
  // button while a filter is still active.
  function fillModalFromResult(result) {
    modalTitle.textContent = result.label;
    attachIconWithFallback(modalIcon, "item", result.itemPath);
    var unit = result.isFluid ? " m³" : "";
    if (result.locations.length === 0) {
      modalSummary.textContent = "Not found in any inventory.";
      modalList.innerHTML = "";
      modalHighlightToggle.style.display = "none";
      return;
    }
    var groups = buildDisplayGroups(result.locations, result.itemPath);
    var locationsText = result.locations.length.toLocaleString() + " location" + (result.locations.length === 1 ? "" : "s");
    if (groups.length > 1 && groups.length !== result.locations.length) {
      locationsText += " (" + groups.length + " types)";
    }
    modalSummary.textContent = result.totalCount.toLocaleString() + unit + " across " + locationsText + ".";
    renderGroupedLocations(modalList, groups, unit, result.label);
    var hasPlottable = result.locations.some(function(loc) { return loc.position; });
    modalHighlightToggle.style.display = hasPlottable ? "block" : "none";
  }

  function showResult(result) {
    lastResult = result;
    lastKind = "item";
    clearHighlight();
    openModal(result.label);
    fillModalFromResult(result);
    modalHighlightToggle.textContent = "Show only these on map";
  }

  // ---- Building info modal --------------------------------------------------

  // The building modal's header eye -- same show/hide control as the
  // building's search-suggestion row, bound to whichever entry the modal is
  // currently showing. Hidden when that entry has no live sidebar checkbox
  // (nothing to flip).
  var currentBuildingModalEntry = null;

  function refreshBuildingModalEye() {
    var entry = currentBuildingModalEntry;
    if (!entry || !entry.row.checkbox) {
      buildingModalVisibilityToggle.style.display = "none";
      return;
    }
    buildingModalVisibilityToggle.style.display = "flex";
    refreshVisibilityToggle(buildingModalVisibilityToggle, entry);
  }

  buildingModalVisibilityToggle.addEventListener("click", function() {
    var entry = currentBuildingModalEntry;
    if (entry && entry.row.checkbox) {
      entry.row.checkbox.click(); // Same path as the suggestion-row eye -- the sidebar's own change handler does the redraw.
    }
    refreshBuildingModalEye();
  });

  function openBuildingModal(entry) {
    currentBuildingModalEntry = entry;
    refreshBuildingModalEye();
    buildingModalTitle.textContent = entry.label;
    var chipColor;
    buildingModalIcon.classList.toggle("vehicleModalIcon", entry.kind === "vehicle");
    if (entry.kind === "vehicle") {
      buildingModalCategory.textContent = entry.isTrain ? "Vehicles › Railway" : "Vehicles";
      chipColor = Filters.vehicleColor();
      buildingModalIcon.onerror = function() { buildingModalIcon.onerror = null; buildingModalIcon.src = DEFAULT_BUILDING_ICON_URL; };
      buildingModalIcon.src = entry.iconUrl;
      buildingModalIcon.style.visibility = "visible";
    } else {
      buildingModalCategory.textContent = entry.subcategory ? entry.category + " › " + entry.subcategory : entry.category;
      chipColor = Filters.buildingCategoryColor(entry.category);
      attachIconWithFallback(buildingModalIcon, "building", entry.typePaths[0]);
    }
    buildingModalCategory.style.background = chipColor + "26"; // ~15% alpha tint, hex-appended (2-digit alpha).
    buildingModalCategory.style.color = chipColor;
    banner.style.display = "none";
    buildingOverlay.style.display = "flex";
  }

  function closeBuildingModal() {
    buildingOverlay.style.display = "none";
    if (highlighting) {
      banner.style.display = "flex";
    } else {
      lastBuilding = null;
      lastKind = null;
    }
  }

  buildingModalClose.addEventListener("click", closeBuildingModal);
  buildingOverlay.addEventListener("click", function(e) {
    if (e.target === buildingOverlay) {
      closeBuildingModal();
    }
  });

  document.addEventListener("keydown", function(e) {
    if (e.key !== "Escape") {
      return;
    }
    if (overlay.style.display !== "none") {
      closeItemModal();
    } else if (buildingOverlay.style.display !== "none") {
      closeBuildingModal();
    } else if (highlighting) {
      clearHighlight(); // No modal open, but a filter is live on the map -- Esc reverts it.
    }
  });

  function statTile(value, label) {
    var tile = el("div", "buildingStatTile");
    tile.appendChild(el("span", "buildingStatValue", value));
    tile.appendChild(el("span", "buildingStatLabel", label));
    return tile;
  }

  function formatMW(mw) {
    return mw.toLocaleString(undefined, { maximumFractionDigits: 1 }) + " MW";
  }

  // One horizontal-bar breakdown section (the building modal's "Recipe mix",
  // the train modal's "Consist mix") appended into the recipes area --
  // rows: [{label, count}], scaled against the largest count.
  function appendBarSection(title, rows, barColor) {
    buildingModalRecipes.appendChild(el("div", "buildingModalSectionLabel", title));
    var maxCount = rows.reduce(function(m, r) { return Math.max(m, r.count); }, 1);
    rows.forEach(function(barRow) {
      var row = el("div", "recipeBarRow");
      row.appendChild(el("span", "recipeBarLabel", barRow.label));
      var track = el("div", "recipeBarTrack");
      var fill = el("div", "recipeBarFill");
      fill.style.width = Math.max(3, (barRow.count / maxCount) * 100) + "%";
      fill.style.background = barColor;
      track.appendChild(fill);
      row.appendChild(track);
      row.appendChild(el("span", "recipeBarCount", barRow.count.toLocaleString()));
      buildingModalRecipes.appendChild(row);
    });
  }

  // The modal's bottom item list ("Combined inventory" for buildings,
  // "Combined cargo" for vehicles/trains) -- rows in
  // aggregateSelectionInventory's {label, count, isFluid, item} shape.
  function fillModalInventory(labelText, rows) {
    if (rows && rows.length > 0) {
      buildingModalInventoryLabel.textContent = labelText;
      buildingModalInventoryLabel.style.display = "block";
      renderLocationList(buildingModalInventory, rows.map(function(entryRow) {
        return [entryRow.label, entryRow.count.toLocaleString() + (entryRow.isFluid ? " m³" : ""), "item", entryRow.item];
      }));
    } else {
      buildingModalInventoryLabel.style.display = "none";
      buildingModalInventory.innerHTML = "";
    }
  }

  // Fills the building modal's stats/recipe-mix/inventory from a
  // collectBuildingInfo() result WITHOUT touching the highlight -- reusable
  // both for a fresh search (runInfoSearchFor) and for reopening from the
  // banner's "Details" button while an isolate is still active.
  function fillBuildingModalFromInfo(entry, info) {
    buildingModalSummary.textContent = info.count.toLocaleString() + " placed across the save.";

    buildingModalStats.innerHTML = "";
    buildingModalStats.appendChild(statTile(info.count.toLocaleString(), "Count"));
    if (info.powerConsumptionMW !== undefined) {
      buildingModalStats.appendChild(statTile(formatMW(info.powerConsumptionMW), "Power draw"));
    } else if (info.powerConsumptionRangeMW) {
      buildingModalStats.appendChild(statTile(
        info.powerConsumptionRangeMW[0].toLocaleString() + "–" + info.powerConsumptionRangeMW[1].toLocaleString() + " MW",
        "Power draw (varies)"));
    } else if (info.powerProductionMW !== undefined) {
      buildingModalStats.appendChild(statTile(formatMW(info.powerProductionMW), "Power output"));
    }
    if (info.recipes && info.recipes.length > 0) {
      buildingModalStats.appendChild(statTile(String(info.recipes.length), "Recipes in use"));
    }

    buildingModalRecipes.innerHTML = "";
    if (info.recipes && info.recipes.length > 0) {
      appendBarSection("Recipe mix", info.recipes, Filters.buildingCategoryColor(entry.category));
    }

    fillModalInventory("Combined inventory", info.inventory);
    buildingModalHighlightToggle.style.display = info.count > 0 ? "block" : "none";
  }

  // Vehicle counterpart of fillBuildingModalFromInfo, from a
  // collectVehicleInfo()/collectTrainInfo() result: fleet stat tiles, the
  // train consist-mix bars, a "Fuel loaded" list, and the combined cargo.
  function fillVehicleModalFromInfo(entry, info) {
    var vehicleColor = Filters.vehicleColor();
    buildingModalStats.innerHTML = "";
    buildingModalRecipes.innerHTML = "";

    if (entry.isTrain) {
      buildingModalSummary.textContent = info.count.toLocaleString() + " train" + (info.count === 1 ? "" : "s") +
        " on the rails, " + info.carCount.toLocaleString() + " cars total.";
      buildingModalStats.appendChild(statTile(info.count.toLocaleString(), "Trains"));
      buildingModalStats.appendChild(statTile(info.carCount.toLocaleString(), "Cars"));
      buildingModalStats.appendChild(statTile(info.locomotiveCount.toLocaleString(), "Locomotives"));
      buildingModalStats.appendChild(statTile(info.wagonCount.toLocaleString(), "Freight cars"));
      if (info.consistBreakdown && info.consistBreakdown.length > 0) {
        appendBarSection("Consist mix", info.consistBreakdown, vehicleColor);
      }
    } else {
      buildingModalSummary.textContent = info.count.toLocaleString() + " in the save.";
      buildingModalStats.appendChild(statTile(info.count.toLocaleString(), "Count"));
      if (info.automatedCount !== undefined) {
        buildingModalStats.appendChild(statTile(info.automatedCount.toLocaleString() + " / " + info.count.toLocaleString(), "On autopilot"));
      }
      if (info.dockedCount !== undefined) {
        buildingModalStats.appendChild(statTile(info.dockedCount.toLocaleString() + " / " + info.count.toLocaleString(), "Docked at port"));
      }
    }

    if (info.fuelInventory && info.fuelInventory.length > 0) {
      buildingModalRecipes.appendChild(el("div", "buildingModalSectionLabel", "Fuel loaded"));
      var fuelList = el("div", "itemLocationList");
      renderLocationList(fuelList, info.fuelInventory.map(function(entryRow) {
        return [entryRow.label, entryRow.count.toLocaleString() + (entryRow.isFluid ? " m³" : ""), "item", entryRow.item];
      }));
      buildingModalRecipes.appendChild(fuelList);
    }

    fillModalInventory(entry.isTrain ? "Combined cargo (all freight cars)" : "Combined cargo", info.inventory);
    buildingModalHighlightToggle.style.display = info.count > 0 ? "block" : "none";
  }

  // Blanks every modal section, fetches, then fills via the given fill
  // function -- the shared skeleton of runBuildingSearchFor/runVehicleSearchFor.
  function runInfoSearchFor(entry, infoPromise, fillFromInfo) {
    openBuildingModal(entry);
    buildingModalSummary.textContent = "Loading…";
    buildingModalStats.innerHTML = "";
    buildingModalRecipes.innerHTML = "";
    buildingModalInventoryLabel.style.display = "none";
    buildingModalInventory.innerHTML = "";
    buildingModalHighlightToggle.style.display = "none";
    infoPromise
      .then(function(info) {
        if (info.error) {
          buildingModalSummary.textContent = info.error;
          return;
        }
        lastBuilding = { entry: entry, info: info };
        lastKind = "building";
        clearHighlight();
        fillFromInfo(entry, info);
        buildingModalHighlightToggle.textContent = "Show only this on map";
      })
      .catch(function(error) {
        buildingModalSummary.textContent = "Search failed: " + error;
      });
  }

  function runBuildingSearchFor(entry) {
    if (!window.MapApp.currentFile) {
      return;
    }
    runInfoSearchFor(entry, SaveClient.buildingInfo(entry.typePaths), fillBuildingModalFromInfo);
  }

  function runVehicleSearchFor(entry) {
    if (!window.MapApp.currentFile) {
      return;
    }
    runInfoSearchFor(entry,
      entry.isTrain ? SaveClient.trainInfo() : SaveClient.vehicleInfo(entry.typePaths),
      fillVehicleModalFromInfo);
  }

  function runSearchFor(itemPath, label) {
    if (!window.MapApp.currentFile) {
      return;
    }
    openModal(label);
    attachIconWithFallback(modalIcon, "item", itemPath);
    modalSummary.textContent = "Searching...";
    modalList.innerHTML = "";
    modalHighlightToggle.style.display = "none";
    SaveClient.findItem(itemPath)
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

  function hideSuggestions() {
    suggestionsEl.style.display = "none";
    currentSuggestions = [];
    currentRowElements = [];
    activeIndex = -1;
  }

  function setActive(index) {
    activeIndex = index;
    for (var i = 0; i < currentRowElements.length; i++) {
      var isActive = i === index;
      currentRowElements[i].classList.toggle("active", isActive);
      if (isActive && currentRowElements[i].scrollIntoView) {
        currentRowElements[i].scrollIntoView({ block: "nearest" });
      }
    }
  }

  function selectSuggestion(entry) {
    searchInput.value = entry.label;
    hideSuggestions();
    if (entry.kind === "building") {
      runBuildingSearchFor(entry);
    } else if (entry.kind === "vehicle") {
      runVehicleSearchFor(entry);
    } else {
      runSearchFor(entry.itemPath, entry.label);
    }
  }

  // Repaints any show/hide eye button (suggestion row or the building
  // modal's) from the entry's current checkbox state -- glyph, blue-shown/
  // pink-hidden class, and tooltip all in one place.
  function refreshVisibilityToggle(btn, entry) {
    var isShown = !entry.row.checkbox || entry.row.checkbox.checked;
    btn.classList.toggle("isShown", isShown);
    btn.innerHTML = isShown ? EYE_OPEN_SVG : EYE_OFF_SVG;
    btn.title = isShown ? "Hide " + entry.label + " on the map" : "Show " + entry.label + " on the map";
  }

  // The show/hide eye button on a building suggestion row -- see the header
  // comment. Reads/writes entry.row.checkbox directly (the same real DOM
  // checkbox the sidebar row owns, see filters.js's appendLeafRow), so this
  // is never a second source of truth for the building's visibility.
  function makeVisibilityToggle(entry) {
    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "visibilityToggle";

    function refresh() {
      refreshVisibilityToggle(btn, entry);
    }
    refresh();

    // Stopped at mousedown (not just click) so this never reaches the row's
    // own mousedown listener below, which would otherwise also treat this as
    // "select this suggestion" and open the info modal.
    btn.addEventListener("mousedown", function(e) {
      e.preventDefault();
      e.stopPropagation();
    });
    btn.addEventListener("click", function(e) {
      e.stopPropagation();
      if (entry.row.checkbox) {
        entry.row.checkbox.click(); // Fires the sidebar's own change handler -- redraw, savedVisibility, all of it, for free.
      }
      refresh();
    });
    return btn;
  }

  // A catalog entry's icon lookup key -- an item's own short ClassName, or
  // the first of a merged building row's typePaths (same-shape/different-
  // material skins visually differ too, but one representative icon per row
  // is all a single <img> can show).
  function catalogIconClassName(entry) {
    return entry.kind === "building" ? entry.typePaths[0] : entry.itemPath;
  }

  function suggestionRow(entry, index) {
    var row = el("div", "searchSuggestionRow");
    var img = document.createElement("img");
    img.className = "searchSuggestionIcon";
    img.alt = "";
    if (entry.kind === "vehicle") {
      // Vehicle glyphs live under icons/vehicles/ and are carried on the
      // entry itself (see filters.js) -- no ClassName-keyed lookup to do.
      img.onerror = function() { img.onerror = null; img.src = DEFAULT_BUILDING_ICON_URL; };
      img.src = entry.iconUrl;
      img.classList.add("searchSuggestionVehicleIcon");
    } else {
      attachIconWithFallback(img, entry.kind, catalogIconClassName(entry));
    }
    row.appendChild(img);
    row.appendChild(el("span", "searchSuggestionLabel", entry.label));
    if (entry.kind === "building" || entry.kind === "vehicle") {
      row.appendChild(makeVisibilityToggle(entry));
    }
    // mousedown (not click) + preventDefault so selecting doesn't first
    // blur the input and let the document-level outside-click handler race
    // in and close the dropdown before the pick registers.
    row.addEventListener("mousedown", function(e) {
      e.preventDefault();
      selectSuggestion(entry);
    });
    row.addEventListener("mouseenter", function() { setActive(index); });
    return row;
  }

  // Substring match, case-insensitive, with prefix matches sorted first so
  // typing "iron" surfaces "Iron Plate"/"Iron Rod" ahead of "Reinforced Iron
  // Plate". Each kind is matched/capped independently, then rendered as its
  // own labeled group ("ITEMS" / "BUILDINGS") -- skipped when only one kind
  // has any matches, so a query that's obviously just an item (or just a
  // building) doesn't show an empty section header for the other.
  function matchCatalog(entries, q) {
    var matches = entries.filter(function(entry) {
      return entry.label.toLowerCase().indexOf(q) !== -1;
    });
    matches.sort(function(a, b) {
      var aPrefix = a.label.toLowerCase().indexOf(q) === 0 ? 0 : 1;
      var bPrefix = b.label.toLowerCase().indexOf(q) === 0 ? 0 : 1;
      return aPrefix - bPrefix;
    });
    return matches.slice(0, MAX_SUGGESTIONS_PER_KIND);
  }

  function renderSuggestions(query) {
    var q = query.trim().toLowerCase();
    if (!q) {
      hideSuggestions();
      return;
    }
    var itemMatches = matchCatalog(catalog.filter(function(e) { return e.kind === "item"; }), q);
    var buildingMatches = matchCatalog(catalog.filter(function(e) { return e.kind === "building"; }), q);
    var vehicleMatches = matchCatalog(catalog.filter(function(e) { return e.kind === "vehicle"; }), q);
    currentSuggestions = itemMatches.concat(buildingMatches, vehicleMatches);

    suggestionsEl.innerHTML = "";
    currentRowElements = [];
    if (currentSuggestions.length === 0) {
      suggestionsEl.appendChild(el("div", "searchSuggestionEmpty", "No matching item, building or vehicle."));
      suggestionsEl.style.display = "block";
      activeIndex = -1;
      return;
    }

    var nonEmptyKinds = [itemMatches, buildingMatches, vehicleMatches].filter(function(m) { return m.length > 0; }).length;
    var showGroupLabels = nonEmptyKinds > 1;
    var index = 0;
    [["Items", itemMatches], ["Buildings", buildingMatches], ["Vehicles", vehicleMatches]].forEach(function(group) {
      var groupEntries = group[1];
      if (groupEntries.length === 0) {
        return;
      }
      if (showGroupLabels) {
        suggestionsEl.appendChild(el("div", "searchSuggestionGroupLabel", group[0]));
      }
      groupEntries.forEach(function(entry) {
        var row = suggestionRow(entry, index);
        currentRowElements.push(row);
        suggestionsEl.appendChild(row);
        index++;
      });
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
      var typedLabel = searchInput.value.trim();
      if (activeIndex >= 0 && currentSuggestions[activeIndex]) {
        selectSuggestion(currentSuggestions[activeIndex]);
      } else if (itemCatalogByLabel.hasOwnProperty(typedLabel)) {
        hideSuggestions();
        runSearchFor(itemCatalogByLabel[typedLabel], typedLabel);
      } else if (buildingCatalogByLabel.hasOwnProperty(typedLabel)) {
        hideSuggestions();
        runBuildingSearchFor(buildingCatalogByLabel[typedLabel]);
      } else if (vehicleCatalogByLabel.hasOwnProperty(typedLabel)) {
        hideSuggestions();
        runVehicleSearchFor(vehicleCatalogByLabel[typedLabel]);
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

  // Building modal's equivalent of the item modal's highlight toggle above --
  // see showBuildingHighlight for why this isolates in place rather than
  // building a synthetic bucket.
  buildingModalHighlightToggle.addEventListener("click", function() {
    if (highlighting) {
      clearHighlight();
    } else if (lastBuilding) {
      showBuildingHighlight(lastBuilding.entry);
      buildingModalHighlightToggle.textContent = "Show all layers again";
      buildingOverlay.style.display = "none";
      bannerLabel.textContent = "Showing only: " + lastBuilding.entry.label;
      banner.style.display = "flex";
    }
  });

  // Banner "Clear filter" reverts the filter; "Details" reopens the full list/
  // modal for whichever kind is currently isolated (keeping the filter
  // active -- closing that back up returns to the banner).
  bannerClear.addEventListener("click", clearHighlight);
  bannerDetails.addEventListener("click", function() {
    if (lastKind === "item" && lastResult) {
      fillModalFromResult(lastResult);
      modalHighlightToggle.textContent = "Show all layers again";
      banner.style.display = "none";
      overlay.style.display = "flex";
    } else if (lastKind === "building" && lastBuilding) {
      openBuildingModal(lastBuilding.entry);
      var fillFromInfo = lastBuilding.entry.kind === "vehicle" ? fillVehicleModalFromInfo : fillBuildingModalFromInfo;
      fillFromInfo(lastBuilding.entry, lastBuilding.info);
      buildingModalHighlightToggle.textContent = "Show all layers again";
    }
  });

  depotButton.addEventListener("click", function() {
    var depotItems = window.MapApp.currentDepotItems || [];
    openModal("Dimensional Depot");
    // Same real game icon as the top-bar button (see index.html's
    // #topBarStatusButtons); the hand-drawn crate stays as the fallback.
    modalIcon.onerror = function() {
      modalIcon.onerror = null;
      modalIcon.src = DEPOT_ICON_URL;
    };
    modalIcon.src = BUILDING_ICON_BASE + "Build_CentralStorage_C.png";
    modalIcon.style.visibility = "visible";
    modalHighlightToggle.style.display = "none";
    if (depotItems.length === 0) {
      modalSummary.textContent = "Empty (or no save loaded yet).";
      modalList.innerHTML = "";
      return;
    }
    var total = depotItems.reduce(function(s, entry) { return s + entry.count; }, 0);
    modalSummary.textContent = total.toLocaleString() + " items across " + depotItems.length + " types.";
    renderLocationList(modalList, depotItems.map(function(entry) {
      return [entry.label, entry.count.toLocaleString(), "item", entry.itemPath];
    }));
  });

  // Rebuilds the item + building catalogs and resets any in-progress search/
  // highlight -- called alongside Filters.build/Altitude.build on every load
  // (see data.js, AFTER Filters.build specifically, so
  // Filters.getBuildingSearchEntries() already reflects the fresh payload),
  // since a reload's fresh buckets shouldn't be silently hidden by a
  // highlight the user set up against the old ones.
  FindItem.build = function(payload) {
    // Hard reset -- Filters.build already cleared/rebuilt every bucket (so the
    // old highlight bucket is gone and savedVisibility is stale), so just drop
    // all find-item state rather than trying to "revert" against buckets that
    // no longer exist.
    overlay.style.display = "none";
    buildingOverlay.style.display = "none";
    banner.style.display = "none";
    highlighting = false;
    highlightedBuildingEntry = null;
    savedVisibility = null;
    lastResult = null;
    lastBuilding = null;
    lastKind = null;
    modalHighlightToggle.textContent = "Show only these on map";
    buildingModalHighlightToggle.textContent = "Show only this on map";
    searchInput.value = "";
    hideSuggestions();

    var itemEntries = (payload.itemCatalog || []).map(function(entry) {
      return { kind: "item", label: entry.label, itemPath: entry.itemPath };
    });
    itemCatalogByLabel = {};
    itemEntries.forEach(function(entry) { itemCatalogByLabel[entry.label] = entry.itemPath; });

    // One entry per merged sidebar row (see filters.js's buildingSearchEntries) --
    // already deduped/labeled exactly the way the sidebar groups same-shape/
    // different-material buildings, so search and sidebar always agree on
    // what counts as "one building".
    var buildingEntries = (Filters.getBuildingSearchEntries ? Filters.getBuildingSearchEntries() : []).map(function(row) {
      return { kind: "building", label: row.label, typePaths: row.typePaths, category: row.category, subcategory: row.subcategory, row: row };
    });
    buildingCatalogByLabel = {};
    buildingEntries.forEach(function(entry) { buildingCatalogByLabel[entry.label] = entry; });

    // One entry per Vehicles sidebar row (see filters.js's vehicleSearchEntries):
    // Tractor/Truck/Drone/... plus the single whole-consist "Train" row --
    // individual locomotives/freight cars are deliberately not searchable.
    var vehicleEntries = (Filters.getVehicleSearchEntries ? Filters.getVehicleSearchEntries() : []).map(function(row) {
      return { kind: "vehicle", label: row.label, typePaths: row.typePaths, isTrain: row.isTrain, iconUrl: row.iconUrl, row: row };
    });
    vehicleCatalogByLabel = {};
    vehicleEntries.forEach(function(entry) { vehicleCatalogByLabel[entry.label] = entry; });

    catalog = itemEntries.concat(buildingEntries, vehicleEntries);

    window.MapApp.currentDepotItems = payload.dimensionalDepot || [];
  };
})();
