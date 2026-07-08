#!/usr/bin/env python3
# Copies just the item/building icon PNGs referenced by game_data/generated/items.json
# and buildings.json (their "icon" field -- see extract_docs_json.py/SCHEMA.md)
# out of a full game asset extraction, into the map's static icon folders, keyed
# by ClassName (e.g. "Desc_IronPlate_C.png", "Build_WorkBench_C.png"). This is
# the one-shot step that lets the huge extraction dump (tens of thousands of
# unrelated meshes/textures/blueprints) be deleted afterwards -- only the
# handful of PNGs actually used by the map need to survive.
#
# Usage: py game_data/copy_icons.py [path/to/extraction/.../Content]
#
# The path argument is the "Content" folder of the extraction -- the icon
# fields are asset paths rooted at "/FactoryGame/...", which resolve to
# <Content>/FactoryGame/... .png on disk.

import json
import shutil
import sys
from pathlib import Path

DEFAULT_CONTENT_ROOT = Path(r"C:\Users\plane.DESKTOP-SAH3OHV\Documents\SatisExtract\FactoryGame\Content")
GENERATED_DIR = Path(__file__).parent / "generated"
ICONS_DIR = Path(__file__).parent.parent / "map" / "static" / "map" / "icons"

# (generated JSON file, destination subfolder under map/static/map/icons/) --
# resources.json shares "items" with items.json (raw resources sit alongside
# regular items in the frontend's icons/items/ folder).
ICON_SOURCES = (
   ("items.json", "items"),
   ("resources.json", "items"),
   ("buildings.json", "buildings"),
)

# A handful of real per-concept icons live in Docs.json under a field
# extract_docs_json.py doesn't parse -- FGSchematic entries carry their icon
# in "mSchematicIcon", a much richer widget-brush struct (tint/size/margin/
# outline/...) rather than the plain "Texture2D /Game/..." string
# mPersistentBigIcon uses, and schematics are otherwise entirely out of scope
# (see extract_docs_json.py's header comment) -- not worth teaching the
# generic extractor a whole second icon-field shape for one struct. Added
# here by hand instead, one at a time, as they're identified: (source path
# rooted at "/FactoryGame/...", destination subfolder, destination filename).
EXTRA_ICON_COPIES = (
   # Hard Drives have no FGItemDescriptor/FGResourceDescriptor of their own
   # (picked up once as a one-off tech unlock, never held in inventory like a
   # normal item) -- this is ResourceSink_Purchasable_HardDrive_C's
   # mSchematicIcon, the real in-game hard drive crate art.
   ("/FactoryGame/Resource/Environment/CrashSites/UI/HardDrive_256", "items", "HardDrive.png"),
   # Geyser (the "Desc_Geyser_C" resourceType -- see sav_data.resourcePurity)
   # is a synthetic key this parser invented; it has no FGResourceDescriptor
   # (or any other Docs.json entry/field) at all. This is the real in-game
   # geyser icon, found by inspecting the extraction directly rather than
   # via any Docs.json field.
   ("/FactoryGame/World/Environment/HotSpring/UI/IconDesc_Geyser_256", "items", "Geyser.png"),
   # AWESOME Shop / HUB concept icons for the map's top-bar progression
   # buttons -- like Hard Drives above, neither is an item/building with a
   # Docs.json descriptor of its own; both paths were found by inspecting the
   # extraction directly.
   ("/FactoryGame/Interface/UI/Assets/Shared/TXUI_ShopUpgrade_256", "items", "AwesomeShop.png"),
   ("/FactoryGame/Buildable/Factory/TradingPost/UI/Hub_512", "items", "Hub.png"),
   # Vehicle map markers (see sav_map_data.VEHICLE_ICONS_BY_TYPE_PATH / the
   # frontend's Vehicles section). Vehicles DO have FGVehicleDescriptor icons
   # in Docs.json, but the monochrome white-on-transparent glyph set reads far
   # better inside a small colored map pin than the full-color renders --
   # these are the game's own UI glyphs for each vehicle type. The Cyber
   # Wagon's glyph isn't in the shared MonochromeIcons folder like the rest;
   # it sits in the vehicle's own UI folder.
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_Explorer", "vehicles", "Explorer.png"),
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_FactoryCart", "vehicles", "FactoryCart.png"),
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_Tractor", "vehicles", "Tractor.png"),
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_Truck", "vehicles", "Truck.png"),
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_Drone", "vehicles", "Drone.png"),
   ("/FactoryGame/Interface/UI/Assets/MonochromeIcons/TXUI_MIcon_Train", "vehicles", "Train.png"),
   ("/FactoryGame/Buildable/Vehicle/CyberWagon/UI/TXUI_MIcon_CyberTruck", "vehicles", "CyberWagon.png"),
)


def loadGeneratedJson(name: str) -> dict:
   with open(GENERATED_DIR / name, encoding="utf-8") as handle:
      return json.load(handle)


def copyIcons(contentRoot: Path) -> None:
   totalCopied = 0
   totalMissing = 0
   for (jsonName, subfolder) in ICON_SOURCES:
      entries = loadGeneratedJson(jsonName)
      destDir = ICONS_DIR / subfolder
      destDir.mkdir(parents=True, exist_ok=True)
      missing = []
      copied = 0
      for (className, entry) in entries.items():
         icon = entry.get("icon")
         if not icon:
            continue
         # icon is like "/FactoryGame/Resource/Parts/IronPlate/UI/IconDesc_IronPlates_256"
         # -- rooted at "/FactoryGame/...", so it hangs directly off contentRoot.
         srcPath = contentRoot / (icon.lstrip("/\\") + ".png")
         if not srcPath.is_file():
            missing.append(className)
            continue
         shutil.copyfile(srcPath, destDir / f"{className}.png")
         copied += 1
      print(f"{jsonName}: copied {copied}, missing {len(missing)}" + (f" (e.g. {missing[:5]})" if missing else ""))
      totalCopied += copied
      totalMissing += len(missing)

   for (icon, subfolder, destName) in EXTRA_ICON_COPIES:
      destDir = ICONS_DIR / subfolder
      destDir.mkdir(parents=True, exist_ok=True)
      srcPath = contentRoot / (icon.lstrip("/\\") + ".png")
      if not srcPath.is_file():
         print(f"extra: MISSING {srcPath}")
         totalMissing += 1
         continue
      shutil.copyfile(srcPath, destDir / destName)
      totalCopied += 1
   print(f"Total: {totalCopied} icon(s) copied, {totalMissing} missing.")


def main() -> None:
   contentRoot = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CONTENT_ROOT
   if not contentRoot.is_dir():
      sys.exit(f"Content root not found: {contentRoot}")
   copyIcons(contentRoot)


if __name__ == "__main__":
   main()
