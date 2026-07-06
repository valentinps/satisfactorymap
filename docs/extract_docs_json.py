#!/usr/bin/env python3
# This file is part of the Satisfactory Save Parser distribution
#                                  (https://github.com/GreyHak/sat_sav_parse).
# Copyright (c) 2024-2026 GreyHak (github.com/GreyHak).
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, version 3.
#
# This program is distributed in the hope that it will be useful, but
# WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
# General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program. If not, see <https://www.gnu.org/licenses/>.

# Extracts recipes/items/buildings/buildingCategories out of the game's
# Docs.json (the Satisfactory Editor SDK reflection dump) into small generated
# JSON files that are actually cheap to load at runtime/build time. Docs.json
# itself is a ~10MB UTF-16 dump of every reflected UClass in the game
# (buildable physics params, schematics, customization swatches, etc) -- most
# of it is irrelevant to us, so this is a one-shot build step, not something
# read directly by the server or the map frontend. Re-run whenever Docs.json
# is updated (new game patch).
#
# Usage: py docs/extract_docs_json.py [path/to/Docs.json]
#
# Category rule (kept simple on purpose so new NativeClass groups in future
# game updates fall into a sane bucket without needing this script to know
# their name up front):
#   - NativeClass group is FGSchematic / FGResourceDescriptor / FGCustomizationRecipe
#     -> skipped entirely (schematics/resources: out of scope for now; customization
#        recipes are skins/patterns/swatches, not craftable recipes, and mostly
#        have no real mDisplayName anyway).
#   - NativeClass group is FGRecipe -> recipes.json.
#   - Otherwise, per entry: ClassName starting with "Build_" -> buildings.json,
#     anything else -> items.json (this naturally covers FGItemDescriptor and
#     its subtypes, ammo, consumables, and also equipment/weapons/vehicles,
#     since those are all inventory-holdable descriptors prefixed Desc_/Equip_).
#   - Entries with no mDisplayName are dropped (noise -- e.g. FGBuildingDescriptor
#     is a separate, mostly-empty shell group distinct from the real buildable
#     classes that carry the actual display name/size/etc).
#
# Build menu category rule: the build-menu category/subcategory/sort-order for
# a buildable is NOT on the Build_*_C buildable class itself -- it's on its
# FGBuildingDescriptor companion class (Desc_*_C), which is otherwise dropped
# above (empty mDisplayName). The two are linked only by naming convention, not
# an explicit field, so buildingCategories.json is keyed by the resolved
# Build_*_C name: strip "Desc_", swap in "Build_", and fall back through a
# handful of known irregular-naming patterns (see resolveBuildClassName). A
# few descriptors (~4, out of 547) are themselves stale/legacy and never
# resolve to a real buildable -- those are dropped with a printed warning
# rather than guessed at.

import json
import re
import sys
from pathlib import Path

DOCS_JSON_PATH = Path(__file__).parent / "Docs.json"
OUTPUT_DIR = Path(__file__).parent / "generated"

SKIPPED_NATIVE_CLASS_SUFFIXES = ("FGSchematic'", "FGResourceDescriptor'", "FGCustomizationRecipe'")
RECIPE_NATIVE_CLASS_SUFFIX = "FGRecipe'"
BUILDING_DESCRIPTOR_NATIVE_CLASS_SUFFIX = "FGBuildingDescriptor'"

# Descriptor ClassName -> real buildable ClassName, for the handful of cases
# where the naming convention (Desc_X_C -> Build_X_C) doesn't hold: typos in
# the game's own class names, Mk2/Mk3 missing an underscore, a renamed asset,
# or an old descriptor that grew a "_2" suffix on the real building.
KNOWN_DESCRIPTOR_TO_BUILD_CORRECTIONS = {
   "Desc_WalkwayTurn_C": "Build_WalkwayTrun_C",  # yes, "Trun" -- typo in the game's own class name
   "Desc_xmassLights_C": "Build_XmassLightsLine_C",
   "Desc_PowerPoleWallMk2_C": "Build_PowerPoleWall_Mk2_C",
   "Desc_PowerPoleWallMk3_C": "Build_PowerPoleWall_Mk3_C",
   "Desc_PowerPoleWallDoubleMk2_C": "Build_PowerPoleWallDouble_Mk2_C",
   "Desc_PowerPoleWallDoubleMk3_C": "Build_PowerPoleWallDouble_Mk3_C",
   "Foundation_ConcretePolished_8x2_C": "Build_Foundation_ConcretePolished_8x2_2_C",
}

