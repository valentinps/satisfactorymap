# Generated data files

Produced by `game_data/extract_docs_json.py` from `game_data/docs.json`. Regenerate after
every game update / Docs.json refresh:

```
py game_data/extract_docs_json.py
```

All three files are keyed by `ClassName` (the short in-game class, e.g.
`Desc_IronPlate_C`, `Recipe_IronPlate_C`, `Build_Wall_Conveyor_8x4_04_C`) --
the same short name `sav_parse.pathNameToReadableName` / the map's typePath
lookups already work with.

## recipes.json

```json
"Recipe_IronPlate_C": {
  "displayName": "Iron Plate",
  "ingredients": [{ "item": "Desc_IronIngot_C", "amount": 3.0 }],
  "product": [{ "item": "Desc_IronPlate_C", "amount": 2.0 }],
  "producedIn": ["Build_ConstructorMk1_C", "BP_WorkBenchComponent_C", "FGBuildableAutomatedWorkBench"],
  "durationSeconds": 6.0
}
```

- `ingredients` / `product`: amount is per craft cycle, not per minute.
- `producedIn`: short class names of buildings/components that can craft this
  recipe. Not always a `Build_*` buildable -- workbench/hand-craft entries show
  up as `BP_WorkBenchComponent_C` or a bare native class like
  `FGBuildableAutomatedWorkBench`. Treat as opaque identifiers, not guaranteed
  to resolve in `buildings.json`.
