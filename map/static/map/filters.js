// Builds the filter sidebar from a loaded map payload and wires checkboxes
// directly to bucket visibility flags on MapApp.layer (no re-fetch/re-filter
// of raw data on toggle -- see map.js BucketedCanvasLayer). Also tags every
// bucket with enough metadata (ids + tooltip info) for Tooltip.js to resolve
// a click into either a quick local description or a server-side detail fetch.
//
// Sidebar structure: top-level sections (Resource Nodes, Production,
// Logistics, Power, Storage, Construction, Vehicles, Other, Collectables,
// Hard Drives) are each a collapsible renderGroup with a master checkbox.
// Logistics additionally nests one more level (Fluids/Items/Train Tracks/
// Hypertube/Vehicles & Transport) before reaching individual building/line
// types. renderGroup() supports this nesting generically (see its doc comment).

var Filters = {};

(function() {
  "use strict";

  var BUILDING_CATEGORY_COLORS = {
    Extraction: "#a0522d",
    Production: "#e67e22",
    Logistics: "#3498db",
    Power: "#f1c40f",
    Storage: "#9b59b6",
    Construction: "#7f8c8d",
    Vehicles: "#1abc9c",
    Other: "#e74c3c",
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

  // Real item icons (see static/icons/items/) read far more clearly on the
  // map than an abstract colored dot for these specific collectables --
  // "remaining" is drawn at full opacity, "collected"/already-dealt-with
  // states are dimmed (COLLECTED_ICON_OPACITY) rather than needing a second
  // image asset just to show the same icon "used up".
  var ICON_BASE_URL = "icons/items/";
  var ITEM_ICON_FILES = {
    slugsBlue: "Blue Power Slug.png",
    slugsYellow: "Yellow Power Slug.png",
    slugsPurple: "Purple Power Slug.png",
    somersloops: "Somersloop.png",
    mercerSpheres: "Mercer Sphere.png",
    hardDrive: "Hard Drive.png",
  };
  var COLLECTED_ICON_OPACITY = 0.4;

  function iconUrl(key) {
    return ICON_BASE_URL + encodeURIComponent(ITEM_ICON_FILES[key]);
  }

  // Resource node icons, keyed by the save's own resourceType pathName (see
  // sav_map_data.collectResourceNodes). Most of these PNGs already existed
  // for other purposes (inventory/tooltip rendering); Crude Oil, Nitrogen
  // Gas, and Water were downloaded from the wiki's fluid icon set
  // (satisfactory.fandom.com/wiki/Category:Fluid_icons) specifically for
  // this. Geyser lives in icons/other/ (not icons/items/) since it isn't an
  // inventory item at all.
  var RESOURCE_ICON_FILES = {
    Desc_Coal_C: "items/Coal.png",
    Desc_OreIron_C: "items/Iron Ore.png",
    Desc_OreCopper_C: "items/Copper Ore.png",
    Desc_OreGold_C: "items/Caterium Ore.png",
    Desc_OreBauxite_C: "items/Bauxite.png",
    Desc_OreUranium_C: "items/Uranium.png",
    Desc_Stone_C: "items/Limestone.png",
    Desc_Sulfur_C: "items/Sulfur.png",
    Desc_RawQuartz_C: "items/Raw Quartz.png",
    Desc_SAM_C: "items/SAM.png",
    Desc_LiquidOil_C: "items/Crude Oil.png",
    Desc_NitrogenGas_C: "items/Nitrogen Gas.png",
    Desc_Water_C: "items/Water.png",
    Desc_Geyser_C: "other/Geyser.png",
  };

  function resourceIconUrl(resourceType) {
    var relativePath = RESOURCE_ICON_FILES[resourceType];
    if (!relativePath) {
      return null;
    }
    var encodedSegments = relativePath.split("/").map(encodeURIComponent);
    return "icons/" + encodedSegments.join("/");
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

  // Simple "home" pentagon silhouette (square body + peaked roof) -- like
  // the player icon above, an inline SVG data: URL avoids adding a binary
  // asset just for this one landmark marker.
  var HUB_COLOR = "#d2691e";
  var HUB_ICON_URL = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">' +
    '<polygon points="16,3 29,14 29,29 3,29 3,14" fill="' + HUB_COLOR + '"/>' +
    '</svg>'
  );

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
  };

  var LINE_LABELS = {
    powerLines: "Power Line",
    belts: "Belt / Lift",
    pipelines: "Pipeline",
    railroads: "Railroad",
    hypertubes: "Hypertube",
  };

  // Top-level section order (per the user's explicit choice).
  var TOP_LEVEL_CATEGORY_ORDER = ["Extraction", "Production", "Logistics", "Power", "Storage", "Construction", "Vehicles", "Other"];
  var LOGISTICS_SUBCATEGORY_ORDER = ["Fluids", "Items", "Hypertube"];
  var VEHICLE_SUBCATEGORY_ORDER = ["Trains", "Trucks", "Drones"];
  var CONSTRUCTION_SUBCATEGORY_ORDER = ["Foundations", "Ramps", "Walls", "Beams & Pillars", "Catwalks", "Railings & Fences", "Stairs", "Doors & Windows", "Roofs", "Other"];

  // Lightweight buildables (foundations/walls/ramps/beams) come in several
  // material skins per shape -- e.g. "Foundation 4m", "Foundation 4m
  // (Asphalt)", "Foundation 4m (Concrete)", "Foundation 4m (Metal)",
  // "Foundation 4m (Polished Concrete)" are all the exact same shape/size,
  // just different paint, and used to show up as 5 separate sidebar rows.
  // Only suffixes confirmed to be pure material/skin are stripped here --
  // other parenthetical suffixes seen in the readable-name data (e.g.
  // "(Window)", "(No Indicator)", "(1 m)") indicate a genuinely different
  // shape or size and must NOT be merged away.
  var MATERIAL_LABEL_SUFFIXES = [" (Asphalt)", " (Concrete)", " (Polished Concrete)", " (Metal)"];

  function mergedConstructionLabel(label) {
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

  // Bucket keys (e.g. "building:Desc_ConstructorMk1_C", "node:...", "line:belts")
  // are stable identifiers for a *kind* of thing, not a specific save's data --
  // so a visibility choice made here survives both a same-file auto-refresh
  // (see data.js's checkForNewerSave) and switching to an entirely different
  // save. Persists for the life of the page (rebuilt fresh only on reload),
  // deliberately never cleared by Filters.build itself.
  var savedVisibility = {};

  // tooltipInfo(idx) -> {title, rows: [[label, value], ...]} for a "static"
  // bucket (no server round-trip; we already know everything worth showing).
  function makePointBucket(key, label, color, points, renderType, pointStride, ids, tooltipKind, tooltipInfo, footprintPixels, drawPriority) {
    return MapApp.layer.addBucket({
      key: key, label: label, color: color, visible: true,
      renderType: renderType || "circle",
      pointStride: pointStride,
      points: new Float32Array(points),
      ids: ids || null,
      tooltipKind: tooltipKind || "none",
      tooltipInfo: tooltipInfo || null,
      footprintPixels: footprintPixels || null,
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

    var expandToggle = el("span", "expandToggle", options.startCollapsed ? "▸" : "▾");
    titleRow.appendChild(expandToggle);

    var parentCheckbox = document.createElement("input");
    parentCheckbox.type = "checkbox";
    parentCheckbox.checked = true;
    titleRow.appendChild(parentCheckbox);

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
        var rowDiv = el("div", "filterRow");
        var checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        // A row's checkbox can control several buckets at once (e.g.
        // Construction merges same-shape/different-material buckets into one
        // row -- see buildConstructionSection), so restoring falls back to
        // "visible" unless a previous visit explicitly recorded otherwise for
        // one of them; in practice they're always toggled together.
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
        rowDiv.appendChild(checkbox);
        rowDiv.appendChild(makeIcon(row.renderType || renderType, row.color || swatchColor, row.iconUrl));
        rowDiv.appendChild(el("label", null, row.label));
        rowDiv.appendChild(el("span", "count", String(row.count)));
        childrenDiv.appendChild(rowDiv);
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
    expandToggle.addEventListener("click", function() {
      setCollapsed(childrenDiv.style.display !== "none");
    });
    titleRow.querySelector(".groupLabel").addEventListener("click", function() {
      setCollapsed(childrenDiv.style.display !== "none");
    });

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

  function buildResourceEntrySection(container, title, resourceEntries) {
    if (resourceEntries.length === 0) {
      return;
    }
    renderGroup(container, title, "circle", NEUTRAL_COLOR, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      resourceEntries.forEach(function(resourceEntry) {
        var result = buildResourceEntryGroup(childrenDiv, resourceEntry);
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: false });
  }

  function buildResourceNodeSection(container, payload) {
    var byResourceType = payload.resourceNodes.byResourceType;
    buildResourceEntrySection(container, "Resource Nodes", byResourceType.filter(function(e) { return !e.isWell; }));
    buildResourceEntrySection(container, "Resource Wells", byResourceType.filter(function(e) { return e.isWell; }));
  }

  // ---- Collectables -----------------------------------------------------

  function buildCollectablesSection(container, payload) {
    var collectables = payload.collectables;
    var kinds = [
      { key: "slugsBlue", label: "Blue Power Slug", color: SLUG_COLORS.slugsBlue },
      { key: "slugsYellow", label: "Yellow Power Slug", color: SLUG_COLORS.slugsYellow },
      { key: "slugsPurple", label: "Purple Power Slug", color: SLUG_COLORS.slugsPurple },
      { key: "somersloops", label: "Somersloop", color: SOMERSLOOP_COLOR },
      { key: "mercerSpheres", label: "Mercer Sphere", color: MERCER_SPHERE_COLOR },
    ];
    renderGroup(container, "Collectables", "circle", NEUTRAL_COLOR, function(childrenDiv) {
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
        var rows = [
          { label: "Remaining", count: pointCount(data.remaining, 3), color: kind.color, buckets: [remainingBucket], iconUrl: url },
          { label: "Collected", count: pointCount(data.collected, 3), color: COLLECTED_COLOR, buckets: [collectedBucket], iconUrl: url },
        ];
        var result = renderGroup(childrenDiv, kind.label + "s", "icon", kind.color, rows, { startCollapsed: true, iconUrl: url });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: false });
  }

  // ---- Hard Drives -------------------------------------------------------

  // "hasDrive" still has something for the player to get (full opacity);
  // "empty"/"dismantled" are already dealt with (dimmed) -- same icon
  // throughout, since it's still physically a hard drive crate.
  var HARD_DRIVE_ICON_OPACITY = {
    hasDrive: 1, empty: COLLECTED_ICON_OPACITY, dismantled: COLLECTED_ICON_OPACITY,
  };

  function buildHardDrivesSection(container, payload) {
    var hardDrives = payload.hardDrives;
    var stateKeys = ["hasDrive", "empty", "dismantled"];
    var url = iconUrl("hardDrive");
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
      var bucket = makeIconBucket("hd:" + stateKey, HARD_DRIVE_LABELS[stateKey], color, points, ids, "static", tooltipInfo, url, HARD_DRIVE_ICON_OPACITY[stateKey]);
      return { label: HARD_DRIVE_LABELS[stateKey], count: pointCount(points, 3), color: color, buckets: [bucket], iconUrl: url };
    });
    renderGroup(container, "Hard Drives", "icon", HARD_DRIVE_COLORS.hasDrive, rows, { startCollapsed: false, iconUrl: url });
  }

  // ---- HUB ------------------------------------------------------------------

  // The HUB is a one-of-a-kind landmark (excluded from collectBuildings --
  // see sav_map_data.HUB_TYPE_PATH) rather than an ordinary building, so it
  // gets its own section/icon instead of showing up under "Other".
  function buildHubSection(container, payload) {
    var hub = payload.hub;
    var count = pointCount(hub.points, 3);
    if (count === 0) {
      return;
    }
    var bucket = makeIconBucket("hub", "HUB", HUB_COLOR, hub.points, hub.ids, "server", null, HUB_ICON_URL, 1);
    var rows = [{ label: "HUB", count: count, color: HUB_COLOR, buckets: [bucket], iconUrl: HUB_ICON_URL }];
    renderGroup(container, "HUB", "icon", HUB_COLOR, rows, { startCollapsed: false, iconUrl: HUB_ICON_URL });
  }

  // ---- Players ------------------------------------------------------------

  // Unlike the other icon buckets above, a player's name/inventory isn't
  // known to the client up front -- it requires the same /api/instance
  // round-trip as buildings (see sav_map_data.describeInstance's player
  // branch), hence tooltipKind "server" instead of "static".
  function buildPlayersSection(container, payload) {
    var players = payload.players;
    var count = pointCount(players.points, 3);
    if (count === 0) {
      return;
    }
    var bucket = makeIconBucket("players", "Players", PLAYER_COLOR, players.points, players.ids, "server", null, PLAYER_ICON_URL, 1);
    var rows = [{ label: "Player", count: count, color: PLAYER_COLOR, buckets: [bucket], iconUrl: PLAYER_ICON_URL }];
    renderGroup(container, "Players", "icon", PLAYER_COLOR, rows, { startCollapsed: false, iconUrl: PLAYER_ICON_URL });
  }

  // ---- Building categories (Production/Power/Storage/Construction/Vehicles/Other) ----

  function buildingRow(typeEntry, color, drawPriority) {
    var bucket = makePointBucket(
      "building:" + typeEntry.typePath, typeEntry.label, color, typeEntry.points, typeEntry.renderType, 4,
      typeEntry.ids, "server", null, typeEntry.footprintPixels, drawPriority);
    return { label: typeEntry.label, count: pointCount(typeEntry.points, 4), color: color, renderType: typeEntry.renderType, buckets: [bucket] };
  }

  // Foundations/walls/ramps/beams sit at ground level under everything else
  // in practice -- drawn first (see makePointBucket's drawPriority) so
  // machines built on top of them paint over them regardless of where
  // "Construction" happens to fall in the sidebar's category order.
  var DRAW_PRIORITY_BY_CATEGORY = { Construction: -1 };

  function lineRow(key, lines) {
    var lineData = lines[key];
    var bucket = makeLineBucket("line:" + key, LINE_LABELS[key], LINE_COLORS[key], lineData.polylines, lineData.ids, "server", null, lineData.pointStride);
    return { label: LINE_LABELS[key], count: lineData.polylines.length, color: LINE_COLORS[key], renderType: "line", buckets: [bucket] };
  }

  function buildSimpleCategorySection(container, category, typeEntries, extraRows) {
    var color = BUILDING_CATEGORY_COLORS[category] || BUILDING_CATEGORY_COLORS.Other;
    var drawPriority = DRAW_PRIORITY_BY_CATEGORY[category] || 0;
    var rows = typeEntries.map(function(typeEntry) { return buildingRow(typeEntry, color, drawPriority); });
    if (extraRows) {
      rows = rows.concat(extraRows);
    }
    rows.sort(function(a, b) { return b.count - a.count; });
    var total = rows.reduce(function(s, r) { return s + r.count; }, 0);
    renderGroup(container, category + " (" + total + ")", "rect", color, rows, { startCollapsed: false });
  }

  // Shared by Logistics (Fluids/Items/Hypertube) and Vehicles (Trains/Trucks/
  // Drones) -- both are "category with subcategories, some of which also pull
  // in a line bucket" in exactly the same shape.
  function buildSubcategorizedSection(container, category, typeEntries, lines, subcategoryOrder, lineKeyBySubcategory) {
    var color = BUILDING_CATEGORY_COLORS[category];
    var bySubcategory = {};
    typeEntries.forEach(function(typeEntry) {
      var subcategory = typeEntry.subcategory || subcategoryOrder[subcategoryOrder.length - 1];
      (bySubcategory[subcategory] = bySubcategory[subcategory] || []).push(typeEntry);
    });

    var totalCount = 0;
    typeEntries.forEach(function(t) { totalCount += pointCount(t.points, 4); });
    Object.keys(lineKeyBySubcategory).forEach(function(subcategory) {
      totalCount += lines[lineKeyBySubcategory[subcategory]].polylines.length;
    });

    renderGroup(container, category + " (" + totalCount + ")", "rect", color, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      subcategoryOrder.forEach(function(subcategory) {
        var entries = bySubcategory[subcategory] || [];
        var rows = entries.map(function(typeEntry) { return buildingRow(typeEntry, color); });
        var lineKey = lineKeyBySubcategory[subcategory];
        if (lineKey) {
          rows.push(lineRow(lineKey, lines));
        }
        if (rows.length === 0) {
          return;
        }
        rows.sort(function(a, b) { return b.count - a.count; });
        var subTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(childrenDiv, subcategory + " (" + subTotal + ")", "rect", color, rows, { startCollapsed: true });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: false });
  }

  // Construction needs both subcategories (Foundations/Ramps/Walls/...) and,
  // within each, merging same-shape-different-material rows into one --
  // neither of which buildSimpleCategorySection/buildSubcategorizedSection
  // do (the latter is for Logistics/Vehicles' "also pull in a line bucket"
  // shape, which Construction doesn't need).
  function buildConstructionSection(container, category, typeEntries) {
    var color = BUILDING_CATEGORY_COLORS[category];
    var bySubcategory = {};
    typeEntries.forEach(function(typeEntry) {
      var subcategory = typeEntry.subcategory || "Other";
      (bySubcategory[subcategory] = bySubcategory[subcategory] || []).push(typeEntry);
    });

    var totalCount = 0;
    typeEntries.forEach(function(t) { totalCount += pointCount(t.points, 4); });

    renderGroup(container, category + " (" + totalCount + ")", "rect", color, function(childrenDiv) {
      var checkboxes = [];
      var allBuckets = [];
      CONSTRUCTION_SUBCATEGORY_ORDER.forEach(function(subcategory) {
        var entries = bySubcategory[subcategory] || [];
        if (entries.length === 0) {
          return;
        }
        var byMergedLabel = {};
        var mergedOrder = [];
        entries.forEach(function(typeEntry) {
          var mergedLabel = mergedConstructionLabel(typeEntry.label);
          if (!byMergedLabel[mergedLabel]) {
            byMergedLabel[mergedLabel] = [];
            mergedOrder.push(mergedLabel);
          }
          byMergedLabel[mergedLabel].push(typeEntry);
        });
        var rows = mergedOrder.map(function(mergedLabel) {
          var group = byMergedLabel[mergedLabel];
          var buckets = group.map(function(typeEntry) { return buildingRow(typeEntry, color, DRAW_PRIORITY_BY_CATEGORY[category] || 0).buckets[0]; });
          var count = group.reduce(function(s, t) { return s + pointCount(t.points, 4); }, 0);
          return { label: mergedLabel, count: count, color: color, renderType: group[0].renderType, buckets: buckets };
        });
        rows.sort(function(a, b) { return b.count - a.count; });
        var subTotal = rows.reduce(function(s, r) { return s + r.count; }, 0);
        var result = renderGroup(childrenDiv, subcategory + " (" + subTotal + ")", "rect", color, rows, { startCollapsed: true });
        checkboxes.push(result.checkbox);
        allBuckets = allBuckets.concat(result.buckets);
      });
      return { buckets: allBuckets, checkboxes: checkboxes };
    }, { startCollapsed: false });
  }

  function buildBuildingCategorySections(container, payload) {
    var byCategory = {};
    payload.buildingCategories.forEach(function(categoryEntry) {
      byCategory[categoryEntry.category] = categoryEntry.types;
    });

    TOP_LEVEL_CATEGORY_ORDER.forEach(function(category) {
      var typeEntries = byCategory[category] || [];
      if (category === "Logistics") {
        buildSubcategorizedSection(container, category, typeEntries, payload.lines, LOGISTICS_SUBCATEGORY_ORDER,
          { Fluids: "pipelines", Items: "belts", Hypertube: "hypertubes" });
      } else if (category === "Vehicles") {
        buildSubcategorizedSection(container, category, typeEntries, payload.lines, VEHICLE_SUBCATEGORY_ORDER,
          { Trains: "railroads" });
      } else if (category === "Power") {
        buildSimpleCategorySection(container, category, typeEntries, [lineRow("powerLines", payload.lines)]);
      } else if (category === "Construction") {
        if (typeEntries.length > 0) {
          buildConstructionSection(container, category, typeEntries);
        }
      } else if (typeEntries.length > 0) {
        buildSimpleCategorySection(container, category, typeEntries, null);
      }
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
    total += pointCount(payload.hub.points, 3);
    Object.keys(payload.collectables).forEach(function(key) {
      var c = payload.collectables[key];
      total += pointCount(c.remaining, 3) + pointCount(c.collected, 3);
    });
    ["hasDrive", "empty", "dismantled"].forEach(function(key) {
      total += pointCount(payload.hardDrives[key], 3);
    });
    Object.keys(payload.lines).forEach(function(key) {
      total += payload.lines[key].polylines.length;
    });
    return total;
  }

  Filters.build = function(payload) {
    var container = document.getElementById("filterPanel");
    container.innerHTML = "";
    MapApp.layer.clearBuckets();

    buildResourceNodeSection(container, payload);
    buildBuildingCategorySections(container, payload);
    buildHubSection(container, payload);
    buildPlayersSection(container, payload);
    buildCollectablesSection(container, payload);
    buildHardDrivesSection(container, payload);

    var totalEl = document.getElementById("totalObjectCount");
    if (totalEl) {
      totalEl.textContent = computeTotalObjectCount(payload).toLocaleString() + " objects loaded";
    }

    MapApp.layer.requestRedraw();
  };

  // "Uncheck all" -- every checkbox at every nesting level (top-level
  // sections, subcategories, and leaf rows) is a real DOM checkbox somewhere
  // under #filterPanel, so unchecking all of them plus every bucket covers
  // the whole tree in one pass without needing to walk the group structure
  // itself. Recorded into savedVisibility too, same as any other toggle (see
  // the row/parent checkbox handlers above), so it survives a reload.
  var uncheckAllButton = document.getElementById("uncheckAllButton");
  if (uncheckAllButton) {
    uncheckAllButton.addEventListener("click", function() {
      var filterPanel = document.getElementById("filterPanel");
      var checkboxes = filterPanel.querySelectorAll("input[type=checkbox]");
      for (var i = 0; i < checkboxes.length; i++) {
        checkboxes[i].checked = false;
      }
      MapApp.layer.buckets.forEach(function(bucket) {
        bucket.visible = false;
        savedVisibility[bucket.key] = false;
      });
      MapApp.layer.requestRedraw();
    });
  }
})();
