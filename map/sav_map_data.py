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
# along with this program. If not, see <http://www.gnu.org/licenses/>.

# Builds a compact, pre-bucketed JSON payload describing a parsed save for the
# interactive map web tool (sav_map_server.py).  All parsing logic is reused
# from sav_parse.py/sav_to_html.py/sav_data; this module only buckets and
# projects the already-parsed data into a shape cheap for the frontend to
# render and filter.

import json
import math
import os
import re
import sys

_MAP_DIR = os.path.dirname(os.path.abspath(__file__))
_REPO_ROOT = os.path.dirname(_MAP_DIR)
_PARSER_DIR = os.path.join(_REPO_ROOT, "parser")
sys.path.insert(0, _PARSER_DIR)                          # sav_parse, sav_to_html, sav_data
sys.path.insert(0, os.path.join(_REPO_ROOT, "patches"))  # local fixes not yet merged upstream

import sav_parse
import sav_to_html
import sav_data.data
import sav_data.resourcePurity
import sav_data.slug
import sav_data.somersloop
import sav_data.mercerSphere
import sav_data.crashSites

# Pipeline segment types that carry a "mSplineData" property (see Build_Pipeline_C /
# Build_PipelineMK2_C below). Junctions/pumps/supports are plain point buildings.
PIPELINE_SEGMENTS = (
   "/Game/FactoryGame/Buildable/Factory/Pipeline/Build_Pipeline.Build_Pipeline_C",
   "/Game/FactoryGame/Buildable/Factory/Pipeline/Build_Pipeline_NoIndicator.Build_Pipeline_NoIndicator_C",
   "/Game/FactoryGame/Buildable/Factory/PipelineMk2/Build_PipelineMK2.Build_PipelineMK2_C",
   "/Game/FactoryGame/Buildable/Factory/PipelineMk2/Build_PipelineMK2_NoIndicator.Build_PipelineMK2_NoIndicator_C",
)

# Railroad track segments also carry "mSplineData" (same mechanism as belts/pipelines).
RAILROAD_SEGMENTS = (
   "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrack.Build_RailroadTrack_C",
   "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrackIntegrated.Build_RailroadTrackIntegrated_C",
)

# Hypertube segments also carry "mSplineData". Build_PipeHyperSupport_C is a
# plain structural post (no path of its own) and stays a point building.
HYPERTUBE_SEGMENTS = (
   "/Game/FactoryGame/Buildable/Factory/PipeHyper/Build_PipeHyper.Build_PipeHyper_C",
   "/Game/FactoryGame/Buildable/Factory/PipeHyperStart/Build_PipeHyperStart.Build_PipeHyperStart_C",
)

CATEGORY_RULES = (
   # (category, tuple-of-substrings-any-of-which-match)
   # Train/truck/drone infrastructure (stations, platforms, signals) lives
   # here alongside the actual vehicles, so everything train/truck/drone
   # related ends up consolidated under Vehicles instead of split with
   # Logistics -- see VEHICLE_SUBCATEGORY_RULES below.
   ("Vehicles",     ("/Buildable/Vehicle/", "Train", "Railroad", "Drone", "Truck", "TruckStation")),
   ("Power",        ("PowerLine", "Generator", "PowerPole", "PowerTower", "PowerStorage", "PowerSwitch", "XmassLightsLine")),
   ("Logistics",    ("Conveyor", "Pipeline", "PipelineJunction", "PipelinePump", "PipelineSupport",
                      "Valve", "PipeHyper", "Splitter", "Merger", "StorageTeleporter")),
   ("Extraction",   ("MinerMk", "OilPump", "WaterPump", "FrackingSmasher", "FrackingExtractor")),
   ("Production",   ("Smelter", "Constructor", "Assembler", "Manufacturer", "Foundry", "Refinery",
                      "Blender", "HadronCollider", "QuantumEncoder", "Converter", "Packager",
                      "GeneratorGeoThermal")),
   ("Storage",      ("Storage", "Container", "FluidBuffer")),
   ("Construction",  ("Foundation", "Wall", "Stair", "Ramp", "Beam", "Pillar", "Walkway", "Roof",
                      "Catwalk", "Door", "Window", "Frame", "Railing")),
)

# Sub-grouping within the Logistics category only. "Items" is the fallback
# for anything not matched here (belts, splitters/mergers, storage teleporter).
LOGISTICS_SUBCATEGORY_RULES = (
   ("Fluids", ("Pipeline", "Valve")),
   ("Hypertube", ("PipeHyper",)),
)
DEFAULT_LOGISTICS_SUBCATEGORY = "Items"

# Sub-grouping within the Vehicles category only. "Trucks" is the fallback
# for every other vehicle (Explorer, Tractor, Cyber Wagon, Golf Cart, the
# Truck itself, etc.) -- per the user, anything that isn't a train or a drone
# counts as a truck for this purpose.
VEHICLE_SUBCATEGORY_RULES = (
   ("Trains", ("Train", "Railroad")),
   ("Drones", ("Drone",)),
)
DEFAULT_VEHICLE_SUBCATEGORY = "Trucks"

# Sub-grouping within the Construction category only -- this is the category
# that exploded in size once lightweight buildables (foundations/walls/ramps/
# beams, see _findLightweightBuildableGroups) started being surfaced as real
# buildings, since every material skin of every shape is its own typePath.
# "Ramp" is checked before "Railings & Fences" so ramp-shaped fence/catwalk
# pieces (FenceRamp/CatwalkRamp) land with the other ramps, not the railings.
CONSTRUCTION_SUBCATEGORY_RULES = (
   ("Foundations", ("Foundation",)),
   ("Ramps", ("Ramp",)),
   ("Walls", ("Wall",)),
   ("Beams & Pillars", ("Beam", "Pillar")),
   ("Catwalks", ("Catwalk",)),
   ("Railings & Fences", ("Railing", "Fence")),
   ("Stairs", ("Stair",)),
   # Checked before "Doors & Windows": some roof tiles have a skylight cutout
   # and are literally named "Build_Roof_Window_01_C" -- still a roof piece,
   # not a door/window building element.
   ("Roofs", ("Roof", "Walkway")),
   ("Doors & Windows", ("Door", "Window")),
)
DEFAULT_CONSTRUCTION_SUBCATEGORY = "Other"

def categorizeSubcategory(category: str, typePath: str) -> str:
   if category == "Logistics":
      rules, default = (LOGISTICS_SUBCATEGORY_RULES, DEFAULT_LOGISTICS_SUBCATEGORY)
   elif category == "Vehicles":
      rules, default = (VEHICLE_SUBCATEGORY_RULES, DEFAULT_VEHICLE_SUBCATEGORY)
   elif category == "Construction":
      rules, default = (CONSTRUCTION_SUBCATEGORY_RULES, DEFAULT_CONSTRUCTION_SUBCATEGORY)
   else:
      return None
   for (subcategory, substrings) in rules:
      for substring in substrings:
         if substring in typePath:
            return subcategory
   return default


def readableLabel(pathName: str) -> str:
   # pathNameToReadableName() appends " (full/original/path)" whenever the
   # short name isn't in sav_data.readableNames' curated dict (common for
   # Recipe_* paths). That's noisy in a tooltip, so trim it for display.
   if not pathName:
      return pathName
   label = sav_parse.pathNameToReadableName(pathName)
   parenIndex = label.find(" (")
   if parenIndex != -1 and label.endswith(")"):
      return label[:parenIndex]
   return label

def categorizeTypePath(typePath: str) -> str:
   for (category, substrings) in CATEGORY_RULES:
      for substring in substrings:
         if substring in typePath:
            return category
   return "Other"

MAP_SIZE = 5000 # map_highres.png dimensions; must match buildMapPayload()'s "mapSize".

# sav_to_html.adjPos()'s world-to-pixel calibration (the 22.887 scale and
# (18282.5, 20480) offsets) is reused as-is to get a blank_map20.png-space
# pixel. blank_map20.png itself has a wide border outside the playable area
# (sav_to_html.CROP_SETTINGS crops it away when producing the final annotated
# maps -- it's real padding the calibration accounts for, not decoration).
# map_highres.png turned out to be cropped to exactly that CROP_SETTINGS
# region and then scaled up (confirmed by pixel-sampling: comparing the two
# images under this crop+scale dropped the median color error from 91 to 9,
# vs. assuming map_highres.png was just an uncropped resize, which doesn't
# account for the border and shifts every point).
_WORLD_TO_PIXEL_SCALE = 22.887
_WORLD_OFFSET = (18282.5, 20480)
_OLD_MAP_DESCALE = sav_to_html.MAP_DESCALE
_CROP_LO = 4096 / _OLD_MAP_DESCALE   # 204.8, matches sav_to_html.CROP_SETTINGS
_CROP_HI = 36864 / _OLD_MAP_DESCALE  # 1843.2
_CROP_SPAN = _CROP_HI - _CROP_LO     # 1638.4
_SCALE_TO_HIGHRES = MAP_SIZE / _CROP_SPAN