- `durationSeconds` is `null` if the field was missing/empty (shouldn't happen
  for real recipes, but the source data isn't assumed reliable).
- `variablePowerRangeMW` (optional, `[min, max]` in MW at 100% clock speed) --
  present only on variable-power recipes, where the recipe itself drives the
  machine's draw (oscillating between min and max over each production cycle)
  instead of the building's own rated figure: every Particle Accelerator/
  Converter/Quantum Encoder recipe (~43 total). Computed from the source's
  `mVariablePowerConsumptionConstant`/`Factor` as
  `[Constant, Constant + Factor]`; constant-power recipes carry the
  do-nothing defaults `Constant=0`/`Factor<=1` and get no field.
  Source-data glitch: the phase-5 Space Elevator part recipes made in the
  Blender/Manufacturer *also* carry real-looking `mVariablePowerConsumption*`
  values, but the game ignores them there (confirmed in-game: those machines
  hold their flat 75/55 MW) -- only `FGBuildableManufacturerVariablePower`
  machines apply recipe-driven power, so the extractor strips the field from
  recipes not produced in one (identified as the buildings carrying
  `powerConsumptionRangeMW`, see `buildings.json` below).
- Only real craftable recipes (`FGRecipe` group). Customization recipes
  (paint/pattern/swatch unlocks) are skipped entirely -- not real crafting.

## items.json

```json
"Desc_IronPlate_C": {
  "displayName": "Iron Plate",
  "description": "Used for crafting.\r\nOne of the most basic parts.",
  "stackSize": "SS_BIG",
  "stackSizeCount": 200,
  "icon": "/FactoryGame/Resource/Parts/IronPlate/UI/IconDesc_IronPlates_256"
}
```

- Covers everything that isn't a `Build_*` buildable and has a non-empty
  display name: raw resources, parts, consumables, ammo, equipment/weapons,
  vehicle descriptors. Broad on purpose -- see `extract_docs_json.py` header
  for the exact rule.
- `stackSize` is the raw game enum (`SS_ONE`, `SS_SMALL`, `SS_MEDIUM`,
  `SS_BIG`, `SS_HUGE`, `SS_FLUID`); `stackSizeCount` is the actual resolved
  number of items per stack for that enum, already computed by the game data
  so you don't need your own enum-to-count table.
- `description` keeps the game's literal `\r\n` line breaks.
- `icon` is `mPersistentBigIcon` reformatted from the game's raw
  `Texture2D /Game/FactoryGame/...UI/IconDesc_Foo_256.IconDesc_Foo_256` into
  an asset path rooted at `/FactoryGame/...` (the `/Game` prefix and the
  trailing `.IconDesc_Foo_256` repeat are stripped). `null` if the source
  entry had no icon (rare).

## resources.json

```json
"Desc_Coal_C": {
  "displayName": "Coal",
  "description": "...",
  "stackSize": "SS_HUGE",
  "stackSizeCount": 500,
  "icon": "/FactoryGame/Resource/RawResources/Coal/UI/IconDesc_CoalOre_256"
}
```

Same shape as `items.json` (it's the exact same extractor function), but for
the `FGResourceDescriptor` group specifically -- raw ore/stone/fluid resources
(Coal, the 5 metal ores, Limestone, Sulfur, Raw Quartz, SAM, Crude Oil,
Nitrogen Gas, Water -- 13 total, every one with a real `mDisplayName`/icon).
Kept out of `items.json` since these aren't crafted/held the way a normal
inventory item is (they come from resource nodes, not a recipe), but they
still show up in the Dimensional Depot/building inventory lists, so they need
an icon too. Geyser (`Desc_Geyser_C` as used elsewhere in this codebase, e.g.
`sav_data.resourcePurity`) is NOT here -- it's a synthetic key this parser
invented for a resource node type that has no real `FGResourceDescriptor` (or
any other Docs.json entry) backing it at all; consumers needing an icon for
it use a hardcoded glyph instead (see filters.js).

## buildings.json

```json
"Build_Wall_Conveyor_8x4_04_C": {
  "displayName": "Perpendicular Wall Conveyor",
  "description": "...",
  "dimensions": { "Width": 800.0, "Height": 400.0, "AngularDepth": 0.0 },
  "clearance": [{ "min": {...}, "max": {...}, "rotated": false }],
  "adaptiveLength": {},
  "icon": "/FactoryGame/Buildable/Factory/..."
}
```

`icon` is resolved the same way as `items.json`'s (see above), but sourced
from the buildable's `Desc_*_C` companion descriptor -- `Build_*_C` entries
never carry `mPersistentBigIcon` themselves. `null` for the ~2 buildings
(out of 546) whose descriptor has no icon in the source data.

### Power consumption (optional fields)

Rated power draw in MW at 100% clock speed, from `mPowerConsumption` /
`mEstimatedMininumPowerConsumption`/`mEstimatedMaximumPowerConsumption`
(sic -- the "Mininum" typo is the game's own field name). At most one of the
two fields is present:

- `powerConsumptionMW` (number) -- steady rated draw, for the ~36 buildings
  with a non-zero `mPowerConsumption` (production machines, but also train/
  truck/drone stations, lights, pumps, the AWESOME Sink, etc).
- `powerConsumptionRangeMW` (`[min, max]`) -- for the three machines whose
  draw oscillates over each production cycle (Particle Accelerator,
  Converter, Quantum Encoder): they have `mPowerConsumption=0` and instead
  carry the game's own estimated min/max across their recipes. The actual
  range while producing depends on the active recipe -- see `recipes.json`'s
  `variablePowerRangeMW`, which overrides this building-level estimate.

Both fields are omitted for buildings with no (or zero) draw: the source data
has an explicit `mPowerConsumption: 0` on plenty of non-consumers (generators,
storage, valves, junctions, the Resource Well Extractor satellite), so a zero
is treated the same as the field being absent rather than emitted as a
misleading "0 MW" rating. Clock-speed scaling is non-linear (exponent
~1.321929, the source's `mPowerConsumptionExponent`) -- see
`POWER_CLOCK_SPEED_EXPONENT` in `map/sav_map_data.py`.

### The three size fields, and why there are three

Buildables in this game don't share one consistent "size" representation.
Rather than guess/normalize, the extractor stores whatever the source data
actually gives per building, in up to three independent buckets. **A given
building may populate one, two, or all three** -- check `dimensions` first,
then `clearance`, then `adaptiveLength` for whichever is non-empty.

1. **`dimensions`** -- fixed footprint, present when the game itself exposes
   simple named fields. Keys are whichever of these existed on that class
   (missing ones are just absent, not zero):
   - `Width`, `Depth`, `Height` -- straightforward box dimensions in
     centimeters (game's native unit; divide by 100 for meters).
   - `AngularDepth` -- extra depth-like offset seen on some walls (angled
     wall pieces), usually `0.0` for straight ones.

   Example: `Build_Foundation_Metal_8x4_C` has `Width: 800, Depth: 800,
   Height: 400` (8m x 8m x 4m foundation).

   The three Blueprint Designer tiers (`Build_BlueprintDesigner_C`/`_MK2_C`/
   `_Mk3_C`) are a special case: they have no `mWidth`/`mDepth`/`mHeight` at
   all, only an `mDimensions` struct in 8m foundation-grid squares (e.g.
   `(X=4,Y=4,Z=4)` for the 32m x 32m x 32m Mk.1). The extractor converts that
   to `Width`/`Depth` in centimeters (`grid units * 800`) so it lands in this
   same bucket rather than needing a fourth size representation.

2. **`clearance`** -- parsed from the game's `mClearanceData`, a physical
   bounding box around the building's origin, given as `min`/`max` corners in
   centimeters. Present on most buildables (even ones that also have
   `dimensions`), and is often the *only* size info for pieces that don't
   expose named width/depth/height (beams, corner pieces, angled buildables).
   - `rotated: true` means the box came with a `RelativeTransform` rotation
     in the source data -- **the box's X/Y/Z axes do not necessarily line up
     with width/depth/height in that case**. The extractor deliberately does
     NOT try to remap axes here; consume the box as "this is the physical
     footprint around the origin" rather than "X is width". Example:
     `Build_Beam_C` has `rotated: true` and its box's short axis (Z, ~0-400)
     is actually the beam's *length* direction, not height.
   - Some buildables have more than one clearance box (list, not single dict)
     if the game defines multiple soft-clearance volumes for that piece.
   - Some buildables have `clearance: []` (empty) -- no clearance data in
     Docs.json for that class (seen on some adaptive-length pieces like
     conveyor belts, which rely on `adaptiveLength` instead). A few classes
     (`Build_Elevator_C`, `Build_FloodlightWall_C`) have *nothing* at all --
     no `clearance`, `dimensions`, or `adaptiveLength` -- despite having a
     real physical footprint in-game; consumers needing a size for these
     have to fall back to a hand-curated value (see
     `HAND_CURATED_FOOTPRINTS_METERS_BY_CLASSNAME` in `map/sav_map_data.py`).
   - Known stale-data quirk: `Build_BigGarageDoor_16x8_C` (and its
     `_Concrete_C`/`_Steel_C` skins) report the exact same `mClearanceData`
     as the unrelated, much smaller `Build_Gate_Automated_8x4_C` -- half the
     real 16m width. `dimensions.Width` (1600) is correct for these; it's
     just `clearance` that's wrong. Confirmed by comparing every building's
     `dimensions.Width`/`Depth` against its `clearance` extents -- these 3
     are the only ones where the named dimension exceeds what clearance
     alone would imply.

3. **`adaptiveLength`** -- present on buildables whose length is chosen by
   the player at build time (belts, pipes, power lines, railway, conveyor
   lifts, beams), so there's no single fixed size to report. Keys are
   whichever of these existed:
   - `MeshLength` / `MeshHeight` -- the length of one repeating mesh segment
     (e.g. `Build_ConveyorBeltMk1_C` -> `MeshLength: 200`), not the total
     buildable length, which is player-chosen and per-instance (only known
     from the save file itself, not Docs.json).
   - `DefaultLength` / `MaxLength` / `Length` / `CachedLength` -- seen on
     beams (`DefaultLength`/`MaxLength`) and wires (`MaxLength`).
   - `MaxPowerTowerLength`, `LengthPerCost` -- power line specific.
   - `FlowIndicatorMinimumPipeLength` -- pipeline specific, not really a size.
   - `OpposingConnectionClearance` -- conveyor lift specific.

### Gotcha: `displayName` can encode a size variant that isn't in the numeric fields

Some buildings' in-game display name includes a size that describes a
*variant*, not necessarily matching how `dimensions` breaks it down. Example:
`Build_Foundation_Metal_8x4_C` -> `displayName: "Foundation (4 m)"` (the "4 m"
refers to the *height* variant of an otherwise-standard 8x8 foundation footprint)
while `dimensions` correctly reports `Width: 800, Depth: 800, Height: 400`.
**Treat `dimensions`/`clearance`/`adaptiveLength` as the source of truth for
math; don't parse size out of `displayName` text.**

### Entries with no size data at all

A building can have `dimensions: {}`, `clearance: []`, and `adaptiveLength: {}`
all empty simultaneously (e.g. logic-only buildables like priority switches).
That's a legitimate "no physical footprint data available", not a bug.

## buildingCategories.json

```json
"Build_WorkBench_C": {
  "topCategory": "Sub_Production",
  "subCategory": "SC_Workstations",
  "menuPriority": 1.0
}
```

Build-menu placement, keyed by the same `Build_*_C` name as `buildings.json`.

- `topCategory` / `subCategory` are the game's internal asset names (folder
  `Sub_<X>` / asset `SC_<X>`), not display strings -- there's no
  human-readable label for either anywhere in Docs.json. Need a hand-curated
  name map for UI display (6 top categories, 50 subcategories total).
- `menuPriority` sorts buildables within a subcategory, lower first.
- Source data quirk: this info lives on a separate `FGBuildingDescriptor`
  companion class (`Desc_*_C`), not on the buildable itself, and the two are
  linked only by naming convention -- not an explicit field. `extract_docs_json.py`
  resolves the vast majority of these automatically (`Desc_X_C` -> `Build_X_C`,
  case differences, a Tris/FlipTris size-token reorder) plus a hardcoded
  correction table (`KNOWN_DESCRIPTOR_TO_BUILD_CORRECTIONS`) for real typos/
  irregularities in the game's own class names -- e.g. `Build_WalkwayTrun_C`
  (yes, "Trun"), `Build_CatwalkCorner_C` (descriptor still says `CatwalkTurn`),
  or `Build_QuarterPipeMiddle_Ficsit_8x1_C` (descriptor says `4x1` where every
  other size token in this file says `8x1`). Every entry currently resolves
  (546/546); if a future Docs.json update introduces a new one that doesn't,
  the script prints a warning and drops it rather than guessing -- don't
  assume every building in `buildings.json` is guaranteed to have a category
  entry here.

## schematics.json

```json
"Research_Quartz_1_2_C": {
  "displayName": "Silica",
  "type": "MAM",
  "techTier": 3,
  "menuPriority": 0.0,
  "cost": [{"item": "Desc_RawQuartz_C", "amount": 20.0}],
  "researchTree": "Quartz",
  "unlockRecipes": ["Recipe_Silica_C"]
}
```

Every `FGSchematic` in Docs.json -- the static side of everything the save's
SchematicManager records as "purchased". Consumed by
`sav_map_data.collectProgression` for the map's progression panels.

- `type` is `mType` with its `EST_` prefix stripped. The values in play:
  `Milestone` (HUB milestones), `Tutorial` (the six tier-0 HUB Upgrades),
  `MAM` (research nodes), `Alternate` (hard-drive rewards), `ResourceSink`
  (AWESOME Shop products), `HardDrive` (one hidden internal node),
  `Customization`, `Custom`.
- `cost` is what it costs to purchase (milestone parts, research items, or
  `Desc_ResourceSinkCoupon_C` for shop products); same shape as a recipe's
  `ingredients`.
- `researchTree` (MAM/HardDrive types only) is the folder token from the
  schematic's own asset path (`.../Research/<Tree>_RS/...`) -- the
  `BPD_ResearchTree_*` assets themselves aren't in Docs.json, so there are no
  display names for trees (hand-curated map in sav_map_data.py).
- `shopCategories` (ResourceSink only) -- `SC_RSS_*_C` shop-tab class names;
  no display strings exist for these either.
- `unlockRecipes` / `giveItems` -- what purchasing actually grants:
  recipes unlocked (`BP_UnlockRecipe_C`) and/or items handed over directly
  (`BP_UnlockGiveItem_C`, e.g. shop ammo packs -- those are repeatable
  purchases the save never marks as "purchased"). Other unlock kinds
  (emotes, tapes, inventory slots, customizer features) aren't extracted.
- A handful of entries have a literal "Discontinued - " display-name prefix:
  legacy nodes the game itself never shows.

## gamePhases.json

```json
"GP_Project_Assembly_Phase_2": {
  "phaseNumber": 2,
  "displayName": "Construction Dock",
  "cost": [{"item": "Desc_SpaceElevatorPart_1_C", "amount": 1000}, ...],
  "lastTierOfPhase": 6
}
```

The Space Elevator / Project Assembly phases. NOT produced by
`extract_docs_json.py` -- the `FGGamePhase` assets aren't reflected in
Docs.json at all. Produced by `game_data/extract_game_phases.py` from an
FModel-style JSON export of the game paks (the same extraction dump
`copy_icons.py` reads, `<Content>/FactoryGame/GamePhases/*.json`):

```
py game_data/extract_game_phases.py [path/to/extraction/.../Content]
```

- `cost` amounts are BASE values -- the game-mode
  `mSpacePartsCostMultiplier` (on the save's BP_GameState_C) scales them at
  load time in `sav_map_data._collectSpaceElevatorState`.
- `displayName` is hand-written in the extractor (wiki-sourced): the assets
  only carry a localization-table key, and the string tables (Game.locres)
  aren't part of the extraction. Phases 0/6/7 (onboarding + the two
  post-Assembly completion states, no costs) stay `null` -- consumers show
  "Phase N".
- `sav_map_data.py` keeps an equivalent hand-written fallback table
  (`_FALLBACK_GAME_PHASES`, phases 1-5 only) for when this file hasn't been
  generated; the generated file wins when present.
