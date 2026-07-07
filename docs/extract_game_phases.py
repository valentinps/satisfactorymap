#!/usr/bin/env python3
# Generates docs/generated/gamePhases.json from the game's own
# GP_Project_Assembly_Phase_* assets (FGGamePhase) -- the Space Elevator
# phase list with each phase's part costs. These assets are NOT in Docs.json
# (which is why sav_map_data.py carries a hand-written wiki-sourced fallback
# table); they come from an FModel-style JSON export of the game's pak files,
# the same extraction dump copy_icons.py reads PNGs from:
#   <Content>/FactoryGame/GamePhases/GP_Project_Assembly_Phase_<N>.json
#
# Usage: py docs/extract_game_phases.py [path/to/extraction/.../Content]
#
# Re-run whenever the extraction dump is refreshed (new game patch).
# sav_map_data._loadGamePhases() picks the generated file over its fallback
# automatically.

import json
import re
import sys
from pathlib import Path

DEFAULT_CONTENT_ROOT = Path(r"C:\Users\plane.DESKTOP-SAH3OHV\Documents\SatisExtract\FactoryGame\Content")
GAME_PHASES_SUBDIR = Path("FactoryGame/GamePhases")
OUTPUT_PATH = Path(__file__).parent / "generated" / "gamePhases.json"

PHASE_FILENAME_RE = re.compile(r"^GP_Project_Assembly_Phase_(\d+)$")
# e.g. "BlueprintGeneratedClass'Desc_SpaceElevatorPart_1_C'"
OBJECT_NAME_CLASS_RE = re.compile(r"'([A-Za-z0-9_]+)'")

# The assets carry mDisplayName as a localization-table reference (TableId
# "Schematics_Data", Key "ProjectAssembly/PhaseN"). When the dump was
# exported with FModel's "Local Resources" (game localization) setting
# enabled, the reference's LocalizedString/SourceString is filled in and used
# directly; otherwise both are null (the strings live in Game.locres, inside
# the paks) and this hand-written fallback map applies, English names
# sourced from the wiki's Project Assembly page. Keys not listed here
# (Phase0 onboarding and the two post-Assembly completion states, none of
# which have costs or a real Space Elevator UI presence) stay unnamed --
# consumers fall back to "Phase N".
PHASE_DISPLAY_NAMES_BY_LOCALIZATION_KEY = {
   "ProjectAssembly/Phase1": "Distribution Platform",
   "ProjectAssembly/Phase2": "Construction Dock",
   "ProjectAssembly/Phase3": "Main Body",
   "ProjectAssembly/Phase4": "Propulsion",
   "ProjectAssembly/Phase5": "Assembly",
}


def shortClassNameFromObjectName(objectName: str) -> str | None:
   match = OBJECT_NAME_CLASS_RE.search(objectName or "")
   return match.group(1) if match else None


def extractPhase(assetPath: Path, phaseNumber: int) -> dict:
   with open(assetPath, encoding="utf-8") as handle:
      exportedObjects = json.load(handle)
   # An FModel export is a list of exported objects; the FGGamePhase asset has
   # exactly one, but scan by Type rather than assuming index 0.
   properties = {}
   for exportedObject in exportedObjects:
      if exportedObject.get("Type") == "FGGamePhase":
         properties = exportedObject.get("Properties", {})
         break
   cost = []
   for costEntry in properties.get("mCosts", []):
      item = shortClassNameFromObjectName((costEntry.get("ItemClass") or {}).get("ObjectName"))
      amount = costEntry.get("Amount")
      if item and amount is not None:
         cost.append({"item": item, "amount": amount})
   displayNameReference = properties.get("mDisplayName") or {}
   displayName = (displayNameReference.get("LocalizedString")
                  or displayNameReference.get("SourceString")
                  or PHASE_DISPLAY_NAMES_BY_LOCALIZATION_KEY.get(displayNameReference.get("Key")))
   return {
      "phaseNumber": phaseNumber,
      "displayName": displayName,
      "cost": cost,
      # Highest HUB tier available while this phase is active -- not consumed
      # by the map yet, kept because it's the only other gameplay-relevant
      # field these assets carry.
      "lastTierOfPhase": properties.get("mLastTierOfPhase"),
   }


def main() -> None:
   contentRoot = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CONTENT_ROOT
   phasesDir = contentRoot / GAME_PHASES_SUBDIR
   if not phasesDir.is_dir():
      sys.exit(f"Game phases folder not found: {phasesDir}")
   gamePhases = {}
   for assetPath in sorted(phasesDir.glob("GP_Project_Assembly_Phase_*.json")):
      match = PHASE_FILENAME_RE.match(assetPath.stem)
      if not match:
         continue
      gamePhases[assetPath.stem] = extractPhase(assetPath, int(match.group(1)))
   OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
   with open(OUTPUT_PATH, "w", encoding="utf-8") as handle:
      json.dump(gamePhases, handle, indent=2, ensure_ascii=False, sort_keys=True)
      handle.write("\n")
   print(f"gamePhases.json: {len(gamePhases)} phases "
         f"({sum(1 for phase in gamePhases.values() if phase['cost'])} with costs)")


if __name__ == "__main__":
   main()