def _adjPosBlankMap20(pos, yFlag):
   return (pos / _WORLD_TO_PIXEL_SCALE + _WORLD_OFFSET[yFlag]) / _OLD_MAP_DESCALE

def _adjPos(pos, yFlag):
   return (_adjPosBlankMap20(pos, yFlag) - _CROP_LO) * _SCALE_TO_HIGHRES

def projectXY(position) -> list:
   # adjPos's Y output is a standard top-down image pixel row (it's fed
   # directly into PIL drawing in sav_to_html.py). Leaflet's CRS.Simple
   # treats increasing "lat" as moving up the screen, so the row needs to be
   # flipped here or every point ends up mirrored vertically relative to the
   # map_highres.png background.
   px = _adjPos(position[0], False)
   py = _adjPos(position[1], True)
   return [px, MAP_SIZE - py]

WORLD_UNITS_PER_METER = 100.0 # Unreal Engine's default unit is centimeters.

def worldZToMeters(z: float) -> float:
   return z / WORLD_UNITS_PER_METER

# A world-space LENGTH (not a position) converted to map-pixel-space: same
# chain as _adjPos but without the additive offset, since lengths don't get
# translated, only scaled.
_PIXELS_PER_WORLD_UNIT = (1 / _WORLD_TO_PIXEL_SCALE / _OLD_MAP_DESCALE) * _SCALE_TO_HIGHRES

def metersToPixelLength(meters: float) -> float:
   return meters * WORLD_UNITS_PER_METER * _PIXELS_PER_WORLD_UNIT

def projectVectorXY(worldVector) -> list:
   # Same scale chain as projectXY, but for a direction/delta rather than a
   # position: no additive offset (vectors don't get translated), but the
   # same Y-axis flip projectXY applies to positions also has to apply to a
   # vector's Y component, since it's a linear (sign-flipping) operation.
   return [worldVector[0] * _PIXELS_PER_WORLD_UNIT, -worldVector[1] * _PIXELS_PER_WORLD_UNIT]

# Real building footprints aren't stored anywhere in the save (it's static
# game/mesh data). sav_data/detailedModels.json is satisfactory-calculator.com's
# own per-building-type collision polygon data (covers 131 building types,
# scale/offset-corrected -- see _loadScimFootprintsByTypePath below), which is
# both more precise and far more complete than guessing dimensions from wiki
# text. A few building types it doesn't cover fall back to hand-curated
# estimates (also wiki-sourced, cross-checked against the SCIM data's measured
# values for buildings present in both, e.g. Assembler/Refinery/Smelter/etc.
# matched almost exactly).
FALLBACK_FOOTPRINTS_METERS = (
   ("ConveyorLift", (1.0, 1.0)), # See CONVEYOR_LIFT_TYPE_PATHS -- not a real collision box, just a visible/clickable marker.
   ("Converter", (16.0, 16.0)),
   ("MinerMk2", (6.0, 14.0)),
   ("MinerMk3", (6.0, 14.0)),
   ("StorageContainerMk2", (5.0, 10.0)), # Industrial Storage Container
   # Biomass Burner (with input, Build_GeneratorBiomass_Automated_C) isn't in
   # the SCIM dataset, but it's the exact same physical building as the
   # input-less Biomass Burner (Build_GeneratorBiomass_C, which IS covered --
   # just with a conveyor input attached, no different footprint) -- reusing
   # that measured size here instead of falling back to a plain point.
   # Deliberately does NOT match Build_GeneratorIntegratedBiomass_C (the one
   # built into the HUB, which isn't a real placed building and should stay
   # a point).
   ("GeneratorBiomass_Automated", (7.84, 7.546)),
   ("TrainDockingStation", (16.0, 34.0)), # Freight Platform (solid and liquid)
   ("TrainPlatformEmpty", (16.0, 34.0)),
   # Foundations/ramps come in many material skins (Asphalt/Concrete/
   # ConcretePolished/Metal/plain) and thicknesses (8x1/8x2/8x4), but the
   # SCIM dataset only happens to cover one variant of each shape -- every
   # other material/thickness measured identically to the covered one
   # (confirmed by cross-checking multiple covered variants), so they all
   # share the same fallback rather than rendering as plain points.
   # "Foundation_" (not "Foundation") deliberately excludes
   # "FoundationPassthrough_*" (the small lift/pipe/hypertube floor-hole
   # pieces, a genuinely different/smaller shape) and "Foundation/Build_
   # Pillar*" (folder path only, not a "Foundation_" substring). Likewise
   # "_Ramp_" (underscores on both sides) matches "Build_Ramp_*" without
   # matching "FenceRamp_*"/"CatwalkRamp_*"/"RailingRamp_*" (compound names
   # with no underscore before "Ramp" -- thin railings, a different shape).
   ("QuarterPipe", (4.0, 4.0)), # Checked first: also matches "Foundation_"'s folder path otherwise.
   ("Foundation_", (8.0, 8.0)),
   ("_Ramp_", (8.0, 8.0)),
   ("RampDouble", (8.0, 8.0)),
   ("RampInverted", (8.0, 8.0)),
   ("InvertedRamp", (8.0, 8.0)),
   # Smart/Programmable Splitter and Priority Merger (plus all the Lift-mounted
   # variants of every splitter/merger) aren't in the SCIM dataset, but share
   # the same physical attachment footprint in-game as the plain
   # Splitter/Merger, which is -- measured from that same dataset.
   ("CA_Splitter", (4.3, 4.6)),
   ("CA_Merger", (4.3, 4.0)),
)

def _loadScimFootprintsByTypePath() -> dict:
   jsonPath = os.path.join(_PARSER_DIR, "sav_data", "detailedModels.json")
   try:
      with open(jsonPath, "r", encoding="utf-8") as fin:
         rawData = json.load(fin)
   except (OSError, ValueError):
      return {}

   footprintsByTypePath = {}
   for (typePath, entry) in rawData.items():
      scale = entry.get("scale", 1)
      xOffset = entry.get("xOffset", 0)
      yOffset = entry.get("yOffset", 0)
      xs = []
      ys = []
      for form in entry.get("forms", []):
         for (px, py) in form.get("points", []):
            xs.append((px + xOffset) * scale)
            ys.append((py + yOffset) * scale)
      if xs:
         footprintsByTypePath[typePath] = ((max(xs) - min(xs)) / WORLD_UNITS_PER_METER, (max(ys) - min(ys)) / WORLD_UNITS_PER_METER)
   return footprintsByTypePath

SCIM_FOOTPRINTS_METERS_BY_TYPEPATH = _loadScimFootprintsByTypePath()

def footprintPixels(typePath: str):
   # Returns None for anything not covered -- callers should render those as
   # a plain point, not a box.
   footprint = SCIM_FOOTPRINTS_METERS_BY_TYPEPATH.get(typePath)
   if footprint is None:
      for (substring, fallback) in FALLBACK_FOOTPRINTS_METERS:
         if substring in typePath:
            footprint = fallback
            break
   if footprint is None:
      return None
   (widthMeters, depthMeters) = footprint
   return [metersToPixelLength(widthMeters / 2), metersToPixelLength(depthMeters / 2)]

def yawFromQuaternion(rotation) -> float:
   (qx, qy, qz, qw) = rotation
   return math.atan2(2 * (qw * qz + qx * qy), 1 - 2 * (qy * qy + qz * qz))

def rotateVectorByQuaternion(rotation, vector) -> list:
   (qx, qy, qz, qw) = rotation
   (vx, vy, vz) = vector
   tx = 2 * (qy * vz - qz * vy)
   ty = 2 * (qz * vx - qx * vz)
   tz = 2 * (qx * vy - qy * vx)
   return [
      vx + qw * tx + (qy * tz - qz * ty),
      vy + qw * ty + (qz * tx - qx * tz),
      vz + qw * tz + (qx * ty - qy * tx),
   ]

