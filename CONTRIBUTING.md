# Building & contributing

This page covers building the site and the desktop app from source, and
regenerating the game-derived data. For what the project *is*, see the
[README](README.md).

## Building the site

Requires a Rust toolchain ([rustup](https://rustup.rs/); on Windows also the
Visual Studio Build Tools "Desktop development with C++" workload),
[wasm-pack](https://rustwasm.github.io/wasm-pack/) (`cargo install
wasm-pack`), and Python 3.10+ for the build script.

The repo does not ship the game-derived data (item/building JSONs, icons, the
map image) — it's extracted from the game's own files and too
large/derivative to version. Get it one of two ways:

- **Quick setup — download the pre-extracted archive**:
  [game_data.zip from the latest game-data release](https://github.com/valentinps/satisfactorymap/releases/download/game-data-v2/game_data.zip)
  (the same archive the production build downloads — see `build.sh`),
  then unpack it —
  ```bash
  py game_data/package_game_data.py unpack path/to/game_data.zip
  ```
- **You have the game installed**: follow the "Generating ..." sections below.

Then:

```bash
py tools/build_site.py     # assembles the deployable static site into dist/
py tools/serve_site.py     # serves dist/ at http://127.0.0.1:8791/
```

`tools/serve_site.py` sends the same COOP/COEP headers as production
(`dist/_headers`); any static file host works for deployment.

The production site (satisfactorymap.net) is a Cloudflare Pages project
connected to this repo: every push to `main` runs the build and deploys
automatically — no manual deploy step. A one-off manual deploy (e.g. of a
locally built `dist/` from another branch) also works:

```bash
npx wrangler pages deploy dist/
```

Note the build needs the game-derived data (see above) — a fork deploying
its own instance has to provide it to whatever builds the site.

## Desktop app (Tauri)

The browser build runs the parser in a WebAssembly worker, hard-capped at 4 GB
of linear memory (wasm32). Very large saves (roughly 2–4× the current biggest,
~1.2M+ objects) exceed that and can't be loaded in a browser at all. The
**desktop app** wraps the *same* `sav_core` engine and the *same* frontend
(`dist/`) in a native [Tauri v2](https://v2.tauri.app/) window — no wasm, no
4 GB ceiling. The frontend picks its transport at runtime from
`window.__TAURI__`, so `dist/` is byte-identical to the browser build; the
desktop shell just talks to `sav_core` through native commands instead of the
worker, and loads the `.sav` from a path (native file dialog) rather than
marshaling it through the wasm boundary.

Prereqs: Rust toolchain, the [Tauri CLI](https://v2.tauri.app/reference/cli/)
(`cargo install tauri-cli --version '^2'`), and WebView2 (preinstalled on
Windows 11). Build `dist/` first (`python tools/build_site.py`) — the desktop
app bundles it.

```bash
python tools/build_site.py                       # produce dist/ (once, or after frontend/wasm changes)
cd rust_parser/tauri
cargo tauri dev                                  # dev loop: launches the window
cargo tauri build                                # Windows installer (MSI/NSIS), unsigned
```

`cargo tauri dev` embeds `../../dist` at compile time, so rebuild `dist/` (and
restart) after changing the frontend. The wasm worker path is untouched — the
same `dist/` still serves in the browser via `serve_site.py`.

## Project layout

| Path | Contents |
| --- | --- |
| `map/static/map/` | the web frontend (vanilla JS + Leaflet + WebGL layer, `worker.js`/`save_client.js` host the WASM parser) |
| `map/static/map/map.css` | design tokens (colour/type/spacing/radius scales), the app-shell grid, and per-feature styling |
| `map/static/map/ui.css` + `ui.js` | the shared UI primitives every feature builds from: buttons, fields, dialogs, list rows, bars, toggles, and the Escape-layer stack. New chrome should reuse these rather than restyle its own |
| `map/static/map/icons/` | *(generated)* item/building icon PNGs |
| `rust_parser/core/` | `sav_core`: the save parser + map-payload builder (pure Rust, embeds the game-data tables) |
| `rust_parser/wasm/` | `sav_wasm`: the wasm-bindgen boundary the worker loads |
| `rust_parser/tauri/` | `sav_tauri`: native desktop shell (Tauri v2) over `sav_core`, mirrors the wasm binding |
| `game_data/` | extraction scripts, `SCHEMA.md`, and the folder rules in [`game_data/README.md`](game_data/README.md) |
| `game_data/curated/` | *(committed)* the hand-maintained inputs: name corrections, type paths, category labels/overrides, pickup item classes |
| `game_data/docs.json` | *(not committed)* the game's own data dump, input to `extract_docs_json.py` |
| `game_data/generated/docs/` | *(generated)* item/building/recipe/schematic/category/phase JSONs |
| `game_data/generated/world/` | *(generated)* level-export tables: resource nodes, slugs, somersloops, mercer spheres, crash sites, dropped items, creature spawners, caves, world bounds |
| `game_data/generated/` | *(generated)* `map_highres.png` + its tile pyramid |
| `tools/` | `build_site.py` / `serve_site.py` / `benchmark.py` / `fetch_test_saves.py` / `e2e_editor.py` / `ui_shots.py` / `ui_behaviour.py` / `release.py` |
| `dist/` | *(generated)* the assembled static site |

Everything marked *(generated)* is git-ignored and produced by the steps
below — or restored from an archive via `package_game_data.py unpack`.

## Running the tests

The Rust suite reads real save files from `map/uploads/` (gitignored — saves
are tens of MB). Fetch the public corpus first, then test:

```bash
py tools/fetch_test_saves.py     # downloads the test-saves-v2 release assets
cd rust_parser
cargo test -p sav_core --release # release: debug parses of 50MB saves crawl
```

The same corpus feeds `tools/e2e_editor.py` (browser-driven editor
regression, needs `pip install playwright`) and the CI workflow in
`.github/workflows/ci.yml`, which runs the Rust suite and the wasm build on
every push to `main` and on pull requests.

The frontend has two browser-driven guards of its own — the chrome has no unit
tests, so these are what make a CSS or layout change verifiable:

```bash
py tools/ui_shots.py --serve --out ui_shots/before   # record, then make changes
py tools/ui_shots.py --serve --out ui_shots/after --baseline ui_shots/before
py tools/ui_behaviour.py --serve                     # dialogs, Escape layers, docks
py tools/ui_classes.py                               # static: nothing renders unstyled
```

`ui_shots.py` captures 17 UI states at three viewport widths and diffs them
pixel-wise against a previous run; `ui_behaviour.py` asserts the things a
screenshot cannot see (that dialogs are real modals, that the tooltip still
paints above one, that opening a category does not resize the map). Both run
against `dist/`, so build or copy the changed files there first.

`ui_classes.py` needs no browser at all: it flags any element the JS builds
whose classes are ALL unstyled, which is what a renamed CSS class with a missed
call site looks like. Worth running after any rename in `ui.css`/`map.css`.

Note: `sav_core` embeds `game_data/generated/{docs,world}/*.json` and the icon
manifest at compile time, and **nothing generated is committed**, so the Rust
crates do not build until you have either run `game_data/extract_all.py` or
unpacked `game_data.zip` (the build script says so if you forget). See
[`game_data/README.md`](game_data/README.md).

## Generating game data

Everything game-derived regenerates with one command, fed by two inputs:

```bash
py game_data/extract_all.py path\to\en-US.json path\to\extraction\Content
```

1. **`en-US.json`** — the game's own reflection dump, straight from the
   install at `Satisfactory\CommunityResources\Docs\en-US.json` (copied to
   `game_data/docs.json`; omit the argument to reuse the existing copy).
2. **The FModel extraction** (its `Content` folder). One shared path tree,
   but it mixes the game's TWO archives, and exports from BOTH are needed
   (in [FModel](https://fmodel.app/), pointed at the game's `Paks` folder):
   - **`.utoc`/`.ucas` (cooked assets)** — right-click
     `FactoryGame/Content/FactoryGame` → *Save Folder's Packages Properties
     (.json)* and *Save Folder's Packages Textures*. Provides the
     GamePhases, creature descriptors, the `Map/GameLevel01` world cells,
     the sliced map render and every icon PNG.
   - **`.pak` (loose files)** — right-click
     `FactoryGame/Content/Localization/StringTables` → *Export Folder's
     Packages Raw Data*. Provides the string-table source CSVs (the official
     display strings, e.g. creature names in `World_Data.csv`). A normal
     package export **skips** these — they aren't assets.

   The orchestrator preflights every required path and tells you exactly
   which FModel export is missing before running anything. Add `--pack` to
   also refresh `game_data.zip` at the end.

The individual steps live in `game_data/extractors/` and stay runnable on
their own (all take the extraction's `Content` folder as argument, except
the first which reads `game_data/docs.json`):

| Script | Output |
|---|---|
| `extract_docs_json.py` | `generated/docs/`: `items`/`resources`/`buildings`/`recipes`/`buildingCategories`/`schematics.json` — see `game_data/SCHEMA.md` |
| `extract_game_phases.py` | `gamePhases.json` (Space Elevator phase costs; optional, has a built-in fallback table) |
| `extract_spawners.py` | `generated/world/creatureSpawners.json` (every creature spawner with position + creature class, e.g. all Lizard Doggo spawns) and `creatures.json` (official display name + icon per creature class, from the StringTables CSVs) |
| `extract_collectables.py` | `generated/world/`: power slugs, somersloops, mercer spheres, crash sites (incl. their unlock cost/power requirements, derived from `mUnlockCost` + docs.json labels), free dropped items, resource purity, plus `consumables.json` (Paleberry/Beryl Nut/Bacon Agaric plants) — fully regenerated from the world cells (replacing the old GreyHak-derived tables, validated 1:1 against them; run after `extract_docs_json.py`). Pickup item classes are cooked into the actors but FModel can't decode that struct, so they come from the committed `curated/pickupItems.json`; `--items-from-save some.sav` learns new ones from a save and writes them back there (the save must have physically visited them: saves only serialize actors whose world cell has streamed in near a player) |
| `extract_world_bounds.py` | `generated/world/worldBounds.json`: the map's two invisible edges. The damaging perimeter comes from the `FGDamageOverTimeVolume` actors carrying a `/World/Hazard/WorldPerimeter/` DoT class — eleven walls (three of them rotated, cutting the NE/NW/SW corners) reduced to a 7-vertex safe-side polygon, plus the ceiling/floor slabs as altitudes. The water limit is the union of the 270 `FGWaterVolume` actors (all `mResourceClass` `Desc_Water_C`, i.e. swimmable and extractor-valid), which stops far inside the rendered ocean — 31 water-plane patches, the biggest 51 × 34 km |
| `extract_caves.py` | `generated/world/caves.json` — one outline polygon per cave system (~84), traced from the cooked level data: the game's own cave atmosphere volumes (the fog/lighting regions it swaps you into underground, ~108 of them, some carrying authored names like `Atmosphere_SwampCave`), the `BP_CaveFloor` tunnel splines, the cave-only foliage clusters and the placed cave rock kit, unioned and contoured. Nothing in a save records a cave, so this is the only source |
| `extract_map_image.py` | `map_highres.png`, fused from the game's 4-corner sliced map render (`FactoryGame/Interface/UI/Assets/MapTest/SlicedMap`) |
| `copy_icons.py` | the icon PNGs under `map/static/map/icons/`, copied last — it reads the generated JSONs above to know which few hundred of the dump's tens of thousands of files are needed |

Buildings missing size data in `buildings.json` (a small number of
logic-only buildables) fall back to a plain circle marker on the map. The
extraction dump can be deleted once the pipeline has run.

## Sharing the generated data

Once you've generated everything, bundle it for someone who doesn't have the
game files:

```bash
py game_data/package_game_data.py pack          # writes game_data.zip in the repo root
```

The archive contains `game_data/generated/` (the `docs/` and `world/` tables,
the map image and its tiles) and `map/static/map/icons/` — i.e. exactly what
`extract_all.py` produces and git ignores. The recipient clones the repo and runs:

```bash
py game_data/package_game_data.py unpack game_data.zip
```

## Benchmarking

`tools/benchmark.py` reproduces the numbers in
[docs/BENCHMARK.md](docs/BENCHMARK.md):

```bash
pip install playwright        # uses the installed system Chrome
python tools/benchmark.py path/to/save.sav
```
