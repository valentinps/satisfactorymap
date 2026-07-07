# Satisfactory Save Map

Interactive web-based map viewer for [Satisfactory](https://www.satisfactorygame.com/) save files.

## Setup

```bash
git clone --recurse-submodules https://github.com/valentinps/sat_sav_parse.git
cd sat_sav_parse
pip install -r requirements.txt
```

If you cloned without `--recurse-submodules`:
```bash
git submodule update --init
```

You also need `map_highres.png` in the repo root (not versioned due to file size) — see "Generating the map image" below.

## Usage

```bash
py map/sav_map_server.py
```

The server opens a landing page in your browser where you can choose how to load saves:

- **Upload save** — drag and drop or pick a `.sav` file
- **Local folder** — point at your Satisfactory save directory
- **SFTP server** — connect to a dedicated server over SFTP (requires `pip install paramiko`)

Each restart lands on the mode-selection page; it does not remember your last choice.

## Generating game data

Buildings render as boxes sized from `docs/generated/buildings.json` (see
`docs/generated/SCHEMA.md`), extracted from the game's own `Docs.json`.
Buildings missing size data there (a small number of logic-only buildables)
fall back to a plain circle marker on the map.

Neither `docs/Docs.json` nor the generated JSON files under `docs/generated/`
(`buildings.json`, `items.json`, `resources.json`, `recipes.json`,
`buildingCategories.json`) are committed — get `Docs.json` from your game
install at `Satisfactory\CommunityResources\Docs\en-US.json`, copy it to
`docs/Docs.json`, then regenerate the rest (also whenever the game updates):

```bash
py docs/extract_docs_json.py
```

## Generating the map image

`map_highres.png` is not committed either — it's fused from the game's own
4-corner sliced map render, the same way icons are extracted (via a full game
asset extraction, e.g. [FModel](https://fmodel.app/)):

```bash
py docs/extract_map_image.py path/to/extraction/Content
```

The path argument is the extraction's `Content` folder, same as
`copy_icons.py`. The tiles live at
`FactoryGame/Interface/UI/Assets/MapTest/SlicedMap/Map_X-Y.png` within it;
the script stitches the four of them into `map_highres.png` in the repo root.

## Generating icons

Item/building icons under `map/static/map/icons/` are not committed either —
they're copied out of a full game asset extraction (e.g. via
[FModel](https://fmodel.app/)) keyed by `ClassName`, using the generated JSON
above to know which PNGs are needed:

```bash
py docs/copy_icons.py path/to/extraction/Content
```

The path argument is the extraction's `Content` folder. This only copies the
handful of PNGs actually referenced by `items.json`/`resources.json`/
`buildings.json` (a few hundred files), not the full extraction dump (tens of
thousands of unrelated assets), so the extraction itself can be deleted
afterwards. Run `docs/extract_docs_json.py` first if `docs/generated/` is
missing/stale — `copy_icons.py` reads the icon paths from there.

## Parser dependency

Save file parsing is handled by [GreyHak/sat_sav_parse](https://github.com/GreyHak/sat_sav_parse), included as a git submodule at `parser/`.

`patches/sav_parse.py` overrides one file from the submodule with a fix not yet merged upstream (TextProperty parsing when `isTextCultureInvariant` is unset).

To update the parser submodule to a newer upstream commit:
```bash
git -C parser pull origin main
git add parser
git commit -m "parser: update submodule"
```