# Conveyor lifts share sav_data.data.CONVEYOR_BELTS with plain belts (both
# ride the same mSplineData/mTopTransform rendering path in the game's own
# code), but a lift is really a short vertical connector, not a horizontal
# run -- drawn as a line its start/end points project to almost the same
# (x,y), so on the map it reads as a near-invisible sliver instead of a
# clickable building. Splitting lifts out so they render as a small box via
# the normal point-building path (collectBuildings/footprintPixels below)
# instead of a line bucket makes them visible and hoverable like every
# other machine.
CONVEYOR_BELT_ONLY_TYPE_PATHS = tuple(typePath for typePath in sav_data.data.CONVEYOR_BELTS if "ConveyorLift" not in typePath)

# typePaths that get their own dedicated line-bucket (collectSplinePaths) instead
# of being plotted as point buildings -- avoids drawing both a dot and a line
# for every belt/pipeline segment.
LINE_RENDERED_TYPE_PATHS = set(CONVEYOR_BELT_ONLY_TYPE_PATHS) | set(PIPELINE_SEGMENTS) | set(RAILROAD_SEGMENTS) | set(HYPERTUBE_SEGMENTS)

# Always-present engine singletons that match the "/Buildable/" filter but
# aren't actually placed by the player -- BP_ProjectAssembly_C in particular
# sits at a fixed, purely symbolic altitude (~23.5km) tied to the rocket
# launch/ending sequence, which otherwise blows out the altitude filter's range.
EXCLUDED_BUILDING_TYPE_PATHS = {
   "/Game/FactoryGame/Buildable/Factory/ProjectAssembly/BP_ProjectAssembly.BP_ProjectAssembly_C",
}

# The HUB (Build_TradingPost_C) isn't in the SCIM footprint dataset and is a
# one-of-a-kind landmark rather than an ordinary building, so it gets its own
# house icon marker (see collectHub below) instead of rendering as a plain
# point in the catch-all "Other" category.
HUB_TYPE_PATH = "/Game/FactoryGame/Buildable/Factory/TradingPost/Build_TradingPost.Build_TradingPost_C"

LIGHTWEIGHT_BUILDABLE_SUBSYSTEM_TYPE_PATH = "/Script/FactoryGame.FGLightweightBuildableSubsystem"

def _findLightweightBuildableGroups(levels):
   # Foundations/walls/ramps/beams (anything highly repetitive) bypass the
   # normal one-ActorHeader-per-building representation entirely: the engine
   # batches them into a single FGLightweightBuildableSubsystem actor for
   # performance, with sav_parse.py's Object.parse() already fully decoding
   # that into actorSpecificInfo (see sav_parse.py ~line 580) as
   # [lightweightVersion, [buildItemTypePath, [instance, ...]], ...] -- it's
   # just never been surfaced as actual buildings until now. Each instance is
   # [rotationQuaternion, position, swatchLevelPath, patternLevelPath,
   #  [primaryColor, secondaryColor], paintFinishLevelPath, patternRotation,
   #  recipeLevelPath, blueprintProxyLevelPath, lightweightDataProperty,
   #  serviceProvider, playerInfoTableIndex].
   subsystemInstanceName = None
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == LIGHTWEIGHT_BUILDABLE_SUBSYSTEM_TYPE_PATH:
            subsystemInstanceName = header.instanceName
            break
      if subsystemInstanceName is not None:
         break
   if subsystemInstanceName is None:
      return []
   for level in levels:
      for object in level.objects:
         if object.instanceName == subsystemInstanceName:
            info = getattr(object, "actorSpecificInfo", None)
            if not info:
               return []
            return info[1:] # Drop the leading lightweightVersion int.
   return []

def collectBuildings(levels) -> dict:
   # categoryBuckets: category -> typePath -> {"label": str, "points": [x,y,yaw,z,...], "ids": [instanceName,...]}
   categoryBuckets: dict[str, dict[str, dict]] = {}
   categoryCache: dict[str, str] = {}
   for level in levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         if isinstance(actorOrComponentObjectHeader, sav_parse.ActorHeader):
            typePath = actorOrComponentObjectHeader.typePath
            if typePath in LINE_RENDERED_TYPE_PATHS or typePath in EXCLUDED_BUILDING_TYPE_PATHS or typePath == HUB_TYPE_PATH:
               continue
            if "/Buildable/" in typePath or "/Build_" in typePath:
               if typePath not in categoryCache:
                  categoryCache[typePath] = categorizeTypePath(typePath)
               category = categoryCache[typePath]
               typeBuckets = categoryBuckets.setdefault(category, {})
               bucket = typeBuckets.get(typePath)
               if bucket is None:
                  bucket = {"label": readableLabel(typePath), "points": [], "ids": [], "footprintPixels": footprintPixels(typePath)}
                  typeBuckets[typePath] = bucket
               (px, py) = projectXY(actorOrComponentObjectHeader.position)
               bucket["points"].append(px)
               bucket["points"].append(py)
               bucket["points"].append(yawFromQuaternion(actorOrComponentObjectHeader.rotation))
               bucket["points"].append(worldZToMeters(actorOrComponentObjectHeader.position[2]))
               bucket["ids"].append(actorOrComponentObjectHeader.instanceName)

   for (typePath, instances) in _findLightweightBuildableGroups(levels):
      if typePath in LINE_RENDERED_TYPE_PATHS or typePath in EXCLUDED_BUILDING_TYPE_PATHS:
         continue
      if typePath not in categoryCache:
         categoryCache[typePath] = categorizeTypePath(typePath)
      category = categoryCache[typePath]
      typeBuckets = categoryBuckets.setdefault(category, {})
      bucket = typeBuckets.get(typePath)
      if bucket is None:
         bucket = {"label": readableLabel(typePath), "points": [], "ids": [], "footprintPixels": footprintPixels(typePath)}
         typeBuckets[typePath] = bucket
      for (idx, instance) in enumerate(instances):
         (rotationQuaternion, position) = (instance[0], instance[1])
         (px, py) = projectXY(position)
         bucket["points"].append(px)
         bucket["points"].append(py)
         bucket["points"].append(yawFromQuaternion(rotationQuaternion))
         bucket["points"].append(worldZToMeters(position[2]))
         bucket["ids"].append(f"LightweightBuildable:{typePath}:{idx}")

   buildingCategories = []
   for category in categoryBuckets:
      types = []
      for typePath in categoryBuckets[category]:
         bucket = categoryBuckets[category][typePath]
         types.append({
            "typePath": typePath, "label": bucket["label"], "points": bucket["points"], "ids": bucket["ids"],
            "footprintPixels": bucket["footprintPixels"],
            "renderType": "rect" if bucket["footprintPixels"] is not None else "circle",
            "subcategory": categorizeSubcategory(category, typePath),
         })
      buildingCategories.append({"category": category, "types": types})
   return buildingCategories

