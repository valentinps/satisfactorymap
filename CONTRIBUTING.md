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
  [game_data.zip on Google Drive](https://drive.google.com/file/d/16JshnM65xrTpwxwbYs2iHmoog2AKDGZN/view?usp=sharing),
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
| `map/static/map/icons/` | *(generated)* item/building icon PNGs |
| `rust_parser/core/` | `sav_core`: the save parser + map-payload builder (pure Rust, embeds the game-data tables) |
| `rust_parser/wasm/` | `sav_wasm`: the wasm-bindgen boundary the worker loads |
| `rust_parser/tauri/` | `sav_tauri`: native desktop shell (Tauri v2) over `sav_core`, mirrors the wasm binding |
| `game_data/` | extraction scripts + hand-curated game metadata (`categoryLabels.json`, `categoryOverrides.json`, `SCHEMA.md`) |
| `game_data/sav_data/` | *(committed)* static world tables (resource nodes, slugs, crash sites...) converted from the upstream parser project |
| `game_data/docs.json` | *(not committed)* the game's own data dump, input to `extract_docs_json.py` |
| `game_data/generated/` | *(generated)* item/building/recipe/schematic JSONs + `map_highres.png` |
| `tools/` | `build_site.py` / `serve_site.py` / `benchmark.py` / `fetch_test_saves.py` / `e2e_editor.py` / `release.py` |
| `dist/` | *(generated)* the assembled static site |

Everything marked *(generated)* is git-ignored and produced by the steps
below — or restored from an archive via `package_game_data.py unpack`.

## Running the tests

The Rust suite reads real save files from `map/uploads/` (gitignored — saves
are tens of MB). Fetch the public corpus first, then test:

```bash
py tools/fetch_test_saves.py     # downloads the test-saves-v1 release assets
cd rust_parser
cargo test -p sav_core --release # release: debug parses of 50MB saves crawl
```

The same corpus feeds `tools/e2e_editor.py` (browser-driven editor
regression, needs `pip install playwright`) and the CI workflow in
`.github/workflows/ci.yml`, which runs the Rust suite and the wasm build on
every push to `main` and on pull requests.

Note: `sav_core` embeds `game_data/generated/*.json` and the icon manifest at
compile time, so building the Rust crates also requires the game data to be
extracted first.

## Generating game data

Buildings render as boxes sized from `game_data/generated/buildings.json` (see
`game_data/SCHEMA.md`), extracted from the game's own data dump.
Buildings missing size data there (a small number of logic-only buildables)
fall back to a plain circle marker on the map.

Get the dump from your game install at
`Satisfactory\CommunityResources\Docs\en-US.json`, copy it to
`game_data/docs.json`, then regenerate the JSONs under `game_data/generated/`
(also whenever the game updates):

```bash
py game_data/extract_docs_json.py
```

`gamePhases.json` (Space Elevator phase costs) is not part of that dump — it
comes from the same FModel extraction as the icons below (optional; there's a
built-in fallback table):

```bash
py game_data/extract_game_phases.py path/to/extraction/Content
```

## Generating the map image

`game_data/generated/map_highres.png` is fused from the game's own 4-corner
sliced map render, taken from a full game asset extraction (e.g. via
[FModel](https://fmodel.app/)):

```bash
py game_data/extract_map_image.py path/to/extraction/Content
```

The path argument is the extraction's `Content` folder, same as
`copy_icons.py`. The tiles live at
`FactoryGame/Interface/UI/Assets/MapTest/SlicedMap/Map_X-Y.png` within it;
the script stitches the four of them together.

## Generating icons

Item/building icons under `map/static/map/icons/` are copied out of a full
game asset extraction (e.g. via [FModel](https://fmodel.app/)) keyed by
`ClassName`, using the generated JSON above to know which PNGs are needed:

```bash
py game_data/copy_icons.py path/to/extraction/Content
```

The path argument is the extraction's `Content` folder. This only copies the
handful of PNGs actually referenced by `items.json`/`resources.json`/
`buildings.json` (a few hundred files), not the full extraction dump (tens of
thousands of unrelated assets), so the extraction itself can be deleted
afterwards. Run `game_data/extract_docs_json.py` first if
`game_data/generated/` is missing/stale — `copy_icons.py` reads the icon
paths from there.

## Sharing the generated data

Once you've generated everything, bundle it for someone who doesn't have the
game files:

```bash
py game_data/package_game_data.py pack          # writes game_data.zip in the repo root
```

The archive contains `game_data/generated/` (JSONs + map image) and
`map/static/map/icons/`. The recipient clones the repo and runs:

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
