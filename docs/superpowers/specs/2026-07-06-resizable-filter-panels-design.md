# Resizable filter panels (nav + detail)

## Problem

The map's two left sidebar panels (`#categoryNavPanel`, `#categoryDetailColumn`)
have fixed-at-render widths: the nav panel auto-fits its content
(`autoSizeNavPanel()` in `map/static/map/filters.js`), the detail panel is a
flat 280px (`--detail-col-width` in `map/static/map/map.css`). Neither can be
resized by the user. Goal: let the user drag either panel's edge to resize it,
without losing the current auto-fit-on-load behavior.

## Components

- Two invisible drag strips, one per panel, laid directly over each panel's
  existing right border (`~6px` wide, full panel height, `position: absolute`,
  centered on the visual border line via the panel's `position: relative`).
  No separate visible handle graphic -- `cursor: col-resize` plus a subtle
  background highlight only on hover/while dragging is the only visual cue.
  `#categoryNavResizeHandle` sits on `#categoryNavPanel`'s right edge;
  `#categoryDetailResizeHandle` sits on `#categoryDetailColumn`'s right edge.
- One generic helper in filters.js: `attachResizeHandle(handleEl, cssVarName, min, max, onDragStart)`.
  Uses pointerdown/pointermove/pointerup on the handle (captured via
  `setPointerCapture`) to track drag delta, computes the new width from the
  panel's width at drag-start plus the delta, clamps to `[min, max]`, writes
  the result straight to the same CSS custom property
  (`--nav-col-width`/`--detail-col-width`) the layout already reads, and calls
  the existing `notifyMapResized()` so Leaflet's `invalidateSize()` fires and
  the map reflows live during the drag.

## Persistence rule

- `autoSizeNavPanel()` currently re-runs on every `Filters.build()` call (every
  save load/switch within a page session, not just first page open) --
  overwriting the nav panel's width to fit content each time.
- New module-scoped flag `navPanelUserResized` (default `false`).
  `autoSizeNavPanel()` early-returns without recomputing when it's `true`. The
  nav handle's pointerdown sets it `true`. This makes a manual resize stick
  across save switches within the session, but a full page refresh resets the
  flag, so a fresh load always goes back to auto-fit -- matching "keep the
  current automatic size on load".
- The detail panel has no existing auto-size call to fight with (its width
  only ever comes from the static CSS default), so a manual resize there
  simply persists for the session with no extra flag needed.

## Bounds

- Each panel clamps to `[150px, 700px]` while dragging (the wider range,
  matching the TODO's "show more text/more of the map" intent).
- Additional safety clamp on the max, not user-facing: cap each panel's max at
  `min(700, window.innerWidth * 0.35)` so on a narrow window the two panels
  combined can't squeeze the map area to nothing.

## Edge cases

- The detail column is `display:none` when no category is selected (see
  `body.no-category-selected #categoryDetailColumn` in map.css). Its resize
  handle is a child of that column, so it's automatically non-interactive and
  invisible too -- no extra guard needed.
- During a drag, `document.body.style.userSelect` is set to `"none"` (restored
  on pointerup) so dragging doesn't select surrounding page text.

## Testing

Manual browser check only (no existing test suite covers this UI):
1. Drag the nav panel edge -- map reflows live, width stays within bounds.
2. Drag the detail panel edge -- same.
3. Resize nav panel, then switch/reload a save in the same session -- manual
   width persists (does not snap back to auto-fit).
4. Full page refresh -- nav panel goes back to auto-fit on the new load.
5. Try dragging past both the 150/700 bounds and the narrow-viewport safety
   clamp -- width stays clamped, no map/layout breakage.