def collectSplinePaths(levels, typePaths) -> dict:
   # Belts/pipelines/railroads/hypertubes store their path as a "mSplineData"
   # property: an array of structs, each with "Location" (actor-local space,
   # first point always [0,0,0]) plus "ArriveTangent"/"LeaveTangent" vectors
   # (also actor-local) -- the same Location+tangent representation Unreal's
   # own spline component uses, which lets the frontend render a real curve
   # through each segment (cubic Hermite, converted to a canvas bezier -- see
   # map.js's _drawLineBucket) instead of a jagged straight-line approximation.
   # Conveyor lifts have no curve, so instead they carry a single
   # "mTopTransform" struct with a "Translation" offset from the actor's base
   # to its top (no tangent data -- zero vectors there degenerate to a curve
   # indistinguishable from a straight line over such a short segment).
   # World position = actor position + quaternion-rotated local offset (the
   # same rotation also applies to the tangent vectors, since they're
   # actor-local directions, not positions -- so no translation for those).
   # Returns {"polylines": [...], "ids": [...], "pointStride": 7} with one id
   # (the segment's own instanceName) per polyline; each vertex is
   # [x, y, arriveTangentX, arriveTangentY, leaveTangentX, leaveTangentY, z]
   # -- z deliberately last, matching every other bucket type's convention
   # (altitude filtering always reads index stride-1).
   actorTransforms: dict[str, tuple] = {}
   for level in levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         if isinstance(actorOrComponentObjectHeader, sav_parse.ActorHeader):
            if actorOrComponentObjectHeader.typePath in typePaths:
               actorTransforms[actorOrComponentObjectHeader.instanceName] = (
                  actorOrComponentObjectHeader.position, actorOrComponentObjectHeader.rotation)

   ZERO_VECTOR = [0.0, 0.0, 0.0]
   polylines = []
   ids = []
   for level in levels:
      for object in level.objects:
         transform = actorTransforms.get(object.instanceName)
         if transform is None:
            continue
         (position, rotation) = transform
         localPoints = [] # (location, arriveTangent, leaveTangent) triples, actor-local.

         splineData = sav_parse.getPropertyValue(object.properties, "mSplineData")
         if splineData is not None:
            for splinePoint in splineData:
               location = sav_parse.getPropertyValue(splinePoint[0], "Location")
               if location is not None:
                  arriveTangent = sav_parse.getPropertyValue(splinePoint[0], "ArriveTangent") or ZERO_VECTOR
                  leaveTangent = sav_parse.getPropertyValue(splinePoint[0], "LeaveTangent") or ZERO_VECTOR
                  localPoints.append((location, arriveTangent, leaveTangent))
         else:
            topTransform = sav_parse.getPropertyValue(object.properties, "mTopTransform")
            if topTransform is not None:
               translation = sav_parse.getPropertyValue(topTransform[0], "Translation")
               if translation is not None:
                  localPoints = [(ZERO_VECTOR, ZERO_VECTOR, ZERO_VECTOR), (translation, ZERO_VECTOR, ZERO_VECTOR)]

         flatPoints = []
         for (location, arriveTangent, leaveTangent) in localPoints:
            worldOffset = rotateVectorByQuaternion(rotation, location)
            (px, py) = projectXY([position[0] + worldOffset[0], position[1] + worldOffset[1]])
            (arriveX, arriveY) = projectVectorXY(rotateVectorByQuaternion(rotation, arriveTangent))
            (leaveX, leaveY) = projectVectorXY(rotateVectorByQuaternion(rotation, leaveTangent))
            flatPoints.extend([px, py, arriveX, arriveY, leaveX, leaveY, worldZToMeters(position[2] + worldOffset[2])])
         if len(flatPoints) >= 14:
            polylines.append(flatPoints)
            ids.append(object.instanceName)
   return {"polylines": polylines, "ids": ids, "pointStride": 7}

def collectPowerLines(levels) -> dict:
   # Mirrors sav_to_html.py's wireLines logic (lines 251-252, 344-349): each
   # POWER_LINE actor connects from its own position to one destination point
   # given by its "mWireInstances" property's "Locations" entry -- a plain
   # straight connection, no spline/tangent data exists for these. Returns
   # {"polylines": [...], "ids": [...], "pointStride": 3} with one id per polyline.
   powerLineActorPositions: dict[str, list] = {}
   for level in levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         if isinstance(actorOrComponentObjectHeader, sav_parse.ActorHeader):
            if actorOrComponentObjectHeader.typePath in sav_data.data.POWER_LINE:
               powerLineActorPositions[actorOrComponentObjectHeader.instanceName] = actorOrComponentObjectHeader.position

   polylines = []
   ids = []
   for level in levels:
      for object in level.objects:
         if object.instanceName in powerLineActorPositions:
            wireInstances = sav_parse.getPropertyValue(object.properties, "mWireInstances")
            if wireInstances is not None:
               for (name, destinationPosition) in wireInstances[0][0]:
                  if name == "Locations":
                     srcPosition = powerLineActorPositions[object.instanceName]
                     (srcX, srcY) = projectXY(srcPosition)
                     (dstX, dstY) = projectXY(destinationPosition)
                     polylines.append([
                        srcX, srcY, worldZToMeters(srcPosition[2]),
                        dstX, dstY, worldZToMeters(destinationPosition[2]),
                     ])
                     ids.append(object.instanceName)
   return {"polylines": polylines, "ids": ids, "pointStride": 3}

def _purityName(purity) -> str:
   if purity is None:
      return "UNKNOWN"
   return purity.name

# Maps the engine's own per-node purity override enum value (mPurityOverride's
# second element, e.g. "RP_Pure") onto the same Purity enum the static
# sav_data.resourcePurity.RESOURCE_PURITY table uses, so both sources feed
# the same downstream purity-bucketing/coloring code.
_PURITY_OVERRIDE_NAME_TO_ENUM = {
   "RP_Impure": sav_data.resourcePurity.Purity.IMPURE,
   "RP_Normal": sav_data.resourcePurity.Purity.NORMAL,
   "RP_Pure": sav_data.resourcePurity.Purity.PURE,
}

# The well's own core actor (sits at the same spot as its Pressurizer) --
# it turns out to carry a mResourceClassOverride too (mirroring its well's
# resource), but it isn't a real extraction point itself, so it's always
# excluded regardless of what overrides it happens to carry.
FRACKING_CORE_TYPE_PATH = "/Game/FactoryGame/Resource/BP_FrackingCore.BP_FrackingCore_C"
FRACKING_SATELLITE_TYPE_PATH = "/Game/FactoryGame/Resource/BP_FrackingSatellite.BP_FrackingSatellite_C"

def collectResourceNodes(levels) -> dict:
   # Mirrors sav_to_html.py's exact approach (lines 242-250, 366-375) for
   # discovering which resource nodes exist and where, via the save's own
   # ActorHeaders.
   minerInstances = set()
   minedResourceActors: dict[str, tuple] = {} # instanceName -> (position, typePath)
   for level in levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         if isinstance(actorOrComponentObjectHeader, sav_parse.ActorHeader):
            if actorOrComponentObjectHeader.typePath in sav_data.data.MINERS:
               minerInstances.add(actorOrComponentObjectHeader.instanceName)
            elif actorOrComponentObjectHeader.typePath in sav_data.data.MINED_RESOURCES:
               if actorOrComponentObjectHeader.typePath == FRACKING_CORE_TYPE_PATH:
                  continue # See FRACKING_CORE_TYPE_PATH above -- never a real node.
               minedResourceActors[actorOrComponentObjectHeader.instanceName] = (
                  actorOrComponentObjectHeader.position, actorOrComponentObjectHeader.typePath)

   minedResourceInstanceNames = set()
   # "Game mode" settings (Purity Modifier and/or Node Randomization, set at
   # world creation) override a node's actual resource type/purity directly
   # on the node's own actor -- mResourceClassOverride/mPurityOverride --
   # rather than changing the static map layout sav_data.resourcePurity's
   # RESOURCE_PURITY table was extracted from. With either setting active,
   # the large majority of nodes end up with a genuinely different resource
   # type than that static table says (confirmed against a real save with
   # Node Randomization on: ~75% of nodes' actual mResourceClassOverride
   # disagreed with RESOURCE_PURITY's entry for that exact instance) -- so
   # these per-node overrides, when present, are read as the authoritative
   # source and the static table is only a fallback for nodes that don't
   # carry them. In practice that fallback is just Geyser nodes: per the
   # game's own rules neither setting ever touches Geysers, so they alone
   # never carry these override properties.
   overridesByInstanceName: dict[str, tuple] = {} # instanceName -> (resourceType, purity)
   for level in levels:
      for object in level.objects:
         if object.instanceName in minerInstances:
            extractableResource = sav_parse.getPropertyValue(object.properties, "mExtractableResource")
            if extractableResource is not None:
               minedResourceInstanceNames.add(extractableResource.pathName)
         elif object.instanceName in minedResourceActors:
            resourceClassOverride = sav_parse.getPropertyValue(object.properties, "mResourceClassOverride")
            purityOverride = sav_parse.getPropertyValue(object.properties, "mPurityOverride")
            overrideResourceType = None
            if resourceClassOverride is not None and getattr(resourceClassOverride, "pathName", None):
               overrideResourceType = resourceClassOverride.pathName.rsplit(".", 1)[-1]
            overridePurity = None
            if isinstance(purityOverride, list) and len(purityOverride) == 2:
               overridePurity = _PURITY_OVERRIDE_NAME_TO_ENUM.get(purityOverride[1])
            if overrideResourceType is not None or overridePurity is not None:
               overridesByInstanceName[object.instanceName] = (overrideResourceType, overridePurity)

   # resourceBuckets: resourceType -> {"label": str, "mined": {purity: {"points":[],"ids":[]}}, "unmined": {...}}
   resourceBuckets: dict[str, dict] = {}
   for (instanceName, (position, typePath)) in minedResourceActors.items():
      staticEntry = sav_data.resourcePurity.RESOURCE_PURITY.get(instanceName)
      (overrideResourceType, overridePurity) = overridesByInstanceName.get(instanceName, (None, None))
      resourceType = overrideResourceType or (staticEntry[0] if staticEntry else None)
      purity = overridePurity if overridePurity is not None else (staticEntry[1] if staticEntry else None)
      if resourceType is None:
         continue # Genuinely unknown (no override and no static entry) -- not a real, displayable node.
      # Well/non-well is a fixed physical-world fact (which actor this is),
      # never affected by either game-mode setting above, so it's read
      # straight from the actor's own typePath rather than the (now
      # override-shadowed) static table's "core" field.
      isWell = typePath == FRACKING_SATELLITE_TYPE_PATH
      bucketKey = resourceType + (":well" if isWell else "")
      bucket = resourceBuckets.get(bucketKey)
      if bucket is None:
         label = readableLabel(resourceType) + (" (Resource Well)" if isWell else "")
         # "resourceType" (the plain Desc_*_C path, no ":well" suffix) is
         # kept separately from bucketKey so the frontend can still look up
         # the right icon by it -- bucketKey only exists to keep the
         # well/non-well split internally, the API payload doesn't need it.
         # "isWell" lets the frontend route this into its own "Resource
         # Wells" section instead of "Resource Nodes" (see filters.js).
         bucket = {"label": label, "resourceType": resourceType, "isWell": isWell, "mined": {}, "unmined": {}}
         resourceBuckets[bucketKey] = bucket
      minedFlag = instanceName in minedResourceInstanceNames
      stateBuckets = bucket["mined"] if minedFlag else bucket["unmined"]
      purityName = _purityName(purity)
      purityBucket = stateBuckets.setdefault(purityName, {"points": [], "ids": [], "worldPositions": []})
      (px, py) = projectXY(position)
      purityBucket["points"].append(px)
      purityBucket["points"].append(py)
      purityBucket["points"].append(worldZToMeters(position[2]))
      purityBucket["ids"].append(instanceName)
      # Raw world-space X/Y (not the projected map-pixel px/py above) --
      # sent alongside so the tooltip's Coordinates row/copy button doesn't
      # need a live-actor lookup, which would fail for a MINED node that's
      # since been dismantled or otherwise removed from the save.
      purityBucket["worldPositions"].append(position[0])
      purityBucket["worldPositions"].append(position[1])

   byResourceType = []
   for bucketKey in resourceBuckets:
      bucket = resourceBuckets[bucketKey]
      byResourceType.append({
         "resourceType": bucket["resourceType"],
         "label": bucket["label"],
         "isWell": bucket["isWell"],
         "mined": {"byPurity": bucket["mined"]},
         "unmined": {"byPurity": bucket["unmined"]},
      })
   return {"byResourceType": byResourceType}

