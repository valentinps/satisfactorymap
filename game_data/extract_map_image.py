#!/usr/bin/env python3
# Fuses the game's own 4-corner sliced map render into a single
# game_data/generated/map_highres.png. These tiles are the actual textures the game's in-game map
# UI uses -- pixel-sampling confirmed they already cover the exact same crop
# region as the old blank_map20.png-derived map_highres.png (mean abs RGB
# diff ~4.9 after a straight resize, no extra cropping/offset needed), just at
# native resolution instead of an upscale, so this is a drop-in replacement.
#
# Usage: py game_data/extract_map_image.py [path/to/extraction/Content]
#
# The path argument is the "Content" folder of the extraction, same as
# copy_icons.py -- the tiles sit at
# <Content>/FactoryGame/Interface/UI/Assets/MapTest/SlicedMap/Map_X-Y.png.

import sys
from pathlib import Path

from PIL import Image

DEFAULT_CONTENT_ROOT = Path(r"C:\Users\plane.DESKTOP-SAH3OHV\Documents\SatisExtract\FactoryGame\Content")
SLICED_MAP_SUBPATH = Path("FactoryGame/Interface/UI/Assets/MapTest/SlicedMap")
OUTPUT_FILE = Path(__file__).parent / "generated" / "map_highres.png"

# Map_X-Y.png tile grid position -> (column, row), confirmed by comparing
# adjacent tile edge pixels (matching edges have near-zero diff; mismatched
# edges differ by ~90-150 mean abs value).
TILE_GRID = {
   "0-0": (0, 0),
   "1-0": (1, 0),
   "0-1": (0, 1),
   "1-1": (1, 1),
}


def fuseMapImage(contentRoot: Path) -> None:
   slicedMapDir = contentRoot / SLICED_MAP_SUBPATH
   tiles = {}
   for name in TILE_GRID:
      tilePath = slicedMapDir / f"Map_{name}.png"
      if not tilePath.is_file():
         sys.exit(f"Missing tile: {tilePath}")
      tiles[name] = Image.open(tilePath).convert("RGB")

   tileSize = tiles["0-0"].size
   if any(tile.size != tileSize for tile in tiles.values()):
      sys.exit(f"Tile size mismatch: expected all tiles to be {tileSize}")
   tileW, tileH = tileSize

   fused = Image.new("RGB", (tileW * 2, tileH * 2))
   for (name, (col, row)) in TILE_GRID.items():
      fused.paste(tiles[name], (col * tileW, row * tileH))

   OUTPUT_FILE.parent.mkdir(parents=True, exist_ok=True)
   fused.save(OUTPUT_FILE)
   print(f"Wrote {OUTPUT_FILE} ({fused.width}x{fused.height}, up from the old 5000x5000).")
   print('Update MAP_SIZE in map/sav_map_data.py and mapSize in map/static/map/map.js to match if the resolution changed.')


def main() -> None:
   contentRoot = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CONTENT_ROOT
   if not contentRoot.is_dir():
      sys.exit(f"Content root not found: {contentRoot}")
   fuseMapImage(contentRoot)


if __name__ == "__main__":
   main()
