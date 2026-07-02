// Hover-to-inspect tooltips that follow the cursor. "static" buckets
// (resource nodes, collectables, hard drives) render instantly from data
// already in the payload. "server" buckets (buildings, power lines, belts,
// pipelines, railroads) fetch /api/instance for rich per-instance detail
// (recipe, inventory, overclock, somersloop slots, items in transit on a
// belt, etc.) since that data is deliberately not in the bulk payload -- see
// sav_map_data.describeInstance(). The fetch is debounced so sweeping the
// mouse across many buildings doesn't fire a request per building.
//
// Clicking an element "pins" its tooltip: it stops following the cursor and
// becomes clickable (e.g. to expand "All properties"), staying open until
// the user clicks anywhere outside it (see map.js's click handler, plus the
// document-level listener at the bottom of this file for clicks that land
// outside the map entirely, like the sidebar).

var Tooltip = {};

(function() {
  "use strict";

  var FETCH_DEBOUNCE_MS = 120;

  var tooltipEl = null;
  var currentId = null;
  var pendingTimer = null;
  var pinned = false;

  function ensureElement() {
    if (!tooltipEl) {
      tooltipEl = document.createElement("div");
      tooltipEl.id = "tt-tooltip";
      document.body.appendChild(tooltipEl);
    }
    return tooltipEl;
  }

  function position(clientX, clientY) {
    var element = ensureElement();
    var offset = 14;
    var x = clientX + offset;
    var y = clientY + offset;
    var maxX = window.innerWidth - element.offsetWidth - 8;
    var maxY = window.innerHeight - element.offsetHeight - 8;
    if (x > maxX) x = clientX - element.offsetWidth - offset;
    if (y > maxY) y = clientY - element.offsetHeight - offset;
    element.style.left = Math.max(4, x) + "px";
    element.style.top = Math.max(4, y) + "px";
  }

  function setContent(node) {
    var element = ensureElement();
    element.innerHTML = "";
    element.appendChild(node);
    element.style.display = "block";
  }

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function row(label, value) {
    var r = el("div", "tt-row");
    r.appendChild(el("span", "tt-row-label", label));
    r.appendChild(el("span", "tt-row-value", String(value)));
    return r;
  }

  // Raw property values can be long structs/arrays rendered as text -- a
  // stacked layout (label above value) reads far better than cramming that
  // into a narrow right-aligned column like the summary rows use.
  function rawRow(label, value) {
    var r = el("div", "tt-raw-row");
    r.appendChild(el("div", "tt-raw-row-label", label));
    r.appendChild(el("div", "tt-raw-row-value", String(value)));
    return r;
  }

  function inventorySection(title, items) {
    if (!items || items.length === 0) {
      return null;
    }
    var section = el("div", "tt-section");
    section.appendChild(el("div", "tt-section-title", title));
    items.forEach(function(entry) {
      var value = entry.unit ? entry.count + " " + entry.unit : "x" + entry.count;
      section.appendChild(row(entry.item, value));
    });
    return section;
  }

  // Altitude comes straight from the hit test (every bucket type already
  // carries its Z in map-pixel-space data -- see map.js's hitTest), so it's
  // available immediately and shown the same way regardless of whether the
  // rest of the tooltip is "static" or needs a server round-trip.
  function altitudeRow(z) {
    return row("Altitude", z === undefined ? "?" : Math.round(z * 10) / 10 + " m");
  }

  // Compact copy icon, sat right after the row's LABEL (not its value) so
  // the value column still lines up with every other row's (Altitude in
  // particular, directly above it) -- a trailing "Copy" button after the
  // value pushed the coordinates themselves out of alignment with that.
  // Only actually clickable once the tooltip is pinned (it sits under the
  // same pointer-events:none/auto toggle as "All properties", see
  // Tooltip.pin below). getText is called lazily on click rather than
  // capturing a value up front, so a button created before the real
  // coordinates have loaded (see coordinatesRow) still copies the right
  // thing if clicked after they arrive.
  var COPY_ICON_SVG =
    '<svg width="11" height="11" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">' +
    '<rect x="5.5" y="5.5" width="9" height="9" rx="1.5" stroke="currentColor" stroke-width="1.3"/>' +
    '<path d="M2.5 10.5v-8a1 1 0 0 1 1-1h8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>' +
    '</svg>';

  function copyIconButton(getText) {
    var button = document.createElement("button");
    button.type = "button";
    button.className = "tt-copy-icon-btn";
    button.title = "Copy";
    button.innerHTML = COPY_ICON_SVG;
    button.addEventListener("click", function(event) {
      event.stopPropagation(); // Don't let this bubble into the document-level unpin-on-outside-click handler.
      var text = getText();
      if (!text || !navigator.clipboard) {
        return;
      }
      navigator.clipboard.writeText(text).then(function() {
        button.classList.add("tt-copy-icon-btn-done");
        setTimeout(function() { button.classList.remove("tt-copy-icon-btn-done"); }, 1000);
      }).catch(function() {});
    });
    return button;
  }

  // worldX/worldY are the save's own raw world-space units (the same
  // numbers the game's own debug coordinates/teleport commands use) --
  // deliberately NOT the meters conversion Altitude uses, since these are
  // meant to be copy-pasted elsewhere, where the raw units are what's
  // actually useful. Shown as "..." (no copy icon) until known -- see
  // buildStaticContent's background position fetch, the one case where
  // these aren't available immediately.
  function coordinatesRow(worldX, worldY) {
    var known = worldX !== undefined && worldY !== undefined;
    var text = known ? Math.round(worldX) + ", " + Math.round(worldY) : "...";
    var r = el("div", "tt-row");
    var labelWrap = el("span", "tt-row-label-with-icon");
    labelWrap.appendChild(el("span", null, "Coordinates"));
    if (known) {
      labelWrap.appendChild(copyIconButton(function() { return Math.round(worldX) + ", " + Math.round(worldY); }));
    }
    r.appendChild(labelWrap);
    r.appendChild(el("span", "tt-row-value", text));
    return r;
  }

  // Every tooltip's "where is this" info, grouped together instead of
  // Altitude floating alone above an otherwise-unnamed block of rows.
  function positionSection(z, worldX, worldY) {
    var section = el("div", "tt-section", undefined);
    section.appendChild(el("div", "tt-section-title", "Position"));
    section.appendChild(altitudeRow(z));
    section.appendChild(coordinatesRow(worldX, worldY));
    return section;
  }

  function buildStaticContent(title, rows, z, worldPosition) {
    var root = el("div", "tt-popup");
    root.appendChild(el("div", "tt-title", title));
    root.appendChild(positionSection(z, worldPosition && worldPosition[0], worldPosition && worldPosition[1]));
    if (rows.length > 0) {
      var production = el("div", "tt-section");
      production.appendChild(el("div", "tt-section-title", "Production"));
      rows.forEach(function(pair) { production.appendChild(row(pair[0], pair[1])); });
      root.appendChild(production);
    }
    return root;
  }

  function buildLoadingContent(title, z) {
    var root = el("div", "tt-popup");
    root.appendChild(el("div", "tt-title", title));
    root.appendChild(positionSection(z));
    root.appendChild(el("div", "tt-loading", "Loading details..."));
    return root;
  }

  function buildErrorContent(title, message, z) {
    var root = el("div", "tt-popup");
    root.appendChild(el("div", "tt-title", title));
    root.appendChild(positionSection(z));
    root.appendChild(el("div", "tt-error", message));
    return root;
  }

  function buildDetailContent(detail, z) {
    var root = el("div", "tt-popup");
    root.appendChild(el("div", "tt-title", detail.label || detail.instanceName));
    root.appendChild(positionSection(z, detail.position && detail.position[0], detail.position && detail.position[1]));

    // Everything that isn't "where is this" -- recipe/power/status plus
    // every inventory listing -- grouped under one "Production" heading
    // instead of a long unnamed list of rows followed by a separate run of
    // individually-titled inventory sections.
    var productionRows = [];
    if (detail.stationName) productionRows.push(["Station", detail.stationName]);
    if (detail.recipe) productionRows.push(["Recipe", detail.recipe]);
    if (detail.clockSpeedPercent !== undefined) productionRows.push(["Clock speed", detail.clockSpeedPercent + "%"]);
    if (detail.productionProgressPercent !== undefined) productionRows.push(["Progress", detail.productionProgressPercent + "%"]);
    if (detail.basePowerConsumptionMW !== undefined) productionRows.push(["Base power", detail.basePowerConsumptionMW + " MW"]);
    if (detail.basePowerConsumptionRangeMW !== undefined) productionRows.push(["Power range", detail.basePowerConsumptionRangeMW[0] + "-" + detail.basePowerConsumptionRangeMW[1] + " MW"]);
    if (detail.basePowerConsumptionMeanMW !== undefined) productionRows.push(["Mean power", detail.basePowerConsumptionMeanMW + " MW"]);
    if (detail.runningStatus) productionRows.push(["Status", detail.runningStatus]);
    if (detail.powerProductionMW !== undefined) productionRows.push(["Power production", detail.powerProductionMW + " MW"]);
    if (detail.powerStoredMWh !== undefined) productionRows.push(["Charge", detail.powerStoredMWh + " MWh"]);
    if (detail.fluidType) productionRows.push(["Fluid type", detail.fluidType]);
    if (detail.fluidContent !== undefined) productionRows.push(["Fluid content", detail.fluidContent]);
    if (detail.loadMode) productionRows.push(["Mode", detail.loadMode]);

    var inventorySections = [
      inventorySection("Items In Transit", detail.itemsOnBelt),
      inventorySection("Input Inventory", detail.inputInventory),
      inventorySection("Output Inventory", detail.outputInventory),
      inventorySection("Storage", detail.storageInventory),
      inventorySection("Buffer", detail.bufferInventory),
      inventorySection("Cargo", detail.cargoInventory),
      inventorySection("Inventory", detail.playerInventory),
      inventorySection("Power Shard / Somersloop Slots", detail.powerShardSlots),
    ].filter(function(section) { return section !== null; });

    if (productionRows.length > 0 || inventorySections.length > 0) {
      var production = el("div", "tt-section");
      production.appendChild(el("div", "tt-section-title", "Production"));
      productionRows.forEach(function(pair) { production.appendChild(row(pair[0], pair[1])); });
      inventorySections.forEach(function(section) { production.appendChild(section); });
      root.appendChild(production);
    }

    if (detail.rawProperties && detail.rawProperties.length > 0) {
      var details = document.createElement("details");
      details.className = "tt-raw";
      var summary = document.createElement("summary");
      summary.textContent = "All properties (" + detail.rawProperties.length + ")";
      details.appendChild(summary);
      var rawList = el("div", "tt-raw-list");
      detail.rawProperties.forEach(function(prop) {
        rawList.appendChild(rawRow(prop.name, prop.value));
      });
      details.appendChild(rawList);
      root.appendChild(details);
    }
    return root;
  }

  // Shared by hover (show) and click (pin) -- only the pinned/pointer-events
  // side effects differ between the two entry points below.
  function renderHit(clientX, clientY, hit) {
    var bucket = hit.bucket;

    if (hit.id === currentId) {
      position(clientX, clientY); // Same element -- just follow the cursor (no-op once pinned).
      return;
    }
    currentId = hit.id;
    if (pendingTimer) {
      clearTimeout(pendingTimer);
      pendingTimer = null;
    }

    if (bucket.tooltipKind === "static") {
      // info.position (raw world-space [x, y], see e.g. filters.js's
      // buildResourceEntryGroup) comes from the same static reference data
      // already used to plot this point on the map, so it's available
      // synchronously here -- including for already-collected/dismantled
      // entries, where a live /api/instance lookup would fail outright
      // (their actor is actually removed from the save once collected).
      var info = bucket.tooltipInfo(hit.index);
      setContent(buildStaticContent(info.title, info.rows, hit.z, info.position));
      position(clientX, clientY);
      return;
    }

    setContent(buildLoadingContent(bucket.label, hit.z));
    position(clientX, clientY);

    var filename = window.MapApp.currentFile;
    if (!filename) {
      setContent(buildErrorContent(bucket.label, "No save currently loaded.", hit.z));
      return;
    }

    var requestedId = hit.id;
    pendingTimer = setTimeout(function() {
      fetch("/api/instance?file=" + encodeURIComponent(filename) + "&instance=" + encodeURIComponent(requestedId))
        .then(function(response) { return response.json(); })
        .then(function(detail) {
          if (currentId !== requestedId) return; // Hovered/clicked away before this resolved.
          if (detail.error) {
            setContent(buildErrorContent(bucket.label, detail.error, hit.z));
            return;
          }
          setContent(buildDetailContent(detail, hit.z));
          position(clientX, clientY);
        })
        .catch(function(error) {
          if (currentId !== requestedId) return;
          setContent(buildErrorContent(bucket.label, "Failed to load: " + error, hit.z));
        });
    }, FETCH_DEBOUNCE_MS);
  }

  Tooltip.isPinned = function() {
    return pinned;
  };

  Tooltip.hide = function() {
    currentId = null;
    pinned = false;
    if (pendingTimer) {
      clearTimeout(pendingTimer);
      pendingTimer = null;
    }
    if (tooltipEl) {
      tooltipEl.style.display = "none";
      tooltipEl.style.pointerEvents = "none";
    }
  };

  // Hover preview -- ignored entirely while a tooltip is pinned (see map.js's
  // mousemove handler, which checks isPinned() before calling this).
  Tooltip.show = function(clientX, clientY, hit) {
    renderHit(clientX, clientY, hit);
  };

  // Click-to-pin: freezes the tooltip in place and makes it interactive
  // (pointer-events:auto) so "All properties" etc. can actually be clicked.
  Tooltip.pin = function(clientX, clientY, hit) {
    currentId = null; // Force renderHit to treat this as a fresh hit even if already showing via hover.
    pinned = true;
    ensureElement().style.pointerEvents = "auto";
    renderHit(clientX, clientY, hit);
  };

  Tooltip.unpin = function() {
    Tooltip.hide();
  };

  // Clicks on the map itself are handled by map.js's own click handler
  // (which calls Tooltip.pin/unpin based on hitTest). This only covers
  // clicks that land outside both the tooltip AND the map entirely -- e.g.
  // the sidebar -- which wouldn't otherwise reach either of those.
  document.addEventListener("click", function(event) {
    if (!pinned || !tooltipEl) {
      return;
    }
    var insideTooltip = tooltipEl.contains(event.target);
    var insideMap = window.MapApp && MapApp.map && MapApp.map.getContainer().contains(event.target);
    if (!insideTooltip && !insideMap) {
      Tooltip.unpin();
    }
  });
})();
