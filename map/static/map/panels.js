// Shell chrome: the two docks and the app bar.
//
// The docks are pinned to the window's edges on top of the map (see map.css):
// attached and flush, but out of flow, so showing or resizing one never
// changes the map's box. Nothing here needs to tell Leaflet about a dock.
//
// Owns:
//   - collapsing/showing the left dock (the app bar's hamburger)
//   - dragging its width (persisted in localStorage)
//   - the left dock's two-pane push navigation (categories -> one category)
//   - the right tool dock: which tool panel is in it, and whether it is open
//   - the "Progression" dropdown, and the desktop app's update pill
(function() {
  "use strict";

  var body = document.body;
  var mapEl = document.getElementById("map");
  var sidebar = document.getElementById("sidebar");
  var menuButton = document.getElementById("menuButton");
  var dockHandle = document.getElementById("dockResizeHandle");
  var pagerNav = document.getElementById("dockPaneNav");
  var pagerDetail = document.getElementById("dockPaneDetail");
  var detailBackBtn = document.getElementById("detailBackBtn");
  var detailTitle = document.getElementById("detailTitle");
  var detailSwatch = document.getElementById("detailSwatch");
  var toolPanels = document.getElementById("toolPanels");

  var DOCK_WIDTH_KEY = "smap.dockLeftWidth";
  var DOCK_MIN = 220, DOCK_MAX = 480;

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

  // Re-apply the persisted width before the first layout the user sees.
  var storedWidth = readStoredWidth(DOCK_WIDTH_KEY);
  if (storedWidth !== null) {
    document.documentElement.style.setProperty("--dock-left-width", storedWidth + "px");
  }

  window.Panels = {
    storedNavWidth: function() { return readStoredWidth(DOCK_WIDTH_KEY); },
  };

  // ---- Left dock: show/hide -------------------------------------------------

  function setDockHidden(hidden) {
    body.classList.toggle("dock-hidden", hidden);
    menuButton.classList.toggle("is-active", hidden);
    var label = hidden ? "Show the layers panel" : "Hide the layers panel";
    menuButton.title = label;
    menuButton.setAttribute("aria-label", label);
    menuButton.setAttribute("aria-expanded", String(!hidden));
  }

  // Below the drawer breakpoint (see map.css) the dock stops being a column of
  // the grid and overlays the map instead, so leaving it open would mean a
  // screen that is mostly panel. Collapse it on the way in and restore it on
  // the way out -- but only until the user expresses a preference, after which
  // their choice sticks and the breakpoint stops touching it.
  var DRAWER_BREAKPOINT = 820;
  var drawerQuery = window.matchMedia("(max-width: " + DRAWER_BREAKPOINT + "px)");
  var userChoseDockState = false;

  menuButton.addEventListener("click", function() {
    userChoseDockState = true;
    setDockHidden(!body.classList.contains("dock-hidden"));
  });

  function syncDockToWidth() {
    if (!userChoseDockState) {
      setDockHidden(drawerQuery.matches);
    }
  }

  if (drawerQuery.addEventListener) {
    drawerQuery.addEventListener("change", syncDockToWidth);
  }
  syncDockToWidth();

  // ---- Left dock: two-pane push navigation ----------------------------------
  //
  // Both panes are always in the DOM (the pager slides between them), so the
  // one that is off-screen has to be taken out of the tab order and hidden
  // from assistive tech -- otherwise Tab walks into a pane nobody can see.
  // `inert` does both in one attribute.
  function syncPaneInertness() {
    var detailOpen = body.classList.contains("category-open");
    pagerNav.inert = detailOpen;
    pagerDetail.inert = !detailOpen;
  }

  Panels.showCategoryDetail = function(title, color) {
    detailTitle.textContent = title || "";
    detailSwatch.style.background = color || "transparent";
    detailSwatch.style.display = color ? "inline-block" : "none";
    body.classList.add("category-open");
    syncPaneInertness();
  };

  Panels.showCategoryList = function() {
    body.classList.remove("category-open");
    syncPaneInertness();
  };

  Panels.isCategoryDetailOpen = function() {
    return body.classList.contains("category-open");
  };

  // The back button and Escape are the two ways out. Filters owns what
  // "deselect" means to the map, so route through it when it is loaded.
  function goBack() {
    if (window.Filters && Filters.deselectAllCategories) {
      Filters.deselectAllCategories();
    } else {
      Panels.showCategoryList();
    }
  }

  detailBackBtn.addEventListener("click", goBack);

  UI.onEscape(UI.LAYER.view, function() {
    if (!Panels.isCategoryDetailOpen()) {
      return false;
    }
    goBack();
    return true;
  });

  syncPaneInertness();

  // ---- Right dock: one tool at a time ---------------------------------------
  //
  // Every tool panel (paste placement, network finder) is authored as a
  // body-level element in index.html and adopted into the dock here, so a tool
  // does not have to know it lives in a dock -- and, more usefully, so two
  // tools cannot end up drawn on the same pixels, which is exactly what the
  // free-floating versions did (both were anchored top-right).
  var currentTool = null;

  Panels.openTool = function(el) {
    if (!el) {
      return;
    }
    if (currentTool && currentTool !== el) {
      Panels.closeTool(currentTool);
    }
    if (el.parentNode !== toolPanels) {
      toolPanels.appendChild(el);
    }
    el.style.display = "";
    currentTool = el;
    body.classList.add("tool-open");
    Analytics.toolOpened(el.id);
  };

  Panels.closeTool = function(el) {
    if (el && el !== currentTool) {
      el.style.display = "none"; // A tool that was already swapped out; the dock did not change.
      return;
    }
    if (currentTool) {
      currentTool.style.display = "none";
      currentTool = null;
    }
    body.classList.remove("tool-open");
  };

  Panels.isToolOpen = function(el) {
    return el ? currentTool === el : currentTool !== null;
  };

  // Pre-adopt the panels so their first open does not reparent (and reset)
  // live form state.
  ["pastePanel", "networkPanel"].forEach(function(id) {
    var el = document.getElementById(id);
    if (el) {
      el.style.display = "none";
      toolPanels.appendChild(el);
    }
  });

  // ---- Left dock: drag to resize --------------------------------------------

  // The element (not the CSS variable) provides the drag's starting width: the
  // variable's initial value is a clamp() expression, so only the laid-out
  // element knows the real current pixel width.
  dockHandle.addEventListener("pointerdown", function(e) {
    if (e.button !== 0) {
      return;
    }
    e.preventDefault();
    dockHandle.setPointerCapture(e.pointerId);
    dockHandle.classList.add("dragging");
    body.classList.add("dockResizing");
    var startX = e.clientX;
    var startWidth = sidebar.getBoundingClientRect().width;
    var lastWidth = Math.round(startWidth);

    function onMove(ev) {
      lastWidth = Math.round(Math.min(DOCK_MAX, Math.max(DOCK_MIN, startWidth + (ev.clientX - startX))));
      document.documentElement.style.setProperty("--dock-left-width", lastWidth + "px");
    }

    function onEnd() {
      dockHandle.removeEventListener("pointermove", onMove);
      dockHandle.removeEventListener("pointerup", onEnd);
      dockHandle.removeEventListener("pointercancel", onEnd);
      dockHandle.classList.remove("dragging");
      body.classList.remove("dockResizing");
      storeWidth(DOCK_WIDTH_KEY, lastWidth);
    }

    dockHandle.addEventListener("pointermove", onMove);
    dockHandle.addEventListener("pointerup", onEnd);
    dockHandle.addEventListener("pointercancel", onEnd);
  });

  // ---- Keep Leaflet in step with the window ---------------------------------
  //
  // The docks overlay the map rather than taking a column of the grid (see
  // map.css), so opening, closing or resizing one does NOT change the map's
  // box -- which is exactly why none of this has to compensate for anything.
  // The map's size is a function of the window alone, and this is here for the
  // one thing that still changes it: the window itself being resized.
  //
  // Do not be tempted to make the docks resize the map again "so the map is
  // the space between them". That is where the ~141px sideways lurch on every
  // dock toggle came from; docs/dock-map-anchoring.md has the two failed
  // attempts at correcting it.
  if (mapEl && window.ResizeObserver) {
    var pending = false;
    new ResizeObserver(function() {
      if (pending) {
        return;
      }
      pending = true;
      requestAnimationFrame(function() {
        pending = false;
        if (window.MapApp && MapApp.map) {
          MapApp.map.invalidateSize({ animate: false });
        }
      });
    }).observe(mapEl);
  }

  // ---- Desktop-only chrome ---------------------------------------------------

  // Inside the desktop app the "download the desktop app" link is noise.
  var desktopAppLink = document.getElementById("desktopAppLink");
  if (desktopAppLink && window.__TAURI__) {
    desktopAppLink.style.display = "none";
  }

  // Desktop auto-update (Tauri updater plugin). Quiet check shortly after
  // startup; a waiting update shows the accent pill in the app bar. Clicking
  // downloads + verifies the signed installer and runs it (passive mode) -- on
  // Windows the app exits into the installer and reopens updated. Any failure
  // (offline, no latest.json on the newest release yet, endpoint change) is
  // silently ignored: updating is never worth an error dialog on launch.
  var updatePill = document.getElementById("updatePill");
  var updatePillLabel = document.getElementById("updatePillLabel");
  if (updatePill && window.__TAURI__ && window.__TAURI__.updater) {
    setTimeout(function() {
      window.__TAURI__.updater.check().then(function(update) {
        if (!update) {
          return;
        }
        updatePillLabel.textContent = "Update to v" + update.version;
        updatePill.title = "A new version is ready -- click to download and install";
        updatePill.style.display = "flex";
        updatePill.addEventListener("click", function() {
          updatePill.disabled = true;
          var received = 0, total = 0;
          update.downloadAndInstall(function(progress) {
            if (progress.event === "Started") {
              total = progress.data.contentLength || 0;
            } else if (progress.event === "Progress") {
              received += progress.data.chunkLength;
              updatePillLabel.textContent = total
                ? "Downloading… " + Math.round(received / total * 100) + "%"
                : "Downloading…";
            } else if (progress.event === "Finished") {
              updatePillLabel.textContent = "Installing…";
            }
          }).catch(function() {
            updatePill.disabled = false;
            updatePillLabel.textContent = "Update failed — retry";
          });
        });
      }).catch(function() { /* no manifest / offline: stay quiet */ });
    }, 3000);
  }

  // ---- "Progression" dropdown (app bar, right) -------------------------------
  // Open/close chrome only -- the rows' own click handlers live in
  // progression.js/finditem.js, bound by id. A row click bubbles here and
  // closes the menu so the dialog it opens isn't sitting under it.
  var statusMenuButton = document.getElementById("statusMenuButton");
  var statusMenu = document.getElementById("statusMenu");
  if (statusMenuButton && statusMenu) {
    var setStatusMenuOpen = function(open) {
      statusMenu.style.display = open ? "block" : "none";
      statusMenuButton.classList.toggle("open", open);
      statusMenuButton.setAttribute("aria-expanded", String(open));
    };
    statusMenuButton.addEventListener("click", function() {
      setStatusMenuOpen(statusMenu.style.display === "none");
    });
    statusMenu.addEventListener("click", function(e) {
      if (e.target.closest(".statusMenuRow")) {
        setStatusMenuOpen(false);
      }
    });
    document.addEventListener("click", function(e) {
      if (statusMenu.style.display !== "none" &&
          !e.target.closest("#statusMenuWrap")) {
        setStatusMenuOpen(false);
      }
    });
    UI.onEscape(UI.LAYER.menu, function() {
      if (statusMenu.style.display === "none") {
        return false;
      }
      setStatusMenuOpen(false);
      return true;
    });
  }
})();
