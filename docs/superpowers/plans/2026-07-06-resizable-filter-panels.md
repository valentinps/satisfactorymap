# Resizable Filter Panels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user drag the right edge of either left sidebar panel (`#categoryNavPanel`, `#categoryDetailColumn`) in the interactive map to resize it, while keeping the existing auto-fit-on-load behavior.

**Architecture:** Two invisible drag strips laid directly over each panel's existing right border (no separate visible handle graphic). A shared pointer-event helper in `map/static/map/filters.js` tracks the drag and writes the new width straight into the same CSS custom properties (`--nav-col-width`/`--detail-col-width`) the layout already reads, then pokes Leaflet via the existing `notifyMapResized()`. A new flag suppresses the nav panel's existing auto-fit-on-every-save-load so a manual resize survives switching saves within a session, but a full page refresh still starts fresh at auto-fit.

**Tech Stack:** Plain HTML/CSS/vanilla JS (no build step, no JS test framework in this repo — verified via `find . -iname package.json` returning nothing). Testing for this feature is manual browser verification, matching the project's existing pattern for this UI (there's no automated test to write).

## Global Constraints

- Panel resize range: `[150px, 700px]`, additionally capped at `window.innerWidth * 0.35` per panel (from `docs/superpowers/specs/2026-07-06-resizable-filter-panels-design.md`).
- No separate visible handle element — the grabbable area is an invisible strip directly over the existing panel border, full panel height, `cursor: col-resize`.
- Manual nav-panel resize persists across save switches within a session (a new `navPanelUserResized` flag suppresses `autoSizeNavPanel()`'s recompute); resets only on a full page refresh. Detail panel needs no equivalent flag (nothing currently auto-sizes it).
- No localStorage persistence across page refreshes (explicitly out of scope per the approved spec).

---

### Task 1: Resize handle markup + CSS

**Files:**
- Modify: `map/static/map/index.html:31-56`
- Modify: `map/static/map/map.css` (the `#categoryNavPanel` rule at line 246 and `#categoryDetailColumn` rule at line 278, plus one new rule block appended after `#categoryDetailPane`, currently ending around line 293)

**Interfaces:**
- Produces: two DOM elements, `#categoryNavResizeHandle` and `#categoryDetailResizeHandle`, each with class `resizeHandle`. Task 2 attaches pointer-event listeners to these by ID.

- [ ] **Step 1: Add the two handle elements in index.html**

Open `map/static/map/index.html`. Find this block (currently lines 30-57):

```html
<div id="sidebar">
  <div id="categoryNavPanel">
    <div id="categoryNavHeader">
      <button id="checkAllButton" title="Show everything on the map">Check all</button>
      <button id="uncheckAllButton" title="Hide everything on the map">Uncheck all</button>
    </div>
    <div id="categoryNavColumn"></div>

    <div id="sidebarFooter">
      <div id="loadPanel">
        <label for="saveSelect">Save file</label>
        <select id="saveSelect"></select>
        <button id="loadButton">Load</button>
        <div id="loadStatus"></div>
        <div id="loadProgressBar"><div id="loadProgressFill"></div></div>
        <div id="totalObjectCount"></div>
      </div>

      <div id="sftpPanel" style="display:none"></div>

      <div id="gameSettingsPanel" style="display:none"></div>
    </div>
  </div>

  <div id="categoryDetailColumn">
    <div id="categoryDetailPane"></div>
  </div>
</div>
```

Replace it with (adds `#categoryNavResizeHandle` as the last child of `#categoryNavPanel`, and `#categoryDetailResizeHandle` as the last child of `#categoryDetailColumn`):

```html
<div id="sidebar">
  <div id="categoryNavPanel">
    <div id="categoryNavHeader">
      <button id="checkAllButton" title="Show everything on the map">Check all</button>
      <button id="uncheckAllButton" title="Hide everything on the map">Uncheck all</button>
    </div>
    <div id="categoryNavColumn"></div>

    <div id="sidebarFooter">
      <div id="loadPanel">
        <label for="saveSelect">Save file</label>
        <select id="saveSelect"></select>
        <button id="loadButton">Load</button>
        <div id="loadStatus"></div>
        <div id="loadProgressBar"><div id="loadProgressFill"></div></div>
        <div id="totalObjectCount"></div>
      </div>

      <div id="sftpPanel" style="display:none"></div>

      <div id="gameSettingsPanel" style="display:none"></div>
    </div>
    <div id="categoryNavResizeHandle" class="resizeHandle"></div>
  </div>

  <div id="categoryDetailColumn">
    <div id="categoryDetailPane"></div>
    <div id="categoryDetailResizeHandle" class="resizeHandle"></div>
  </div>
</div>
```

- [ ] **Step 2: Add `position: relative` to both panels so the handles anchor to their own edge**

In `map/static/map/map.css`, find:

```css
#categoryNavPanel {
  flex: none;
  width: var(--nav-col-width);
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-right: 1px solid #333;
}
```

Add `position: relative;` so it reads:

```css
#categoryNavPanel {
  position: relative;
  flex: none;
  width: var(--nav-col-width);
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-right: 1px solid #333;
}
```

Find:

```css
#categoryDetailColumn {
  flex: none;
  width: var(--detail-col-width);
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
```

Add `position: relative;` so it reads:

```css
#categoryDetailColumn {
  position: relative;
  flex: none;
  width: var(--detail-col-width);
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
```

- [ ] **Step 3: Add the `.resizeHandle` CSS rule**

In `map/static/map/map.css`, directly after the `#categoryDetailPane` rule (currently ending around line 293), add:

```css

/* Invisible drag strip over a panel's right edge -- the whole edge is
   grabbable, not a separate visible handle graphic (see
   docs/superpowers/specs/2026-07-06-resizable-filter-panels-design.md).
   Requires the panel itself to be position:relative (see #categoryNavPanel/
   #categoryDetailColumn above) so "right:-3px" anchors to that panel's own
   edge instead of the whole #sidebar's. */
.resizeHandle {
  position: absolute;
  top: 0;
  bottom: 0;
  right: -3px;
  width: 6px;
  cursor: col-resize;
  z-index: 5;
  background: transparent;
}

.resizeHandle:hover,
.resizeHandle.dragging {
  background: rgba(255, 255, 255, 0.15);
}
```

- [ ] **Step 4: Verify the page still loads with no console errors**

Run: `py map/sav_map_server.py --no-browser` (from repo root), then open `http://127.0.0.1:<port>` (the port it prints) in a browser and open devtools console.
Expected: page loads normally, no new console errors, and hovering the thin (~6px) area right at the nav panel's right border (between the nav panel and wherever the map/detail column starts) shows a `col-resize` cursor and a faint highlight strip. Nothing is draggable yet (Task 2 wires that up) -- that's expected at this point.
Stop the server (Ctrl+C) when done checking.

- [ ] **Step 5: Commit**

```bash
git add map/static/map/index.html map/static/map/map.css
git commit -m "Add resize handle markup/CSS for the two left filter panels"
```

---

### Task 2: Drag-to-resize behavior + session-sticky nav panel flag

**Files:**
- Modify: `map/static/map/filters.js:461` (add flag near `var categoryEntries = [];`)
- Modify: `map/static/map/filters.js:484-496` (`autoSizeNavPanel`, add early-return guard)
- Modify: `map/static/map/filters.js:1085-1093` (init section, add handle wiring before the closing `})();`)

**Interfaces:**
- Consumes: `notifyMapResized()` (already defined at `filters.js:469`, no signature change).
- Produces: `attachResizeHandle(handleEl, cssVarName, min, max, onDragStart)` — a module-private function, not exported on `window.Filters`. Not consumed outside this file.

- [ ] **Step 1: Add the session-sticky flag**

In `map/static/map/filters.js`, find:

```js
  var categoryEntries = [];
```

Change to:

```js
  var categoryEntries = [];

  // True once the user has manually dragged the nav panel's resize handle --
  // suppresses autoSizeNavPanel()'s auto-fit recompute on later Filters.build()
  // calls (which fire on every save load/switch, not just first page open) so
  // a manual resize survives switching saves within the same page session.
  // Reset only by a full page refresh (this whole module reloads then).
  var navPanelUserResized = false;
```

- [ ] **Step 2: Guard `autoSizeNavPanel` on the flag**

Find:

```js
  function autoSizeNavPanel() {
    var navColumn = document.getElementById("categoryNavColumn");
    if (!navColumn || navColumn.children.length === 0) {
      return;
    }
```

Change to:

```js
  function autoSizeNavPanel() {
    if (navPanelUserResized) {
      return;
    }
    var navColumn = document.getElementById("categoryNavColumn");
    if (!navColumn || navColumn.children.length === 0) {
      return;
    }
```

- [ ] **Step 3: Add the generic drag helper and wire both handles**

Find the init section near the end of the file:

```js
  var checkAllButton = document.getElementById("checkAllButton");
  if (checkAllButton) {
    checkAllButton.addEventListener("click", function() { setAllVisibility(true); });
  }
  var uncheckAllButton = document.getElementById("uncheckAllButton");
  if (uncheckAllButton) {
    uncheckAllButton.addEventListener("click", function() { setAllVisibility(false); });
  }
})();
```

Change to (adds the helper function and two wiring calls before the closing `})();`):

```js
  var checkAllButton = document.getElementById("checkAllButton");
  if (checkAllButton) {
    checkAllButton.addEventListener("click", function() { setAllVisibility(true); });
  }
  var uncheckAllButton = document.getElementById("uncheckAllButton");
  if (uncheckAllButton) {
    uncheckAllButton.addEventListener("click", function() { setAllVisibility(false); });
  }

  // Makes an invisible drag strip over a panel's edge resize the panel by
  // writing straight to the same CSS custom property the layout already
  // reads (--nav-col-width/--detail-col-width), clamped to [min,max] and
  // additionally to at most 35% of the current window width (so the two
  // panels together can't squeeze the map to nothing on a narrow window).
  // Uses pointer capture on the handle itself so the drag keeps tracking
  // even if the cursor slips off the thin strip mid-drag.
  function attachResizeHandle(handleEl, cssVarName, min, max, onDragStart) {
    if (!handleEl) {
      return;
    }
    handleEl.addEventListener("pointerdown", function(e) {
      e.preventDefault();
      if (onDragStart) {
        onDragStart();
      }
      var panel = handleEl.parentElement;
      var startWidth = panel.getBoundingClientRect().width;
      var startX = e.clientX;
      handleEl.classList.add("dragging");
      document.body.style.userSelect = "none";
      handleEl.setPointerCapture(e.pointerId);

      function onMove(moveEvent) {
        var delta = moveEvent.clientX - startX;
        var effectiveMax = Math.min(max, window.innerWidth * 0.35);
        var width = Math.max(min, Math.min(startWidth + delta, effectiveMax));
        document.documentElement.style.setProperty(cssVarName, width + "px");
        notifyMapResized();
      }
      function onUp(upEvent) {
        handleEl.releasePointerCapture(upEvent.pointerId);
        handleEl.classList.remove("dragging");
        document.body.style.userSelect = "";
        handleEl.removeEventListener("pointermove", onMove);
        handleEl.removeEventListener("pointerup", onUp);
      }
      handleEl.addEventListener("pointermove", onMove);
      handleEl.addEventListener("pointerup", onUp);
    });
  }

  attachResizeHandle(document.getElementById("categoryNavResizeHandle"), "--nav-col-width", 150, 700, function() {
    navPanelUserResized = true;
  });
  attachResizeHandle(document.getElementById("categoryDetailResizeHandle"), "--detail-col-width", 150, 700, null);
})();
```

- [ ] **Step 4: Manually verify the drag works**

Run: `py map/sav_map_server.py` (from repo root; omit `--no-browser` so it opens for you, or add it and open the printed URL yourself).
Load any save from the dropdown so the nav panel populates.
Expected:
- Hovering the nav panel's right edge shows `col-resize` cursor + faint highlight.
- Dragging it left/right resizes the nav panel live, and the map reflows to fill the freed/taken space (no stale gap or overlap).
- Click a category so the detail column appears; its right edge is also draggable the same way.
- Width doesn't go below ~150px or above ~700px (or 35% of your window width, whichever is smaller) no matter how far you drag.

- [ ] **Step 5: Manually verify session-sticky persistence**

With the same page still open: drag the nav panel to a custom width, then load a *different* save from the dropdown (or reload the same one via the Load button).
Expected: nav panel keeps your dragged width (does not snap back to auto-fit).
Then fully refresh the browser tab (F5/Ctrl+R).
Expected: nav panel goes back to auto-fitting the content on this fresh load.

- [ ] **Step 6: Commit**

```bash
git add map/static/map/filters.js
git commit -m "Add drag-to-resize for the two left filter panels"
```

---

## Self-Review Notes

- **Spec coverage:** handle placement/no-visible-graphic (Task 1), generic drag helper + CSS var writes + notifyMapResized (Task 2 Step 3), session-sticky flag + auto-fit guard (Task 2 Steps 1-2), bounds incl. viewport-relative safety cap (Task 2 Step 3), detail column's `display:none` edge case (no extra guard needed since its handle is a child, verified in the design step -- no code required, nothing to task out), userSelect disable during drag (Task 2 Step 3). All 5 spec test scenarios map to Task 2 Steps 4-5. Nothing in the spec lacks a task.
- **Placeholder scan:** none found -- every step has literal, complete code or literal exact commands/expected output.
- **Type consistency:** `attachResizeHandle(handleEl, cssVarName, min, max, onDragStart)` signature and `navPanelUserResized` flag name are identical everywhere they're referenced across Task 2's steps.