# Fixed-size fields: present when the buildable has one unchanging footprint.
DIMENSION_FIELDS = ("mWidth", "mDepth", "mHeight", "mAngularDepth")
# Variable-length fields: present on belts/pipes/wire/beams/rail/lifts, where
# the player picks the actual length at build time.
ADAPTIVE_LENGTH_FIELDS = (
   "mMeshLength", "mMeshHeight", "mDefaultLength", "mMaxLength", "mLength",
   "mCachedLength", "mMaxPowerTowerLength", "mLengthPerCost",
   "mFlowIndicatorMinimumPipeLength", "mOpposingConnectionClearance",
)

QUOTED_PATH_RE = re.compile(r'"([^"]+)"')
INGREDIENT_RE = re.compile(r'ItemClass="[^"]*\.([A-Za-z0-9_]+)\'"\s*,\s*Amount=([\d.]+)')
CLEARANCE_BOX_RE = re.compile(
   r"ClearanceBox=\(Min=\(X=([\-\d.]+),Y=([\-\d.]+),Z=([\-\d.]+)\),"
   r"Max=\(X=([\-\d.]+),Y=([\-\d.]+),Z=([\-\d.]+)\)"
)
BUILD_CATEGORY_PATH_RE = re.compile(r"BuildCategories/(Sub_\w+)/(SC_\w+)\.")
# e.g. "Desc_Wall_Concrete_8x1_Tris_C" -- the descriptor puts the size before
# the Tris/FlipTris tag, the real buildable puts it after (Build_Wall_Concrete_Tris_8x1_C).
TRIS_SIZE_SWAP_RE = re.compile(r"^Desc_(.+)_(\d+x\d+)_(Tris|FlipTris)_C$")


def loadDocsJson(path: Path) -> list:
   with open(path, encoding="utf-16") as handle:
      return json.load(handle)


def shortClassNamesFromPathList(raw: str) -> list:
   # Turns a UE array-of-quoted-asset-paths string, e.g.
   # ("/Game/.../Build_Foo.Build_Foo_C","/Script/FactoryGame.FGBar") into
   # ["Build_Foo_C", "FGBar"].
   return [path.rsplit(".", 1)[-1] for path in QUOTED_PATH_RE.findall(raw)]


def parseItemAmountList(raw: str) -> list:
   # Parses mIngredients/mProduct strings like
   # ((ItemClass="...Desc_IronIngot.Desc_IronIngot_C'",Amount=3)) into
   # [{"item": "Desc_IronIngot_C", "amount": 3.0}].
   return [
      {"item": itemClass, "amount": float(amount)}
      for itemClass, amount in INGREDIENT_RE.findall(raw)
   ]


def parseClearanceBoxes(raw: str) -> list:
   # Parses mClearanceData into a list of axis-aligned boxes as given by the
   # game (no attempt to remap axes to width/depth/height -- some buildables
   # carry a RelativeTransform rotation, which would make that remapping wrong).
   if not raw:
      return []
   rotated = "Rotation=" in raw
   return [
      {
         "min": {"x": float(minX), "y": float(minY), "z": float(minZ)},
         "max": {"x": float(maxX), "y": float(maxY), "z": float(maxZ)},
         "rotated": rotated,
      }
      for minX, minY, minZ, maxX, maxY, maxZ in CLEARANCE_BOX_RE.findall(raw)
   ]


def extractDimensions(entry: dict) -> dict:
   return {
      field[1:]: float(entry[field])
      for field in DIMENSION_FIELDS
      if entry.get(field)
   }


def extractAdaptiveLength(entry: dict) -> dict:
   return {
      field[1:]: float(entry[field])
      for field in ADAPTIVE_LENGTH_FIELDS
      if entry.get(field)
   }


def extractRecipe(entry: dict) -> dict:
   return {
      "displayName": entry.get("mDisplayName", ""),
      "ingredients": parseItemAmountList(entry.get("mIngredients", "")),
      "product": parseItemAmountList(entry.get("mProduct", "")),
      "producedIn": shortClassNamesFromPathList(entry.get("mProducedIn", "")),
      "durationSeconds": float(entry["mManufactoringDuration"]) if entry.get("mManufactoringDuration") else None,
   }


def extractItem(entry: dict) -> dict:
   return {
      "displayName": entry.get("mDisplayName", ""),
      "description": entry.get("mDescription", ""),
      "stackSize": entry.get("mStackSize", ""),
      "stackSizeCount": int(entry["mCachedStackSize"]) if entry.get("mCachedStackSize") else None,
   }