def _positionFromSlugEntry(entry):
   return entry # POWER_SLUGS_* store position directly as (x,y,z)

def _positionFromDetailedEntry(entry):
   return entry[2] # SOMERSLOOPS/MERCER_SPHERES store (id, rotationQuat, position, detailsDict)

def _splitCollectableKind(levels, staticDict, positionExtractor) -> dict:
   collectedInstanceNames = set()
   for level in levels:
      if level.collectables1 is not None:
         for collectable in level.collectables1:
            if collectable.pathName in staticDict:
               collectedInstanceNames.add(collectable.pathName)

   remaining = {"points": [], "ids": [], "worldPositions": []}
   collected = {"points": [], "ids": [], "worldPositions": []}
   for instanceName in staticDict:
      position = positionExtractor(staticDict[instanceName])
      (px, py) = projectXY(position)
      bucket = collected if instanceName in collectedInstanceNames else remaining
      bucket["points"].append(px)
      bucket["points"].append(py)
      bucket["points"].append(worldZToMeters(position[2]))
      bucket["ids"].append(instanceName)
      # Raw world-space X/Y, sent alongside so the tooltip's Coordinates
      # row/copy button works even once collected -- a collected pickup's
      # actor is actually removed from the save (confirmed against a real
      # save), so a live /api/instance lookup can never find one. This
      # static reference data is already exactly what plots its icon on the
      # map regardless of collected state, so it's available unconditionally.
      bucket["worldPositions"].append(position[0])
      bucket["worldPositions"].append(position[1])

   return {"remaining": remaining["points"], "remainingIds": remaining["ids"], "remainingWorldPositions": remaining["worldPositions"],
           "collected": collected["points"], "collectedIds": collected["ids"], "collectedWorldPositions": collected["worldPositions"]}

def collectCollectables(levels) -> dict:
   return {
      "slugsBlue": _splitCollectableKind(levels, sav_data.slug.POWER_SLUGS_BLUE, _positionFromSlugEntry),
      "slugsYellow": _splitCollectableKind(levels, sav_data.slug.POWER_SLUGS_YELLOW, _positionFromSlugEntry),
      "slugsPurple": _splitCollectableKind(levels, sav_data.slug.POWER_SLUGS_PURPLE, _positionFromSlugEntry),
      "somersloops": _splitCollectableKind(levels, sav_data.somersloop.SOMERSLOOPS, _positionFromDetailedEntry),
      "mercerSpheres": _splitCollectableKind(levels, sav_data.mercerSphere.MERCER_SPHERES, _positionFromDetailedEntry),
   }

PLAYER_TYPE_PATH = "/Game/FactoryGame/Character/Player/Char_Player.Char_Player_C"

def collectPlayers(levels) -> dict:
   points = []
   ids = []
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == PLAYER_TYPE_PATH:
            (px, py) = projectXY(header.position)
            points.append(px)
            points.append(py)
            points.append(worldZToMeters(header.position[2]))
            ids.append(header.instanceName)
   return {"points": points, "ids": ids}

def collectHub(levels) -> dict:
   points = []
   ids = []
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == HUB_TYPE_PATH:
            (px, py) = projectXY(header.position)
            points.append(px)
            points.append(py)
            points.append(worldZToMeters(header.position[2]))
            ids.append(header.instanceName)
   return {"points": points, "ids": ids}

def collectHardDrives(levels) -> dict:
   (_, notOpened, openWithDrive, openAndEmpty, dismantled) = sav_to_html.getCrashSiteState(levels)

   def bucketFor(instanceNames):
      points = []
      ids = []
      worldPositions = []
      for instanceName in instanceNames:
         if instanceName in sav_data.crashSites.CRASH_SITES:
            position = sav_data.crashSites.CRASH_SITES[instanceName][2]
            (px, py) = projectXY(position)
            points.append(px)
            points.append(py)
            points.append(worldZToMeters(position[2]))
            ids.append(instanceName)
            # Raw world-space X/Y -- see _splitCollectableKind's comment;
            # same reasoning applies here once a hard drive is dismantled.
            worldPositions.append(position[0])
            worldPositions.append(position[1])
      return (points, ids, worldPositions)

   (notOpenedPoints, notOpenedIds, notOpenedWorldPositions) = bucketFor(notOpened)
   (openWithDrivePoints, openWithDriveIds, openWithDriveWorldPositions) = bucketFor(openWithDrive)
   (openEmptyPoints, openEmptyIds, openEmptyWorldPositions) = bucketFor(openAndEmpty)
   (dismantledPoints, dismantledIds, dismantledWorldPositions) = bucketFor(dismantled)
   return {
      "notOpened": notOpenedPoints, "notOpenedIds": notOpenedIds, "notOpenedWorldPositions": notOpenedWorldPositions,
      "openWithDrive": openWithDrivePoints, "openWithDriveIds": openWithDriveIds, "openWithDriveWorldPositions": openWithDriveWorldPositions,
      "openEmpty": openEmptyPoints, "openEmptyIds": openEmptyIds, "openEmptyWorldPositions": openEmptyWorldPositions,
      "dismantled": dismantledPoints, "dismantledIds": dismantledIds, "dismantledWorldPositions": dismantledWorldPositions,
   }

def _textPropertyValue(value):
   # Mirrors sav_parse.parseTextProperty()'s output shapes: [flags, historyType,
   # isTextCultureInvariant, s] for HistoryType.NONE, or [flags, historyType,
   # namespace, key, value] for HistoryType.BASE. Returns None for anything else.
   if isinstance(value, list) and len(value) == 4 and value[1] == 255:
      return value[3]
   if isinstance(value, list) and len(value) == 5 and value[1] == 0:
      return value[4]
   return None

