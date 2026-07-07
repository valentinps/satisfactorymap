#!/usr/bin/env python3
# Bundles every game-derived artifact into a single zip archive -- and unpacks
# such an archive back into place -- so someone WITHOUT the game files (no
# Docs.json dump, no FModel extraction) can run the map viewer from a plain
# clone of this repo plus the one archive.
#
# What goes in (exactly the paths .gitignore keeps out of the repo, minus the
# raw docs.json dump, which the viewer never reads at runtime):
#   - game_data/generated/           (JSONs from extract_docs_json.py /
#                                     extract_game_phases.py, plus
#                                     map_highres.png from extract_map_image.py)
#   - map/static/map/icons/          (PNGs from copy_icons.py)
#
# Usage:
#   py game_data/package_game_data.py pack [zipPath]     (default: game_data.zip in the repo root)
#   py game_data/package_game_data.py unpack <zipPath>
#
# Archive members are stored relative to the repo root, so unpack is just a
# guarded extract into the repo root (guarded: only members under the two
# expected folders are accepted, so a hand-tampered zip can't write elsewhere).

import sys
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ZIP_PATH = REPO_ROOT / "game_data.zip"

# Everything under these repo-root-relative folders is packed/unpacked.
PACKAGED_DIRS = (
   "game_data/generated",
   "map/static/map/icons",
)


def collectFiles() -> list:
   files = []
   missingDirs = []
   for relDir in PACKAGED_DIRS:
      absDir = REPO_ROOT / relDir
      if not absDir.is_dir():
         missingDirs.append(relDir)
         continue
      files.extend(path for path in sorted(absDir.rglob("*")) if path.is_file())
   if missingDirs:
      sys.exit(f"Missing folder(s): {missingDirs} -- generate them first "
               "(see README.md's 'Generating game data' / 'Generating icons' / "
               "'Generating the map image' sections).")
   return files


def pack(zipPath: Path) -> None:
   files = collectFiles()
   with zipfile.ZipFile(zipPath, "w", compression=zipfile.ZIP_DEFLATED) as archive:
      for path in files:
         archive.write(path, path.relative_to(REPO_ROOT).as_posix())
   sizeMb = zipPath.stat().st_size / (1024 * 1024)
   print(f"Wrote {zipPath} ({len(files)} files, {sizeMb:.1f} MB).")


def unpack(zipPath: Path) -> None:
   if not zipPath.is_file():
      sys.exit(f"Archive not found: {zipPath}")
   extracted = 0
   skipped = []
   with zipfile.ZipFile(zipPath) as archive:
      for member in archive.infolist():
         if member.is_dir():
            continue
         # Only accept members inside the expected folders; this also rejects
         # absolute paths and ".." traversal since those can't start with a
         # PACKAGED_DIRS prefix.
         if not any(member.filename.startswith(relDir + "/") for relDir in PACKAGED_DIRS):
            skipped.append(member.filename)
            continue
         archive.extract(member, REPO_ROOT)
         extracted += 1
   if skipped:
      print(f"Skipped {len(skipped)} unexpected member(s), e.g. {skipped[:5]}")
   print(f"Extracted {extracted} files into {REPO_ROOT}.")


def main() -> None:
   command = sys.argv[1] if len(sys.argv) > 1 else ""
   if command == "pack" and len(sys.argv) <= 3:
      pack(Path(sys.argv[2]) if len(sys.argv) == 3 else DEFAULT_ZIP_PATH)
   elif command == "unpack" and len(sys.argv) == 3:
      unpack(Path(sys.argv[2]))
   else:
      sys.exit("Usage: py game_data/package_game_data.py pack [zipPath]\n"
               "       py game_data/package_game_data.py unpack <zipPath>")


if __name__ == "__main__":
   main()
