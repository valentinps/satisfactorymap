# v0.2.0

The window is a proper app now instead of a scatter of floating cards, the
world itself is on the map — border, water, caves — a new planning tool draws
the shortest belt or train network joining any set of points, and saves with
geared-up items on a belt load again.

## New

- **A docked app shell.** The chrome used to float over the map as
  free-standing cards: four separately-built modal dialogs, five progress
  bars, six list-row shapes. Everything is now one layout — an app bar across
  the top, the layers dock pinned left, the tool dock (altitude rail, paste
  and network panels) pinned right — built from one shared set of UI
  primitives, so the panels finally match each other. The dialogs are native
  `<dialog>` elements, which means the browser handles the focus trap, focus
  restore, Escape and stacking: Escape now peels exactly one layer at a time,
  and the hover tooltip paints above a modal instead of under it.

- **The world border, the water limit and the caves are map layers.** A "World"
  category (hidden by default, like the other world layers) draws the world
  perimeter in red with the altitudes its damage slabs start at, the water
  limit in cyan — 8.2 km of real water inside a 70 km ocean — and 84 caves
  traced from the cooked world export, each with its name, area and altitude
  range in the tooltip. Nothing in a save records a cave, so the outlines come
  from the game's own cave atmosphere volumes, tunnel splines and cave
  foliage, unioned and traced; they are approximate, and the map says so.

- **Optimal network finder.** A planning tool for "where should the belts,
  trains or power actually run". Type its name in the search bar, feed it a
  set of points — clicked map, clicked buildings, a whole rectangle selection
  in bulk, or typed X/Y metres — and it draws the shortest set of links
  joining them. Two link styles, both exact: **Straight** (Euclidean minimum
  spanning tree) and **X/Y only** (rectilinear — genuinely re-solved for the
  L1 metric, not the straight tree with corners drawn on, because changing the
  metric changes which points the optimal tree even connects). Optionally mark
  one point as a destination and a slider trades total network length against
  how far everything has to travel to reach it: on 5,700 real machines, 40%
  along the slider costs 9% more network and cuts total travel by 47%.

- **Search improvements.**
  - A collectable's suggestion row carries the same show/hide eye a building
    row does, so "Mercer Sphere" can answer both what is stashed in
    inventories and what is still lying out there.
  - Every individual location in an item's result list gets a crosshair that
    flies the map to that one machine and marks it with a solid pink pin —
    "where is *this* one" without blanking every other layer for context.
  - A query's words now match anywhere in a label, in any order, so "coal
    node" finds "Coal Ore (Resource Node)". Tighter matches still rank first,
    so "iron" keeps surfacing Iron Plate ahead of Reinforced Iron Plate.

## Fixes

- **Saves with stateful gear on a belt load again.** Anything that carries
  per-item state — a jetpack with fuel, a loaded weapon, a gas mask with a
  filter — parked on a conveyor made the whole save fail with
  `Value ... does not match the expected value 0`. Since save version 44 an
  item slot carries a state flag and an inline state record; the per-belt
  format read it, but the conveyor *chain* format — where every belt item
  actually lives since 1.0 — assumed it away, so the first geared-up item
  turned the rest of the object into noise. Both formats read it now, and the
  state rides along verbatim, so a jetpack that survives a cut belt line comes
  back with its fuel rather than empty. Reported as issue #22.

- **The item search counts what is riding the belts.** It summed every
  inventory but nothing in transit, so a running factory under-reported by its
  whole belt network — half the iron ingots on one 54 MB save (166k of 331k),
  559k items on another. Items now count against the belt they are physically
  on, so every location listed is still a real placed building at a real
  position.

- **Centred things are actually centred.** The panels floating over the map
  were siblings of the map, not of the visible area, so their centre was up to
  109 px off from the search field above them — visible under the search pill,
  where the active-filter banner sat noticeably to one side. Every centred
  element now lines up on the same axis at every viewport size and dock state.

- **Rows that had lost their styling.** A rename of the shared UI classes left
  the selection dialogs rendering their rows as raw text ("Concrete51,836" with
  no column, hairline or hover), the vehicle modal's "Fuel loaded" label
  unstyled, and the hamburger's "click here to get the panel back" accent
  missing. Auditing every class the JS emits against the classes the
  stylesheets define turned all three up.

## Under the hood

- **Game data v3**, refreshed from the current game files.
- **UI regression harnesses.** `ui_shots.py` captures 17 UI states (empty,
  loaded, each dialog, each tool panel, three narrow viewports) and pixel-diffs
  them against a previous run; `ui_behaviour.py` covers what a screenshot
  cannot — that dialogs are real modals, that the tooltip paints above one,
  that Escape peels exactly one layer, that opening a dock never resizes the
  map. `ui_classes.py` checks every class the JS emits against the stylesheets.
- **Design tokens.** The colour, type, spacing and radius vocabulary lives in
  one block: 19 distinct font sizes became 8 steps, 11 radii became 5, 25
  `!important` declarations became none, and ~180 raw hex literals became 17
  genuine one-offs.
- The belt-item fix is gated by a new test save in the public corpus, and the
  whole 22-save corpus is checked for byte-identical map payloads across the
  change.

## Checksums (SHA-256)

```
{CHECKSUMS}
```
