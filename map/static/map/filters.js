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
// Spawners (creature spawn markers, grouped by family), and Dropped Items
// (loose ground stacks) are their own separate sections. renderGroup() supports the
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

  // No FGItemDescriptor (above), but a Desc_ entry in readableNameCorrections
  // all the same -- so "Hard Drive" IS in the searchable item catalog (see the
  // rust core's list_searchable_items), and this is the key its crash-site
  // rows get registered under for the search bar's eye (see
  // collectableToggleRows).
  var HARD_DRIVE_ITEM_CLASS = "Desc_HardDrive_C";

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

  // Wildlife/enemy species -- both the spawn markers (see
  // buildSpawnersSection) and the live tamed creatures under Entities are
  // drawn with the game's own per-species art, keyed by class name
  // (icons/creatures/<Char_*_C>.png -- see game_data/extractors/copy_icons.py).
  // The two deliberately share that artwork: a tamed doggo IS the same
  // animal as the doggo its spawner produces, and the pin's heart badge (see
  // map.js's _paintPin) is what says "this one is yours", rather than a
  // second, unrelated-looking icon.
  var CREATURE_COLOR = "#c9a35c";
  var CREATURE_ICON_BASE = "icons/creatures/";

  function creatureIconUrl(iconClass) {
    return CREATURE_ICON_BASE + encodeURIComponent(iconClass) + ".png";
  }

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
    powerLines: "#ffe119",
    belts: "#f58231",
    pipelines: "#3cb44b",
    railroads: "#ffffff",
    hypertubes: "#4363d8",
    vehiclePaths: "#a9a9a9",
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

  // Same-thing-to-the-user variants merged into one sidebar row, by
  // stripping a known label suffix so they land in the same merged group:
  //
  // - Material skins on lightweight buildables (foundations/walls/ramps/
  //   beams) -- e.g. "Foundation 4m (Asphalt)"/"(Concrete)"/"(Metal)"/
  //   "(Polished Concrete)" are the exact same shape/size as "Foundation
  //   4m", just different paint, and would otherwise be 5 separate rows.
  //   Only suffixes confirmed to be pure material/skin are stripped --
  //   other parenthetical suffixes in the readable-name data (e.g.
  //   "(Window)", "(No Indicator)", "(1 m)") indicate a genuinely
  //   different shape or size and must NOT be merged away.
  // - " on Lift": the game silently swaps a splitter/merger placed on a
  //   conveyor lift to a distinct Build_*Lift_C class ("Conveyor Splitter
  //   on Lift", ...). Functionally it IS that splitter/merger; showing it
  //   as a second object type just reads as noise.
  //
  // Applied across every building category since it's a no-op for any label
  // that never carries one of these suffixes. Tooltips/selection keep each
  // bucket's own full label, so the variant is still identifiable per object.
  var MERGED_LABEL_SUFFIXES = [" (Asphalt)", " (Concrete)", " (Polished Concrete)", " (Metal)", " on Lift"];

  function mergedRowLabel(label) {
    for (var i = 0; i < MERGED_LABEL_SUFFIXES.length; i++) {
      var suffix = MERGED_LABEL_SUFFIXES[i];
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
  // so a visibility choice made here survives a same-file auto-refresh (see
  // data.js's checkForNewerSave), switching to an entirely different save,
  // and (via localStorage) closing the tab entirely: the filter you set up
  // for your factory is still applied next visit. Only explicitly-toggled
  // keys are stored, so a kind nobody ever touched defaults to visible --
  // unless an enclosing group was toggled, which stores group-level state
  // that new kinds inherit (see the hierarchical persistence below).
  var VISIBILITY_STORAGE_KEY = "smapSavedVisibility";
  var savedVisibility = {};
  try {
    savedVisibility = JSON.parse(localStorage.getItem(VISIBILITY_STORAGE_KEY)) || {};
  } catch (e) { /* corrupt/blocked storage: start fresh */ }
  // ---- Hierarchical persistence -------------------------------------------
  // A group toggle is stored as ONE "group:<path>" entry (path = the group
  // titles from the top-level section down, counts stripped, "/"-joined)
  // rather than one entry per bucket the group happened to contain in that
  // save. Per-bucket entries only exist for rows toggled *after* the
  // enclosing group's last toggle -- the group toggle deletes them, having
  // overridden them anyway (see setCheckedDeep). Restoring resolves
  // most-specific-first: explicit bucket entry, innermost enclosing group,
  // ..., outermost, the "*" entry Check/Uncheck-all writes, then visible.
  // This is what makes a category toggled off stay WHOLLY off in a different
  // save: object kinds that didn't exist when the category was toggled have
  // no bucket entry, so they inherit the category's stored state instead of
  // defaulting back to visible and leaving the category partially shown.
  //
  // The stack tracks the group ancestry while renderGroup builds its
  // children (building is fully synchronous) -- appendLeafRow reads it to
  // restore, renderGroup itself to know its own path.
  var groupPathStack = [];

  function groupTitleKey(title) {
    // "Production (1,234)" / "Hard Drives (3/12)" -> stable, count-free key.
    return title.replace(/\s*\([\d,\/]+\)\s*$/, "");
  }

  // Top-level categories hidden unless explicitly enabled: the world layers
  // -- every resource node/well, every collectable, every creature spawner,
  // every loose ground stack -- are thousands of pins that buried the
  // player's own factory.
  // An explicit choice scoped to the category still wins and persists (a
  // per-bucket entry, or a stored group toggle at any level inside it), but
  // Check-all's blanket "*" entry deliberately does NOT reach these: it
  // predates whatever category was added since (or was stored to mean "my
  // factory", not "the whole planet"), and letting it through re-buried the
  // map on the next load. Check all still shows everything for the current
  // session -- it flips the buckets directly, this only governs restore.
  var DEFAULT_HIDDEN_CATEGORIES = {
    "Resource Nodes": true,
    "Resource Wells": true,
    "Collectables": true,
    "Spawners": true,
    "Dropped Items": true,
    "World": true,
  };

  function savedGroupStateForStack() {
    for (var i = groupPathStack.length; i > 0; i--) {
      var key = "group:" + groupPathStack.slice(0, i).join("/");
      if (savedVisibility.hasOwnProperty(key)) {
        return savedVisibility[key];
      }
    }
    if (DEFAULT_HIDDEN_CATEGORIES[groupPathStack[0]]) {
      return false;
    }
    if (savedVisibility.hasOwnProperty("*")) {
      return savedVisibility["*"];
    }
    return true;
  }

  var persistTimer = null;
  function persistVisibility() {
    // Debounced: "Uncheck all" writes hundreds of keys in one burst.
    clearTimeout(persistTimer);
    persistTimer = setTimeout(function() {
      try {
        localStorage.setItem(VISIBILITY_STORAGE_KEY, JSON.stringify(savedVisibility));
      } catch (e) { /* storage full/blocked: filters still work for the session */ }
    }, 250);
  }

  // Every group's master checkbox, with the flattened bucket list it covers --
  // so any visibility change (a leaf toggle, check/uncheck-all, a context-menu
  // hide) can resync every ancestor's checked/indeterminate state from the
  // buckets' actual visibility instead of trusting it was kept up to date.
  // Rebuilt in Filters.build alongside the buckets themselves.
  var groupCheckboxStates = [];
  function refreshGroupCheckboxes() {
    groupCheckboxStates.forEach(function(entry) {
      var visibleCount = 0;
      entry.buckets.forEach(function(bucket) { if (bucket.visible) visibleCount++; });
      entry.checkbox.checked = visibleCount > 0;
      entry.checkbox.indeterminate = visibleCount > 0 && visibleCount < entry.buckets.length;
    });
  }

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

  // Raw world-space [x, y] for the tooltip's Coordinates row, recovered from
  // a stride-3 [x, y, z] points array. The payload used to ship parallel
  // worldPositions arrays; the projection is exactly invertible, so they were
  // dropped from the payload (slim_payload_value in mapdata/mod.rs).
  function worldPositionAt(points, index) {
    if (!points) {
      return undefined;
    }
    return EditorTool.mapPxToWorldXY(points[index * 3], points[index * 3 + 1]);
  }

  function makeIconBucket(key, label, color, points, ids, tooltipKind, tooltipInfo, url, opacity, pinFillColor, pointStride) {
    return MapApp.layer.addBucket({
      key: key, label: label, color: color, visible: true,
      renderType: "icon",
      // Almost always 3 ([x, y, z]); vehicles pass 4 so their pin bucket can
      // share the exact same [x, y, yaw, z] points array as their box bucket
      // (every icon consumer -- draw, grid, hit-test -- reads x/y directly
      // and altitude at stride-1, so the extra yaw column is just skipped).
      pointStride: pointStride || 3,
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
      // [x, y, arriveTangentX, arriveTangentY, leaveTangentX, leaveTangentY, z]
      // (belts/pipelines/railroads/hypertubes -- enough to draw the real
      // curve through each spline point). z is LAST so altitude reads at
      // stride-1 work for both strides; see map.js line ~22 and the tangent
      // reads in _drawLineBucket -- do NOT reorder to put z third.
      pointStride: pointStride || 3,
      lines: polylines.map(function(line) { return new Float32Array(line); }),
      ids: ids || null,
      tooltipKind: tooltipKind || "none",
      tooltipInfo: tooltipInfo || null,
    });
  }

  var el = UI.el;
  var makeToggle = UI.toggle;

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
    // back to the enclosing groups' stored state (see the hierarchical-
    // persistence comment above) unless a previous visit explicitly recorded
    // otherwise for one of them; in practice they're always toggled together.
    var restoredVisible = row.buckets.reduce(function(acc, bucket) {
      return savedVisibility.hasOwnProperty(bucket.key) ? savedVisibility[bucket.key] : acc;
    }, savedGroupStateForStack());
    checkbox.checked = restoredVisible;
    row.buckets.forEach(function(bucket) { bucket.visible = restoredVisible; });
    checkbox.addEventListener("change", function() {
      row.buckets.forEach(function(bucket) {
        bucket.visible = checkbox.checked;
        savedVisibility[bucket.key] = checkbox.checked;
      });
      persistVisibility();
      refreshGroupCheckboxes();
      MapApp.layer.requestRedraw();
    });
    rowDiv.appendChild(rowToggle.wrapper);
    rowDiv.appendChild(makeIcon(row.renderType || renderType, row.color || swatchColor, row.iconUrl));
    var labelEl = el("label", null, row.displayLabel || row.label);
    // row.hint: the long version of a caveat the row's label can only gesture
    // at (see the Caves row's "approximate"), shown on hover.
    if (row.hint) {
      labelEl.title = row.hint;
    }
    rowDiv.appendChild(labelEl);
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

    // On the ancestry stack while the children build, so appendLeafRow (here
    // or in any nested renderGroup) restores against the right group path.
    groupPathStack.push(groupTitleKey(title));
    var groupPath = groupPathStack.join("/");

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
    groupPathStack.pop();

    group.appendChild(childrenDiv);
    container.appendChild(group);

    // Lets an ancestor group's parentCheckbox recurse into this group's
    // children via setCheckedDeep without dispatching events (see above).
    parentCheckbox._childCheckboxes = childCheckboxes;

    // The master checkbox's initial state comes from the buckets' actual
    // (possibly savedVisibility-restored) state, NOT a hardcoded "checked" --
    // a hardcoded default desynced the sidebar after a second save load:
    // every group read as ON while the restored leaf state kept things
    // hidden. Registered so later toggles keep resyncing it (see
    // refreshGroupCheckboxes).
    groupCheckboxStates.push({ checkbox: parentCheckbox, buckets: allBuckets });
    var visibleCount = 0;
    allBuckets.forEach(function(bucket) { if (bucket.visible) visibleCount++; });
    parentCheckbox.checked = allBuckets.length === 0 || visibleCount > 0;
    parentCheckbox.indeterminate = visibleCount > 0 && visibleCount < allBuckets.length;

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
      // One stored "group:" entry for the whole toggle; the per-bucket and
      // nested-group entries it just overrode are deleted rather than
      // rewritten, so a future save's NEW kinds under this group inherit
      // this state too (see the hierarchical-persistence comment above).
      allBuckets.forEach(function(bucket) {
        bucket.visible = checked;
        delete savedVisibility[bucket.key];
      });
      var descendantPrefix = "group:" + groupPath + "/";
      Object.keys(savedVisibility).forEach(function(key) {
        if (key.indexOf(descendantPrefix) === 0) {
          delete savedVisibility[key];
        }
      });
      savedVisibility["group:" + groupPath] = checked;
      persistVisibility();
      refreshGroupCheckboxes();
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

  // Vehicle counterpart of buildingSearchEntries: one row per vehicle type in
  // the save, plus the single "Train" row (whole consists -- individual
  // locomotives/freight cars are deliberately not searchable, matching how
  // the sidebar groups them). Same row objects the Vehicles sidebar section
  // renders, so entry.row.checkbox is live here too.
  var vehicleSearchEntries = [];
  Filters.getVehicleSearchEntries = function() { return vehicleSearchEntries; };
  Filters.vehicleColor = function() { return VEHICLE_COLOR; };

  // Wildlife rows -- every creature-spawner species (see buildSpawnersSection)
  // plus the tamed creatures under Entities. Unlike buildings/vehicles there's
  // no per-type info endpoint to open a modal against, so these are searchable
  // purely to find and toggle the layer; each row carries its own live sidebar
  // checkbox, exactly like the building rows do.
  var wildlifeSearchEntries = [];
  Filters.getWildlifeSearchEntries = function() { return wildlifeSearchEntries; };

  // One entry per resource node / resource well type (see
  // buildResourceEntryGroup), covering all of that resource's purity x
  // mined/unmined leaf rows at once -- the sidebar has no single "every iron
  // node" toggle either, and the purity split is exactly what makes a
  // resource worth looking up ("how many pure iron nodes are still free?").
  var resourceSearchEntries = [];
  Filters.getResourceSearchEntries = function() { return resourceSearchEntries; };
  // The color the resource groups' own headers use -- a resource spans three
  // purity colors, so no single row's color represents it.
  Filters.resourceColor = function() { return PURITY_COLORS.NORMAL; };

  // Item ClassName -> the Collectables sidebar rows for that same pickup
  // (Remaining/Collected, or a hard drive's three states -- see
  // buildCollectablesSection/buildHardDrivesGroup). A handful of items -- the
  // three slugs, Somersloop, Mercer Sphere, Hard Drive -- are ALSO world
  // pickups with a map layer of their own, so "Mercer Sphere" in the search
  // bar honestly means two things: what's stashed in inventories (the item
  // search's answer) and what's still lying out there (this layer). Rather
  // than a second, near-identical dropdown entry for the layer, the one item
  // suggestion gets the same show/hide eye a layer row has -- driving these
  // real sidebar checkboxes, never a second source of truth. Absent for every
  // other item, which is what makes the eye appear on exactly these six rows
  // and nowhere else.
  var collectableToggleRows = {};
  Filters.getCollectableToggleRows = function(itemPath) {
    return collectableToggleRows[itemPath] || null;
  };

  // Whole-category search entries: a handful of logistics families whose rows
  // are scattered across build-menu subcategories (one row per mark), so
  // "show me every belt" isn't a single toggle anywhere in the sidebar. Each
  // entry drives the real per-mark row checkboxes -- never a second source of
  // truth. Icons are the family's Mk.1 buildable, the recognisable one.
  //
  // Belts and pipes are spline (line) rows; conveyor LIFTS are not -- a lift
  // is a vertical structure the parser draws as a building box, not a spline
  // (see the rust core's conveyor_belt_only_type_paths), so its rows are
  // ordinary merged building rows and are picked up by class name instead.
  var LAYER_CATEGORY_DEFS = [
    { key: "belts", label: "Conveyor Belts", iconClassName: "Build_ConveyorBeltMk1_C" },
    { key: "lifts", label: "Conveyor Lifts", iconClassName: "Build_ConveyorLiftMk1_C" },
    { key: "pipes", label: "Pipelines", iconClassName: "Build_Pipeline_NoIndicator_C" },
  ];
  // Anchored on the Mk-numbered lift classes specifically: "Lift" alone also
  // matches Build_FoundationPassthrough_Lift_C, which is a hole in a floor.
  var CONVEYOR_LIFT_CLASS_PATTERN = /Build_ConveyorLiftMk\d/;
  var layerCategoryRows = { belts: [], lifts: [], pipes: [] };
  Filters.getLayerCategorySearchEntries = function() {
    return LAYER_CATEGORY_DEFS
      .filter(function(def) { return (layerCategoryRows[def.key] || []).length > 0; })
      .map(function(def) {
        return { label: def.label, iconClassName: def.iconClassName, rows: layerCategoryRows[def.key] };
      });
  };

  // Sizes the dock to fit the widest category card instead of a fixed guess,
  // so it wastes no horizontal space (and the map gets the rest). Measured by
  // momentarily letting the list size to its content -- each card's label has
  // flex:1, so at max-content it collapses to the label's natural
  // (un-stretched) width, making the dock exactly as wide as its longest row.
  // Clamped so the save panel and the Check/Uncheck header stay usable at the
  // low end and the map never loses an absurd amount at the high end.
  // Leaflet is not poked here: panels.js watches the map element itself.
  function autoSizeNavPanel() {
    // A hand-dragged width (see panels.js's resize handle, persisted across
    // sessions) always wins over the automatic fit -- otherwise every save
    // load would snap the dock back and silently undo the user's resize.
    if (window.Panels && Panels.storedNavWidth() !== null) {
      return;
    }
    var navColumn = document.getElementById("categoryNavColumn");
    if (!navColumn || navColumn.children.length === 0) {
      return;
    }
    var previous = navColumn.style.width;
    navColumn.style.width = "max-content";
    var natural = navColumn.offsetWidth;
    navColumn.style.width = previous;
    var width = Math.max(248, Math.min(natural + 24, 380));
    document.documentElement.style.setProperty("--dock-left-width", width + "px");
  }

  // Selection now drives the dock's two-pane push navigation (panels.js)
  // rather than opening a second column: the dock keeps its width, so
  // browsing categories never resizes the map.
  function deselectAllCategories() {
    categoryEntries.forEach(function(entry) {
      entry.navRow.classList.remove("active");
      entry.detailGroup.classList.remove("active");
    });
    Panels.showCategoryList();
  }

  function selectCategory(navRow, detailGroup) {
    var selected = null;
    categoryEntries.forEach(function(entry) {
      var isThis = entry.navRow === navRow;
      entry.navRow.classList.toggle("active", isThis);
      entry.detailGroup.classList.toggle("active", isThis);
      if (isThis) {
        selected = entry;
      }
    });
    Panels.showCategoryDetail(selected ? selected.title : "",
                              selected ? selected.color : null);
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
    // Disclosure chevron at the far right edge (after the toggle switch):
    // these rows push the dock to the category's contents, but nothing about
    // a colour chip + label + switch said "openable" -- rows read as pure
    // visibility toggles without it.
    titleRow.appendChild(UI.chevron(14, "chev-right navChevron"));
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

    // See bucketCategoryCheckbox's doc comment above -- `title` carries a
    // trailing " (1,234)" total count (most call sites) that reads oddly
    // repeated in a right-click menu, so it's stripped for display only;
    // the checkbox itself doesn't care either way.
    var cleanTitle = title.replace(/\s*\([\d,]+\)\s*$/, "");

    // title/color travel with the entry so the detail pane's header can name
    // the category you pushed into, in its own colour.
    categoryEntries.push({
      navRow: titleRow,
      detailGroup: detailGroup,
      title: cleanTitle,
      color: renderType === "icon" ? null : swatchColor,
    });
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

  // The purity order the map itself reads in (best first) -- used to rank the
  // purity rows so the search modal's distribution bars read Pure -> Normal
  // -> Impure rather than in whatever order the payload happened to list them.
  var PURITY_ORDER = ["PURE", "NORMAL", "IMPURE", "UNKNOWN"];

  // One resource (e.g. "Crude Oil") -> Mined/Unmined subgroups -> purity
  // rows nested inside each, instead of one flat list of 6 "Unmined, Pure" /
  // "Mined, Pure" / etc. rows -- mined vs. unmined is the choice that
  // actually matters when deciding what to look at, so it gets to be the
  // grouping level, with purity as the detail nested underneath it.
  //
  // `section` is the sidebar heading ("Resource Nodes"/"Resource Wells"); the
  // leaf rows it produces are also registered as ONE search entry for the
  // whole resource, so "iron" reaches the node layer and not just the item
  // (see resourceSearchEntries).
  function buildResourceEntryGroup(childrenDiv, resourceEntry, section) {
    var url = resourceIconUrl(resourceEntry.resourceType);
    var searchRows = [];
    // Crude Oil exists BOTH as ordinary nodes and as resource wells, under the
    // same resourceType -- so keying purely on that gave the oil wells the
    // exact same bucket keys as the oil nodes. Keys are the identity behind
    // saved visibility, the right-click "Hide layer/category" lookup and the
    // search isolate's save/restore, so the two layers were quietly sharing
    // all three. Wells get their own namespace; plain nodes keep the original
    // key so nobody's stored filters reset for the other 12 resources.
    var keyPrefix = resourceEntry.isWell ? "node:well:" : "node:";
    var result = renderGroup(childrenDiv, stripWellSuffix(resourceEntry.label), "icon", PURITY_COLORS.NORMAL, function(stateChildrenDiv) {
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
          var tooltipInfo = function(index) {
            return { title: resourceEntry.label, rows: [["Purity", purityLabel], ["Status", stateLabel]],
                     position: worldPositionAt(purityData.points, index) };
          };
          var bucket = makeIconBucket(
            keyPrefix + resourceEntry.resourceType + ":" + state + ":" + purityName, resourceEntry.label,
            purityColor, purityData.points, purityData.ids, "static", tooltipInfo, url, opacity, purityColor);
          // purity/mined tag the row with the two axes the sidebar splits it
          // by, so the search modal can re-aggregate them (total, mined vs
          // untouched, and the purity distribution) without re-reading the payload.
          rows.push({ label: purityLabel, count: count, color: purityColor, buckets: [bucket], iconUrl: url,
                      purityLabel: purityLabel, purityRank: PURITY_ORDER.indexOf(purityName),
                      mined: state === "mined" });
        });
        if (rows.length === 0) {
          return; // No point offering a toggle for an empty Mined/Unmined subgroup.
        }
        searchRows = searchRows.concat(rows);
        var subTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(stateChildrenDiv, stateLabel + " (" + subTotal + ")", "icon", PURITY_COLORS.NORMAL, rows, { startCollapsed: true, iconUrl: url });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: true, iconUrl: url });
    if (searchRows.length > 0) {
      // The label must never collide with the ORE ITEM of the same name, which
      // is in the item catalog too -- "Iron Ore" the thing on a belt and "Iron
      // Ore" the hole in the ground are different answers to the same query.
      // Well entries already carry " (Resource Well)" from the payload (see
      // WELL_LABEL_SUFFIX); node entries get the matching suffix here.
      var label = resourceEntry.isWell ? resourceEntry.label
                                       : resourceEntry.label + " (Resource Node)";
      resourceSearchEntries.push({ label: label, iconUrl: url, section: section, rows: searchRows });
    }
    return result;
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
        var result = buildResourceEntryGroup(childrenDiv, resourceEntry, title);
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
        var position = worldPositionAt(points, index);
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
    return { total: total, rows: rows, result: renderGroup(childrenDiv, title, "icon", HARD_DRIVE_COLORS.hasDrive, rows, { startCollapsed: true, iconUrl: url }) };
  }

  function buildCollectablesSection(navList, detailPane, payload) {
    var collectables = payload.collectables;
    // itemPath: the pickup's own item descriptor ClassName -- the key the item
    // search looks these rows up by (see collectableToggleRows). Same classes
    // the rust core's COLLECTABLE_ITEMS table uses.
    var kinds = [
      { key: "slugsBlue", itemPath: "Desc_Crystal_C", label: "Blue Power Slug", color: SLUG_COLORS.slugsBlue },
      { key: "slugsYellow", itemPath: "Desc_Crystal_mk2_C", label: "Yellow Power Slug", color: SLUG_COLORS.slugsYellow },
      { key: "slugsPurple", itemPath: "Desc_Crystal_mk3_C", label: "Purple Power Slug", color: SLUG_COLORS.slugsPurple },
      { key: "somersloops", itemPath: "Desc_WAT1_C", label: "Somersloop", color: SOMERSLOOP_COLOR },
      { key: "mercerSpheres", itemPath: "Desc_WAT2_C", label: "Mercer Sphere", color: MERCER_SPHERE_COLOR },
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
          return { title: kind.label, rows: [["Status", "Remaining"]], position: worldPositionAt(data.remaining, index) };
        };
        var collectedInfo = function(index) {
          return { title: kind.label, rows: [["Status", "Collected"]], position: worldPositionAt(data.collected, index) };
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
        // Registered AFTER renderGroup, so every row already carries the live
        // checkbox appendLeafRow attached (see collectableToggleRows).
        collectableToggleRows[kind.itemPath] = rows;
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      if (hardDriveTotal > 0) {
        var hardDriveGroup = buildHardDrivesGroup(childrenDiv, payload);
        collectableToggleRows[HARD_DRIVE_ITEM_CLASS] = hardDriveGroup.rows;
        checkboxes.push(hardDriveGroup.result.checkbox);
        allBuckets = allBuckets.concat(hardDriveGroup.result.buckets);
      }
      return { buckets: allBuckets, checkboxes: checkboxes };
    });
  }

  // ---- Spawners (creature spawn markers -- static world data) --------------

  // payload.spawners (see the rust core's collect_spawners) is the same for
  // every save: creature spawn markers from the cooked level data, not save
  // actors -- the save's own spawner actors never say which creature they
  // spawn. Pins follow the collectables' design (real icon on a white pin
  // circle), using the per-species creature icons (see creatureIconUrl).
  //
  // Creature families with several variants each get a collapsible subgroup
  // (matched on the class name); everything else (Lizard Doggo, Flightless
  // Birb, the Space Giraffe-Tick-Penguin-Whale Thing) is a loose row after
  // them. First match wins; the patterns are mutually exclusive today.
  var SPAWNER_FAMILIES = [
    { title: "Hogs", pattern: /Hog/ },
    { title: "Crab Hatchers", pattern: /Hatcher/ },
    { title: "Stingers", pattern: /Stinger/ },
    { title: "Spitters", pattern: /Spitter/ },
  ];

  function buildSpawnersSection(navList, detailPane, payload) {
    var spawnerTypes = payload.spawners || [];
    var rowsByFamily = SPAWNER_FAMILIES.map(function() { return []; });
    var looseRows = [];
    var total = 0;
    spawnerTypes.forEach(function(spawnerType) {
      var count = pointCount(spawnerType.points, 3);
      if (count === 0) {
        return;
      }
      total += count;
      var url = creatureIconUrl(spawnerType.typePath);
      // The bucket label says "Spawner" so a rectangle-selection list can't
      // read as live creatures; the sidebar row shows just the species
      // (displayLabel), already sitting under the "Spawners" heading.
      var label = spawnerType.label + " Spawner";
      var tooltipInfo = function(index) {
        return { title: label, rows: [], position: worldPositionAt(spawnerType.points, index) };
      };
      var bucket = makeIconBucket("spawner:" + spawnerType.typePath, label, CREATURE_COLOR,
        spawnerType.points, spawnerType.ids, "static", tooltipInfo, url, 1);
      var row = { label: label, displayLabel: spawnerType.label, count: count,
                  color: CREATURE_COLOR, buckets: [bucket], iconUrl: url };
      for (var i = 0; i < SPAWNER_FAMILIES.length; i++) {
        if (SPAWNER_FAMILIES[i].pattern.test(spawnerType.typePath)) {
          rowsByFamily[i].push(row);
          return;
        }
      }
      looseRows.push(row);
    });
    if (total === 0) {
      return;
    }
    // Searchable by species from the top bar, the same way buildings/vehicles
    // are (see wildlifeSearchEntries) -- "stinger" is a far more natural way
    // to reach that layer than remembering it lives under Spawners > Stingers.
    // Every row here gets a live checkbox from appendLeafRow below.
    var spawnerRows = [];
    rowsByFamily.forEach(function(rows) { spawnerRows = spawnerRows.concat(rows); });
    spawnerRows = spawnerRows.concat(looseRows);
    // `section` is the sidebar heading the row lives under -- the search
    // modal shows it as the entry's category chip, and tells "N spawn points"
    // from "N live creatures" by it.
    spawnerRows.forEach(function(row) { row.section = "Spawners"; });
    wildlifeSearchEntries = wildlifeSearchEntries.concat(spawnerRows);
    renderTopLevelCategory(navList, detailPane, "Spawners (" + total + ")", "icon", CREATURE_COLOR, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      SPAWNER_FAMILIES.forEach(function(family, familyIndex) {
        var rows = rowsByFamily[familyIndex];
        if (rows.length === 0) {
          return;
        }
        var familyTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(childrenDiv, family.title + " (" + familyTotal + ")", "icon",
          CREATURE_COLOR, rows, { startCollapsed: true, iconUrl: rows[0].iconUrl });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      looseRows.forEach(function(row) {
        checkboxes.push(appendLeafRow(childrenDiv, row, "icon", CREATURE_COLOR));
        allBuckets = allBuckets.concat(row.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
      // iconUrl: the nav row borrows the first species' own art rather than a
      // made-up generic creature glyph -- whatever this world actually spawns.
    }, { iconUrl: spawnerRows[0].iconUrl });
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
        return {
          title: itemEntry.label,
          rows: [["Amount", itemEntry.counts[index]], ["Status", "On the ground"]],
          position: worldPositionAt(itemEntry.points, index),
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

  // ---- World (the map's own features, not the player's) ---------------------

  // Three static layers the game never draws on its own map, all traced from
  // the cooked level data and identical for every save: the caves, the
  // damaging world border, and the edge of the real water. They share a
  // category because they answer the same kind of question -- "what is the
  // terrain actually like here?" -- and because none of them belongs to the
  // save being viewed.
  //
  // Colors: orchid for caves, red for the thing that damages you, cyan for
  // water. None collides with a line layer (belts orange, pipes green, power
  // yellow, hypertubes blue, rails white, vehicle paths grey).
  var WORLD_COLORS = {
    caves: "#c66bff",
    worldPerimeter: "#ff5a4d",
    waterLimit: "#35d0f5",
  };

  // payload.caves (see the rust core's collect_caves): one line per cave
  // outline ring, all in a single bucket so the whole layer is one checkbox.
  // Outlines rather than fills: the renderer draws lines, and an outline
  // reads better over a factory than 84 translucent blobs. The shape is the
  // game's own cave fog volume plus the cave geometry inside it, so it is
  // generous by tens of meters -- "the cave is in here", not a survey.
  function buildCavesRow(payload) {
    var caves = payload.caves;
    if (!caves || !caves.polylines || caves.polylines.length === 0) {
      return null;
    }
    var labels = caves.labels || [];
    var areas = caves.areas || [];
    var depths = caves.depths || [];
    var tooltipInfo = function(index) {
      var rows = [];
      var area = areas[index];
      if (typeof area === "number") {
        rows.push(["Area", area >= 10000 ? (area / 10000).toFixed(1) + " ha"
                                         : Math.round(area).toLocaleString() + " m²"]);
      }
      var depth = depths[index];
      if (depth && depth.length === 2) {
        rows.push(["Altitude range", Math.round(depth[0]) + " m to " + Math.round(depth[1]) + " m"]);
      }
      // Same caveat as the sidebar row, where someone reading a size off the
      // tooltip will actually see it.
      rows.push(["Outline", "Approximate"]);
      var ring = caves.polylines[index];
      return { title: labels[index] || "Cave", rows: rows,
               position: ring ? EditorTool.mapPxToWorldXY(ring[0], ring[1]) : undefined };
    };
    var bucket = makeLineBucket("caves", "Cave", WORLD_COLORS.caves, caves.polylines,
      caves.ids, "static", tooltipInfo, 3);
    // The count is caves, not rings: a cave whose footprint came out in two
    // touching pieces is still one cave.
    var seen = {};
    var caveCount = 0;
    (caves.ids || []).forEach(function(id) {
      var caveId = String(id).split("#")[0];
      if (!seen[caveId]) {
        seen[caveId] = true;
        caveCount++;
      }
    });
    // "(approximate)" is not modesty, it is the honest reading of the source:
    // an outline is the game's cave fog volume plus the cave geometry inside
    // it, which runs generous by tens of meters and can miss a bare tunnel
    // entirely. Good enough to find a cave, wrong to trust as its wall.
    return { label: "Caves", displayLabel: "Caves (approximate)",
             hint: "Traced from the game's own cave fog volumes and the cave geometry inside "
                   + "them — typically tens of meters wider than the real rock, and a "
                   + "bare tunnel with no cave props can be missed. Use it to find a cave, "
                   + "not to judge where its walls are.",
             count: caveCount, color: WORLD_COLORS.caves,
             renderType: "line", buckets: [bucket] };
  }

  // payload.mapLimits (see the rust core's collect_map_limits): the world
  // border is where the out-of-bounds damage volumes start; the water limit
  // is where swimmable, extractor-valid water ends -- which is well inside
  // the ocean the game draws, so on the west side there is a 334 m strip
  // that looks like sea and holds no water at all. One row per ring, so the
  // two toggle independently.
  //
  // Every ring is drawn at sea level: a line's z only feeds the altitude
  // filter and depth sorting, and these are limits rather than structures --
  // see the collector for why that beats their real span.
  function buildLimitRows(payload) {
    var limits = payload.mapLimits;
    if (!limits || !limits.polylines || limits.polylines.length === 0) {
      return [];
    }
    return limits.polylines.map(function(ring, index) {
      // kind, not ids[index]: bulk id arrays come back with the instance-name
      // prefix re-added (see save_client.js's expandPayloadIds), which is
      // fine for the hit-test identity but would wreck a color/key lookup.
      var kind = (limits.kinds || [])[index] || String(index);
      var label = (limits.labels || [])[index] || "Limit";
      var color = WORLD_COLORS[kind] || NEUTRAL_COLOR;
      var detailRows = (limits.rows || [])[index] || [];
      var tooltipInfo = function() {
        return { title: label, rows: detailRows,
                 position: EditorTool.mapPxToWorldXY(ring[0], ring[1]) };
      };
      var bucket = makeLineBucket("limit:" + kind, label, color, [ring],
        [(limits.ids || [])[index] || kind], "static", tooltipInfo, 3);
      return { label: label, count: 1, color: color, renderType: "line",
               buckets: [bucket] };
    });
  }

  function buildWorldSection(navList, detailPane, payload) {
    var rows = [];
    var caveRow = buildCavesRow(payload);
    if (caveRow) {
      rows.push(caveRow);
    }
    rows = rows.concat(buildLimitRows(payload));
    if (rows.length === 0) {
      return;
    }
    renderTopLevelCategory(navList, detailPane, "World", "line", WORLD_COLORS.caves, rows);
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

  // ---- Entities (Players + tamed creatures) -------------------------------

  // Unlike the other icon buckets above, a player's name/inventory isn't
  // known to the client up front -- it requires the same /api/instance
  // round-trip as buildings (see the rust core's describe_instance player
  // branch), hence tooltipKind "server" instead of "static". Creatures use it
  // too, for their pet name and the "(Tamed)" title.
  //
  // payload.creatures holds ONLY tamed creatures (see collect_creatures): an
  // untamed one exists in the save purely because its region happened to be
  // loaded when the player saved, and despawns the moment that region
  // unloads, so plotting it would promise a doggo that isn't really there.
  // Where wild ones live is the Spawners layer's job. A tamed one is
  // permanent, so it gets the species icon with a heart badge (pinBadge) --
  // same animal as its spawner's pin, visibly claimed.
  function buildEntitiesSection(navList, detailPane, payload) {
    var rows = [];

    var players = payload.players;
    var playerCount = pointCount(players.points, 3);
    if (playerCount > 0) {
      var playerBucket = makeIconBucket("players", "Players", PLAYER_COLOR, players.points, players.ids, "server", null, PLAYER_ICON_URL, 1);
      rows.push({ label: "Player", count: playerCount, color: PLAYER_COLOR, buckets: [playerBucket], iconUrl: PLAYER_ICON_URL });
    }

    var creatureRows = [];
    (payload.creatures || []).forEach(function(creatureType) {
      var count = pointCount(creatureType.points, 3);
      if (count === 0) {
        return;
      }
      var url = creatureIconUrl(creatureType.iconClass);
      var bucket = makeIconBucket(
        "creature:" + creatureType.typePath, creatureType.label, CREATURE_COLOR, creatureType.points,
        creatureType.ids, "server", null, url, 1);
      bucket.pinBadge = "heart";
      creatureRows.push({ label: creatureType.label, count: count, color: CREATURE_COLOR, buckets: [bucket], iconUrl: url });
    });
    rows = rows.concat(creatureRows);

    if (rows.length === 0) {
      return;
    }
    // Searchable alongside the spawners -- "doggo" should reach the tamed
    // doggo layer as readily as it reaches the doggo spawners. `section` as
    // in buildSpawnersSection: the row's sidebar heading.
    creatureRows.forEach(function(row) { row.section = "Entities"; });
    wildlifeSearchEntries = wildlifeSearchEntries.concat(creatureRows);
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
      var count = pointCount(vehicleType.points, 4);
      if (count === 0) {
        return;
      }
      var url = VEHICLE_ICON_BASE + encodeURIComponent(vehicleType.icon);
      var buckets = [];
      // The vehicle's actual oriented box (hand-curated size -- see
      // sav_map_data.VEHICLE_FOOTPRINTS_METERS_BY_TYPE_PATH), drawn under
      // the pin. Shares the pin bucket's ids/points, so it's excluded from
      // rectangle selection or every vehicle would be counted twice.
      var boxBucket = null;
      if (vehicleType.footprintPixels) {
        boxBucket = makePointBucket(
          "vehiclebox:" + vehicleType.typePath, vehicleType.label, VEHICLE_COLOR, vehicleType.points,
          "rect", 4, vehicleType.ids, "server", null, vehicleType.footprintPixels);
        boxBucket.excludeFromSelection = true;
        // The road/rail a vehicle drives on sits at essentially the
        // vehicle's own altitude -- without this clearance the path line
        // always won the hover along the box's whole midline (see hitTest's
        // line-vs-box rule). 5m is above any vehicle's own height but well
        // below a genuine bridge deck one clearance level up.
        boxBucket.lineHitClearanceM = 5;
        buckets.push(boxBucket);
      }
      var pinBucket = makeIconBucket(
        "vehicle:" + vehicleType.typePath, vehicleType.label, VEHICLE_COLOR, vehicleType.points,
        vehicleType.ids, "server", null, url, 1, VEHICLE_COLOR, 4);
      buckets.push(pinBucket);
      // Hovering either representation lights up (and keeps visible) both:
      // the box redraws its pin on top of the highlight fill, the pin
      // retraces its box -- see map.js's _redrawHighlight. Same index in
      // both buckets, since they share the exact same points/ids arrays.
      if (boxBucket) {
        boxBucket.companionPinBucket = pinBucket;
        pinBucket.companionBoxBucket = boxBucket;
      }
      rows.push({ label: vehicleType.label, count: count, color: VEHICLE_COLOR, buckets: buckets, iconUrl: url,
                  typePaths: [vehicleType.typePath] });
    });

    var trainRow = buildTrainRow(payload);
    if (trainRow) {
      trainRow.isTrain = true; // Summarized per consist via /api/vehicle-info?types=train, not per typePath.
      rows.push(trainRow);
    }

    vehicleSearchEntries = rows;
    if (rows.length === 0) {
      return;
    }
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    renderTopLevelCategory(navList, detailPane, "Vehicles (" + total + ")", "icon", VEHICLE_COLOR, rows,
      { iconUrl: VEHICLE_ICON_BASE + "Truck.png" });
  }

  // One pin per assembled train (at its lead car), plus every car's oriented
  // box, as a single "Train" row. The pin's id is the abstract BP_Train_C
  // consist actor -- describeInstance resolves it to the train's name,
  // consist composition, and total cargo -- while each car's box keeps the
  // car's own id, so clicking a specific wagon still describes that wagon.
  // Hovering/clicking the pin lights up the whole consist: the pin bucket's
  // trainCarHighlights maps each train id to its cars' indices in the shared
  // cars bucket (see map.js's _redrawHighlight).
  function buildTrainRow(payload) {
    var trains = payload.trains;
    if (!trains || !trains.consists || trains.consists.length === 0) {
      return null;
    }
    var pinPoints = [];
    var pinIds = [];
    var carPoints = [];
    var carIds = [];
    var carHighlightsByTrainId = {};
    var pinIndexByLeadCarIndex = {};
    trains.consists.forEach(function(consist, consistIndex) {
      pinPoints.push(consist.pin[0], consist.pin[1], consist.pin[2]);
      pinIds.push(consist.id);
      // The pin sits at the consist's lead car (see sav_map_data.collectTrains).
      pinIndexByLeadCarIndex[carIds.length] = consistIndex;
      var indices = [];
      for (var i = 0; i < consist.cars.ids.length; i++) {
        indices.push(carIds.length + i);
      }
      carHighlightsByTrainId[consist.id] = indices;
      carIds = carIds.concat(consist.cars.ids);
      for (var p = 0; p < consist.cars.points.length; p++) {
        carPoints.push(consist.cars.points[p]);
      }
    });
    var url = VEHICLE_ICON_BASE + "Train.png";
    var carsBucket = makePointBucket(
      "trainCars", "Train Car", VEHICLE_COLOR, carPoints, "rect", 4, carIds,
      "server", null, trains.carFootprintPixels);
    // Same reasoning as the road vehicles' boxes above: the rail under a car
    // registers at the car's own altitude, and it must not steal the hover.
    carsBucket.lineHitClearanceM = 5;
    var pinBucket = makeIconBucket(
      "trains", "Train", VEHICLE_COLOR, pinPoints, pinIds, "server", null, url, 1, VEHICLE_COLOR);
    // The pin is the abstract consist actor; its cars (selectable above) are
    // the physical objects, so counting both would double-count every train.
    pinBucket.excludeFromSelection = true;
    pinBucket.trainCarHighlights = { bucket: carsBucket, indicesById: carHighlightsByTrainId };
    // Highlighting a LEAD car's box would otherwise swallow the train's pin
    // sitting on it (non-lead cars have no pin of their own -- the sparse
    // map leaves them undefined, which _redrawHighlight skips).
    carsBucket.companionPinBucket = pinBucket;
    carsBucket.companionPinIndexByPoint = pinIndexByLeadCarIndex;
    return { label: "Train", count: trains.consists.length, color: VEHICLE_COLOR,
             buckets: [carsBucket, pinBucket], iconUrl: url };
  }

  // ---- Building categories (from game_data/generated/buildingCategories.json, plus Unknown) ----

  function buildingRow(typeEntry, color, drawPriority) {
    var bucket = makePointBucket(
      "building:" + typeEntry.typePath, typeEntry.label, color, typeEntry.points, typeEntry.renderType, 4,
      typeEntry.ids, "server", null, typeEntry.footprintPixels, drawPriority,
      typeEntry.tiltedFootprints, typeEntry.maxFootprintRadius);
    return { label: typeEntry.label, count: pointCount(typeEntry.points, 4), color: color, renderType: typeEntry.renderType, buckets: [bucket] };
  }

  // Same-shape/different-material typeEntries (see mergedRowLabel) merged
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

  // Registers a line row in the building search catalog (see
  // buildingSearchEntries) when its payload data carries a typePath --
  // searching "power line"/"railroad"/"conveyor belt" then finds the row,
  // and its eye toggle flips the exact sidebar checkbox. Before this, only
  // point/rect building rows were searchable, and (worse) power lines
  // matched a duplicate dot-rendered building row that collectBuildings no
  // longer emits -- so the search toggle flipped an invisible bucket while
  // the actual lines stayed put.
  function registerLineSearchRow(row, lineData) {
    if (lineData.typePath) {
      row.typePaths = [lineData.typePath];
      row.category = lineData.category || "Unknown";
      row.subcategory = lineData.subcategory;
      buildingSearchEntries.push(row);
    }
    return row;
  }

  function lineRow(key, lines) {
    var lineData = lines[key];
    var bucket = makeLineBucket("line:" + key, LINE_LABELS[key], LINE_COLORS[key], lineData.polylines, lineData.ids, "server", null, lineData.pointStride);
    var row = { label: LINE_LABELS[key], count: lineData.polylines.length, color: LINE_COLORS[key], renderType: "line", buckets: [bucket] };
    return registerLineSearchRow(row, lineData);
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
    var row = lineRowFromData(keyPrefix + group.mark, group.label, null, color, group);
    return registerLineSearchRow(row, group);
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
      // material typeEntries (see mergedRowLabel) collapse into one row
      // instead of one row per material skin.
      var mergedGroups = {};
      var mergedOrder = [];
      categoryEntry.types.forEach(function(typeEntry) {
        var mergedLabel = mergedRowLabel(typeEntry.label);
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
        if (CONVEYOR_LIFT_CLASS_PATTERN.test(row.typePaths[0])) {
          layerCategoryRows.lifts.push(row);
        }
        addRow(category, g.subcategory, row);
      });
    });

    // Per-mark belts/pipes, and per-tier vehicle paths (Explorer/FactoryCart/
    // Tractor/Truck/Universal Vehicle Path -- five distinct buildables, each
    // its own toggleable line bucket), placed by the category/subcategory
    // sav_map_data attached to each group.
    (payload.belts || []).forEach(function(group) {
      var row = beltPipeRow("line:belt:", LINE_COLORS.belts, group);
      layerCategoryRows.belts.push(row); // Lifts are never in here -- see LAYER_CATEGORY_DEFS.
      addRow(group.category || "Unknown", group.subcategory, row);
    });
    (payload.pipes || []).forEach(function(group) {
      var row = beltPipeRow("line:pipe:", LINE_COLORS.pipelines, group);
      layerCategoryRows.pipes.push(row);
      addRow(group.category || "Unknown", group.subcategory, row);
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
    (payload.vehicles || []).forEach(function(vehicleType) { total += pointCount(vehicleType.points, 4); });
    // Trains count their physical cars (locomotives/freight cars), not the
    // abstract one-pin-per-consist entries -- this is a placed-object tally.
    ((payload.trains || {}).consists || []).forEach(function(consist) { total += consist.cars.ids.length; });
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

  // The dock's empty state (see #dockEmptyState in index.html): shown while
  // there is no save, because an empty category list otherwise reads as a
  // broken panel rather than "nothing loaded yet".
  function setEmptyState(empty) {
    var el = document.getElementById("dockEmptyState");
    if (el) {
      el.style.display = empty ? "" : "none";
    }
    // Nothing to check or uncheck until there is a save -- the header is two
    // dead buttons above an empty list otherwise.
    var header = document.getElementById("categoryNavHeader");
    if (header) {
      header.style.display = empty ? "none" : "";
    }
  }

  Filters.build = function(payload) {
    // Bottleneck markers belong to the save being replaced -- and clearing
    // them here (rather than letting clearBuckets silently drop them) is what
    // hands back the altitude window showing them widened. Runs before
    // Altitude.build, which then restores against the user's real range.
    if (window.Bottleneck) {
      Bottleneck.clear();
    }
    var navList = document.getElementById("categoryNavColumn");
    var detailPane = document.getElementById("categoryDetailPane");
    navList.innerHTML = "";
    detailPane.innerHTML = "";
    setEmptyState(false);
    categoryEntries = [];
    buildingSearchEntries = [];
    vehicleSearchEntries = [];
    wildlifeSearchEntries = [];
    resourceSearchEntries = [];
    collectableToggleRows = {};
    layerCategoryRows = { belts: [], lifts: [], pipes: [] };
    bucketLayerCheckbox = {};
    bucketLayerLabel = {};
    bucketCategoryCheckbox = {};
    bucketCategoryLabel = {};
    groupCheckboxStates = [];
    MapApp.layer.clearBuckets();

    buildResourceNodeSection(navList, detailPane, payload);
    buildBuildingCategorySections(navList, detailPane, payload);
    buildVehiclesSection(navList, detailPane, payload);
    buildHubSection(navList, detailPane, payload);
    buildEntitiesSection(navList, detailPane, payload);
    buildCollectablesSection(navList, detailPane, payload);
    buildSpawnersSection(navList, detailPane, payload);
    buildDroppedItemsSection(navList, detailPane, payload);
    buildWorldSection(navList, detailPane, payload);

    // Fit the nav panel to the category labels now that they all exist.
    autoSizeNavPanel();

    // Nothing selected on a fresh load -- the whole detail column stays
    // hidden (see deselectAllCategories) until the user actually clicks a
    // category in the nav column.
    deselectAllCategories();

    // The no-save payload (vanilla world, see build_static_payload_json in
    // the Rust parser) has no save to count or export: keep that chrome
    // hidden until a real save arrives.
    var totalEl = document.getElementById("totalObjectCount");
    if (totalEl) {
      totalEl.innerHTML = "";
      if (!payload.noSave) {
        totalEl.appendChild(el("span", "totalObjectCountValue", computeTotalObjectCount(payload).toLocaleString()));
        totalEl.appendChild(el("span", "totalObjectCountLabel", " objects"));
      }
    }
    // A save is loaded, so the "save details" disclosure has something to
    // disclose (game settings, export) -- reveal its summary row.
    var detailsToggle = document.getElementById("saveDetailsToggle");
    if (detailsToggle) {
      detailsToggle.style.display = payload.noSave ? "none" : "flex";
    }

    // Fresh buckets from clearBuckets() above have no hiddenIndices yet --
    // hides the "Restore N hidden objects" button left over from whatever
    // was hidden in the previous save.
    Filters.refreshHiddenObjectsIndicator();

    MapApp.layer.requestRedraw();
  };

  // Tears the whole per-save UI down without building a new one -- the
  // "unload save" flow (see data.js's clearSave). Mirrors the reset half of
  // Filters.build: empties the sidebar tree and every per-save lookup, drops
  // the buckets, and hides the footer's save-details chrome.
  Filters.clear = function() {
    if (window.Bottleneck) {
      Bottleneck.clear(); // See Filters.build -- also hands back a widened altitude window.
    }
    document.getElementById("categoryNavColumn").innerHTML = "";
    document.getElementById("categoryDetailPane").innerHTML = "";
    setEmptyState(true);
    categoryEntries = [];
    buildingSearchEntries = [];
    vehicleSearchEntries = [];
    wildlifeSearchEntries = [];
    resourceSearchEntries = [];
    collectableToggleRows = {};
    layerCategoryRows = { belts: [], lifts: [], pipes: [] };
    bucketLayerCheckbox = {};
    bucketLayerLabel = {};
    bucketCategoryCheckbox = {};
    bucketCategoryLabel = {};
    groupCheckboxStates = [];
    MapApp.layer.clearBuckets();
    deselectAllCategories();
    var totalEl = document.getElementById("totalObjectCount");
    if (totalEl) {
      totalEl.innerHTML = "";
    }
    var detailsToggle = document.getElementById("saveDetailsToggle");
    if (detailsToggle) {
      detailsToggle.style.display = "none";
      detailsToggle.classList.remove("open");
      detailsToggle.setAttribute("aria-expanded", "false");
    }
    var detailsBody = document.getElementById("saveDetails");
    if (detailsBody) {
      detailsBody.style.display = "none";
    }
    Filters.refreshHiddenObjectsIndicator();
    MapApp.layer.requestRedraw();
  };

  // "Check all" / "Uncheck all" -- every checkbox at every nesting level
  // (top-level sections, subcategories, and leaf rows) is a real DOM
  // checkbox somewhere under #sidebar (nav column rows + every category's
  // detail content, selected or not), so setting all of them plus every
  // bucket covers the whole tree in one pass without needing to walk the
  // group structure itself. Persisted as a single "*" entry replacing the
  // whole store -- the global version of the group toggles' one-entry rule
  // (see the hierarchical-persistence comment above), so kinds/categories
  // that only exist in a future save inherit the same choice instead of
  // defaulting back to visible. Both live in the nav column's header (not the detail
  // pane) since they act globally, across every category -- not just
  // whichever one happens to be selected. Scoped to the nav column + detail
  // pane, NOT all of #sidebar: #sidebarFooter also holds checkboxes that are
  // not visibility toggles (the server-fetch "Remember password" box), and a
  // bulk pass flipping that one would silently store/keep the admin password
  // without firing its change handler.
  function setAllVisibility(checked) {
    var scopes = [document.getElementById("categoryNavColumn"),
                  document.getElementById("categoryDetailPane")];
    var checkboxes = [];
    scopes.forEach(function(scope) {
      if (!scope) return;
      var found = scope.querySelectorAll("input[type=checkbox]");
      for (var i = 0; i < found.length; i++) checkboxes.push(found[i]);
    });
    for (var i = 0; i < checkboxes.length; i++) {
      checkboxes[i].checked = checked;
      checkboxes[i].indeterminate = false;
    }
    MapApp.layer.buckets.forEach(function(bucket) {
      bucket.visible = checked;
    });
    savedVisibility = { "*": checked };
    persistVisibility();
    MapApp.layer.requestRedraw();
  }

  // "Save details" disclosure in the footer (see index.html): the
  // object-count chip row expands to game settings + the save-export button.
  // Collapsed on every load -- the summary count is what's usually wanted.
  var saveDetailsToggle = document.getElementById("saveDetailsToggle");
  var saveDetailsBody = document.getElementById("saveDetails");
  if (saveDetailsToggle && saveDetailsBody) {
    saveDetailsToggle.addEventListener("click", function() {
      var open = saveDetailsBody.style.display !== "none";
      saveDetailsBody.style.display = open ? "none" : "block";
      saveDetailsToggle.setAttribute("aria-expanded", String(!open));
      saveDetailsToggle.title = open ? "Show save details" : "Hide save details";
      saveDetailsToggle.classList.toggle("open", !open);
    });
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
  // #gameSettingsPanel/#altitudePanel only appear once relevant.
  var resetHiddenButton = document.getElementById("resetHiddenButton");
  // The dock's Back button and Escape both pop the detail pane, and popping
  // it has to clear the row selection too -- so they route through here
  // rather than just sliding the pager back (see panels.js).
  Filters.deselectAllCategories = deselectAllCategories;

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