def buildSaveIndex(parsedSave: sav_parse.ParsedSave) -> dict:
   # One-time O(n) pass so describeInstance() doesn't rescan the whole save
   # on every click. Cached by sav_map_server.py alongside the map payload.
   headersByInstanceName = {}
   objectsByInstanceName = {}
   for level in parsedSave.levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         headersByInstanceName[actorOrComponentObjectHeader.instanceName] = actorOrComponentObjectHeader
      for object in level.objects:
         objectsByInstanceName[object.instanceName] = object

   # Player-given train station names live on a separate
   # FGTrainStationIdentifier actor (its own ActorHeader, not a component of
   # the station), which references the station via "mStation" and holds the
   # name in "mStationName". Build the reverse lookup (station -> name) once.
   stationNameByStationInstance = {}
   for instanceName in headersByInstanceName:
      header = headersByInstanceName[instanceName]
      if getattr(header, "typePath", None) == "/Script/FactoryGame.FGTrainStationIdentifier":
         identifierObject = objectsByInstanceName.get(instanceName)
         if identifierObject is None:
            continue
         station = sav_parse.getPropertyValue(identifierObject.properties, "mStation")
         stationName = _textPropertyValue(sav_parse.getPropertyValue(identifierObject.properties, "mStationName"))
         if station is not None and hasattr(station, "pathName") and stationName:
            stationNameByStationInstance[station.pathName] = stationName

   # The fluid *type* flowing through a given pipe segment isn't stored on
   # the segment itself -- only mFluidBox (the current amount). It IS
   # authoritatively stored on a separate per-network actor the game itself
   # maintains: "/Script/FactoryGame.FGPipeNetwork" has "mFluidDescriptor"
   # (a direct reference to the actual fluid item) and
   # "mFluidIntegrantScriptInterfaces" (references to every member
   # connector in that network). Each pipe's own FGPipeConnectionComponent
   # sub-objects (".PipelineConnection0"/"1"/"FGPipeConnectionFactory") carry
   # "mPipeNetworkID", so cross-referencing a network's member connectors'
   # IDs against its mFluidDescriptor gives a direct, complete mapping --
   # confirmed against a dedicated test save (a single pipe holding Nitrogen
   # Gas, fully disconnected from anything else).
   pipeNetworkIdToFluid = {}
   for instanceName in headersByInstanceName:
      header = headersByInstanceName[instanceName]
      if getattr(header, "typePath", None) != "/Script/FactoryGame.FGPipeNetwork":
         continue
      networkActorObject = objectsByInstanceName.get(instanceName)
      if networkActorObject is None:
         continue
      fluidDescriptor = sav_parse.getPropertyValue(networkActorObject.properties, "mFluidDescriptor")
      if fluidDescriptor is None or not hasattr(fluidDescriptor, "pathName") or not fluidDescriptor.pathName:
         continue
      fluidLabel = readableLabel(fluidDescriptor.pathName)
      members = sav_parse.getPropertyValue(networkActorObject.properties, "mFluidIntegrantScriptInterfaces") or []
      for memberReference in members:
         if not hasattr(memberReference, "pathName") or not memberReference.pathName:
            continue
         # Each reference points at the pipe/machine ACTOR itself, not its
         # connector sub-object -- mPipeNetworkID lives on the connector
         # (".PipelineConnection0"/"1"/"FGPipeConnectionFactory"), so every
         # naming convention seen so far is tried.
         for connectorSuffix in (".PipelineConnection0", ".PipelineConnection1", ".FGPipeConnectionFactory"):
            connectorObject = objectsByInstanceName.get(memberReference.pathName + connectorSuffix)
            if connectorObject is None:
               continue
            networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
            if networkId is not None:
               pipeNetworkIdToFluid[networkId] = fluidLabel

   # Lightweight buildables (see _findLightweightBuildableGroups) have no
   # real instanceName/Object of their own to look up at tooltip time -- this
   # indexes the synthetic "LightweightBuildable:<typePath>:<idx>" ids
   # collectBuildings() already generated against the same instance data.
   lightweightInstancesById = {}
   for (typePath, instances) in _findLightweightBuildableGroups(parsedSave.levels):
      for (idx, instance) in enumerate(instances):
         lightweightInstancesById[f"LightweightBuildable:{typePath}:{idx}"] = {"typePath": typePath}

   return {
      "headers": headersByInstanceName,
      "objects": objectsByInstanceName,
      "stationNameByStationInstance": stationNameByStationInstance,
      "pipeNetworkIdToFluid": pipeNetworkIdToFluid,
      "lightweightInstancesById": lightweightInstancesById,
   }

def _resolveComponentObject(saveIndex, properties, propertyName):
   reference = sav_parse.getPropertyValue(properties, propertyName)
   if reference is not None and hasattr(reference, "pathName"):
      return saveIndex["objects"].get(reference.pathName)
   return None

# Hand-curated, wiki-sourced (satisfactory.wiki.gg) rated power consumption in
# MW at 100% clock speed. The save itself only stores a *live* power draw
# (FGPowerInfoComponent.mTargetConsumption, see describeInstance below) which
# ramps down to 0.1MW whenever a machine is idle or output-blocked, so it
# can't answer "how much power does this use when actually running" -- that
# rated figure is static per building+recipe and isn't serialized anywhere in
# the save, hence this table. Checked as substrings against the instance's
# typePath, first match wins (mirrors CATEGORY_RULES below).
# Particle Accelerator (typePath substring "HadronCollider") is handled
# separately below since its rated power depends on the active recipe.
# Converter and Quantum Encoder get a (min, max) range instead of a single
# value because their actual draw oscillates continuously over each
# production cycle rather than holding at one steady value.
BASE_POWER_MW_BY_TYPEPATH_SUBSTRING = (
   ("QuantumEncoder", (0.1, 2000.0)),
   ("Converter", (100.0, 400.0)),
   ("Smelter", 4.0),
   ("Foundry", 16.0),
   ("Constructor", 4.0),
   ("Manufacturer", 55.0),
   ("Assembler", 15.0),
   ("Refinery", 30.0),
   ("Blender", 75.0),
   ("Packager", 10.0),
   ("MinerMk1", 5.0),
   ("MinerMk2", 15.0),
   ("MinerMk3", 45.0),
   ("OilPump", 40.0),
   ("WaterPump", 20.0),
   ("FrackingSmasher", 150.0),  # Resource Well Pressurizer
   ("FrackingExtractor", 0.0),  # Resource Well Extractor satellite -- doesn't require power
)

# The Particle Accelerator's rated power range (MW at 100% clock) depends on
# its active recipe, but every recipe collapses into exactly one of two tiers
# (per the wiki) -- matched here by keyword against the recipe's own
# typePath/pathName. Falls back to the lighter tier (the more common one) if
# no recipe is set yet.
PARTICLE_ACCELERATOR_POWER_RANGES_MW = (
   (("DarkMatter", "Ficsonium", "NuclearPasta"), (500.0, 1500.0)),
   ((), (250.0, 750.0)),
)

# Power consumption does NOT scale linearly with clock speed -- this exponent
# (changed from 1.6 to 1.321928 in patch 0.7) is confirmed against the wiki's
# own stated examples (50% clock -> 40% power, 200% clock -> 250% power) and
# independently verified against the Particle Accelerator's overclocking
# table, which scales its min/max/mean identically using this same exponent.
POWER_CLOCK_SPEED_EXPONENT = 1.321928

def _ratedPowerForTypePath(typePath, recipePathName):
   if typePath is None:
      return None
   if "HadronCollider" in typePath:
      haystack = recipePathName or ""
      for keywords, mwRange in PARTICLE_ACCELERATOR_POWER_RANGES_MW:
         if any(keyword in haystack for keyword in keywords):
            return mwRange
      return PARTICLE_ACCELERATOR_POWER_RANGES_MW[-1][1]
   for substring, ratedMW in BASE_POWER_MW_BY_TYPEPATH_SUBSTRING:
      if substring in typePath:
         return ratedMW
   return None

def _scaleRatedPowerForClockSpeed(ratedMW, clockSpeedFraction):
   factor = clockSpeedFraction ** POWER_CLOCK_SPEED_EXPONENT
   if isinstance(ratedMW, tuple):
      scaledMin = ratedMW[0] * factor
      scaledMax = ratedMW[1] * factor
      return (round(scaledMin, 1), round(scaledMax, 1), round((scaledMin + scaledMax) / 2, 1))
   return round(ratedMW * factor, 1)