def extractBuilding(entry: dict) -> dict:
   return {
      "displayName": entry.get("mDisplayName", ""),
      "description": entry.get("mDescription", ""),
      "dimensions": extractDimensions(entry),
      "clearance": parseClearanceBoxes(entry.get("mClearanceData", "")),
      "adaptiveLength": extractAdaptiveLength(entry),
   }


def resolveBuildClassName(descriptorClassName: str, buildClassNames: set) -> str | None:
   guess = "Build_" + descriptorClassName[len("Desc_"):] if descriptorClassName.startswith("Desc_") else "Build_" + descriptorClassName
   if guess in buildClassNames:
      return guess
   lowerLookup = {name.lower(): name for name in buildClassNames}
   if guess.lower() in lowerLookup:
      return lowerLookup[guess.lower()]
   trisMatch = TRIS_SIZE_SWAP_RE.match(descriptorClassName)
   if trisMatch:
      base, size, trisTag = trisMatch.groups()
      reordered = f"Build_{base}_{trisTag}_{size}_C"
      if reordered in buildClassNames:
         return reordered
   corrected = KNOWN_DESCRIPTOR_TO_BUILD_CORRECTIONS.get(descriptorClassName)
   if corrected in buildClassNames:
      return corrected
   return None


def extractBuildCategory(entry: dict) -> dict | None:
   match = BUILD_CATEGORY_PATH_RE.search(entry.get("mSubCategories", ""))
   if not match:
      return None
   return {
      "topCategory": match.group(1),
      "subCategory": match.group(2),
      "menuPriority": float(entry["mMenuPriority"]) if entry.get("mMenuPriority") else None,
   }


def extractBuildingCategories(docsData: list, buildClassNames: set) -> dict:
   categories = {}
   unresolved = []
   for nativeClassGroup in docsData:
      if not nativeClassGroup.get("NativeClass", "").endswith(BUILDING_DESCRIPTOR_NATIVE_CLASS_SUFFIX):
         continue
      for entry in nativeClassGroup.get("Classes", []):
         descriptorClassName = entry.get("ClassName")
         category = extractBuildCategory(entry)
         if not descriptorClassName or not category:
            continue
         buildClassName = resolveBuildClassName(descriptorClassName, buildClassNames)
         if buildClassName is None:
            unresolved.append(descriptorClassName)
            continue
         categories[buildClassName] = category
   if unresolved:
      print(f"buildingCategories: {len(unresolved)} descriptor(s) did not resolve to a buildable, skipped: {unresolved}")
   return categories


def extractAll(docsData: list) -> tuple:
   recipes = {}
   items = {}
   buildings = {}
   for nativeClassGroup in docsData:
      nativeClassName = nativeClassGroup.get("NativeClass", "")
      if nativeClassName.endswith(SKIPPED_NATIVE_CLASS_SUFFIXES):
         continue
      if nativeClassName.endswith(RECIPE_NATIVE_CLASS_SUFFIX):
         for entry in nativeClassGroup.get("Classes", []):
            className = entry.get("ClassName")
            if className and entry.get("mDisplayName"):
               recipes[className] = extractRecipe(entry)
         continue
      for entry in nativeClassGroup.get("Classes", []):
         className = entry.get("ClassName")
         if not className or not entry.get("mDisplayName"):
            continue
         if className.startswith("Build_"):
            buildings[className] = extractBuilding(entry)
         else:
            items[className] = extractItem(entry)
   return (recipes, items, buildings)


def writeJson(path: Path, data: dict) -> None:
   path.parent.mkdir(parents=True, exist_ok=True)
   with open(path, "w", encoding="utf-8") as handle:
      json.dump(data, handle, indent=2, ensure_ascii=False, sort_keys=True)
      handle.write("\n")


def main() -> None:
   docsPath = Path(sys.argv[1]) if len(sys.argv) > 1 else DOCS_JSON_PATH
   docsData = loadDocsJson(docsPath)
   recipes, items, buildings = extractAll(docsData)
   buildingCategories = extractBuildingCategories(docsData, set(buildings.keys()))
   writeJson(OUTPUT_DIR / "recipes.json", recipes)
   writeJson(OUTPUT_DIR / "items.json", items)
   writeJson(OUTPUT_DIR / "buildings.json", buildings)
   writeJson(OUTPUT_DIR / "buildingCategories.json", buildingCategories)
   print(f"recipes: {len(recipes)}, items: {len(items)}, buildings: {len(buildings)}, buildingCategories: {len(buildingCategories)}")


if __name__ == "__main__":
   main()
