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

You also need `map_highres.png` in the repo root (not versioned due to file size).

## Usage

```bash
py map/sav_map_server.py
```

The server opens a landing page in your browser where you can choose how to load saves:

- **Upload save** — drag and drop or pick a `.sav` file
- **Local folder** — point at your Satisfactory save directory
- **SFTP server** — connect to a dedicated server over SFTP (requires `pip install paramiko`)

Each restart lands on the mode-selection page; it does not remember your last choice.

## Building footprints

Buildings render as boxes sized from `docs/generated/buildings.json` (see
`docs/generated/SCHEMA.md`), which is extracted from the game's own
`Docs.json` and bundled in this repo — no extra download needed. Buildings
missing size data there (a small number of logic-only buildables) fall back
to a plain circle marker on the map.

## Parser dependency

Save file parsing is handled by [GreyHak/sat_sav_parse](https://github.com/GreyHak/sat_sav_parse), included as a git submodule at `parser/`.

`patches/sav_parse.py` overrides one file from the submodule with a fix not yet merged upstream (TextProperty parsing when `isTextCultureInvariant` is unset).

To update the parser submodule to a newer upstream commit:
```bash
git -C parser pull origin main
git add parser
git commit -m "parser: update submodule"
```