# Raw (unpackaged) fluid/gas resources that can sit in a building's
# mInventoryStacks (e.g. a Refinery's liquid input/output) at 1000x scale.
# Not in sav_data anywhere, so hand-curated here from sav_data/readableNames.py's
# known Desc_* entries.
FLUID_ITEM_SHORT_NAMES = {
   "Desc_Water_C", "Desc_LiquidOil_C", "Desc_HeavyOilResidue_C", "Desc_LiquidFuel_C",
   "Desc_LiquidTurboFuel_C", "Desc_AluminaSolution_C", "Desc_SulfuricAcid_C",
   "Desc_NitricAcid_C", "Desc_NitrogenGas_C", "Desc_LiquidBiofuel_C", "Desc_RocketFuel_C",
}

def _isFluidItemPath(itemPath: str) -> bool:
   return itemPath.rsplit(".", 1)[-1] in FLUID_ITEM_SHORT_NAMES

def _inventoryContents(componentObject) -> list:
   if componentObject is None:
      return []
   stacks = sav_parse.getPropertyValue(componentObject.properties, "mInventoryStacks")
   if stacks is None:
      return []
   # Each inventory slot is its own stack (e.g. 24 separate 100-item stacks
   # of the same part in a storage container), so merge same-item stacks
   # into one summed row for a readable tooltip.
   countByItem: dict[str, float] = {}
   fluidItemLabels: set = set()
   for stack in stacks:
      item = sav_parse.getPropertyValue(stack[0], "Item")
      numItems = sav_parse.getPropertyValue(stack[0], "NumItems")
      if item and numItems:
         itemPath = item[0] if isinstance(item, (list, tuple)) else item
         if itemPath:
            label = readableLabel(itemPath)
            countByItem[label] = countByItem.get(label, 0) + numItems
            if _isFluidItemPath(itemPath):
               fluidItemLabels.add(label)
   contents = []
   for label in countByItem:
      if label in fluidItemLabels:
         # Fluids held as inventory stacks (e.g. a Refinery's liquid input/
         # output) are stored at 1000x scale -- e.g. "18053" is really 18.1 m3.
         contents.append({"item": label, "count": round(countByItem[label] / 1000, 1), "unit": "m³"})
      else:
         contents.append({"item": label, "count": countByItem[label]})
   return contents

def describeInstance(saveIndex: dict, instanceName: str) -> dict:
   # Lightweight buildables (foundations/walls/ramps/beams/decorative pieces)
   # have no real ActorHeader/Object of their own -- see
   # _findLightweightBuildableGroups and buildSaveIndex's
   # lightweightInstancesById -- so they're resolved from that separate
   # index instead of the normal headers/objects lookup. Their only "recipe"
   # data available is mBuiltWithRecipe (what recipe constructs this exact
   # object), not mCurrentRecipe (the meaningful, player-chosen production
   # recipe shown for actual manufacturing machines) -- it's identical for
   # every instance of a given type and just restates the object's own
   # identity, so it's deliberately not surfaced here at all.
   lightweightInfo = saveIndex.get("lightweightInstancesById", {}).get(instanceName)
   if lightweightInfo is not None:
      typePath = lightweightInfo["typePath"]
      return {"instanceName": instanceName, "typePath": typePath, "label": readableLabel(typePath)}

   header = saveIndex["headers"].get(instanceName)
   if header is None:
      return {"error": "Instance not found in the currently loaded save (it may have been removed, mined out, or collected)."}

   typePath = getattr(header, "typePath", None) or getattr(header, "className", None)
   result = {
      "instanceName": instanceName,
      "typePath": typePath,
      "label": readableLabel(typePath) if typePath else instanceName,
      "position": getattr(header, "position", None),
   }

   stationName = saveIndex.get("stationNameByStationInstance", {}).get(instanceName)
   if stationName:
      result["stationName"] = stationName

   object = saveIndex["objects"].get(instanceName)
   if object is None:
      return result
   properties = object.properties

   # Players are a different kind of actor entirely -- recipes/power/inventory
   # component names below are all production-building concepts that don't
   # apply here, and "mInventory" in particular would otherwise get picked up
   # by the generic cargoInventory lookup further down and mislabeled
   # "Cargo" (that property name is shared with Train Docking Stations).
   # Handled separately and returned early.
   if typePath == PLAYER_TYPE_PATH:
      playerName = sav_parse.getPropertyValue(properties, "mCachedPlayerName")
      if playerName:
         result["label"] = playerName
      inventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mInventory"))
      if inventory:
         result["playerInventory"] = inventory
      result["rawProperties"] = [{"name": name, "value": sav_parse.toString(value)} for (name, value) in properties]
      return result

   recipe = sav_parse.getPropertyValue(properties, "mCurrentRecipe")
   recipePathName = None
   if recipe is not None and hasattr(recipe, "pathName") and recipe.pathName:
      recipePathName = recipe.pathName
      # readableLabel() turns "Recipe_Alternate_PureCateriumIngot_C" into
      # "Recipe, Alternate, Pure Caterium Ingot" -- both prefixes are
      # redundant once it's already shown under a "Recipe" row.
      recipeLabel = readableLabel(recipe.pathName)
      for noisePrefix in ("Recipe, ", "Alternate, "):
         if recipeLabel.startswith(noisePrefix):
            recipeLabel = recipeLabel[len(noisePrefix):]
      result["recipe"] = recipeLabel

   # mBuiltWithRecipe is deliberately not surfaced: it's always just
   # "Recipe, <this building's own name>" (e.g. a Conveyor Belt Mk6 was
   # built with "Recipe, Conveyor Belt Mk6"), which only duplicates the
   # title and shows as an empty/pointless line for buildings without one.

   # mCurrentPotential is only ever serialized when it differs from 1.0 (100%),
   # so its mere absence doesn't mean "not overclockable" -- gate display on
   # whether this building type supports overclocking at all (recipe-driven
   # production, resource extraction, or power generation).
   canOverclock = (
      sav_parse.getPropertyValue(properties, "mCurrentRecipe") is not None or
      sav_parse.getPropertyValue(properties, "mExtractableResource") is not None or
      (typePath is not None and "Generator" in typePath)
   )
   clockSpeedFraction = 1.0
   if canOverclock:
      clockSpeed = sav_parse.getPropertyValue(properties, "mCurrentPotential")
      clockSpeedFraction = clockSpeed if clockSpeed is not None else 1.0
      result["clockSpeedPercent"] = round(clockSpeedFraction * 100, 1)

      # mIsProducing/mIsProductionPaused are both only ever serialized when
      # True (the engine omits them at their False default), so their mere
      # absence is itself the "not currently producing"/"not paused" signal,
      # not a missing value -- confirmed against the save: mIsProducing's
      # presence lines up exactly with every building's power draw jumping
      # off its 0.1MW idle floor, across every production/extraction/
      # generation building type checked. This is a more direct and reliable
      # "is it actually running right now" signal than the live power draw
      # number itself (which, for Particle Accelerator/Converter/Quantum
      # Encoder, swings wildly even while genuinely running).
      if sav_parse.getPropertyValue(properties, "mIsProductionPaused"):
         result["runningStatus"] = "Paused"
      elif sav_parse.getPropertyValue(properties, "mIsProducing"):
         result["runningStatus"] = "Running"
      else:
         result["runningStatus"] = "Idle"

   progress = sav_parse.getPropertyValue(properties, "mCurrentManufacturingProgress")
   if progress is not None:
      result["productionProgressPercent"] = round(progress * 100, 1)

   # Train stations/platforms report a negligible mTargetConsumption (~0.1MW,
   # presumably signal/lighting power) that isn't a meaningful figure for
   # this building type, so it's omitted entirely rather than shown as noise.
   isTrainPlatform = typePath is not None and ("TrainStation" in typePath or "TrainDockingStation" in typePath or "TrainPlatformEmpty" in typePath)
   if not isTrainPlatform:
      ratedPowerMW = _ratedPowerForTypePath(typePath, recipePathName)
      scaled = _scaleRatedPowerForClockSpeed(ratedPowerMW, clockSpeedFraction) if ratedPowerMW is not None else None
      isRangedBuilding = isinstance(scaled, tuple)
      if isRangedBuilding:
         result["basePowerConsumptionRangeMW"] = [scaled[0], scaled[1]]
         result["basePowerConsumptionMeanMW"] = scaled[2]
      elif scaled is not None:
         result["basePowerConsumptionMW"] = scaled

   # Power Storage (battery) buildings hold their current charge in a plain
   # "mPowerStore" float (MWh) directly on the actor -- not inside the
   # FGPowerInfoComponent like every other power-related field above, which
   # is why it was missed entirely until now.
   powerStore = sav_parse.getPropertyValue(properties, "mPowerStore")
   if powerStore is not None:
      result["powerStoredMWh"] = round(powerStore, 1)

   # Generators don't consume power, they produce it -- their FGPowerInfoComponent
   # has none of the consumer-side fields above, and instead reports the current
   # generation amount as mDynamicProductionCapacity (fuel/coal/nuclear generators,
   # which ramp output up/down with mIsFullBlast) or mBaseProduction (Geothermal
   # Generator, whose output instead depends on the geyser it's built on and
   # oscillates slightly over time via mVariablePowerProductionCycleOffset).
   if typePath is not None and "Generator" in typePath:
      powerComponent = _resolveComponentObject(saveIndex, properties, "mPowerInfo")
      if powerComponent is not None:
         production = sav_parse.getPropertyValue(powerComponent.properties, "mDynamicProductionCapacity")
         if production is None:
            production = sav_parse.getPropertyValue(powerComponent.properties, "mBaseProduction")
         if production is not None:
            result["powerProductionMW"] = round(production, 1)

   # Pipelines/pumps don't have a discrete inventory -- mFluidBox is a plain
   # float giving the current fluid amount. The fluid *type* isn't on the
   # segment itself either, only its network ID (see buildSaveIndex's
   # pipeNetworkIdToFluid) -- resolved here via whichever connector
   # sub-object this instance happens to have.
   fluidContent = sav_parse.getPropertyValue(properties, "mFluidBox")
   if fluidContent is not None:
      result["fluidContent"] = round(fluidContent, 1)
      for connectorSuffix in (".PipelineConnection0", ".PipelineConnection1"):
         connectorObject = saveIndex["objects"].get(instanceName + connectorSuffix)
         if connectorObject is None:
            continue
         networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
         fluidLabel = saveIndex["pipeNetworkIdToFluid"].get(networkId)
         if fluidLabel is not None:
            result["fluidType"] = fluidLabel
            break

   # Fuel Generators and the Nuclear Power Plant don't use mInputInventory
   # like other production buildings -- their fuel (and, for Nuclear, the
   # supplemental water) sits in mFuelInventory instead. Combined with
   # mInputInventory below since a building only ever has one or the other.
   inputInventory = (
      _inventoryContents(_resolveComponentObject(saveIndex, properties, "mInputInventory")) +
      _inventoryContents(_resolveComponentObject(saveIndex, properties, "mFuelInventory"))
   )
   if inputInventory:
      result["inputInventory"] = inputInventory
   outputInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mOutputInventory"))
   if outputInventory:
      result["outputInventory"] = outputInventory
   storageInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mStorageInventory"))
   if storageInventory:
      result["storageInventory"] = storageInventory

   # Splitters/Mergers (plain, Smart/Programmable, Priority, and their
   # Lift-mounted variants) hold in-transit items in a single "mBufferInventory"
   # component instead of the mInput/mOutputInventory split used by production
   # buildings -- a different name the lookups above never checked, so this
   # building category silently never showed any inventory at all.
   bufferInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mBufferInventory"))
   if bufferInventory:
      result["bufferInventory"] = bufferInventory

   # The AWESOME Sink and AWESOME Shop also use their own one-off component
   # names instead of the standard ones above.
   couponInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mCouponInventory"))
   if couponInventory:
      result["storageInventory"] = couponInventory
   shopInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mShopInventory"))
   if shopInventory:
      result["storageInventory"] = shopInventory

   # Train Docking Stations (Freight Platforms, solid and liquid) use a
   # plain "mInventory" for cargo -- not mInputInventory/mOutputInventory/
   # mStorageInventory like production/storage buildings.
   cargoInventory = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mInventory"))
   if cargoInventory:
      result["cargoInventory"] = cargoInventory

   # Freight platform load/unload direction. Best-effort label: the property
   # is undocumented, but "reversed" consistently correlated with stations
   # configured to unload (pull cargo off the train) rather than load it.
   orientationReversed = sav_parse.getPropertyValue(properties, "mIsOrientationReversed")
   if orientationReversed is not None:
      result["loadMode"] = "Unloading from train" if orientationReversed else "Loading onto train"

   # mInventoryPotential is the building's Power Shard slot (the in-game UI
   # itself calls it that) -- it can hold Power Shards or Somersloops.
   powerShardSlots = _inventoryContents(_resolveComponentObject(saveIndex, properties, "mInventoryPotential"))
   if powerShardSlots:
      result["powerShardSlots"] = powerShardSlots

   # Belts/lifts have no inventory of their own -- in-transit items live on
   # the shared FGConveyorChainActor (sav_parse.py:698's chainItems, index 2),
   # referenced by this segment's "mConveyorChainActor" property.
   chainActor = _resolveComponentObject(saveIndex, properties, "mConveyorChainActor")
   if chainActor is not None and getattr(chainActor, "actorSpecificInfo", None):
      chainItems = chainActor.actorSpecificInfo[2]
      countByItem: dict[str, int] = {}
      for (itemPath, itemInstanceId) in chainItems:
         label = readableLabel(itemPath)
         countByItem[label] = countByItem.get(label, 0) + 1
      if countByItem:
         result["itemsOnBelt"] = [{"item": label, "count": countByItem[label]} for label in countByItem]

   result["rawProperties"] = [{"name": name, "value": sav_parse.toString(value)} for (name, value) in properties]
   return result

