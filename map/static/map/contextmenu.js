// Right-click context menu for a single map object -- triggered by
// selection.js on a plain right-click (no drag; see its finishSelection).
// "Hide this object" hides just the clicked instance (MapApp.hideObject);
// "Hide layer"/"Hide category" flip the exact same checkboxes the sidebar
// already owns (see filters.js's Filters.hideLayer/hideCategory), so hiding
// from here always stays in sync with the sidebar's own toggle state.

var ContextMenu = {};

(function() {
  "use strict";

  var menu = document.getElementById("contextMenu");

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function addItem(label, onClick) {
    var item = el("div", "contextMenuItem", label);
    item.addEventListener("click", function() {
      onClick();
      hide();
    });
    menu.appendChild(item);
  }

  function hide() {
    if (menu.style.display === "none") {
      return; // Idempotent -- hide() is also called defensively on Escape/blur/wheel/outside-click even when nothing's open.
    }
    menu.style.display = "none";
    // Only the menu's own highlight is ours to clear -- guarding on
    // menu.style.display above (rather than clearing unconditionally on
    // every one of those defensive triggers) is what keeps this from also
    // wiping out an unrelated highlight set by ordinary mouse hovering.
    MapApp.setHighlight(null, null);
  }

  // Clamps the menu inside the viewport instead of letting it overflow off
  // the right/bottom edge when the click lands near either one.
  function positionMenu(clientX, clientY) {
    menu.style.left = "0px";
    menu.style.top = "0px";
    menu.style.display = "block";
    var box = menu.getBoundingClientRect();
    var x = Math.min(clientX, window.innerWidth - box.width - 6);
    var y = Math.min(clientY, window.innerHeight - box.height - 6);
    menu.style.left = Math.max(0, x) + "px";
    menu.style.top = Math.max(0, y) + "px";
  }

  // "Paste here" resolves the clicked screen point back to map pixels.
  function addPasteItem(clientX, clientY) {
    if (!window.EditorTool || !EditorTool.hasClipboard()) {
      return false;
    }
    var latLng = MapApp.map.mouseEventToLatLng({ clientX: clientX, clientY: clientY });
    addItem("Paste here", function() {
      EditorTool.pasteAt(latLng.lng, latLng.lat);
    });
    return true;
  }

  // hit may be null: a right-click on empty map opens a paste-only menu when
  // the editor clipboard holds something (see selection.js).
  ContextMenu.show = function(clientX, clientY, hit) {
    menu.innerHTML = "";
    if (!hit) {
      if (!addPasteItem(clientX, clientY)) {
        return;
      }
      positionMenu(clientX, clientY);
      return;
    }
    var bucket = hit.bucket;
    var info = Filters.contextInfo(bucket);

    // Pins the highlight on exactly this object for as long as the menu is
    // up -- map.js's own hover handler checks ContextMenu.isOpen() and backs
    // off entirely while it's open, so nothing here fights with it. Any
    // tooltip already on screen (the hover preview that was almost
    // certainly showing for this same object right before the right-click)
    // is hidden outright rather than just left frozen in place -- since the
    // hover handler is about to stop tracking the cursor entirely, leaving
    // it up would just mean a stale tooltip stuck at the old mouse position.
    MapApp.setHighlight(bucket, hit.id);
    if (window.Tooltip) {
      window.Tooltip.hide();
    }

    // Save-editor actions -- only for objects the edit engine can transform
    // (see EditorTool.targetsFromHit; null for vehicles/trains/etc).
    var editTargets = window.EditorTool ? EditorTool.targetsFromHit(hit) : null;
    var hasEditItems = false;
    if (editTargets) {
      addItem("Move this object…", function() {
        EditorTool.startMove(editTargets);
      });
      addItem("Copy this object", function() {
        EditorTool.copyTargets(editTargets);
      });
      addItem("Delete this object", function() {
        EditorTool.deleteTargets(editTargets);
      });
      hasEditItems = true;
    }
    if (addPasteItem(clientX, clientY)) {
      hasEditItems = true;
    }
    if (hasEditItems) {
      menu.appendChild(el("div", "contextMenuDivider"));
    }

    addItem("Hide this object", function() {
      MapApp.hideObject(bucket, hit.index);
      Filters.refreshHiddenObjectsIndicator();
    });
    menu.appendChild(el("div", "contextMenuDivider"));
    addItem("Hide layer: " + info.layerLabel, function() {
      Filters.hideLayer(bucket);
    });
    if (info.categoryLabel) {
      addItem("Hide category: " + info.categoryLabel, function() {
        Filters.hideCategory(bucket);
      });
    }
    positionMenu(clientX, clientY);
  };

  ContextMenu.hide = hide;
  ContextMenu.isOpen = function() {
    return menu.style.display !== "none";
  };

  // Closes on essentially anything else the user does -- a click/drag
  // elsewhere, Escape, or losing focus (e.g. alt-tabbing away) -- so it never
  // lingers stale over a spot the map has since panned/zoomed away from.
  document.addEventListener("mousedown", function(e) {
    if (menu.style.display !== "none" && !menu.contains(e.target)) {
      hide();
    }
  });
  document.addEventListener("keydown", function(e) {
    if (e.key === "Escape") {
      hide();
    }
  });
  window.addEventListener("blur", hide);
  var mapContainer = document.getElementById("map");
  if (mapContainer) {
    mapContainer.addEventListener("wheel", hide, { passive: true });
  }
})();
