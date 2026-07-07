# Satisfactory Save Map

Interactive web-based map viewer for [Satisfactory](https://www.satisfactorygame.com/) save files.

## Setup

```bash
git clone --recurse-submodules https://github.com/valentinps/satisfactorymap.git
cd sat_sav_parse
pip install -r requirements.txt
```

If you cloned without `--recurse-submodules`:
```bash
git submodule update --init
```

The repo does not ship the game-derived data (item/building JSONs, icons, the
map image) — it's extracted from the game's own files and too large/derivative
to version. Get it one of two ways:

- **You have a `game_data.zip` archive** (from someone who already extracted
  everything): unpack it and you're done —
  ```bash
  py game_data/package_game_data.py unpack path/to/game_data.zip
  ```
- **You have the game installed**: follow the three "Generating ..." sections
  below.

## Usage

```bash
py map/sav_map_server.py
```

The server opens a landing page in your browser where you can choose how to load saves:

- **Upload save** — drag and drop or pick a `.sav` file
- **Local folder** — point at your Satisfactory save directory
- **SFTP server** — connect to a dedicated server over SFTP (requires `pip install paramiko`)

Each restart lands on the mode-selection page; it does not remember your last choice.

## Project layout

| Path | Contents |
| --- | --- |
| `map/` | Flask server + web frontend |
| `map/static/map/icons/` | *(generated)* item/building icon PNGs |
| `game_data/` | extraction scripts + hand-curated game metadata (`categoryLabels.json`, `categoryOverrides.json`, `SCHEMA.md`) |
| `game_data/docs.json` | *(not committed)* the game's own data dump, input to `extract_docs_json.py` |
| `game_data/generated/` | *(generated)* item/building/recipe/schematic JSONs + `map_highres.png` |
| `parser/` | upstream save parser (git submodule) |
| `patches/` | local overrides of parser files (fixes not yet merged upstream) |

Everything marked *(generated)* is git-ignored and produced by the steps
below — or restored from an archive via `package_game_data.py unpack`.

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

## Parser dependency

Save file parsing is handled by [GreyHak/sat_sav_parse](https://github.com/GreyHak/sat_sav_parse), included as a git submodule at `parser/`.

`patches/sav_parse.py` overrides one file from the submodule with a fix not yet merged upstream (TextProperty parsing when `isTextCultureInvariant` is unset).

To update the parser submodule to a newer upstream commit:
```bash
git -C parser pull origin main
git add parser
git commit -m "parser: update submodule"
```