GAME_STATE_TYPE_PATH_SUBSTRING = "BP_GameState_C"

def _humanizeEnumValue(rawEnumValue):
   # rawEnumValue is ['EnumTypeName', 'EnumTypeName::SHORT_ValueName'] (see
   # sav_parse.parseEnumProperty) -- this strips the namespace prefix and the
   # enum's own short-code prefix (e.g. "NPS_", "NRM_"), then splits the
   # remaining PascalCase into separate words, e.g. "NPS_AllPure" -> "All Pure".
   if not isinstance(rawEnumValue, list) or len(rawEnumValue) != 2:
      return None
   valueName = rawEnumValue[1].rsplit("::", 1)[-1]
   valueName = re.sub(r"^[A-Z0-9]+_", "", valueName)
   return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", valueName)

def collectGameSettings(levels) -> dict:
   # Game-mode settings chosen at world creation (Purity Modifier, Node
   # Randomization, the cost-scaling sliders) live as plain properties on the
   # session's one BP_GameState_C actor -- not anywhere else in the save.
   # mPartsCostMultiplier (recipe cost) and mSpacePartsCostMultiplier (Space
   # Elevator parts cost) exist alongside these but aren't surfaced here.
   for level in levels:
      for object in level.objects:
         if GAME_STATE_TYPE_PATH_SUBSTRING in object.instanceName:
            properties = object.properties
            return {
               "powerCostMultiplier": sav_parse.getPropertyValue(properties, "mEnergyCostMultiplier"),
               "nodePuritySettings": _humanizeEnumValue(sav_parse.getPropertyValue(properties, "mNodePuritySettings")),
               "nodeRandomization": _humanizeEnumValue(sav_parse.getPropertyValue(properties, "mNodeRandomization")),
            }
   return {}

def buildMapPayload(parsedSave: sav_parse.ParsedSave) -> dict:
   return {
      "mapSize": MAP_SIZE,
      "sessionName": parsedSave.saveFileInfo.sessionName,
      "saveDatetime": parsedSave.saveFileInfo.saveDatetime.strftime("%Y-%m-%d %H:%M:%S"),
      "buildingCategories": collectBuildings(parsedSave.levels),
      "resourceNodes": collectResourceNodes(parsedSave.levels),
      "collectables": collectCollectables(parsedSave.levels),
      "hardDrives": collectHardDrives(parsedSave.levels),
      "players": collectPlayers(parsedSave.levels),
      "hub": collectHub(parsedSave.levels),
      "gameSettings": collectGameSettings(parsedSave.levels),
      "lines": {
         "powerLines": collectPowerLines(parsedSave.levels),
         "belts": collectSplinePaths(parsedSave.levels, CONVEYOR_BELT_ONLY_TYPE_PATHS),
         "pipelines": collectSplinePaths(parsedSave.levels, PIPELINE_SEGMENTS),
         "railroads": collectSplinePaths(parsedSave.levels, RAILROAD_SEGMENTS),
         "hypertubes": collectSplinePaths(parsedSave.levels, HYPERTUBE_SEGMENTS),
      },
   }
