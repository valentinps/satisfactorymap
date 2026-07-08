// Builds the filter sidebar from a loaded map payload and wires checkboxes
// directly to bucket visibility flags on MapApp.layer (no re-fetch/re-filter
// of raw data on toggle -- see map.js BucketedCanvasLayer). Also tags every
// bucket with enough metadata (ids + tooltip info) for Tooltip.js to resolve
// a click into either a quick local description or a server-side detail fetch.
//
// Sidebar structure: top-level sections are each a collapsible renderGroup
// with a master checkbox. The building categories (Organisation/Walls/
// Production/Power/Logistics/Special, plus the catch-all Unknown) and their
// one level of subcategory come straight from game_data/generated/buildingCategories.json
// + game_data/categoryLabels.json via payload.menuOrder -- see
// buildBuildingCategorySections. Resource Nodes, HUB, Entities, Collectables
// (which nests Hard Drives alongside Power Slugs/Somersloops/Mercer Spheres),
// and Dropped Items (loose ground stacks) are their own separate sections. renderGroup() supports the
// subcategory nesting generically (see its doc comment).

var Filters = {};

(function() {
  "use strict";

  // Keyed by the build-menu category display names from game_data/categoryLabels.json
  // (see sav_map_data.BUILD_MENU_ORDER). "Unknown" is the catch-all for any
  // placed buildable whose class isn't in buildingCategories.json.
  var BUILDING_CATEGORY_COLORS = {
    Special: "#e84393",
    Production: "#e67e22",
    Power: "#f1c40f",
    Logistics: "#3498db",
    Organisation: "#1abc9c",
    Walls: "#95a5a6",
    Foundation: "#7a7a7a",
    Unknown: "#e74c3c",
  };

  // Generic icon color for sections that span a mix of types with no single
  // representative color (the rows inside still show their own correct color).
  var NEUTRAL_COLOR = "#999999";

  var PURITY_COLORS = { PURE: "#80b139", NORMAL: "#f26418", IMPURE: "#d23430", UNKNOWN: "#aaaaaa" };
  var PURITY_LABELS = { PURE: "Pure", NORMAL: "Normal", IMPURE: "Impure", UNKNOWN: "Unknown" };
  var COLLECTED_COLOR = "#666666";

  var SLUG_COLORS = { slugsBlue: "#3355ff", slugsYellow: "#dddd00", slugsPurple: "#c000c0" };
  var SOMERSLOOP_COLOR = "#f43845";
  var MERCER_SPHERE_COLOR = "#4e1071";

  // Real item icons (see static/map/icons/items/, keyed by ClassName -- see
  // game_data/generated/items.json/resources.json's "icon" field and
  // game_data/copy_icons.py) read far more clearly on the map than an abstract
  // colored dot for these specific collectables -- "remaining" is drawn at
  // full opacity, "collected"/already-dealt-with states are dimmed
  // (COLLECTED_ICON_OPACITY) rather than needing a second image asset just
  // to show the same icon "used up".
  var ICON_BASE_URL = "icons/items/";
  var ITEM_ICON_CLASS_NAMES = {
    slugsBlue: "Desc_Crystal_C",
    slugsYellow: "Desc_Crystal_mk2_C",
    slugsPurple: "Desc_Crystal_mk3_C",
    somersloops: "Desc_WAT1_C",
    mercerSpheres: "Desc_WAT2_C",
  };
  var COLLECTED_ICON_OPACITY = 0.4;

  function iconUrl(key) {
    return ICON_BASE_URL + encodeURIComponent(ITEM_ICON_CLASS_NAMES[key]) + ".png";
  }

  // Hard Drives have no FGItemDescriptor/FGResourceDescriptor of their own
  // (picked up once as a one-off tech unlock, never held in inventory), but
  // the game does have real crate art for them -- it just lives under a
  // schematic's mSchematicIcon field, which extract_docs_json.py doesn't
  // parse generically (see game_data/copy_icons.py's EXTRA_ICON_COPIES). Copied
  // in by hand as icons/items/HardDrive.png -- a real game asset, not a
  // hand-picked label file.
  var HARD_DRIVE_ICON_URL = "icons/items/HardDrive.png";

  // Resource node icons -- keyed by the save's own resourceType pathName
  // (see sav_map_data.collectResourceNodes), which is exactly the ClassName
  // the raw resource's own per-class icon is stored under (see
  // game_data/generated/resources.json, copied in by game_data/copy_icons.py), so the
  // URL is fully deterministic -- no lookup table needed. Geyser
  // (Desc_Geyser_C) is the one exception: it's a synthetic resourceType this
  // parser invented for a resource node kind with no FGResourceDescriptor (or
  // any other Docs.json field) behind it at all -- its real icon is copied in
  // by hand instead (see game_data/copy_icons.py's EXTRA_ICON_COPIES) as
  // icons/items/Geyser.png, not ClassName-keyed since there's no class to key it by.
  function resourceIconUrl(resourceType) {
    if (resourceType === "Desc_Geyser_C") {
      return ICON_BASE_URL + "Geyser.png";
    }
    return ICON_BASE_URL + encodeURIComponent(resourceType) + ".png";
  }

  // No "player" icon exists among the real item icons (static/map/icons/items/),
  // so a small inline SVG silhouette is used instead of adding a binary asset
  // just for this one marker -- a data: URL works the same as a real file with
  // makeIconBucket/_drawIconBucket, which only ever calls Image().
  var PLAYER_COLOR = "#2ecc71";
  var PLAYER_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<circle cx="16" cy="9" r="6" fill="' + PLAYER_COLOR + '"/>' +
    '<path d="M4 29c0-7.5 5.4-12 12-12s12 4.5 12 12" fill="' + PLAYER_COLOR + '"/>' +
    '</svg>'
  );

  // Same reasoning as the player icon above -- a simple inline paw-print
  // silhouette stands in for every wildlife/enemy species (Lizard Doggo,
  // Hogs, Spitters, Stingers, Crab Hatchers, ...) rather than sourcing a
  // unique icon per creature; the tooltip/row label still says exactly
  // which species it is.
  var CREATURE_COLOR = "#c9a35c";
  var CREATURE_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<circle cx="16" cy="21" r="7" fill="' + CREATURE_COLOR + '"/>' +
    '<circle cx="7" cy="12" r="4" fill="' + CREATURE_COLOR + '"/>' +
    '<circle cx="16" cy="7" r="4" fill="' + CREATURE_COLOR + '"/>' +
    '<circle cx="25" cy="12" r="4" fill="' + CREATURE_COLOR + '"/>' +
    '</svg>'
  );

  // Simple "home" pentagon silhouette (square body + peaked roof) -- like
  // the player icon above, an inline SVG data: URL avoids adding a binary
  // asset just for this one landmark marker.
  var HUB_COLOR = "#d2691e";
  var HUB_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<polygon points="16,3 29,14 29,29 3,29 3,14" fill="' + HUB_COLOR + '"/>' +
    '</svg>'
  );

  // Vehicles (trucks/tractors/explorers/trains/drones -- see
  // sav_map_data.collectVehicles). The glyphs under icons/vehicles/ are the
  // game's own monochrome UI icons -- white on transparent (extracted by
  // game_data/copy_icons.py) -- so the pin's circle gets a solid color fill
  // (pinFillColor) instead of _drawIconBucket's default white, or the glyph
  // would be invisible. Same orange family as the vehicle-path lines
  // (LINE_COLORS.vehiclePaths) since they're two views of the same system.
  var VEHICLE_COLOR = "#f39c12";
  var VEHICLE_ICON_BASE = "icons/vehicles/";

  var HARD_DRIVE_COLORS = {
    hasDrive: "#3355ff",
    empty: "#cccccc",
    dismantled: "#00cccc",
  };

  var HARD_DRIVE_LABELS = {
    hasDrive: "Has Drive",
    empty: "Empty",
    dismantled: "Dismantled",
  };

  var LINE_COLORS = {
    powerLines: "#5ba3e0",
    belts: "#e67e22",
    pipelines: "#2ecc71",
    railroads: "#cccccc",
    hypertubes: "#00bcd4",
    vehiclePaths: "#f39c12",
  };

  var LINE_LABELS = {
    powerLines: "Power Line",
    belts: "Belt / Lift",
    pipelines: "Pipeline",
    railroads: "Railroad",
    hypertubes: "Hypertube",
  };

  // Category/subcategory order comes from the payload (payload.menuOrder,
  // built from game_data/generated/buildingCategories.json) -- see buildBuildingCategorySections.

  // Lightweight buildables (foundations/walls/ramps/beams) come in several
  // material skins per shape -- e.g. "Foundation 4m", "Foundation 4m
  // (Asphalt)", "Foundation 4m (Concrete)", "Foundation 4m (Metal)",
  // "Foundation 4m (Polished Concrete)" are all the exact same shape/size,
  // just different paint, and would otherwise show up as 5 separate sidebar
  // rows. Only suffixes confirmed to be pure material/skin are stripped here
  // -- other parenthetical suffixes seen in the readable-name data (e.g.
  // "(Window)", "(No Indicator)", "(1 m)") indicate a genuinely different
  // shape or size and must NOT be merged away. Applied across every building
  // category (not just Foundations/Walls/Architecture) since it's a no-op for
  // any label that never carries one of these suffixes.
  var MATERIAL_LABEL_SUFFIXES = [" (Asphalt)", " (Concrete)", " (Polished Concrete)", " (Metal)"];

  function mergedMaterialLabel(label) {
    for (var i = 0; i < MATERIAL_LABEL_SUFFIXES.length; i++) {
      var suffix = MATERIAL_LABEL_SUFFIXES[i];
      if (label.slice(-suffix.length) === suffix) {
        return label.slice(0, -suffix.length);
      }
    }
    return label;
  }

  function pointCount(points, stride) {
    return Math.floor(points.length / stride);
  }

  // Bucket keys (e.g. "building:Desc_ConstructorMk1_C", "node:...", "line:belt:Mk.6")
  // are stable identifiers for a *kind* of thing, not a specific save's data --
  // so a visibility choice made here survives both a same-file auto-refresh
  // (see data.js's checkForNewerSave) and switching to an entirely different
  // save. Persists for the life of the page (rebuilt fresh only on reload),
  // deliberately never cleared by Filters.build itself.
  var savedVisibility = {};

  // bucket.key -> the checkbox/label of the sidebar row ("layer") and the
  // top-level category that own it -- populated by appendLeafRow (every
  // leaf row, however deeply nested) and renderTopLevelCategory (every
  // top-level section) respectively, as the two centralized places that
  // already run for every bucket in the whole tree. Lets the right-click
  // context menu (see ContextMenu) flip the exact same checkboxes the
  // sidebar owns for "Hide layer"/"Hide category" without re-deriving the
  // tree. Rebuilt fresh in Filters.build, same lifetime as the buckets
  // themselves (unlike savedVisibility, which deliberately outlives a reload).
  var bucketLayerCheckbox = {};
  var bucketLayerLabel = {};
  var bucketCategoryCheckbox = {};
  var bucketCategoryLabel = {};

  // tooltipInfo(idx) -> {title, rows: [[label, value], ...]} for a "static"
  // bucket (no server round-trip; we already know everything worth showing).
  function makePointBucket(key, label, color, points, renderType, pointStride, ids, tooltipKind, tooltipInfo, footprintPixels, drawPriority, tiltedFootprints, maxFootprintRadius) {
    return MapApp.layer.addBucket({
      key: key, label: label, color: color, visible: true,
      renderType: renderType || "circle",
      pointStride: pointStride,
      points: new Float32Array(points),
      ids: ids || null,
      tooltipKind: tooltipKind || "none",
      tooltipInfo: tooltipInfo || null,
      footprintPixels: footprintPixels || null,
      // See sav_map_data.collectBuildings -- sparse pointIndex -> flat
      // [x1,y1,x2,y2,...] polygon override for the rare genuinely-tilted
      // instance (a Pillar/Beam bracing a diagonal run), whose true top-down
      // silhouette isn't this bucket's shared axis-aligned footprintPixels
      // rect -- already in final rotated orientation (map.js's
      // _tracePolygon/_pointInPolygon just translate it, no further
      // rotation), plus the largest center-to-edge distance actually used
      // anywhere in the bucket (map.js's hover/click hit-test needs that to
      // size its spatial-grid query radius correctly). null for the
      // overwhelming majority of buckets that never need it.
      tiltedFootprints: tiltedFootprints || null,
      maxFootprintRadius: maxFootprintRadius || (footprintPixels ? Math.hypot(footprintPixels[0], footprintPixels[1]) : 0),
      // Buckets are drawn (and so painted over each other) in this order,
      // ascending -- see map.js's _redraw, which sorts buckets by this
      // before each frame. Plain category order isn't altitude, so without
      // this, ground-level foundations drawn late in the sidebar's category
      // list would visually paint over taller machines built on top of them
      // regardless of which is actually higher up.
      drawPriority: drawPriority || 0,
    });
  }

  function makeIconBucket(key, label, color, points, ids, tooltipKind, tooltipInfo, url, opacity, pinFillColor) {
    return MapApp.layer.addBucket({
      key: key, label: label, color: color, visible: true,
      renderType: "icon",
      pointStride: 3,
      points: new Float32Array(points),
      ids: ids || null,
      tooltipKind: tooltipKind || "none",
      tooltipInfo: tooltipInfo || null,
      iconUrl: url,
      iconOpacity: opacity,
      // Background color of the pin's circle -- defaults to white (see
      // map.js's _drawIconBucket) for collectables/players/HUB; resource
      // nodes override this per-purity (green/orange/red) instead.
      pinFillColor: pinFillColor || null,
    });
  }

  function makeLineBucket(key, label, color, polylines, ids, tooltipKind, tooltipInfo, pointStride) {
    return MapApp.layer.addBucket({
      key: key, label: label, color: color, visible: true,
      renderType: "line",
      // 3 = [x, y, z] per vertex (power lines, plain straight segments); 7 =
      // [x, y, z, arriveTangentX, arriveTangentY, leaveTangentX, leaveTangentY]
      // (belts/pipelines/railroads/hypertubes -- enough to draw the real
      // curve through each spline point, see map.js's _drawLineBucket).
      pointStride: pointStride || 3,
      lines: polylines.map(function(line) { return new Float32Array(line); }),
      ids: ids || null,
      tooltipKind: tooltipKind || "none",
      tooltipInfo: tooltipInfo || null,
    });
  }

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // A checkbox wrapped in a <label> with a slider span, styled in map.css as
  // an animated on/off switch instead of a native checkbox. The <label>
  // wrapping means clicking anywhere on the switch (handle or track) toggles
  // the underlying real <input type=checkbox> exactly like a native
  // checkbox would -- so all existing .checked/"change"-event logic below
  // needs no changes, only what gets appended to the DOM.
  function makeToggle() {
    var wrapper = el("label", "toggleSwitch");
    var checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    wrapper.appendChild(checkbox);
    wrapper.appendChild(el("span", "toggleSlider"));
    return { wrapper: wrapper, checkbox: checkbox };
  }

  function makeIcon(renderType, color, url) {
    var icon = el("span", "icon icon-" + renderType);
    if (renderType === "icon" && url) {
      icon.style.background = "none";
      icon.style.backgroundImage = "url(" + url + ")";
      icon.style.backgroundSize = "contain";
      icon.style.backgroundRepeat = "no-repeat";
      return icon;
    }
    icon.style.background = renderType === "line" ? "none" : color;
    if (renderType === "line") {
      icon.style.borderTop = "2px solid " + color;
    } else if (renderType === "rect") {
      icon.style.borderRadius = "2px";
    }
    return icon;
  }

  // Sets a checkbox's checked state, and recursively does the same for any
  // nested group checkboxes underneath it (see renderGroup's
  // `checkbox._childCheckboxes`), WITHOUT firing "change" events. Used by a
  // parent checkbox's own handler instead of dispatching a synthetic
  // "change" event per descendant -- on a category as deep/wide as
  // Construction (subcategories x dozens of merged-material rows each),
  // dispatching real events meant every single leaf row independently
  // triggered its own full canvas redraw, so one click could synchronously
  // fire hundreds of redraws and freeze the tab. This only touches checkbox
  // DOM state; bucket visibility and the single redraw are handled
  // separately by the caller.
  function setCheckedDeep(checkbox, checked) {
    checkbox.checked = checked;
    var nestedChildren = checkbox._childCheckboxes;
    if (nestedChildren) {
      nestedChildren.forEach(function(child) { setCheckedDeep(child, checked); });
    }
  }

  // Appends one leaf .filterRow (toggle + icon + label + count) to childrenDiv
  // and returns its checkbox. `row.displayLabel` overrides the shown text
  // (e.g. a compact "Mk.6" under a "Conveyor Belts" group) while the bucket
  // keeps its full, unambiguous label for tooltips/selection. Shared by
  // renderGroup's flat-array path and the nested-group builder below.
  function appendLeafRow(childrenDiv, row, renderType, swatchColor) {
    var rowDiv = el("div", "filterRow");
    var rowToggle = makeToggle();
    var checkbox = rowToggle.checkbox;
    // A row's checkbox can control several buckets at once, so restoring falls
    // back to "visible" unless a previous visit explicitly recorded otherwise
    // for one of them; in practice they're always toggled together.
    var restoredVisible = row.buckets.reduce(function(acc, bucket) {
      return savedVisibility.hasOwnProperty(bucket.key) ? savedVisibility[bucket.key] : acc;
    }, true);
    checkbox.checked = restoredVisible;
    row.buckets.forEach(function(bucket) { bucket.visible = restoredVisible; });
    checkbox.addEventListener("change", function() {
      row.buckets.forEach(function(bucket) {
        bucket.visible = checkbox.checked;
        savedVisibility[bucket.key] = checkbox.checked;
      });
      MapApp.layer.requestRedraw();
    });
    rowDiv.appendChild(rowToggle.wrapper);
    rowDiv.appendChild(makeIcon(row.renderType || renderType, row.color || swatchColor, row.iconUrl));
    rowDiv.appendChild(el("label", null, row.displayLabel || row.label));
    rowDiv.appendChild(el("span", "count", String(row.count)));
    childrenDiv.appendChild(rowDiv);
    // Building rows (see buildingSearchEntries) hang onto their own checkbox
    // so the search bar's show/hide toggle can drive this exact element --
    // one source of truth for a building's visibility, whether it's flipped
    // from here or from a search suggestion.
    row.checkbox = checkbox;
    // Every bucket's "layer" is exactly the row that owns it -- recorded here
    // (the single place every leaf row is built, however deeply nested) so
    // the right-click context menu (see ContextMenu/Filters.hideLayer) can
    // find and flip this same checkbox without walking the sidebar tree
    // again. The row's own label/displayLabel (not a bucket's own, narrower
    // label -- e.g. one material skin's) is what "Hide layer" should show,
    // since that's the scope it actually hides.
    row.buckets.forEach(function(bucket) {
      bucketLayerCheckbox[bucket.key] = checkbox;
      bucketLayerLabel[bucket.key] = row.displayLabel || row.label;
    });
    return checkbox;
  }

  // Renders one collapsible group with a master checkbox (toggling it
  // flips every checkbox inside, recursively) and an expand/collapse toggle.
  // `content` is either:
  //   - an array of leaf rows: [{label, count, color, buckets, renderType}, ...]
  //   - a function(childrenDiv) -> {buckets: [...all leaf buckets inside...],
  //     checkboxes: [...immediate child group/row checkboxes...]} for nesting
  //     other renderGroup() calls inside this one.
  // Returns {buckets, checkbox} so a caller can nest this group inside another.
  function renderGroup(container, title, renderType, swatchColor, content, options) {
    options = options || {};
    var group = el("div", "filterGroup");
    var titleRow = el("div", "groupTitle");

    // Top-level categories (see renderTopLevelCategory) own their content's
    // visibility entirely through nav-column selection instead -- without
    // this, the titleRow (physically relocated into the nav column, but
    // still the very same DOM node with this listener attached) would keep
    // reacting to clicks by toggling childrenDiv's inline display itself,
    // fighting with the "active" class that actually controls it there.
    var expandToggle = null;
    if (!options.noExpandToggle) {
      expandToggle = el("span", "expandToggle", options.startCollapsed ? "▸" : "▾");
      titleRow.appendChild(expandToggle);
    }

    var parentToggle = makeToggle();
    var parentCheckbox = parentToggle.checkbox;
    parentCheckbox.checked = true;
    titleRow.appendChild(parentToggle.wrapper);

    titleRow.appendChild(makeIcon(renderType, swatchColor, options.iconUrl));
    titleRow.appendChild(el("span", "groupLabel", title));
    group.appendChild(titleRow);

    var childrenDiv = el("div", "filterChildren");
    if (options.startCollapsed) {
      childrenDiv.style.display = "none";
    }

    var allBuckets = [];
    var childCheckboxes = [];

    if (typeof content === "function") {
      var nested = content(childrenDiv);
      allBuckets = nested.buckets;
      childCheckboxes = nested.checkboxes;
    } else {
      content.forEach(function(row) {
        var checkbox = appendLeafRow(childrenDiv, row, renderType, swatchColor);
        childCheckboxes.push(checkbox);
        allBuckets = allBuckets.concat(row.buckets);
      });
    }

    group.appendChild(childrenDiv);
    container.appendChild(group);

    // Lets an ancestor group's parentCheckbox recurse into this group's
    // children via setCheckedDeep without dispatching events (see above).
    parentCheckbox._childCheckboxes = childCheckboxes;

    function setCollapsed(collapsed) {
      childrenDiv.style.display = collapsed ? "none" : "";
      expandToggle.textContent = collapsed ? "▸" : "▾";
    }
    // The whole title row is clickable (icon swatch, label text, arrow,
    // and any padding/whitespace between them) rather than just the arrow
    // glyph or label text -- those were the only two elements with a click
    // listener before, so clicking anywhere else in the row (a few pixels
    // either side) silently did nothing. The toggle switch is excluded so
    // clicking it flips visibility only, without also collapsing the group.
    if (expandToggle) {
      titleRow.addEventListener("click", function(e) {
        if (e.target.closest(".toggleSwitch")) {
          return;
        }
        setCollapsed(childrenDiv.style.display !== "none");
      });
    }

    parentCheckbox.addEventListener("change", function() {
      var checked = parentCheckbox.checked;
      // Update every descendant checkbox's visual state and every leaf
      // bucket's visibility directly (allBuckets is already the full
      // flattened list of every bucket nested anywhere inside this group),
      // then redraw exactly once for the whole toggle -- instead of once
      // per descendant leaf row.
      childCheckboxes.forEach(function(checkbox) { setCheckedDeep(checkbox, checked); });
      allBuckets.forEach(function(bucket) {
        bucket.visible = checked;
        savedVisibility[bucket.key] = checked;
      });
      MapApp.layer.requestRedraw();
    });

    return { buckets: allBuckets, checkbox: parentCheckbox };
  }

  // Every top-level category (Resource Nodes, Extraction, ..., Entities,
  // Collectables) shows a single row in the narrow left nav
  // column and its full content in the wider right detail pane, only one of
  // which is visible at a time (see selectCategory) -- this is what
  // Filters.build calls instead of renderGroup directly for those ~14 top-
  // level sections. Reuses renderGroup itself, built into a detached
  // <div> purely to get its title-row/children-div construction and
  // checkbox-cascade wiring for free, then splits the two pieces into their
  // new homes; renderGroup's own behavior is completely unchanged, and every
  // *nested* renderGroup call (subcategories, resource types, ...) still
  // works exactly as before since those live inside the relocated children
  // div, untouched.
  var categoryEntries = [];

  // One entry per merged building row created below (see mergedBuildingRow) --
  // reused by finditem.js to make placed buildings searchable from the top
  // search bar, alongside items. Each entry IS the row object itself
  // ({label, count, color, buckets, typePaths, category, subcategory}), plus
  // a `checkbox` property appendLeafRow attaches once the row is actually
  // rendered into the sidebar (see below) -- by the time Filters.build
  // returns, every entry here has a live checkbox, so toggling it from a
  // search suggestion flips the exact same bucket-visibility state (and stays
  // in sync with) the sidebar's own row.
  var buildingSearchEntries = [];
  Filters.getBuildingSearchEntries = function() { return buildingSearchEntries; };
  Filters.buildingCategoryColor = function(category) { return BUILDING_CATEGORY_COLORS[category] || BUILDING_CATEGORY_COLORS.Unknown; };

  // Leaflet doesn't notice its container resized just because a CSS
  // width/left value changed -- invalidateSize() is the real API for that,
  // and it's what actually fires the "resize" event BucketedCanvasLayer
  // already listens for (see map.js's onAdd), so the canvas/tiles catch up
  // to the map filling (or giving back) the space the detail column just
  // vacated.
  function notifyMapResized() {
    if (window.MapApp && MapApp.map) {
      MapApp.map.invalidateSize();
    }
  }

  // Sizes the nav panel to fit the widest category card instead of a fixed
  // guess, so it wastes no horizontal space (and the map gets the rest).
  // Measured by momentarily letting the list size to its content -- each
  // card's label has flex:1, so at max-content it collapses to the label's
  // natural (un-stretched) width, making the column exactly as wide as its
  // longest row. Clamped so the save dropdown / Check-Uncheck header stay
  // usable at the low end and the map never loses an absurd amount at the
  // high end. Writes the result to --nav-col-width (which #map/#sidebar/
  // #categoryNavPanel all derive from) and pokes Leaflet to catch the resize.
  function autoSizeNavPanel() {
    var navColumn = document.getElementById("categoryNavColumn");
    if (!navColumn || navColumn.children.length === 0) {
      return;
    }
    var previous = navColumn.style.width;
    navColumn.style.width = "max-content";
    var natural = navColumn.offsetWidth;
    navColumn.style.width = previous;
    var width = Math.max(232, Math.min(natural + 8, 380));
    document.documentElement.style.setProperty("--nav-col-width", width + "px");
    notifyMapResized();
  }

  function deselectAllCategories() {
    categoryEntries.forEach(function(entry) {
      entry.navRow.classList.remove("active");
      entry.detailGroup.classList.remove("active");
    });
    document.body.classList.add("no-category-selected");
    notifyMapResized();
  }

  function selectCategory(navRow, detailGroup) {
    categoryEntries.forEach(function(entry) {
      var isThis = entry.navRow === navRow;
      entry.navRow.classList.toggle("active", isThis);
      entry.detailGroup.classList.toggle("active", isThis);
    });
    document.body.classList.remove("no-category-selected");
    notifyMapResized();
  }

  function renderTopLevelCategory(navList, detailPane, title, renderType, swatchColor, content, options) {
    options = options || {};
    var staging = el("div");
    // noExpandToggle: selecting the row in the nav column is what reveals
    // its content now, so renderGroup's own arrow/collapse-click machinery
    // (which would otherwise keep fighting the "active" class below, since
    // titleRow gets physically relocated but keeps whatever listeners
    // renderGroup attached to it) is skipped entirely for this level.
    var result = renderGroup(staging, title, renderType, swatchColor, content, { iconUrl: options.iconUrl, noExpandToggle: true });
    var group = staging.firstChild;
    var titleRow = group.firstElementChild; // Appended first inside renderGroup.
    var childrenDiv = group.lastElementChild; // Appended second inside renderGroup.

    titleRow.classList.add("categoryNavRow");
    navList.appendChild(titleRow);

    var detailGroup = el("div", "categoryDetailGroup");
    detailGroup.appendChild(childrenDiv);
    detailPane.appendChild(detailGroup);

    titleRow.addEventListener("click", function(e) {
      if (e.target.closest(".toggleSwitch")) {
        return; // The switch still just toggles visibility, independent of selection.
      }
      if (titleRow.classList.contains("active")) {
        deselectAllCategories(); // Clicking the already-selected category again closes the detail panel.
      } else {
        selectCategory(titleRow, detailGroup);
      }
    });

    categoryEntries.push({ navRow: titleRow, detailGroup: detailGroup });

    // See bucketCategoryCheckbox's doc comment above -- `title` carries a
    // trailing " (1,234)" total count (most call sites) that reads oddly
    // repeated in a right-click menu, so it's stripped for display only;
    // the checkbox itself doesn't care either way.
    var cleanTitle = title.replace(/\s*\([\d,]+\)\s*$/, "");
    result.buckets.forEach(function(bucket) {
      bucketCategoryCheckbox[bucket.key] = result.checkbox;
      bucketCategoryLabel[bucket.key] = cleanTitle;
    });

    return { buckets: result.buckets, checkbox: result.checkbox };
  }

  // ---- Resource Nodes / Resource Wells ---------------------------------

  // sav_map_data.collectResourceNodes appends " (Resource Well)" to a well
  // entry's label so the tooltip (which still uses the full label -- see
  // buildResourceEntryGroup's tooltipInfo) stays unambiguous on its own.
  // Resource Wells now get their own sidebar section instead (see
  // buildResourceWellSection), where that suffix would just be redundant
  // noise repeated on every row -- stripped for the menu row only.
  var WELL_LABEL_SUFFIX = " (Resource Well)";

  function stripWellSuffix(label) {
    if (label.slice(-WELL_LABEL_SUFFIX.length) === WELL_LABEL_SUFFIX) {
      return label.slice(0, -WELL_LABEL_SUFFIX.length);
    }
    return label;
  }

  // One resource (e.g. "Crude Oil") -> Mined/Unmined subgroups -> purity
  // rows nested inside each, instead of one flat list of 6 "Unmined, Pure" /
  // "Mined, Pure" / etc. rows -- mined vs. unmined is the choice that
  // actually matters when deciding what to look at, so it gets to be the
  // grouping level, with purity as the detail nested underneath it.
  function buildResourceEntryGroup(childrenDiv, resourceEntry) {
    var url = resourceIconUrl(resourceEntry.resourceType);
    return renderGroup(childrenDiv, stripWellSuffix(resourceEntry.label), "icon", PURITY_COLORS.NORMAL, function(stateChildrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      ["unmined", "mined"].forEach(function(state) {
        var stateLabel = state === "mined" ? "Mined" : "Unmined";
        // Mined nodes keep their real purity color, just dimmed (same
        // treatment as collected slugs/somersloops/etc.) instead of
        // switching to a flat gray -- still readable as "this purity
        // node, already mined" rather than losing that information.
        var opacity = state === "mined" ? COLLECTED_ICON_OPACITY : 1;
        var purityGroup = resourceEntry[state].byPurity;
        var rows = [];
        Object.keys(purityGroup).forEach(function(purityName) {
          var purityData = purityGroup[purityName];
          var count = pointCount(purityData.points, 3);
          if (count === 0) {
            return; // No point offering a toggle for an empty bucket.
          }
          var purityColor = PURITY_COLORS[purityName] || PURITY_COLORS.UNKNOWN;
          var purityLabel = PURITY_LABELS[purityName] || purityName;
          // worldPositions is a flat [x0,y0,x1,y1,...] array, same order/
          // length as points/ids -- the raw world-space position sav_map_data
          // already computed for this exact point (see collectResourceNodes),
          // used for the tooltip's Coordinates row/copy button without
          // needing a live-actor lookup.
          var tooltipInfo = function(index) {
            var worldPositions = purityData.worldPositions;
            var position = worldPositions ? [worldPositions[index * 2], worldPositions[index * 2 + 1]] : undefined;
            return { title: resourceEntry.label, rows: [["Purity", purityLabel], ["Status", stateLabel]], position: position };
          };
          var bucket = makeIconBucket(
            "node:" + resourceEntry.resourceType + ":" + state + ":" + purityName, resourceEntry.label,
            purityColor, purityData.points, purityData.ids, "static", tooltipInfo, url, opacity, purityColor);
          rows.push({ label: purityLabel, count: count, color: purityColor, buckets: [bucket], iconUrl: url });
        });
        if (rows.length === 0) {
          return; // No point offering a toggle for an empty Mined/Unmined subgroup.
        }
        var subTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(stateChildrenDiv, stateLabel + " (" + subTotal + ")", "icon", PURITY_COLORS.NORMAL, rows, { startCollapsed: true, iconUrl: url });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: true, iconUrl: url });
  }

  // Every top-level section shows its total count in the header (matching
  // the building-category sections below) so it's informative even
  // collapsed -- see buildResourceEntrySection/buildCollectablesSection/
  // buildCollectablesSection's nested Hard Drives group, all now
  // startCollapsed:true by default.
  function resourceEntriesTotal(resourceEntries) {
    var total = 0;
    resourceEntries.forEach(function(resourceEntry) {
      ["unmined", "mined"].forEach(function(state) {
        Object.values(resourceEntry[state].byPurity).forEach(function(p) { total += pointCount(p.points, 3); });
      });
    });
    return total;
  }

  function buildResourceEntrySection(navList, detailPane, title, resourceEntries) {
    if (resourceEntries.length === 0) {
      return;
    }
    var total = resourceEntriesTotal(resourceEntries);
    renderTopLevelCategory(navList, detailPane, title + " (" + total + ")", "circle", NEUTRAL_COLOR, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      resourceEntries.forEach(function(resourceEntry) {
        var result = buildResourceEntryGroup(childrenDiv, resourceEntry);
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    });
  }

  function buildResourceNodeSection(navList, detailPane, payload) {
    var byResourceType = payload.resourceNodes.byResourceType;
    buildResourceEntrySection(navList, detailPane, "Resource Nodes", byResourceType.filter(function(e) { return !e.isWell; }));
    buildResourceEntrySection(navList, detailPane, "Resource Wells", byResourceType.filter(function(e) { return e.isWell; }));
  }

  // ---- Collectables (Power Slugs/Somersloops/Mercer Spheres/Hard Drives) ----

  // "hasDrive" still has something for the player to get (full opacity);
  // "empty"/"dismantled" are already dealt with (dimmed) -- same icon
  // throughout, since it's still physically a hard drive crate.
  var HARD_DRIVE_ICON_OPACITY = {
    hasDrive: 1, empty: COLLECTED_ICON_OPACITY, dismantled: COLLECTED_ICON_OPACITY,
  };

  // Hard Drives nested inside Collectables as their own sub-group, same
  // level as each Power Slug/Somersloop/Mercer Sphere kind below -- they're
  // the same "find it out in the world" flavor of pickup, just with 3 states
  // (has drive/empty/dismantled) instead of remaining/collected.
  function buildHardDrivesGroup(childrenDiv, payload) {
    var hardDrives = payload.hardDrives;
    var stateKeys = ["hasDrive", "empty", "dismantled"];
    var url = HARD_DRIVE_ICON_URL;
    var rows = stateKeys.map(function(stateKey) {
      var color = HARD_DRIVE_COLORS[stateKey];
      var points = hardDrives[stateKey];
      var ids = hardDrives[stateKey + "Ids"];
      var worldPositions = hardDrives[stateKey + "WorldPositions"];
      // What a crash site demands before it hands over its hard drive --
      // either an item stack or a power hookup (see
      // sav_map_data.collectHardDrives) -- always shown, explicitly as
      // "None" rather than omitting the row, so its absence reads as a
      // known fact rather than missing data.
      var requirements = hardDrives[stateKey + "Requirements"];
      function requirementText(requirement) {
        if (!requirement) {
          return "None";
        }
        if (requirement.type === "power") {
          return requirement.watts + "W Power";
        }
        return requirement.quantity + "x " + requirement.item;
      }
      // See sav_map_data.collectHardDrives -- needed even once dismantled,
      // since the actor itself is gone from the save by then.
      var tooltipInfo = function(index) {
        var position = worldPositions ? [worldPositions[index * 2], worldPositions[index * 2 + 1]] : undefined;
        var requirement = requirements ? requirements[index] : null;
        var rows = [["Status", HARD_DRIVE_LABELS[stateKey]], ["Requirement", requirementText(requirement)]];
        return { title: "Hard Drive", rows: rows, position: position };
      };
      // Bucket label is the item-generic "Hard Drive" (not the per-state
      // HARD_DRIVE_LABELS name) -- same reasoning as the Power Slug/
      // Somersloop/Mercer Sphere buckets above, whose remaining/collected
      // buckets both use kind.label rather than "Remaining"/"Collected":
      // selection.js's rectangle-select object list groups purely by
      // bucket.label, so a per-state label here would split one "Hard
      // Drive" into three separate "Has Drive"/"Empty"/"Dismantled" rows
      // instead of one combined count. The per-state name still shows in
      // the sidebar row (row.label below) and the tooltip's "Status" row.
      var bucket = makeIconBucket("hd:" + stateKey, "Hard Drive", color, points, ids, "static", tooltipInfo, url, HARD_DRIVE_ICON_OPACITY[stateKey]);
      return { label: HARD_DRIVE_LABELS[stateKey], count: pointCount(points, 3), color: color, buckets: [bucket], iconUrl: url };
    });
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    // "hasDrive" (rows[0], stateKeys' first entry) is the only state still
    // waiting to be collected -- "empty"/"dismantled" both mean the crash
    // site's already been dealt with, so together they're the "collected"
    // half of the same collected/total format the Power Slug/Somersloop/
    // Mercer Sphere groups above use.
    var collectedCount = total - rows[0].count;
    var title = "Hard Drives (" + collectedCount + "/" + total + ")";
    return { total: total, result: renderGroup(childrenDiv, title, "icon", HARD_DRIVE_COLORS.hasDrive, rows, { startCollapsed: true, iconUrl: url }) };
  }

  function buildCollectablesSection(navList, detailPane, payload) {
    var collectables = payload.collectables;
    var kinds = [
      { key: "slugsBlue", label: "Blue Power Slug", color: SLUG_COLORS.slugsBlue },
      { key: "slugsYellow", label: "Yellow Power Slug", color: SLUG_COLORS.slugsYellow },
      { key: "slugsPurple", label: "Purple Power Slug", color: SLUG_COLORS.slugsPurple },
      { key: "somersloops", label: "Somersloop", color: SOMERSLOOP_COLOR },
      { key: "mercerSpheres", label: "Mercer Sphere", color: MERCER_SPHERE_COLOR },
    ];
    var hardDriveTotal = pointCount(payload.hardDrives.hasDrive, 3) +
      pointCount(payload.hardDrives.empty, 3) + pointCount(payload.hardDrives.dismantled, 3);
    var total = kinds.reduce(function(sum, kind) {
      var data = collectables[kind.key];
      return sum + pointCount(data.remaining, 3) + pointCount(data.collected, 3);
    }, hardDriveTotal);
    renderTopLevelCategory(navList, detailPane, "Collectables (" + total + ")", "circle", NEUTRAL_COLOR, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      kinds.forEach(function(kind) {
        var data = collectables[kind.key];
        var url = iconUrl(kind.key);
        // worldPositions* mirror points/ids (see sav_map_data._splitCollectableKind)
        // -- used for the tooltip's Coordinates row/copy button. Needed
        // even for "Collected" entries: a collected pickup's actor is
        // actually removed from the save, so a live lookup would never
        // find a position for it, but this static reference data still has it.
        var remainingInfo = function(index) {
          var wp = data.remainingWorldPositions;
          return { title: kind.label, rows: [["Status", "Remaining"]], position: wp ? [wp[index * 2], wp[index * 2 + 1]] : undefined };
        };
        var collectedInfo = function(index) {
          var wp = data.collectedWorldPositions;
          return { title: kind.label, rows: [["Status", "Collected"]], position: wp ? [wp[index * 2], wp[index * 2 + 1]] : undefined };
        };
        var remainingBucket = makeIconBucket(
          "collectable:" + kind.key + ":remaining", kind.label, kind.color, data.remaining,
          data.remainingIds, "static", remainingInfo, url, 1);
        var collectedBucket = makeIconBucket(
          "collectable:" + kind.key + ":collected", kind.label, COLLECTED_COLOR, data.collected,
          data.collectedIds, "static", collectedInfo, url, COLLECTED_ICON_OPACITY);
        var remainingCount = pointCount(data.remaining, 3);
        var collectedCount = pointCount(data.collected, 3);
        var rows = [
          { label: "Remaining", count: remainingCount, color: kind.color, buckets: [remainingBucket], iconUrl: url },
          { label: "Collected", count: collectedCount, color: COLLECTED_COLOR, buckets: [collectedBucket], iconUrl: url },
        ];
        // "(collected/total)" instead of just a bare total -- unlike a plain
        // building count, collection progress (how much of this kind is
        // already found) is the number worth seeing at a glance here.
        var kindTitle = kind.label + "s (" + collectedCount + "/" + (remainingCount + collectedCount) + ")";
        var result = renderGroup(childrenDiv, kindTitle, "icon", kind.color, rows, { startCollapsed: true, iconUrl: url });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      if (hardDriveTotal > 0) {
        var hardDriveGroup = buildHardDrivesGroup(childrenDiv, payload);
        checkboxes.push(hardDriveGroup.result.checkbox);
        allBuckets = allBuckets.concat(hardDriveGroup.result.buckets);
      }
      return { buckets: allBuckets, checkboxes: checkboxes };
    });
  }

  // ---- Dropped / ground items ----------------------------------------------

  // Fallback dot color for the rare dropped item whose ClassName has no
  // extracted icon PNG (see sav_map_data._itemIconFilename -- entry.icon is
  // null then, and an icon bucket with a 404ing URL would draw nothing at all).
  var DROPPED_ITEM_COLOR = "#b57edc";

  // Items lying loose on the ground (player-dropped stacks, harvested
  // leaves/wood/etc.) -- one row per item type, drawn with the real item
  // icon. One marker is one dropped stack; the row count is stacks, the
  // tooltip's Amount row has that stack's item count.
  function buildDroppedItemsSection(navList, detailPane, payload) {
    var rows = [];
    (payload.droppedItems || []).forEach(function(itemEntry) {
      var count = pointCount(itemEntry.points, 3);
      if (count === 0) {
        return;
      }
      var url = itemEntry.icon ? ICON_BASE_URL + encodeURIComponent(itemEntry.icon) : null;
      // worldPositions/counts parallel points/ids (see
      // sav_map_data.collectDroppedItems) -- static tooltip, everything's
      // already in the payload.
      var tooltipInfo = function(index) {
        var wp = itemEntry.worldPositions;
        return {
          title: itemEntry.label,
          rows: [["Amount", itemEntry.counts[index]], ["Status", "On the ground"]],
          position: wp ? [wp[index * 2], wp[index * 2 + 1]] : undefined,
        };
      };
      var bucket = url
        ? makeIconBucket("dropped:" + itemEntry.itemPath, itemEntry.label, DROPPED_ITEM_COLOR,
            itemEntry.points, itemEntry.ids, "static", tooltipInfo, url, 1)
        : makePointBucket("dropped:" + itemEntry.itemPath, itemEntry.label, DROPPED_ITEM_COLOR,
            itemEntry.points, "circle", 3, itemEntry.ids, "static", tooltipInfo);
      rows.push({ label: itemEntry.label, count: count, color: DROPPED_ITEM_COLOR,
                  renderType: url ? "icon" : "circle", buckets: [bucket], iconUrl: url });
    });
    if (rows.length === 0) {
      return;
    }
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    renderTopLevelCategory(navList, detailPane, "Dropped Items (" + total + ")", "circle", DROPPED_ITEM_COLOR, rows);
  }

  // ---- HUB ------------------------------------------------------------------

  // The HUB is a one-of-a-kind landmark (excluded from collectBuildings --
  // see sav_map_data.HUB_TYPE_PATH) rather than an ordinary building, so it
  // gets its own section/icon instead of showing up under "Unknown".
  function buildHubSection(navList, detailPane, payload) {
    var hub = payload.hub;
    var count = pointCount(hub.points, 3);
    if (count === 0) {
      return;
    }
    var bucket = makeIconBucket("hub", "HUB", HUB_COLOR, hub.points, hub.ids, "server", null, HUB_ICON_URL, 1);
    var rows = [{ label: "HUB", count: count, color: HUB_COLOR, buckets: [bucket], iconUrl: HUB_ICON_URL }];
    renderTopLevelCategory(navList, detailPane, "HUB", "icon", HUB_COLOR, rows, { iconUrl: HUB_ICON_URL });
  }

  // ---- Entities (Players + wildlife/enemy creatures) ----------------------

  // Unlike the other icon buckets above, a player's name/inventory isn't
  // known to the client up front -- it requires the same /api/instance
  // round-trip as buildings (see sav_map_data.describeInstance's player
  // branch), hence tooltipKind "server" instead of "static". Creatures don't
  // have anything like that to fetch (no inventory/name), so their tooltip
  // is resolved entirely client-side from the species label already in the
  // payload -- see sav_map_data.collectCreatures.
  function buildEntitiesSection(navList, detailPane, payload) {
    var rows = [];

    var players = payload.players;
    var playerCount = pointCount(players.points, 3);
    if (playerCount > 0) {
      var playerBucket = makeIconBucket("players", "Players", PLAYER_COLOR, players.points, players.ids, "server", null, PLAYER_ICON_URL, 1);
      rows.push({ label: "Player", count: playerCount, color: PLAYER_COLOR, buckets: [playerBucket], iconUrl: PLAYER_ICON_URL });
    }

    (payload.creatures || []).forEach(function(creatureType) {
      var count = pointCount(creatureType.points, 3);
      if (count === 0) {
        return;
      }
      var bucket = makeIconBucket(
        "creature:" + creatureType.typePath, creatureType.label, CREATURE_COLOR, creatureType.points,
        creatureType.ids, "server", null, CREATURE_ICON_URL, 1);
      rows.push({ label: creatureType.label, count: count, color: CREATURE_COLOR, buckets: [bucket], iconUrl: CREATURE_ICON_URL });
    });

    if (rows.length === 0) {
      return;
    }
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    renderTopLevelCategory(navList, detailPane, "Entities (" + total + ")", "icon", PLAYER_COLOR, rows, { iconUrl: PLAYER_ICON_URL });
  }

  // ---- Vehicles (trucks/tractors/explorers/trains/drones) ------------------

  // One row per vehicle type present in the save, each pin drawn with the
  // game's own monochrome glyph on a solid VEHICLE_COLOR circle. Vehicles
  // are real actors with inventories (a truck's cargo, a locomotive's
  // freight consist neighbor), so tooltipKind "server" resolves the details
  // through the same /api/instance path buildings use.
  function buildVehiclesSection(navList, detailPane, payload) {
    var rows = [];
    (payload.vehicles || []).forEach(function(vehicleType) {
      var count = pointCount(vehicleType.points, 3);
      if (count === 0) {
        return;
      }
      var url = VEHICLE_ICON_BASE + encodeURIComponent(vehicleType.icon);
      var bucket = makeIconBucket(
        "vehicle:" + vehicleType.typePath, vehicleType.label, VEHICLE_COLOR, vehicleType.points,
        vehicleType.ids, "server", null, url, 1, VEHICLE_COLOR);
      rows.push({ label: vehicleType.label, count: count, color: VEHICLE_COLOR, buckets: [bucket], iconUrl: url });
    });
    if (rows.length === 0) {
      return;
    }
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    renderTopLevelCategory(navList, detailPane, "Vehicles (" + total + ")", "icon", VEHICLE_COLOR, rows,
      { iconUrl: VEHICLE_ICON_BASE + "Truck.png" });
  }

  // ---- Building categories (from game_data/generated/buildingCategories.json, plus Unknown) ----

  function buildingRow(typeEntry, color, drawPriority) {
    var bucket = makePointBucket(
      "building:" + typeEntry.typePath, typeEntry.label, color, typeEntry.points, typeEntry.renderType, 4,
      typeEntry.ids, "server", null, typeEntry.footprintPixels, drawPriority,
      typeEntry.tiltedFootprints, typeEntry.maxFootprintRadius);
    return { label: typeEntry.label, count: pointCount(typeEntry.points, 4), color: color, renderType: typeEntry.renderType, buckets: [bucket] };
  }

  // Same-shape/different-material typeEntries (see mergedMaterialLabel) merged
  // into a single row controlling all of their buckets at once. `typePaths`
  // and `category` aren't used by the sidebar itself -- they're carried
  // along so this same row object can double as a building-search catalog
  // entry (see buildingSearchEntries above).
  function mergedBuildingRow(mergedLabel, typeEntries, color, drawPriority, category) {
    var buckets = typeEntries.map(function(typeEntry) { return buildingRow(typeEntry, color, drawPriority).buckets[0]; });
    var count = typeEntries.reduce(function(s, t) { return s + pointCount(t.points, 4); }, 0);
    return {
      label: mergedLabel, count: count, color: color, renderType: typeEntries[0].renderType, buckets: buckets,
      typePaths: typeEntries.map(function(t) { return t.typePath; }), category: category,
    };
  }

  // Foundations/frames/walls (Organisation/Walls categories) sit at ground
  // level under everything else in practice -- drawn first (see
  // makePointBucket's drawPriority) so machines built on top of them paint
  // over them regardless of where these categories fall in the sidebar's order.
  var DRAW_PRIORITY_BY_CATEGORY = { Organisation: -1, Walls: -1 };

  function lineRow(key, lines) {
    var lineData = lines[key];
    var bucket = makeLineBucket("line:" + key, LINE_LABELS[key], LINE_COLORS[key], lineData.polylines, lineData.ids, "server", null, lineData.pointStride);
    return { label: LINE_LABELS[key], count: lineData.polylines.length, color: LINE_COLORS[key], renderType: "line", buckets: [bucket] };
  }

  // A leaf row from an already-collected line group (per-mark belts/pipes --
  // see collectSplinePathGroups). The bucket keeps the full label
  // (tooltips/selection); displayLabel is the compact "Mk.N" shown in the
  // sidebar under the "Conveyor Belts"/"Pipes" group.
  function lineRowFromData(key, fullLabel, displayLabel, color, lineData) {
    var bucket = makeLineBucket(key, fullLabel, color, lineData.polylines, lineData.ids, "server", null, lineData.pointStride);
    return { label: fullLabel, displayLabel: displayLabel, count: lineData.polylines.length, color: color, renderType: "line", buckets: [bucket] };
  }

  // A belt/pipe group (a per-mark line bucket from collectSplinePathGroups) as
  // a leaf row; the caller places it into the group's build-menu
  // category/subcategory. Keeps the full label ("Conveyor Belt Mk.3") rather
  // than a bare "Mk.3", since it now sits among unrelated leaf rows.
  function beltPipeRow(keyPrefix, color, group) {
    return lineRowFromData(keyPrefix + group.mark, group.label, null, color, group);
  }

  function byCountDesc(a, b) { return b.count - a.count; }

  // Renders one top-level category from a { subOrder, subs, loose } bundle of
  // rows (see buildBuildingCategorySections). A category with any populated
  // subcategory renders as collapsible sub-groups (with any loose,
  // no-subcategory rows as leaves underneath); a category with only loose rows
  // renders as a flat list. Empty categories render nothing.
  function renderCategorySection(navList, detailPane, category, data) {
    var color = BUILDING_CATEGORY_COLORS[category] || BUILDING_CATEGORY_COLORS.Unknown;
    var usedSubs = data.subOrder.filter(function(sub) { return data.subs[sub].length > 0; });
    var looseRows = data.loose.slice().sort(byCountDesc);

    var total = looseRows.reduce(function(s, r) { return s + r.count; }, 0);
    usedSubs.forEach(function(sub) { data.subs[sub].forEach(function(r) { total += r.count; }); });
    if (total === 0) {
      return;
    }

    if (usedSubs.length === 0) {
      renderTopLevelCategory(navList, detailPane, category + " (" + total + ")", "rect", color, looseRows);
      return;
    }

    renderTopLevelCategory(navList, detailPane, category + " (" + total + ")", "rect", color, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      usedSubs.forEach(function(sub) {
        var rows = data.subs[sub].slice().sort(byCountDesc);
        var subTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(childrenDiv, sub + " (" + subTotal + ")", "rect", color, rows, { startCollapsed: true });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      // Rows whose typePath carried no subcategory sit directly under the
      // category, after the named subcategories.
      looseRows.forEach(function(row) {
        checkboxes.push(appendLeafRow(childrenDiv, row, "rect", color));
        allBuckets = allBuckets.concat(row.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    });
  }

  // The whole filter tree of placed buildables, grouped by the build-menu
  // category/subcategory each typePath maps to (order from payload.menuOrder,
  // built from game_data/generated/buildingCategories.json). Point/rect buildings,
  // per-mark belts/pipes/vehicle-paths, and the whole-line kinds (power lines/
  // railroads/hypertubes) are all folded into one category -> subcategory
  // -> rows structure; any typePath not in the build menu lands in "Unknown".
  function buildBuildingCategorySections(navList, detailPane, payload) {
    // catData[category] = { subOrder: [subName,...], subSeen: {}, subs: {subName: [rows]}, loose: [rows] }
    var catData = {};
    var catOrder = [];
    function ensureCat(category) {
      if (!catData[category]) {
        catData[category] = { subOrder: [], subSeen: {}, subs: {}, loose: [] };
        catOrder.push(category);
      }
      return catData[category];
    }
    function ensureSub(category, sub) {
      var data = ensureCat(category);
      if (!data.subSeen[sub]) {
        data.subSeen[sub] = true;
        data.subOrder.push(sub);
        data.subs[sub] = [];
      }
      return data.subs[sub];
    }
    // Seed the category/subcategory order from the build menu so the sidebar
    // reads in the same order as the in-game build menu. "Unknown" isn't in
    // the menu, so it's created on demand below and therefore always sorts last.
    (payload.menuOrder || []).forEach(function(entry) {
      ensureCat(entry.category);
      (entry.subcategories || []).forEach(function(sub) { ensureSub(entry.category, sub); });
    });

    function addRow(category, sub, row) {
      if (sub) {
        ensureSub(category, sub).push(row);
      } else {
        ensureCat(category).loose.push(row);
      }
    }

    payload.buildingCategories.forEach(function(categoryEntry) {
      var category = categoryEntry.category;
      var color = BUILDING_CATEGORY_COLORS[category] || BUILDING_CATEGORY_COLORS.Unknown;
      var drawPriority = DRAW_PRIORITY_BY_CATEGORY[category] || 0;
      // Group by (subcategory, merged label) first so same-shape/different-
      // material typeEntries (see mergedMaterialLabel) collapse into one row
      // instead of one row per material skin.
      var mergedGroups = {};
      var mergedOrder = [];
      categoryEntry.types.forEach(function(typeEntry) {
        var mergedLabel = mergedMaterialLabel(typeEntry.label);
        var key = typeEntry.subcategory + " " + mergedLabel;
        if (!mergedGroups[key]) {
          mergedGroups[key] = { subcategory: typeEntry.subcategory, mergedLabel: mergedLabel, entries: [] };
          mergedOrder.push(key);
        }
        mergedGroups[key].entries.push(typeEntry);
      });
      mergedOrder.forEach(function(key) {
        var g = mergedGroups[key];
        var row = mergedBuildingRow(g.mergedLabel, g.entries, color, drawPriority, category);
        row.subcategory = g.subcategory;
        buildingSearchEntries.push(row);
        addRow(category, g.subcategory, row);
      });
    });

    // Per-mark belts/pipes, and per-tier vehicle paths (Explorer/FactoryCart/
    // Tractor/Truck/Universal Vehicle Path -- five distinct buildables, each
    // its own toggleable line bucket), placed by the category/subcategory
    // sav_map_data attached to each group.
    (payload.belts || []).forEach(function(group) {
      addRow(group.category || "Unknown", group.subcategory, beltPipeRow("line:belt:", LINE_COLORS.belts, group));
    });
    (payload.pipes || []).forEach(function(group) {
      addRow(group.category || "Unknown", group.subcategory, beltPipeRow("line:pipe:", LINE_COLORS.pipelines, group));
    });
    (payload.vehiclePaths || []).forEach(function(group) {
      addRow(group.category || "Unknown", group.subcategory, beltPipeRow("line:vehiclePath:", LINE_COLORS.vehiclePaths, group));
    });

    // Whole-line kinds (power lines, railroads, hypertubes).
    ["powerLines", "railroads", "hypertubes"].forEach(function(key) {
      var lineData = payload.lines[key];
      if (!lineData || lineData.polylines.length === 0) {
        return;
      }
      addRow(lineData.category || "Unknown", lineData.subcategory, lineRow(key, payload.lines));
    });

    catOrder.forEach(function(category) {
      renderCategorySection(navList, detailPane, category, catData[category]);
    });
  }

  // Every placed/discoverable thing in the save, across every bucket kind --
  // buildings (incl. lightweight foundations/walls/ramps), resource nodes,
  // collectables, hard drives, and line segments (belts/pipelines/
  // railroads/hypertubes/power lines each count their own polylines).
  function computeTotalObjectCount(payload) {
    var total = 0;
    payload.buildingCategories.forEach(function(cat) {
      cat.types.forEach(function(t) { total += pointCount(t.points, 4); });
    });
    payload.resourceNodes.byResourceType.forEach(function(r) {
      ["mined", "unmined"].forEach(function(state) {
        Object.values(r[state].byPurity).forEach(function(p) { total += pointCount(p.points, 3); });
      });
    });
    total += pointCount(payload.players.points, 3);
    (payload.creatures || []).forEach(function(creatureType) { total += pointCount(creatureType.points, 3); });
    (payload.vehicles || []).forEach(function(vehicleType) { total += pointCount(vehicleType.points, 3); });
    total += pointCount(payload.hub.points, 3);
    Object.keys(payload.collectables).forEach(function(key) {
      var c = payload.collectables[key];
      total += pointCount(c.remaining, 3) + pointCount(c.collected, 3);
    });
    ["hasDrive", "empty", "dismantled"].forEach(function(key) {
      total += pointCount(payload.hardDrives[key], 3);
    });
    (payload.droppedItems || []).forEach(function(itemEntry) { total += pointCount(itemEntry.points, 3); });
    Object.keys(payload.lines).forEach(function(key) {
      total += payload.lines[key].polylines.length;
    });
    (payload.belts || []).forEach(function(group) { total += group.polylines.length; });
    (payload.pipes || []).forEach(function(group) { total += group.polylines.length; });
    (payload.vehiclePaths || []).forEach(function(group) { total += group.polylines.length; });
    return total;
  }

  Filters.build = function(payload) {
    var navList = document.getElementById("categoryNavColumn");
    var detailPane = document.getElementById("categoryDetailPane");
    navList.innerHTML = "";
    detailPane.innerHTML = "";
    categoryEntries = [];
    buildingSearchEntries = [];
    bucketLayerCheckbox = {};
    bucketLayerLabel = {};
    bucketCategoryCheckbox = {};
    bucketCategoryLabel = {};
    MapApp.layer.clearBuckets();

    buildResourceNodeSection(navList, detailPane, payload);
    buildBuildingCategorySections(navList, detailPane, payload);
    buildVehiclesSection(navList, detailPane, payload);
    buildHubSection(navList, detailPane, payload);
    buildEntitiesSection(navList, detailPane, payload);
    buildCollectablesSection(navList, detailPane, payload);
    buildDroppedItemsSection(navList, detailPane, payload);

    // Fit the nav panel to the category labels now that they all exist.
    autoSizeNavPanel();

    // Nothing selected on a fresh load -- the whole detail column stays
    // hidden (see deselectAllCategories) until the user actually clicks a
    // category in the nav column.
    deselectAllCategories();

    var totalEl = document.getElementById("totalObjectCount");
    if (totalEl) {
      totalEl.innerHTML = "";
      totalEl.appendChild(el("span", "totalObjectCountValue", computeTotalObjectCount(payload).toLocaleString()));
      totalEl.appendChild(el("span", "totalObjectCountLabel", " objects loaded"));
    }

    // Fresh buckets from clearBuckets() above have no hiddenIndices yet --
    // hides the "Restore N hidden objects" button left over from whatever
    // was hidden in the previous save.
    Filters.refreshHiddenObjectsIndicator();

    MapApp.layer.requestRedraw();
  };

  // "Check all" / "Uncheck all" -- every checkbox at every nesting level
  // (top-level sections, subcategories, and leaf rows) is a real DOM
  // checkbox somewhere under #sidebar (nav column rows + every category's
  // detail content, selected or not), so setting all of them plus every
  // bucket covers the whole tree in one pass without needing to walk the
  // group structure itself. Recorded into savedVisibility too, same as any
  // other toggle (see the row/parent checkbox handlers above), so it
  // survives a reload. Both live in the nav column's header (not the detail
  // pane) since they act globally, across every category -- not just
  // whichever one happens to be selected. Excludes the save-file <select>
  // etc. in the footer, which have no checkboxes, so scoping to #sidebar is
  // safe even though the footer now lives inside the nav panel.
  function setAllVisibility(checked) {
    var sidebar = document.getElementById("sidebar");
    var checkboxes = sidebar.querySelectorAll("input[type=checkbox]");
    for (var i = 0; i < checkboxes.length; i++) {
      checkboxes[i].checked = checked;
    }
    MapApp.layer.buckets.forEach(function(bucket) {
      bucket.visible = checked;
      savedVisibility[bucket.key] = checked;
    });
    MapApp.layer.requestRedraw();
  }

  var checkAllButton = document.getElementById("checkAllButton");
  if (checkAllButton) {
    checkAllButton.addEventListener("click", function() { setAllVisibility(true); });
  }
  var uncheckAllButton = document.getElementById("uncheckAllButton");
  if (uncheckAllButton) {
    uncheckAllButton.addEventListener("click", function() { setAllVisibility(false); });
  }

  // Individually-hidden objects (see MapApp.hideObject) aren't tied to any
  // sidebar checkbox, so "Check all" doesn't reach them -- this is the only
  // way to undo one short of reloading the save. Hidden entirely (rather
  // than just disabled) when there's nothing to reset, matching how e.g.
  // #sftpPanel/#gameSettingsPanel/#altitudePanel only appear once relevant.
  var resetHiddenButton = document.getElementById("resetHiddenButton");
  Filters.refreshHiddenObjectsIndicator = function() {
    if (!resetHiddenButton) {
      return;
    }
    var count = MapApp.countHiddenObjects();
    if (count === 0) {
      resetHiddenButton.style.display = "none";
      return;
    }
    resetHiddenButton.textContent = "Restore " + count.toLocaleString() + " hidden object" + (count === 1 ? "" : "s");
    resetHiddenButton.style.display = "block";
  };
  if (resetHiddenButton) {
    resetHiddenButton.addEventListener("click", function() {
      MapApp.resetHiddenObjects();
      Filters.refreshHiddenObjectsIndicator();
    });
  }

  // ---- Right-click context menu support (see ContextMenu in contextmenu.js) --

  // Labels for a bucket's "layer" (its sidebar row) and "category" (its
  // top-level section), for the context menu to show without needing to know
  // anything about the sidebar tree itself.
  Filters.contextInfo = function(bucket) {
    return {
      layerLabel: bucketLayerLabel[bucket.key] || bucket.label,
      categoryLabel: bucketCategoryLabel[bucket.key] || null,
    };
  };

  // Hides every bucket the clicked object's sidebar row controls, by
  // flipping that row's real checkbox -- reuses its existing "change"
  // listener (see appendLeafRow) rather than duplicating the
  // bucket-visibility/savedVisibility bookkeeping here. A no-op if the row
  // is already hidden.
  Filters.hideLayer = function(bucket) {
    var checkbox = bucketLayerCheckbox[bucket.key];
    if (checkbox && checkbox.checked) {
      checkbox.click();
    }
  };

  // Same idea, one level up -- flips the whole top-level category's master
  // checkbox (see renderGroup's parentCheckbox), which already cascades to
  // every nested subcategory/row/bucket underneath it.
  Filters.hideCategory = function(bucket) {
    var checkbox = bucketCategoryCheckbox[bucket.key];
    if (checkbox && checkbox.checked) {
      checkbox.click();
    }
  };
})();
