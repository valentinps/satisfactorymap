// Floating-panel chrome: the top-left menu button that slides the sidebar
// overlay in/out, and the drag handles that resize the nav/detail columns
// (writing --nav-col-width / --detail-col-width, the same CSS variables the
// whole layout already derives from). Widths survive reloads via
// localStorage; filters.js's autoSizeNavPanel defers to a stored nav width
// (see Panels.storedNavWidth) so a save load doesn't undo a manual resize.
(function() {
  "use strict";

  var sidebar = document.getElementById("sidebar");
  var menuButton = document.getElementById("menuButton");
  var navPanel = document.getElementById("categoryNavPanel");
  var detailColumn = document.getElementById("categoryDetailColumn");
  var navHandle = document.getElementById("navResizeHandle");
  var detailHandle = document.getElementById("detailResizeHandle");

  var NAV_WIDTH_KEY = "smap.navColWidth";
  var DETAIL_WIDTH_KEY = "smap.detailColWidth";
  var NAV_MIN = 200, NAV_MAX = 480;
  var DETAIL_MIN = 220, DETAIL_MAX = 560;

  function readStoredWidth(key) {
    try {
      var value = parseInt(window.localStorage.getItem(key), 10);
      return isNaN(value) ? null : value;
    } catch (e) {
      return null; // localStorage can throw when blocked -- resizing still works, it just won't persist.
    }
  }

  function storeWidth(key, value) {
    try {
      window.localStorage.setItem(key, String(value));
    } catch (e) { /* see readStoredWidth */ }
  }

  window.Panels = {
    storedNavWidth: function() { return readStoredWidth(NAV_WIDTH_KEY); },
  };

  // Re-apply persisted widths before the first layout the user sees.
  var storedNav = readStoredWidth(NAV_WIDTH_KEY);
  if (storedNav !== null) {
    document.documentElement.style.setProperty("--nav-col-width", storedNav + "px");
  }
  var storedDetail = readStoredWidth(DETAIL_WIDTH_KEY);
  if (storedDetail !== null) {
    document.documentElement.style.setProperty("--detail-col-width", storedDetail + "px");
  }

  // ---- Show/hide the whole sidebar overlay --------------------------------

  menuButton.addEventListener("click", function() {
    var hidden = sidebar.classList.toggle("hidden");
    menuButton.classList.toggle("panelHidden", hidden);
    // The hamburger+logo card sits flush on the sidebar while it's shown
    // (its visual header); detached it floats as its own rounded card.
    var brandCluster = document.getElementById("brandCluster");
    if (brandCluster) {
      brandCluster.classList.toggle("detached", hidden);
    }
    menuButton.title = hidden ? "Show the side panel" : "Hide the side panel";
    menuButton.setAttribute("aria-expanded", String(!hidden));
  });

  // ---- Drag-to-resize handles ----------------------------------------------

  // measureEl (not the CSS variable) provides the drag's starting width --
  // the variable's initial value is a clamp() expression, so only the laid-
  // out element knows the real current pixel width.
  function setupResizeHandle(handle, measureEl, cssVar, storageKey, min, max) {
    handle.addEventListener("pointerdown", function(e) {
      if (e.button !== 0) {
        return;
      }
      e.preventDefault();
      handle.setPointerCapture(e.pointerId);
      handle.classList.add("dragging");
      document.body.classList.add("panelResizing");
      var startX = e.clientX;
      var startWidth = measureEl.getBoundingClientRect().width;
      var lastWidth = Math.round(startWidth);

      function onMove(ev) {
        lastWidth = Math.round(Math.min(max, Math.max(min, startWidth + (ev.clientX - startX))));
        document.documentElement.style.setProperty(cssVar, lastWidth + "px");
      }

      function onEnd() {
        handle.removeEventListener("pointermove", onMove);
        handle.removeEventListener("pointerup", onEnd);
        handle.removeEventListener("pointercancel", onEnd);
        handle.classList.remove("dragging");
        document.body.classList.remove("panelResizing");
        storeWidth(storageKey, lastWidth);
      }

      handle.addEventListener("pointermove", onMove);
      handle.addEventListener("pointerup", onEnd);
      handle.addEventListener("pointercancel", onEnd);
    });
  }

  setupResizeHandle(navHandle, navPanel, "--nav-col-width", NAV_WIDTH_KEY, NAV_MIN, NAV_MAX);
  setupResizeHandle(detailHandle, detailColumn, "--detail-col-width", DETAIL_WIDTH_KEY, DETAIL_MIN, DETAIL_MAX);
})();
