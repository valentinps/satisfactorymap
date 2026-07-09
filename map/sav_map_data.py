#!/usr/bin/env python3
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
import sav_data.freeStuff
import sav_data.readableNames

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

# Self-driving vehicle path segments -- placed like any other buildable
# connector (Explorer/FactoryCart/Tractor/Truck/Universal are per-vehicle-type
# recipes of the same building), carrying a "mSplinePoints" property in the
# exact same Location/ArriveTangent/LeaveTangent shape belts/pipes/rails/
# hypertubes use for "mSplineData" (see collectSplinePaths). This replaced an
# older mSavedPaths/FGDrivingTargetList linked-list system (still referenced
# by parser/sav_cli.py's --export-vehicle-path) that current saves no longer
# populate. Build_VehiclePathNode_*_C (the junction points these segments
# connect) stay plain point buildings, same as pipeline junctions/supports.
# Each tier lives in its own vehicle-specific folder (Universal is the odd one
# out, sitting in a shared "VehiclePath" folder alongside the node classes) --
# not a shared "VehiclePath" folder for all five, so these can't be generated
# from one f-string template the way the folder name might suggest.
VEHICLE_PATH_SEGMENTS = (
   "/Game/FactoryGame/Buildable/Vehicle/Explorer/Build_VehiclePath_Explorer.Build_VehiclePath_Explorer_C",
   "/Game/FactoryGame/Buildable/Vehicle/Golfcart/Build_VehiclePath_FactoryCart.Build_VehiclePath_FactoryCart_C",
   "/Game/FactoryGame/Buildable/Vehicle/Tractor/Build_VehiclePath_Tractor.Build_VehiclePath_Tractor_C",
   "/Game/FactoryGame/Buildable/Vehicle/Truck/Build_VehiclePath_Truck.Build_VehiclePath_Truck_C",
   "/Game/FactoryGame/Buildable/Vehicle/VehiclePath/Build_VehiclePath_Universal.Build_VehiclePath_Universal_C",
)

# --- Build-menu categories --------------------------------------------------
# The map's building category tree is driven by three generated/hand-curated
# files under game_data/ (see game_data/extract_docs_json.py, game_data/categoryLabels.json,
# game_data/categoryOverrides.json):
#   - game_data/generated/buildingCategories.json: ClassName (short, e.g.
#     "Build_ConstructorMk1_C") -> {topCategory, subCategory, menuPriority},
#     extracted straight from the game's Docs.json. The truthful data --
#     never hand-edit this to reorganize the menu, use the override file below.
#   - game_data/categoryLabels.json: hand-guessed display names for the internal
#     Sub_*/SC_* names above (Docs.json has no display string for either).
#   - game_data/categoryOverrides.json: optional hand-maintained regrouping, applied
#     on top of the two files above at load time -- e.g. filing SC_Floors/
#     SC_Ramps/SC_QuatPipes under a brand-new "Foundation" top category instead
#     of wherever Docs.json's own topCategory puts them. See its own comment
#     for the exact shape; entirely optional; missing/empty is a no-op.
# Any placed buildable whose class isn't in buildingCategories.json (new game
# content Docs.json hasn't caught up to yet, or one of the handful of stale
# descriptors that don't resolve to a real buildable) falls into the catch-all
# "Unknown" category. Resource nodes, collectables, hard drives and entities
# are surfaced by their own collectors and are intentionally not part of this.
OTHER_CATEGORY = "Unknown"

# Docs.json/buildingCategories.json don't record the in-game top-tab order,
# only per-building sort priority *within* a subcategory -- this ordering is
# a guess (matches how a new player unlocks/uses these groups), not extracted
# data. Correct freely if it doesn't match the real build menu.
TOP_CATEGORY_ORDER_GUESS = ("Sub_Organisation", "Sub_Walls", "Sub_Production", "Sub_Power", "Sub_Transport", "Sub_Special")

def _shortClassName(typePath: str) -> str:
   if not typePath:
      return None
   pos = typePath.rfind(".")
   return typePath[pos+1:] if pos != -1 else typePath

def _loadJsonFile(path: str) -> dict:
   try:
      with open(path, encoding="utf-8") as handle:
         return json.load(handle)
   except (OSError, ValueError):
      return {}

def _loadBuildMenuCategories():
   # Returns (classNameToCatSub, menuOrder) where classNameToCatSub maps a
   # short ClassName -> (category, subcategory) display-label pair, and
   # menuOrder is the ordered [{"category", "subcategories": [...]}] the
   # frontend renders the tree in.
   categoriesPath = os.path.join(_REPO_ROOT, "game_data", "generated", "buildingCategories.json")
   labelsPath = os.path.join(_REPO_ROOT, "game_data", "categoryLabels.json")
   overridesPath = os.path.join(_REPO_ROOT, "game_data", "categoryOverrides.json")
   classNameToCatSub: dict[str, tuple] = {}
   menuOrder = []
   buildingCategories = _loadJsonFile(categoriesPath)
   if not buildingCategories:
      return (classNameToCatSub, menuOrder)

   labels = _loadJsonFile(labelsPath)
   topLabels = labels.get("topCategories", {})
   subLabels = labels.get("subCategories", {})

   # game_data/categoryOverrides.json is entirely optional (missing/empty is a
   # no-op) -- see its own comment and the doc comment above this function.
   # subcategoryOverrides moves a whole internal subcategory (SC_*) to a
   # different top category id, which can be an existing "Sub_*" one or a
   # brand-new one the override file invents; topCategoryLabels supplies the
   # display label for the latter case (an existing "Sub_*" id already has
   # one via topLabels above). Override labels win on a collision, though in
   # practice they only ever add ids topLabels doesn't have.
   overrides = _loadJsonFile(overridesPath)
   subcategoryOverrides = overrides.get("subcategoryOverrides", {})
   topLabels = dict(topLabels)
   topLabels.update(overrides.get("topCategoryLabels", {}))

   # internal subcategory name -> (internal top category, display label, best/lowest menuPriority seen)
   subcategoryInfo: dict[str, tuple] = {}
   for className, entry in buildingCategories.items():
      topInternal = entry.get("topCategory")
      subInternal = entry.get("subCategory")
      topInternal = subcategoryOverrides.get(subInternal, topInternal)
      topLabel = topLabels.get(topInternal, topInternal)
      subLabel = subLabels.get(subInternal, subInternal)
      classNameToCatSub[className] = (topLabel, subLabel)
      priority = entry.get("menuPriority")
      if priority is None:
         priority = 0.0
      existing = subcategoryInfo.get(subInternal)
      if existing is None or priority < existing[2]:
         subcategoryInfo[subInternal] = (topInternal, subLabel, priority)

   subsByTop: dict[str, list] = {}
   for (topInternal, subLabel, priority) in subcategoryInfo.values():
      subsByTop.setdefault(topInternal, []).append((priority, subLabel))
   # Every top category id actually in play (built-in or override-introduced)
   # needs a slot in the tab order -- topLabels alone would miss a brand-new
   # override id if the override file didn't also bother repeating it in
   # topCategoryLabels (it's still filed correctly, just unlabeled/raw-id).
   topOrder = list(TOP_CATEGORY_ORDER_GUESS) + sorted(set(subsByTop) - set(TOP_CATEGORY_ORDER_GUESS))
   for topInternal in topOrder:
      subs = subsByTop.get(topInternal)
      if not subs:
         continue
      subs.sort(key=lambda item: (item[0], item[1]))
      menuOrder.append({
         "category": topLabels.get(topInternal, topInternal),
         "subcategories": [label for (_, label) in subs],
      })
   return (classNameToCatSub, menuOrder)

CLASSNAME_TO_CATSUB, BUILD_MENU_ORDER = _loadBuildMenuCategories()

def categorizeSubcategory(category: str, typePath: str) -> str:
   entry = CLASSNAME_TO_CATSUB.get(_shortClassName(typePath))
   return entry[1] if entry is not None else None


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
   entry = CLASSNAME_TO_CATSUB.get(_shortClassName(typePath))
   return entry[0] if entry is not None else OTHER_CATEGORY

MAP_SIZE = 8192 # map_highres.png dimensions; must match buildMapPayload()'s "mapSize".

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
#
# map_highres.png is now generated by game_data/extract_map_image.py, which fuses
# the game's own 4-corner sliced map render (see that script's header) instead
# of upscaling blank_map20.png -- but it covers the identical crop region
# (reconfirmed the same way: resizing the fused tiles down to the old 5000px
# size and diffing against the old file landed at a mean abs error of ~4.9),
# so no changes were needed here beyond MAP_SIZE.
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
# game/mesh data). game_data/generated/buildings.json (see game_data/SCHEMA.md)
# is extracted straight from the game's own Docs.json and covers ~546
# building types -- every placed buildable of interest -- so it's used as the
# only source for footprint boxes now, keyed by short ClassName the same way
# CLASSNAME_TO_CATSUB is.
FALLBACK_FOOTPRINTS_METERS = (
   # ConveyorLift isn't a real collision box, just a visible/clickable marker
   # (see CONVEYOR_LIFT_TYPE_PATHS) -- buildings.json has no clearance/
   # dimensions data for it at all (it's an adaptive-length piece), and even
   # if it did, a lift's real footprint is a thin vertical connector that
   # would barely register on a top-down map.
   ("ConveyorLift", (1.0, 1.0)),
)

def _footprintMetersFromBuildingEntry(entry: dict):
   # Prefers clearance (mClearanceData): the union bounding box across all of
   # a building's clearance volumes, taking only the X/Y (top-down) extents
   # and ignoring Z (height) -- exactly what a 2D map footprint needs. This is
   # deliberately preferred over `dimensions` even when both are present,
   # because `dimensions`' Width/Depth/Height keys don't reliably map to
   # horizontal-vs-vertical axes for every building shape: a wall reports
   # Width=800/Height=400 with no Depth at all, and naively using Height as a
   # second horizontal axis would draw it as a solid 8x4m block instead of the
   # thin ~0.5m-thick panel its clearance box actually describes. See
   # game_data/SCHEMA.md's "three size fields" section.
   clearance = entry.get("clearance")
   if clearance:
      minX = min(box["min"]["x"] for box in clearance)
      maxX = max(box["max"]["x"] for box in clearance)
      minY = min(box["min"]["y"] for box in clearance)
      maxY = max(box["max"]["y"] for box in clearance)
      (widthCm, depthCm) = (maxX - minX, maxY - minY)
      # Docs.json's own mClearanceData is occasionally stale: e.g.
      # Build_BigGarageDoor_16x8_C (Roll-Up Gate) reports mWidth=1600 (16m,
      # matching its displayName/description) but carries the exact same
      # mClearanceData as the unrelated small Build_Gate_Automated_8x4_C (8m),
      # giving a box only half the real width. Whichever clearance axis is
      # already the larger of the two (the "long" axis for any wall/door/gate-
      # shaped buildable) gets bumped up to `dimensions.Width` if that's bigger
      # still -- never shrunk, and the other (thickness) axis is left alone,
      # so this is a no-op for the vast majority of buildables where clearance
      # already matches or exceeds Width (verified against every building.json
      # entry: BigGarageDoor's 3 material skins are the only ones affected).
      dimensions = entry.get("dimensions") or {}
      width = dimensions.get("Width")
      if width is not None and width > max(widthCm, depthCm):
         if widthCm >= depthCm:
            widthCm = width
         else:
            depthCm = width
      return (widthCm / WORLD_UNITS_PER_METER, depthCm / WORLD_UNITS_PER_METER)
   dimensions = entry.get("dimensions") or {}
   (width, depth) = (dimensions.get("Width"), dimensions.get("Depth"))
   if width is not None and depth is not None:
      return (width / WORLD_UNITS_PER_METER, depth / WORLD_UNITS_PER_METER)
   return None

# A handful of buildables carry no usable size data in Docs.json at all --
# no mWidth/mDepth/mHeight, no mClearanceData, no adaptive-length field
# either (verified against the full raw dump, not just buildings.json).
# Hand-measured/wiki-sourced fallback for those, same spirit as the ConveyorLift
# marker below but keyed by exact ClassName since these aren't multi-variant
# families needing substring matching.
HAND_CURATED_FOOTPRINTS_METERS_BY_CLASSNAME = {
   "Build_Elevator_C": (8.0, 8.0),  # Personnel Elevator -- one foundation square; shaft height is player-built/variable, but the base footprint is fixed.
   # Wall-Mounted Flood Light has no collision at all in-game (confirmed by
   # the wiki, not just a data gap) -- this is a nominal marker size for
   # visibility/click-ability, not a measured footprint.
   "Build_FloodlightWall_C": (0.6, 0.3),
}

def _loadRawBuildingsJson() -> dict:
   buildingsPath = os.path.join(_REPO_ROOT, "game_data", "generated", "buildings.json")
   try:
      with open(buildingsPath, encoding="utf-8") as handle:
         return json.load(handle)
   except (OSError, ValueError):
      return {}

# Cached once -- reused both for the ordinary X/Y footprint below and for the
# full X/Y/Z half-extents _footprintHalfExtentsMeters needs for tilted
# instances (see _tiltedFootprintPixels).
_RAW_BUILDINGS_JSON = _loadRawBuildingsJson()

def _loadBuildingFootprints() -> dict:
   footprintsByClassName = dict(HAND_CURATED_FOOTPRINTS_METERS_BY_CLASSNAME)
   for (className, entry) in _RAW_BUILDINGS_JSON.items():
      footprint = _footprintMetersFromBuildingEntry(entry)
      if footprint is not None:
         footprintsByClassName[className] = footprint
   return footprintsByClassName

BUILDING_FOOTPRINTS_METERS_BY_CLASSNAME = _loadBuildingFootprints()

def _footprintHalfExtentsMeters(className: str):
   # Half-extents (width, depth, height) in meters -- unlike
   # BUILDING_FOOTPRINTS_METERS_BY_CLASSNAME (X/Y only, full width/depth,
   # sized for the ordinary flat-on-the-ground case), this also keeps the Z
   # half-extent from the same clearance box, needed to reconstruct a
   # tilted instance's true 3D box (see _tiltedFootprintPixels). Returns None
   # if there's nothing to compute it from (no clearance, no dimensions).
   entry = _RAW_BUILDINGS_JSON.get(className)
   if entry is None:
      return None
   clearance = entry.get("clearance")
   if clearance:
      minX = min(box["min"]["x"] for box in clearance)
      maxX = max(box["max"]["x"] for box in clearance)
      minY = min(box["min"]["y"] for box in clearance)
      maxY = max(box["max"]["y"] for box in clearance)
      minZ = min(box["min"]["z"] for box in clearance)
      maxZ = max(box["max"]["z"] for box in clearance)
      return ((maxX - minX) / 2 / WORLD_UNITS_PER_METER, (maxY - minY) / 2 / WORLD_UNITS_PER_METER,
              (maxZ - minZ) / 2 / WORLD_UNITS_PER_METER)
   dimensions = entry.get("dimensions") or {}
   (width, depth) = (dimensions.get("Width"), dimensions.get("Depth"))
   if width is not None and depth is not None:
      height = dimensions.get("Height") or 0.0
      return (width / 2 / WORLD_UNITS_PER_METER, depth / 2 / WORLD_UNITS_PER_METER, height / 2 / WORLD_UNITS_PER_METER)
   return None

# Below this, qx^2+qy^2 (see _tiltIntensity) is treated as floating-point
# noise around a pure yaw rotation rather than a genuine tilt -- small enough
# to not visibly matter (well under a degree of true pitch/roll) while safely
# clearing the noise floor seen on real pure-yaw quaternions in practice.
_TILT_THRESHOLD = 0.001

def _tiltIntensity(rotation) -> float:
   (qx, qy, _qz, _qw) = rotation
   return qx * qx + qy * qy

def _convexHull(points: list) -> list:
   # Standard monotone-chain convex hull (Andrew's algorithm) over a small
   # point set (8 box corners here) -- O(n log n), trivial at this size.
   # Returns hull vertices in order (winding direction doesn't matter for
   # either canvas path-filling or the ray-casting point-in-polygon test the
   # frontend uses -- see map.js's _tracePolygon/_pointInPolygon).
   pts = sorted(set(points))
   if len(pts) <= 2:
      return pts
   def cross(o, a, b):
      return (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
   lower = []
   for p in pts:
      while len(lower) >= 2 and cross(lower[-2], lower[-1], p) <= 0:
         lower.pop()
      lower.append(p)
   upper = []
   for p in reversed(pts):
      while len(upper) >= 2 and cross(upper[-2], upper[-1], p) <= 0:
         upper.pop()
      upper.append(p)
   return lower[:-1] + upper[:-1]

def _boxSilhouettePolygonPixels(rotation, cornerRangesCm) -> list:
   # The true top-down silhouette of a local-space box after a FULL rotation
   # (not just yaw): the convex hull of its 8 corners projected to the XY
   # plane -- generally a hexagon, not a rectangle, for a genuinely tilted
   # box (a plain axis-aligned bounding box was tried first and rejected: it
   # can only grow along world X/Y, so it can never point toward the tilt's
   # actual diagonal direction). cornerRangesCm is ((minX,maxX), (minY,maxY),
   # (minZ,maxZ)) in centimeters, deliberately NOT assumed symmetric around
   # the origin -- adaptive-length Beams' boxes start at the instance's own
   # position and extend one-way along the beam axis (see
   # _footprintForInstance).
   # Projected via projectVectorXY (not a bare meters->pixels scale) so this
   # picks up the same Y-axis flip every other world-space vector on the map
   # goes through -- corners are computed directly in centimeters (this
   # project's native world unit) to feed it without a separate conversion.
   cornersPixels = []
   for cx in cornerRangesCm[0]:
      for cy in cornerRangesCm[1]:
         for cz in cornerRangesCm[2]:
            rotated = rotateVectorByQuaternion(rotation, [cx, cy, cz])
            cornersPixels.append(tuple(projectVectorXY(rotated)))
   hull = _convexHull(cornersPixels)
   flatPolygon = []
   for (x, y) in hull:
      flatPolygon.append(x)
      flatPolygon.append(y)
   return flatPolygon

def _tiltedFootprintPolygon(rotation, halfExtentsMeters):
   # Silhouette of an origin-centered box -- see collectBuildings' use of
   # this for why it's only ever computed for the rare genuinely-tilted
   # instances (e.g. Pillars bracing a run between two out-of-line snap
   # points). Adaptive-length Beams don't come through here -- their box
   # isn't origin-centered and their length is per-instance (see
   # _footprintForInstance's beam path).
   (halfWidthM, halfDepthM, halfHeightM) = halfExtentsMeters
   (halfWidthCm, halfDepthCm, halfHeightCm) = (
      halfWidthM * WORLD_UNITS_PER_METER, halfDepthM * WORLD_UNITS_PER_METER, halfHeightM * WORLD_UNITS_PER_METER)
   return _boxSilhouettePolygonPixels(rotation, (
      (-halfWidthCm, halfWidthCm), (-halfDepthCm, halfDepthCm), (-halfHeightCm, halfHeightCm)))

def _loadAdaptiveBeamSpecs() -> dict:
   # ClassName -> (crossHalfACm, crossHalfBCm, defaultLengthCm) for every
   # buildable placed the way Beams are: a stick of player-chosen length
   # snapped between two arbitrary points, at any angle. Identified purely by
   # properties, not by name: an adaptiveLength block carrying both
   # DefaultLength and MaxLength (belts/pipes/ladders only have
   # MeshLength/MeshHeight, poles/supports only a placeholder Length, power
   # lines have MaxLength but no DefaultLength -- none of those are
   # free-angle sticks). As of 1.1 that's exactly the 8 Build_Beam_* types.
   # Their clearance box is authored with the length running along Z (0 ->
   # DefaultLength, NOT origin-centered), but placed instances empirically
   # extend along local +X (verified against a real save: a yaw-only 33.7
   # degree "Braided Cable Cluster" of BeamLength 2884 lands exactly
   # 2400/1600cm away in X/Y), so only the clearance X/Y extents are kept
   # here, as the beam's cross-section.
   specs = {}
   for (className, entry) in _RAW_BUILDINGS_JSON.items():
      adaptive = entry.get("adaptiveLength") or {}
      if not adaptive.get("MaxLength") or not adaptive.get("DefaultLength"):
         continue
      clearance = entry.get("clearance")
      if not clearance:
         continue
      crossHalfACm = (max(box["max"]["x"] for box in clearance) - min(box["min"]["x"] for box in clearance)) / 2
      crossHalfBCm = (max(box["max"]["y"] for box in clearance) - min(box["min"]["y"] for box in clearance)) / 2
      specs[className] = (crossHalfACm, crossHalfBCm, adaptive["DefaultLength"])
   return specs

_ADAPTIVE_BEAM_SPECS_BY_CLASSNAME = _loadAdaptiveBeamSpecs()

def _footprintForInstance(typePath: str, rotation, bucketFootprintPixels, beamLengthCm=None):
   # Returns (yaw, polygonOrNone) for one placed instance. polygonOrNone is a
   # flat [x1,y1,x2,y2,...] pixel-offset list (relative to the instance's
   # own position, already in final rotated orientation -- no further yaw
   # needed at render time) -- see _boxSilhouettePolygonPixels' doc comment.
   # Two cases produce a polygon:
   # - Adaptive-length Beams (see _loadAdaptiveBeamSpecs) ALWAYS get one,
   #   even for a pure-yaw rotation: the bucket's shared footprint rect only
   #   covers the cross-section at the beam's base, not the per-instance
   #   player-chosen length, and the beam extends one-way from its position
   #   (local +X) rather than being centered on it. A horizontal beam's
   #   rotation maps that +X run into the map plane; a vertical one
   #   degenerates to the small cross-section quad -- both fall out of the
   #   same hull.
   # - Anything else only for the rare genuinely-tilted rotation, where the
   #   shared axis-aligned rect can't represent the true silhouette.
   beamSpec = _ADAPTIVE_BEAM_SPECS_BY_CLASSNAME.get(_shortClassName(typePath))
   if beamSpec is not None:
      (crossHalfACm, crossHalfBCm, defaultLengthCm) = beamSpec
      # BeamLength can be missing (pre-lightweight-v2 saves, or a beam that
      # somehow surfaced as a regular actor) or 0 -- fall back to the
      # build-gun default rather than collapsing to a zero-length sliver.
      lengthCm = beamLengthCm if beamLengthCm else defaultLengthCm
      return (0.0, _boxSilhouettePolygonPixels(rotation, (
         (0.0, lengthCm), (-crossHalfACm, crossHalfACm), (-crossHalfBCm, crossHalfBCm))))
   if bucketFootprintPixels is None or _tiltIntensity(rotation) <= _TILT_THRESHOLD:
      return (_renderedYaw(rotation), None)
   halfExtents = _footprintHalfExtentsMeters(_shortClassName(typePath))
   if halfExtents is None:
      return (_renderedYaw(rotation), None)
   return (0.0, _tiltedFootprintPolygon(rotation, halfExtents))

def footprintPixels(typePath: str):
   # Returns None for anything not covered -- callers should render those as
   # a plain point, not a box.
   footprint = BUILDING_FOOTPRINTS_METERS_BY_CLASSNAME.get(_shortClassName(typePath))
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

# Buildings that snap between two arbitrary connection points (Pillars in
# particular; Beams too, though those never reach here anymore -- every Beam
# instance takes _footprintForInstance's dedicated polygon path) can end up
# with a rotation that ISN'T a pure yaw -- e.g. a pillar segment bracing a
# diagonal run between two out-of-line points genuinely has pitch/roll baked
# into its quaternion alongside whatever yaw. yawFromQuaternion still returns *a* number for
# those (atan2 is defined for any input), but it isn't a meaningful top-down
# angle -- confirmed against a real save: ~25% of one save's ~20800 Concrete
# Pillar segments carry a non-trivial pitch/roll component, spread across a
# wide range of tilt amounts, not just clean 90 degree-equivalent flips.
# That's exactly what _footprintForInstance's _tiltIntensity check is for,
# though: by the time a rotation reaches here, it's already been confirmed to
# be a pure yaw (or fallen back here because no polygon could be computed --
# see there), so the value below is always a real, meaningful angle. It is
# NOT safe to special-case a square footprint (halfWidth == halfDepth) to
# always render axis-aligned here -- a square only repeats every 90 degrees,
# not at every angle, so forcing yaw to 0 would silently discard a genuine
# 45-degree (or any non-90-multiple) placement. (An earlier version of this
# function did exactly that, back when it also had to cover the tilted case
# itself -- once tilt got its own dedicated path, the shortcut no longer had
# a reason to exist and was just quietly wrong.)
def _renderedYaw(rotation) -> float:
   return yawFromQuaternion(rotation)

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
LINE_RENDERED_TYPE_PATHS = (set(CONVEYOR_BELT_ONLY_TYPE_PATHS) | set(PIPELINE_SEGMENTS) | set(RAILROAD_SEGMENTS)
   | set(HYPERTUBE_SEGMENTS) | set(VEHICLE_PATH_SEGMENTS))

# Always-present engine singletons that match the "/Buildable/" filter but
# aren't actually placed by the player -- BP_ProjectAssembly_C in particular
# sits at a fixed, purely symbolic altitude (~23.5km) tied to the rocket
# launch/ending sequence, which otherwise blows out the altitude filter's range.
# BP_Train_C is the abstract train-consist actor (one per assembled train,
# grouping its locomotives/wagons); it always sits at the world origin
# (confirmed against a real save: every BP_Train_C at exactly (0,0,0)), so
# plotting it would just stack meaningless markers at the map center -- the
# train's physical position is already covered by its Locomotive/Freight Car
# actors (see VEHICLE_ICONS_BY_TYPE_PATH).
EXCLUDED_BUILDING_TYPE_PATHS = {
   "/Game/FactoryGame/Buildable/Factory/ProjectAssembly/BP_ProjectAssembly.BP_ProjectAssembly_C",
   "/Game/FactoryGame/Buildable/Vehicle/Train/-Shared/BP_Train.BP_Train_C",
}

# --- Vehicles ----------------------------------------------------------------
# Drivable/self-driving vehicles are ordinary ActorHeaders under
# /Buildable/Vehicle/ (the delivery drone is the odd one out, living under
# /Buildable/Factory/DroneStation/), but none of them have a
# buildingCategories.json entry -- they're vehicles, not build-menu buildables
# -- so they all used to land in the catch-all "Unknown" category as
# anonymous dots. Surfaced as their own "Vehicles" map section instead (see
# collectVehicles below), each drawn with the game's own monochrome vehicle
# glyph (extracted by game_data/copy_icons.py's EXTRA_ICON_COPIES into
# map/static/map/icons/vehicles/). Types without a glyph of their own reuse
# the closest match (Fluid Truck -> Truck, Locomotive/Freight Car -> Train).
VEHICLE_ICONS_BY_TYPE_PATH = {
   "/Game/FactoryGame/Buildable/Vehicle/Explorer/BP_Explorer.BP_Explorer_C": "Explorer.png",
   "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_Golfcart.BP_Golfcart_C": "FactoryCart.png",
   "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_GolfcartGold.BP_GolfcartGold_C": "FactoryCart.png",
   "/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C": "Tractor.png",
   "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_Truck.BP_Truck_C": "Truck.png",
   "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_FluidTruck.BP_FluidTruck_C": "Truck.png",
   "/Game/FactoryGame/Buildable/Vehicle/Cyberwagon/Testa_BP_WB.Testa_BP_WB_C": "CyberWagon.png",
   "/Game/FactoryGame/Buildable/Factory/DroneStation/BP_DroneTransport.BP_DroneTransport_C": "Drone.png",
   "/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C": "Train.png",
   "/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C": "Train.png",
}

# Vehicles aren't buildables, so Docs.json carries no mClearanceData/mWidth
# for them (see buildings.json -- checked: no BP_Truck/BP_Locomotive entries
# at all) and footprintPixels() can't cover them. Hand-curated top-down boxes
# instead, (length along local +X = driving direction, width along local Y),
# in meters, wiki-sourced/eyeballed against in-game foundation grid -- these
# draw the vehicle's oriented rectangle under its icon pin, so close-enough
# visual sizes are fine. Locomotive/Freight Car use the game's 16m
# car-spacing on track as their length.
VEHICLE_FOOTPRINTS_METERS_BY_TYPE_PATH = {
   "/Game/FactoryGame/Buildable/Vehicle/Explorer/BP_Explorer.BP_Explorer_C": (7.0, 4.5),
   "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_Golfcart.BP_Golfcart_C": (3.2, 2.2),
   "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_GolfcartGold.BP_GolfcartGold_C": (3.2, 2.2),
   "/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C": (8.5, 5.5),
   "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_Truck.BP_Truck_C": (10.5, 5.5),
   "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_FluidTruck.BP_FluidTruck_C": (10.5, 5.5),
   "/Game/FactoryGame/Buildable/Vehicle/Cyberwagon/Testa_BP_WB.Testa_BP_WB_C": (6.5, 3.5),
   "/Game/FactoryGame/Buildable/Factory/DroneStation/BP_DroneTransport.BP_DroneTransport_C": (9.0, 9.0),
   "/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C": (16.0, 5.4),
   "/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C": (16.0, 5.4),
}

TRAIN_TYPE_PATH = "/Game/FactoryGame/Buildable/Vehicle/Train/-Shared/BP_Train.BP_Train_C"
LOCOMOTIVE_TYPE_PATH = "/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C"
FREIGHT_WAGON_TYPE_PATH = "/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C"
RAILCAR_TYPE_PATHS = {LOCOMOTIVE_TYPE_PATH, FREIGHT_WAGON_TYPE_PATH}

def _vehicleFootprintPixels(typePath):
   footprint = VEHICLE_FOOTPRINTS_METERS_BY_TYPE_PATH.get(typePath)
   if footprint is None:
      return None
   (lengthMeters, widthMeters) = footprint
   return [metersToPixelLength(lengthMeters / 2), metersToPixelLength(widthMeters / 2)]

def collectVehicles(levels) -> list:
   # Same typePath/label/points/ids shape as collectBuildings' buckets
   # (stride-4 [x, y, yaw, z] points, tooltipKind "server" on the frontend --
   # describeInstance resolves a vehicle's position/inventory generically),
   # plus the icon filename under icons/vehicles/ for the map pin and the
   # hand-curated footprintPixels so the frontend can draw the vehicle's
   # oriented box under that pin. Locomotives/Freight Cars are deliberately
   # NOT here anymore -- they're grouped into per-consist entries by
   # collectTrains instead of being plotted as anonymous per-car pins.
   typeBuckets: dict[str, dict] = {}
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath in VEHICLE_ICONS_BY_TYPE_PATH \
               and header.typePath not in RAILCAR_TYPE_PATHS:
            bucket = typeBuckets.get(header.typePath)
            if bucket is None:
               bucket = {"label": readableLabel(header.typePath), "icon": VEHICLE_ICONS_BY_TYPE_PATH[header.typePath],
                         "points": [], "ids": [], "footprintPixels": _vehicleFootprintPixels(header.typePath)}
               typeBuckets[header.typePath] = bucket
            (px, py) = projectXY(header.position)
            bucket["points"].append(px)
            bucket["points"].append(py)
            bucket["points"].append(_renderedYaw(header.rotation))
            bucket["points"].append(worldZToMeters(header.position[2]))
            bucket["ids"].append(header.instanceName)
   return [
      {"typePath": typePath, "label": bucket["label"], "icon": bucket["icon"],
       "points": bucket["points"], "ids": bucket["ids"], "footprintPixels": bucket["footprintPixels"]}
      for (typePath, bucket) in sorted(typeBuckets.items(), key=lambda entry: entry[1]["label"])
   ]

# --- Trains -------------------------------------------------------------------
# A placed train is several physical actors (locomotives + freight cars, each
# its own ActorHeader with position/rotation) tied together by one abstract
# BP_Train_C consist actor (see EXCLUDED_BUILDING_TYPE_PATHS -- it sits at the
# world origin, so it's never plotted directly). The consist object's
# properties link to its cars: "FirstVehicle"/"LastVehicle" object references
# (no "m" prefix, unlike most gameplay properties -- confirmed against a real
# save), and each railroad vehicle carries its coupled neighbors NOT as
# properties but in its binary trailing data: sav_parse.py's
# BP_Locomotive/BP_FreightWagon branch decodes it as actorSpecificInfo =
# [trainList, previousCoupling, nextCoupling] (ObjectReferences, empty
# pathName when that end is uncoupled). Walking from FirstVehicle through
# whichever coupling points at a not-yet-visited car enumerates the consist
# in physical order regardless of individual cars' orientation. A train whose
# links don't resolve degrades to its cars showing up as single-car consists,
# never disappearing entirely.

def _trainConsistsFromMaps(headersByInstanceName: dict, objectsByInstanceName: dict) -> list:
   # Shared core for collectTrains (map payload) and buildSaveIndex (tooltip
   # lookups). Returns [{"id": trainInstanceName, "label": strOrNone,
   # "cars": [{"id", "typePath", "position", "rotation"}, ...]}, ...] with
   # every railcar in the save appearing exactly once -- cars claimed by a
   # BP_Train_C consist in consist order, plus a single-car consist for any
   # orphan the walk never reached.
   def carEntry(carInstanceName):
      header = headersByInstanceName.get(carInstanceName)
      if header is None or getattr(header, "typePath", None) not in RAILCAR_TYPE_PATHS:
         return None
      return {"id": carInstanceName, "typePath": header.typePath,
              "position": header.position, "rotation": header.rotation}

   trains = []
   claimed = set()
   railcarIds = []
   for (instanceName, header) in headersByInstanceName.items():
      typePath = getattr(header, "typePath", None)
      if typePath in RAILCAR_TYPE_PATHS:
         railcarIds.append(instanceName)
      elif typePath != TRAIN_TYPE_PATH:
         continue
      else:
         trainObject = objectsByInstanceName.get(instanceName)
         if trainObject is None:
            continue
         first = sav_parse.getPropertyValue(trainObject.properties, "FirstVehicle") \
            or sav_parse.getPropertyValue(trainObject.properties, "mFirstVehicle")
         last = sav_parse.getPropertyValue(trainObject.properties, "LastVehicle") \
            or sav_parse.getPropertyValue(trainObject.properties, "mLastVehicle")
         ordered = []
         seen = set()
         current = first.pathName if first is not None and getattr(first, "pathName", None) else None
         while current and current not in seen and len(ordered) < 100: # A consist is at most a few dozen cars -- the cap only guards against a malformed coupling cycle.
            seen.add(current)
            ordered.append(current)
            carObject = objectsByInstanceName.get(current)
            current = None
            couplings = getattr(carObject, "actorSpecificInfo", None) if carObject is not None else None
            if isinstance(couplings, list) and len(couplings) == 3:
               for coupled in couplings[1:]:
                  coupledName = getattr(coupled, "pathName", None)
                  if coupledName and coupledName not in seen:
                     current = coupledName
                     break
         # A one-car train has FirstVehicle == LastVehicle; anything the walk
         # missed (unknown coupling property names) at least gets its endpoint.
         lastName = last.pathName if last is not None and getattr(last, "pathName", None) else None
         if lastName and lastName not in seen:
            ordered.append(lastName)
         cars = []
         for carInstanceName in ordered:
            entry = carEntry(carInstanceName)
            if entry is not None and carInstanceName not in claimed:
               claimed.add(carInstanceName)
               cars.append(entry)
         if cars:
            label = _textPropertyValue(sav_parse.getPropertyValue(trainObject.properties, "mTrainName"))
            trains.append({"id": instanceName, "label": label, "cars": cars})
   for carInstanceName in railcarIds:
      if carInstanceName not in claimed:
         entry = carEntry(carInstanceName)
         if entry is not None:
            trains.append({"id": carInstanceName, "label": None, "cars": [entry]})
   return trains

def _headerAndObjectMaps(levels):
   headersByInstanceName = {}
   objectsByInstanceName = {}
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         headersByInstanceName[header.instanceName] = header
      for object in level.objects:
         objectsByInstanceName[object.instanceName] = object
   return (headersByInstanceName, objectsByInstanceName)

def collectTrains(levels) -> dict:
   # One entry per assembled train (not per car): a single pin at the lead
   # car, plus every car's oriented box so the frontend can draw and
   # group-highlight the whole consist. Cars keep their own ids -- clicking a
   # car's box describes that car; clicking the pin describes the whole train
   # (see describeInstance's BP_Train_C branch).
   (headersByInstanceName, objectsByInstanceName) = _headerAndObjectMaps(levels)
   consists = []
   for train in _trainConsistsFromMaps(headersByInstanceName, objectsByInstanceName):
      carPoints = []
      carIds = []
      carKinds = []
      for car in train["cars"]:
         (px, py) = projectXY(car["position"])
         carPoints.append(px)
         carPoints.append(py)
         carPoints.append(_renderedYaw(car["rotation"]))
         carPoints.append(worldZToMeters(car["position"][2]))
         carIds.append(car["id"])
         carKinds.append(readableLabel(car["typePath"]))
      leadPosition = train["cars"][0]["position"]
      (pinX, pinY) = projectXY(leadPosition)
      consists.append({
         "id": train["id"], "label": train["label"],
         "pin": [pinX, pinY, worldZToMeters(leadPosition[2])],
         "cars": {"points": carPoints, "ids": carIds, "kinds": carKinds},
      })
   consists.sort(key=lambda entry: (entry["label"] is None, entry["label"] or ""))
   return {
      "consists": consists,
      # Locomotive and Freight Car boxes are the same size -- one shared
      # footprint for the frontend's single train-cars bucket.
      "carFootprintPixels": _vehicleFootprintPixels(LOCOMOTIVE_TYPE_PATH),
   }

# The HUB (Build_TradingPost_C) isn't in the SCIM footprint dataset and is a
# one-of-a-kind landmark rather than an ordinary building, so it gets its own
# house icon marker (see collectHub below) instead of rendering as a plain
# point in the catch-all "Unknown" category.
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

def _lightweightBeamLengthCm(lightweightDataProperty):
   # instance[9] of a lightweight buildable (see _findLightweightBuildableGroups'
   # layout comment) is a (properties, propertyTypes) pair, present only when
   # the game attached extra per-instance data -- in practice exactly the
   # adaptive-length Beams, whose one property is BeamLength (a
   # FloatProperty, in centimeters, sav_parse tags the payload type
   # "/Script/FactoryGame.BuildableBeamLightweightData"). None for every
   # other buildable and for pre-v2 lightweight saves.
   if not lightweightDataProperty:
      return None
   (properties, _propertyTypes) = lightweightDataProperty
   return sav_parse.getPropertyValue(properties, "BeamLength")

def _newBuildingBucket(typePath: str) -> dict:
   footprint = footprintPixels(typePath)
   return {
      "label": readableLabel(typePath), "points": [], "ids": [], "footprintPixels": footprint,
      # Sparse pointIndex -> flat [x1,y1,x2,y2,...] polygon (pixel offsets
      # from the instance's own position, already in final rotated
      # orientation), populated for any instance whose true top-down
      # silhouette isn't this bucket's shared axis-aligned footprintPixels
      # rect: the rare genuinely-tilted instance, plus EVERY instance of an
      # adaptive-length Beam type (per-instance player-chosen length -- see
      # _footprintForInstance) -- None (not even an empty dict) when nothing
      # in this bucket ever needed it, so the frontend can cheaply skip the
      # whole per-point override lookup for the overwhelming majority of
      # buckets.
      "tiltedFootprints": {},
      # Largest distance from center to any point actually used anywhere in
      # this bucket (starts at the plain rect's own corner distance, grows if
      # any tilted polygon reaches further) -- the frontend's hover/click
      # hit-test needs this, not the plain footprintPixels, to size its
      # cursor-centered spatial-grid query radius, or a tilted instance's
      # enlarged silhouette could fall outside that radius and become
      # unclickable.
      "maxFootprintRadius": math.hypot(footprint[0], footprint[1]) if footprint is not None else 0.0,
   }

def _appendBuildingInstance(bucket: dict, typePath: str, rotation, position, instanceId: str, beamLengthCm=None) -> None:
   (px, py) = projectXY(position)
   (yaw, tiltedPolygon) = _footprintForInstance(typePath, rotation, bucket["footprintPixels"], beamLengthCm)
   if tiltedPolygon is not None:
      bucket["tiltedFootprints"][len(bucket["ids"])] = tiltedPolygon
      polygonRadius = max(math.hypot(tiltedPolygon[i], tiltedPolygon[i + 1]) for i in range(0, len(tiltedPolygon), 2))
      bucket["maxFootprintRadius"] = max(bucket["maxFootprintRadius"], polygonRadius)
   bucket["points"].append(px)
   bucket["points"].append(py)
   bucket["points"].append(yaw)
   bucket["points"].append(worldZToMeters(position[2]))
   bucket["ids"].append(instanceId)

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
            if typePath in VEHICLE_ICONS_BY_TYPE_PATH:
               continue # Surfaced by collectVehicles as the "Vehicles" section, not an "Unknown" building.
            if "/Buildable/" in typePath or "/Build_" in typePath:
               if typePath not in categoryCache:
                  categoryCache[typePath] = categorizeTypePath(typePath)
               category = categoryCache[typePath]
               typeBuckets = categoryBuckets.setdefault(category, {})
               bucket = typeBuckets.get(typePath)
               if bucket is None:
                  bucket = _newBuildingBucket(typePath)
                  typeBuckets[typePath] = bucket
               _appendBuildingInstance(bucket, typePath, actorOrComponentObjectHeader.rotation,
                                        actorOrComponentObjectHeader.position, actorOrComponentObjectHeader.instanceName)

   for (typePath, instances) in _findLightweightBuildableGroups(levels):
      if typePath in LINE_RENDERED_TYPE_PATHS or typePath in EXCLUDED_BUILDING_TYPE_PATHS:
         continue
      if typePath not in categoryCache:
         categoryCache[typePath] = categorizeTypePath(typePath)
      category = categoryCache[typePath]
      typeBuckets = categoryBuckets.setdefault(category, {})
      bucket = typeBuckets.get(typePath)
      if bucket is None:
         bucket = _newBuildingBucket(typePath)
         typeBuckets[typePath] = bucket
      for (idx, instance) in enumerate(instances):
         (rotationQuaternion, position) = (instance[0], instance[1])
         _appendBuildingInstance(bucket, typePath, rotationQuaternion, position, f"LightweightBuildable:{typePath}:{idx}",
                                 beamLengthCm=_lightweightBeamLengthCm(instance[9]))

   buildingCategories = []
   for category in categoryBuckets:
      types = []
      for typePath in categoryBuckets[category]:
         bucket = categoryBuckets[category][typePath]
         types.append({
            "typePath": typePath, "label": bucket["label"], "points": bucket["points"], "ids": bucket["ids"],
            "footprintPixels": bucket["footprintPixels"],
            "tiltedFootprints": bucket["tiltedFootprints"] or None,
            "maxFootprintRadius": bucket["maxFootprintRadius"],
            "renderType": "rect" if bucket["footprintPixels"] is not None else "circle",
            "subcategory": categorizeSubcategory(category, typePath),
         })
      buildingCategories.append({"category": category, "types": types})
   return buildingCategories

def collectSplinePaths(levels, typePaths, splinePropertyName="mSplineData") -> dict:
   # Belts/pipelines/railroads/hypertubes store their path as a "mSplineData"
   # property (vehicle path segments -- see VEHICLE_PATH_SEGMENTS -- use the
   # same shape under the name "mSplinePoints", hence the parameter): an array
   # of structs, each with "Location" (actor-local space, first point always
   # [0,0,0]) plus "ArriveTangent"/"LeaveTangent" vectors (also actor-local)
   # -- the same Location+tangent representation Unreal's own spline
   # component uses, which lets the frontend render a real curve through each
   # segment (cubic Hermite, converted to a canvas bezier -- see map.js's
   # _drawLineBucket) instead of a jagged straight-line approximation.
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

   polylines = []
   ids = []
   for level in levels:
      for object in level.objects:
         transform = actorTransforms.get(object.instanceName)
         if transform is None:
            continue
         (position, rotation) = transform
         flatPoints = _splineSegmentPolyline(object, position, rotation, splinePropertyName)
         if flatPoints is not None:
            polylines.append(flatPoints)
            ids.append(object.instanceName)
   return {"polylines": polylines, "ids": ids, "pointStride": 7}

# Shared per-segment geometry for collectSplinePaths / collectSplinePathGroups:
# turns one belt/pipe/rail/hypertube/vehicle-path segment object into its flat
# [x, y, arriveTanX, arriveTanY, leaveTanX, leaveTanY, z, ...] vertex list (see
# collectSplinePaths's doc comment), or None if it has too few points to draw.
def _splineSegmentPolyline(object, position, rotation, splinePropertyName):
   ZERO_VECTOR = [0.0, 0.0, 0.0]
   localPoints = [] # (location, arriveTangent, leaveTangent) triples, actor-local.
   splineData = sav_parse.getPropertyValue(object.properties, splinePropertyName)
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
   return flatPoints if len(flatPoints) >= 14 else None

# One line-bucket per "mark" (Belts Mk.1..Mk.6, Pipes Mk.1/Mk.2), so the
# frontend can toggle each mark independently. typePaths sharing a readable
# label are merged (e.g. Pipeline / Pipeline_NoIndicator both = "Pipeline
# Mk.1"); the row label is shortened to just the "Mk.N" tail since it sits
# under a "Belts"/"Pipes" group. Single pass over the levels (grouping by
# typePath as it goes) rather than one collectSplinePaths call per mark.
# Returns [{"label", "polylines", "ids", "pointStride"}, ...], empties dropped.
def collectSplinePathGroups(levels, typePaths, splinePropertyName="mSplineData") -> list:
   labelByTypePath = {typePath: readableLabel(typePath) for typePath in typePaths}
   # A representative typePath per readable label -- typePaths that share a label
   # (e.g. Pipeline / Pipeline_NoIndicator both "Pipeline Mk.1") also share a
   # build-menu category/subcategory, so the first one is enough to look it up.
   typePathByLabel: dict[str, str] = {}
   for typePath in typePaths:
      typePathByLabel.setdefault(labelByTypePath[typePath], typePath)

   actorInfo: dict[str, tuple] = {} # instanceName -> (position, rotation, groupLabel)
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath in labelByTypePath:
            actorInfo[header.instanceName] = (header.position, header.rotation, labelByTypePath[header.typePath])

   byLabel: dict[str, dict] = {}
   order = []
   for level in levels:
      for object in level.objects:
         info = actorInfo.get(object.instanceName)
         if info is None:
            continue
         (position, rotation, groupLabel) = info
         flatPoints = _splineSegmentPolyline(object, position, rotation, splinePropertyName)
         if flatPoints is None:
            continue
         if groupLabel not in byLabel:
            byLabel[groupLabel] = {"polylines": [], "ids": []}
            order.append(groupLabel)
         byLabel[groupLabel]["polylines"].append(flatPoints)
         byLabel[groupLabel]["ids"].append(object.instanceName)

   groups = []
   for label in order:
      match = re.search(r"Mk\.?\s*\d+", label)
      representativeTypePath = typePathByLabel.get(label)
      groups.append({
         # "label" stays the full, unambiguous name (kept on the map bucket
         # for tooltips/selection); "mark" is the compact tail shown in the
         # sidebar under the "Conveyor Belts"/"Pipes" group. category/subcategory
         # place the group in the build-menu tree (see buildBuildingCategorySections).
         "label": label,
         "mark": match.group(0) if match else label,
         "typePath": representativeTypePath,
         "category": categorizeTypePath(representativeTypePath) if representativeTypePath else OTHER_CATEGORY,
         "subcategory": categorizeSubcategory(None, representativeTypePath) if representativeTypePath else None,
         "polylines": byLabel[label]["polylines"],
         "ids": byLabel[label]["ids"],
         "pointStride": 7,
      })
   groups.sort(key=lambda group: group["mark"])
   return groups

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
# the same downstream purity-bucketing/coloring code. The engine's own enum
# misspells impure as "RP_Inpure" (confirmed against a real "All Impure"
# save -- every single override used that spelling, never "RP_Impure"), so
# that's the one that actually needs to match; "RP_Impure" is kept too in
# case a different game version ever uses the corrected spelling.
_PURITY_OVERRIDE_NAME_TO_ENUM = {
   "RP_Inpure": sav_data.resourcePurity.Purity.IMPURE,
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
   # Both lists need checking, not just collectables1 -- confirmed against a
   # real save where collectables1 alone missed most already-collected
   # Somersloops/Mercer Spheres (e.g. one save: collectables1 found 84/106
   # collected Somersloops -- 22 "remaining" -- while the true count, cross-
   # checked in-game, was 3 remaining; collectables1 UNION collectables2
   # correctly found 103/106). sav_to_html.py's own comment ("collectables2
   # can be a subset of collectables1") doesn't hold here -- for every
   # collectable kind tested, collectables2 alone found *more* matches than
   # collectables1 alone, so this takes the union of both rather than
   # trusting either list to be complete on its own.
   collectedInstanceNames = set()
   for level in levels:
      for collectableList in (level.collectables1, level.collectables2):
         if collectableList is not None:
            for collectable in collectableList:
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

# Items dropped loose on the ground (player-dropped stacks, leaves/wood/etc.
# already spawned in the world) -- each is its own actor of this one engine
# class, holding what-and-how-many in an "mPickupItems" property.
ITEM_PICKUP_TYPE_PATH = "/Script/FactoryGame.FGItemPickup_Spawnable"

_ITEM_ICONS_DIR = os.path.join(_MAP_DIR, "static", "map", "icons", "items")

def _itemIconFilename(itemShortName: str) -> str:
   # The item's ClassName-keyed icon file under static/map/icons/items/ (see
   # game_data/copy_icons.py), or None if no icon was extracted for it -- the
   # frontend needs to know up front, because map.js's _drawIconBucket
   # silently draws nothing at all for an icon URL that 404s, which would
   # make such a bucket invisible rather than falling back to a plain dot.
   filename = itemShortName + ".png"
   return filename if os.path.exists(os.path.join(_ITEM_ICONS_DIR, filename)) else None

def _itemPickupActorInstanceNames(levels) -> set:
   instanceNames = set()
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == ITEM_PICKUP_TYPE_PATH:
            instanceNames.add(header.instanceName)
   return instanceNames

def _uncollectedCatalogDrops(levels) -> list:
   # sav_data.freeStuff.FREE_DROPPED_ITEMS catalogs every free item stack the
   # vanilla world spawns (ammo/medkit/equipment caches lying around the map,
   # exported from a fully-explored fresh save). A given stack only exists as
   # an actual FGItemPickup_Spawnable actor in a save once its map area has
   # been generated (visited) -- until then the save has no trace of it, even
   # though it's perfectly real in-game, so the catalog is what reveals it.
   # Yields (itemShortName, quantity, position, instanceName) for every
   # catalog stack that ISN'T live in the save already (visited areas' actors
   # -- those are picked up by the ordinary save scan, and their live
   # quantity, which the player may have changed, wins over the catalog's)
   # and ISN'T recorded as collected (picking a spawned stack up removes its
   # actor and logs it in the level collectables lists -- checking both
   # lists' union, same reasoning as _splitCollectableKind).
   presentActors = _itemPickupActorInstanceNames(levels)
   catalogInstanceNames = set()
   for entries in sav_data.freeStuff.FREE_DROPPED_ITEMS.values():
      for (_, _, instanceName) in entries:
         catalogInstanceNames.add(instanceName)
   collectedInstanceNames = set()
   for level in levels:
      for collectableList in (level.collectables1, level.collectables2):
         if collectableList is not None:
            for collectable in collectableList:
               if collectable.pathName in catalogInstanceNames:
                  collectedInstanceNames.add(collectable.pathName)

   drops = []
   for (itemFullPath, entries) in sav_data.freeStuff.FREE_DROPPED_ITEMS.items():
      shortName = itemFullPath.rsplit(".", 1)[-1]
      for (quantity, position, instanceName) in entries:
         if instanceName not in presentActors and instanceName not in collectedInstanceNames:
            drops.append((shortName, quantity, position, instanceName))
   return drops

def collectDroppedItems(levels) -> list:
   # One bucket per item type lying loose on the ground, mirroring the
   # points/ids/worldPositions shape of the other static-tooltip collectors
   # (see collectResourceNodes), plus a parallel "counts" list (stack size
   # per drop -- a dropped stack of 500 Iron Plates is one actor/point).
   # Two sources merged: FGItemPickup_Spawnable actors actually in the save
   # (player-dropped stacks + world spawns in visited areas), and the static
   # world-spawn catalog for stacks in not-yet-generated areas (see
   # _uncollectedCatalogDrops, which owns the dedup between the two).
   # Returns [{"itemPath":, "label":, "icon":, "points":, "ids":, "counts":,
   # "worldPositions":}, ...] sorted by drop count descending.
   pickupPositions: dict[str, list] = {}
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == ITEM_PICKUP_TYPE_PATH:
            pickupPositions[header.instanceName] = header.position

   buckets: dict[str, dict] = {}

   def appendDrop(shortName, position, instanceName, numItems):
      bucket = buckets.get(shortName)
      if bucket is None:
         bucket = {"itemPath": shortName, "label": readableLabel(shortName),
                   "icon": _itemIconFilename(shortName),
                   "points": [], "ids": [], "counts": [], "worldPositions": []}
         buckets[shortName] = bucket
      (px, py) = projectXY(position)
      bucket["points"].extend([px, py, worldZToMeters(position[2])])
      bucket["ids"].append(instanceName)
      bucket["counts"].append(numItems)
      # Raw world-space X/Y for the tooltip's Coordinates row/copy button,
      # same split as collectResourceNodes/_splitCollectableKind.
      bucket["worldPositions"].extend([position[0], position[1]])

   for level in levels:
      for object in level.objects:
         position = pickupPositions.get(object.instanceName)
         if position is None:
            continue
         pickupItems = sav_parse.getPropertyValue(object.properties, "mPickupItems")
         if pickupItems is None:
            continue
         item = sav_parse.getPropertyValue(pickupItems[0], "Item")
         numItems = sav_parse.getPropertyValue(pickupItems[0], "NumItems")
         itemPath = item[0] if isinstance(item, (list, tuple)) else item
         if not itemPath or not numItems: # NumItems 0 = an already-picked-up leftover actor.
            continue
         appendDrop(itemPath.rsplit(".", 1)[-1], position, object.instanceName, numItems)

   for (shortName, quantity, position, instanceName) in _uncollectedCatalogDrops(levels):
      appendDrop(shortName, position, instanceName, quantity)

   return sorted(buckets.values(), key=lambda bucket: (-len(bucket["ids"]), bucket["label"]))

PLAYER_TYPE_PATH = "/Game/FactoryGame/Character/Player/Char_Player.Char_Player_C"

# Of all the wildlife/enemy creatures the save tracks, only the Lizard Doggo
# is shown on the map -- per the user, the others (Hogs, Spitters, Stingers,
# Crab Hatchers, ...) are noise for this purpose.
LIZARD_DOGGO_TYPE_PATH = "/Game/FactoryGame/Character/Creature/Wildlife/SpaceRabbit/Char_SpaceRabbit.Char_SpaceRabbit_C"

def collectCreatures(levels) -> list:
   # Returns the same typePath/label/points/ids shape as collectBuildings's
   # per-category "types" list (currently always zero-or-one entry, but kept
   # list-shaped in case more species are added later). Uses tooltipKind
   # "server" (see filters.js), same as buildings/players -- describeInstance
   # resolves position/rawProperties/petName from a live lookup, so no
   # worldPositions array is needed here the way static-tooltip buckets
   # (resource nodes/collectables) require.
   typeBuckets: dict[str, dict] = {}
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == LIZARD_DOGGO_TYPE_PATH:
            bucket = typeBuckets.get(header.typePath)
            if bucket is None:
               bucket = {"label": readableLabel(header.typePath), "points": [], "ids": []}
               typeBuckets[header.typePath] = bucket
            (px, py) = projectXY(header.position)
            bucket["points"].append(px)
            bucket["points"].append(py)
            bucket["points"].append(worldZToMeters(header.position[2]))
            bucket["ids"].append(header.instanceName)
   return [
      {"typePath": typePath, "label": bucket["label"], "points": bucket["points"], "ids": bucket["ids"]}
      for (typePath, bucket) in typeBuckets.items()
   ]

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

CENTRAL_STORAGE_SUBSYSTEM_TYPE_PATH = "/Script/FactoryGame.FGCentralStorageSubsystem"

def collectDimensionalDepotContents(levels) -> list:
   # The Dimensional Depot (Build_CentralStorage_C uploaders scattered
   # around the map, one shared pool between all of them) isn't a normal
   # per-building inventory -- the global FGCentralStorageSubsystem holds
   # its contents directly as a flat "mStoredItems" list of
   # {ItemClass, Amount} pairs, already aggregated across every uploader,
   # unlike a building's per-slot mInventoryStacks. Zero-amount entries
   # (items once stored, now fully withdrawn) are dropped. Returns
   # [{"itemPath":, "label":, "count":}, ...] sorted by count descending.
   subsystemInstanceName = None
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == CENTRAL_STORAGE_SUBSYSTEM_TYPE_PATH:
            subsystemInstanceName = header.instanceName
   if subsystemInstanceName is None:
      return []
   subsystemObject = None
   for level in levels:
      for object in level.objects:
         if object.instanceName == subsystemInstanceName:
            subsystemObject = object
   if subsystemObject is None:
      return []
   storedItems = sav_parse.getPropertyValue(subsystemObject.properties, "mStoredItems") or []
   items = []
   for entry in storedItems:
      itemClass = sav_parse.getPropertyValue(entry[0], "ItemClass")
      amount = sav_parse.getPropertyValue(entry[0], "Amount")
      if itemClass is None or not getattr(itemClass, "pathName", None) or not amount:
         continue
      shortName = itemClass.pathName.rsplit(".", 1)[-1]
      items.append({"itemPath": shortName, "label": readableLabel(shortName), "count": amount})
   items.sort(key=lambda entry: entry["count"], reverse=True)
   return items

# --- Progression (MAM / alternate recipes / AWESOME Shop / HUB milestones /
# --- Space Elevator) -------------------------------------------------------
# Everything the game calls "unlocked" -- HUB milestones, MAM research nodes,
# alternate recipes from hard drives, AWESOME Shop purchases -- is one flat
# list of purchased FGSchematic references on the save's single
# SchematicManager actor. game_data/generated/schematics.json (see
# extract_docs_json.py) provides the static side: display names, which panel
# each schematic belongs to (its mType), tech tiers, coupon costs, research
# trees, and which recipes it unlocks.
_SCHEMATICS_JSON = _loadJsonFile(os.path.join(_REPO_ROOT, "game_data", "generated", "schematics.json"))
_RECIPES_JSON = _loadJsonFile(os.path.join(_REPO_ROOT, "game_data", "generated", "recipes.json"))

def recipeLabel(recipePathName: str) -> str:
   # The real in-game recipe display name from game_data/generated/recipes.json
   # (e.g. "Alternate: Caterium Wire"). sav_data.readableNames' curated table
   # barely covers Recipe_* paths, so going through readableLabel() produces
   # comma-fied class-name guesses like "Recipe, Alternate, Wire 2" -- kept
   # only as the fallback (minus its noise prefixes) for recipes Docs.json
   # doesn't know (mods, removed content).
   entry = _RECIPES_JSON.get(_shortClassName(recipePathName))
   if entry and entry.get("displayName"):
      return entry["displayName"]
   label = readableLabel(recipePathName)
   for noisePrefix in ("Recipe, ", "Alternate, "):
      if label.startswith(noisePrefix):
         label = label[len(noisePrefix):]
   return label

# In-game MAM tab names for schematics.json's researchTree tokens (the
# BPD_ResearchTree_* assets carry the real display names but aren't in
# Docs.json). Tokens missing here fall back to the raw token. The single-node
# "HardDrive" tree is the internal machinery behind hard-drive scanning, not
# a tab the MAM UI shows -- excluded entirely.
_RESEARCH_TREE_LABELS = {
   "AlienOrganisms": "Alien Organisms",
   "AlienTech": "Alien Technology",
   "Caterium": "Caterium",
   "Mycelia": "Mycelia",
   "Nutrients": "Nutrients",
   "PowerSlugs": "Power Slugs",
   "Quartz": "Quartz",
   "Sulfur": "Sulfur",
   "XMas": "FICSMAS",
}
_HIDDEN_RESEARCH_TREES = {"HardDrive"}

SCHEMATIC_MANAGER_TYPE_PATH_SUBSTRING = "BP_SchematicManager_C"
RESEARCH_MANAGER_TYPE_PATH_SUBSTRING = "BP_ResearchManager_C"
GAME_PHASE_MANAGER_TYPE_PATH_SUBSTRING = "BP_GamePhaseManager_C"
SPACE_ELEVATOR_TYPE_PATH = "/Game/FactoryGame/Buildable/Factory/SpaceElevator/Build_SpaceElevator.Build_SpaceElevator_C"

# Space Elevator phase names/costs (static game data). Docs.json doesn't
# cover the GP_Project_Assembly_Phase_* assets, so the real source is
# game_data/generated/gamePhases.json, generated by game_data/extract_game_phases.py
# from the FModel JSON export of the game's own FGGamePhase assets --
#   {"GP_Project_Assembly_Phase_1": {"phaseNumber": 1, "displayName": ...,
#     "cost": [{"item": "Desc_SpaceElevatorPart_1_C", "amount": 50}]}, ...}
# This hand-written table (sourced from the wiki's 1.0 Project Assembly
# page, verified identical to the extracted assets) is only the fallback for
# when the generated file hasn't been produced yet. Costs are BASE amounts --
# the game-mode mSpacePartsCostMultiplier is applied at collect time.
_FALLBACK_GAME_PHASES = {
   "GP_Project_Assembly_Phase_1": {"phaseNumber": 1, "displayName": "Distribution Platform", "cost": [
      {"item": "Desc_SpaceElevatorPart_1_C", "amount": 50}]},
   "GP_Project_Assembly_Phase_2": {"phaseNumber": 2, "displayName": "Construction Dock", "cost": [
      {"item": "Desc_SpaceElevatorPart_1_C", "amount": 1000},
      {"item": "Desc_SpaceElevatorPart_2_C", "amount": 1000},
      {"item": "Desc_SpaceElevatorPart_3_C", "amount": 100}]},
   "GP_Project_Assembly_Phase_3": {"phaseNumber": 3, "displayName": "Main Body", "cost": [
      {"item": "Desc_SpaceElevatorPart_2_C", "amount": 2500},
      {"item": "Desc_SpaceElevatorPart_4_C", "amount": 500},
      {"item": "Desc_SpaceElevatorPart_5_C", "amount": 100}]},
   "GP_Project_Assembly_Phase_4": {"phaseNumber": 4, "displayName": "Propulsion", "cost": [
      {"item": "Desc_SpaceElevatorPart_7_C", "amount": 500},
      {"item": "Desc_SpaceElevatorPart_6_C", "amount": 500},
      {"item": "Desc_SpaceElevatorPart_8_C", "amount": 250},
      {"item": "Desc_SpaceElevatorPart_9_C", "amount": 100}]},
   "GP_Project_Assembly_Phase_5": {"phaseNumber": 5, "displayName": "Assembly", "cost": [
      {"item": "Desc_SpaceElevatorPart_9_C", "amount": 1000},
      {"item": "Desc_SpaceElevatorPart_10_C", "amount": 1000},
      {"item": "Desc_SpaceElevatorPart_12_C", "amount": 256},
      {"item": "Desc_SpaceElevatorPart_11_C", "amount": 200}]},
}

def _loadGamePhases() -> dict:
   generated = _loadJsonFile(os.path.join(_REPO_ROOT, "game_data", "generated", "gamePhases.json"))
   return generated or _FALLBACK_GAME_PHASES

_GAME_PHASES = _loadGamePhases()

# Cases the generic camel-case splitter below gets wrong.
_SHOP_CATEGORY_LABEL_OVERRIDES = {
   "SC_RSS_Equipment2_C": "Equipment",
   # The game's own joke name for its oddities tab -- keep it verbatim
   # instead of splitting it into "Massage-2 ABb".
   "SC_RSS_Massage-2ABb_C": "Massage-2ABb",
}

def _humanizeShopCategory(shortClassName: str) -> str:
   # "SC_RSS_FoundationMaterials_C" -> "Foundation Materials". The SC_RSS_*
   # category classes have no Docs.json entry, so their ClassName is the only
   # label source.
   override = _SHOP_CATEGORY_LABEL_OVERRIDES.get(shortClassName)
   if override is not None:
      return override
   name = shortClassName
   if name.startswith("SC_RSS_"):
      name = name[len("SC_RSS_"):]
   if name.endswith("_C"):
      name = name[:-2]
   return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", name)

def _firstRecipeProductItem(schematicEntry: dict):
   # The first unlocked recipe's first product (or, for shop products that
   # hand items over directly instead of unlocking a recipe, the first given
   # item) -- the closest thing a schematic has to a representative
   # item/building for an icon.
   for recipeClassName in schematicEntry.get("unlockRecipes", []):
      recipe = _RECIPES_JSON.get(recipeClassName)
      if recipe:
         for product in recipe.get("product", []):
            return product.get("item")
   for givenItem in schematicEntry.get("giveItems", []):
      return givenItem.get("item")
   return None

def _findObjectByTypePathSubstring(levels, substring: str):
   for level in levels:
      for object in level.objects:
         if substring in object.instanceName:
            return object
   # Some managers' instanceName doesn't contain the class token (e.g.
   # "...PersistentLevel.SchematicManager"), so fall back to matching the
   # ActorHeader's typePath and resolving that header's instanceName.
   instanceName = None
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and substring in header.typePath:
            instanceName = header.instanceName
            break
      if instanceName is not None:
         break
   if instanceName is None:
      return None
   for level in levels:
      for object in level.objects:
         if object.instanceName == instanceName:
            return object
   return None

def _shortNamesFromObjectReferenceList(references) -> set:
   shortNames = set()
   for reference in references or []:
      pathName = getattr(reference, "pathName", None)
      if pathName:
         shortNames.add(pathName.rsplit(".", 1)[-1])
   return shortNames

def _labeledCost(costEntries) -> list:
   return [{"item": cost["item"], "label": readableLabel(cost["item"]), "amount": cost["amount"]}
           for cost in costEntries or []]

def collectProgression(levels) -> dict:
   # One purchased-schematics pass feeding the four schematic-driven panels
   # (MAM, alternate recipes, AWESOME Shop, HUB milestones), plus the Space
   # Elevator/game-phase state. Every panel lists ALL known entries with a
   # per-entry "done" flag rather than only the unlocked ones -- seeing
   # what's still missing is half the point of a progression view.
   schematicManager = _findObjectByTypePathSubstring(levels, SCHEMATIC_MANAGER_TYPE_PATH_SUBSTRING)
   purchased = set()
   if schematicManager is not None:
      purchased = _shortNamesFromObjectReferenceList(
         sav_parse.getPropertyValue(schematicManager.properties, "mPurchasedSchematics"))

   researchManager = _findObjectByTypePathSubstring(levels, RESEARCH_MANAGER_TYPE_PATH_SUBSTRING)
   unlockedTrees = set()
   if researchManager is not None:
      unlockedTrees = {name.replace("BPD_ResearchTree_", "").replace("_C", "") for name in
                       _shortNamesFromObjectReferenceList(
                          sav_parse.getPropertyValue(researchManager.properties, "mUnlockedResearchTrees"))}

   # -- MAM research, grouped by tree ----------------------------------------
   nodesByTree: dict[str, list] = {}
   # -- Alternate recipes -----------------------------------------------------
   alternateRecipes = []
   # -- AWESOME Shop, grouped by shop tab ------------------------------------
   shopByCategory: dict[str, list] = {}
   couponsSpent = 0
   # -- HUB milestones, grouped by tier (tier 0 = the initial HUB upgrades) --
   milestonesByTier: dict[int, list] = {}

   for (className, entry) in _SCHEMATICS_JSON.items():
      schematicType = entry.get("type")
      done = className in purchased
      # A handful of legacy research nodes linger in Docs.json with a literal
      # "Discontinued - " display name; the game itself never shows them.
      if (entry.get("displayName") or "").startswith("Discontinued"):
         continue
      if schematicType == "MAM":
         tree = entry.get("researchTree")
         if tree in _HIDDEN_RESEARCH_TREES:
            continue
         nodesByTree.setdefault(tree, []).append({
            "className": className, "label": entry.get("displayName"), "done": done,
            "cost": _labeledCost(entry.get("cost")),
            "menuPriority": entry.get("menuPriority", 0.0),
         })
      elif schematicType == "Alternate":
         # Only hard-drive alternates that actually unlock a recipe -- the two
         # EST_Alternate inventory-slot upgrades aren't recipes.
         productItem = _firstRecipeProductItem(entry)
         if not entry.get("unlockRecipes"):
            continue
         alternateRecipes.append({
            "className": className, "label": entry.get("displayName"), "done": done,
            "techTier": entry.get("techTier", 0), "productItem": productItem,
         })
      elif schematicType == "ResourceSink":
         # Repeatable item bundles (ammo packs, biomass, raw-part bundles --
         # anything whose only "unlock" is handing items over) can be bought
         # again and again and are never recorded in mPurchasedSchematics, so
         # they'd sit at "locked" forever in a progression view. Only actual
         # one-time unlocks belong here.
         if entry.get("giveItems") and not entry.get("unlockRecipes"):
            continue
         couponCost = 0
         for cost in entry.get("cost", []):
            if cost.get("item") == "Desc_ResourceSinkCoupon_C":
               couponCost = int(cost.get("amount", 0))
         if done:
            couponsSpent += couponCost
         categories = entry.get("shopCategories") or [None]
         categoryLabel = _humanizeShopCategory(categories[0]) if categories[0] else "Other"
         shopByCategory.setdefault(categoryLabel, []).append({
            "className": className, "label": entry.get("displayName"), "done": done,
            "couponCost": couponCost, "productItem": _firstRecipeProductItem(entry),
            "menuPriority": entry.get("menuPriority", 0.0),
         })
      elif schematicType in ("Milestone", "Tutorial"):
         tier = entry.get("techTier", 0)
         milestonesByTier.setdefault(tier, []).append({
            "className": className, "label": entry.get("displayName"), "done": done,
            "cost": _labeledCost(entry.get("cost")),
            "menuPriority": entry.get("menuPriority", 0.0),
         })

   mamTrees = []
   for tree in sorted(nodesByTree, key=lambda name: _RESEARCH_TREE_LABELS.get(name, name)):
      nodes = nodesByTree[tree]
      nodes.sort(key=lambda node: (node.pop("menuPriority"), node["label"]))
      mamTrees.append({
         "tree": tree,
         "label": _RESEARCH_TREE_LABELS.get(tree, tree),
         "treeUnlocked": tree in unlockedTrees,
         "doneCount": sum(1 for node in nodes if node["done"]),
         "nodes": nodes,
      })

   alternateRecipes.sort(key=lambda recipe: (recipe["techTier"], recipe["label"]))

   shopCategories = []
   for categoryLabel in sorted(shopByCategory):
      entries = shopByCategory[categoryLabel]
      entries.sort(key=lambda item: (item.pop("menuPriority"), item["label"]))
      shopCategories.append({
         "label": categoryLabel,
         "doneCount": sum(1 for item in entries if item["done"]),
         "entries": entries,
      })

   hubTiers = []
   for tier in sorted(milestonesByTier):
      milestones = milestonesByTier[tier]
      milestones.sort(key=lambda milestone: (milestone.pop("menuPriority"), milestone["label"]))
      hubTiers.append({
         "tier": tier,
         "label": "HUB Upgrades" if tier == 0 else f"Tier {tier}",
         "doneCount": sum(1 for milestone in milestones if milestone["done"]),
         "milestones": milestones,
      })

   return {
      "mamTrees": mamTrees,
      "alternateRecipes": alternateRecipes,
      "shopCategories": shopCategories,
      "couponsSpent": couponsSpent,
      "hubTiers": hubTiers,
      "spaceElevator": _collectSpaceElevatorState(levels),
   }

def _phaseInfo(phaseReference) -> dict:
   pathName = getattr(phaseReference, "pathName", None)
   if not pathName:
      return None
   assetName = pathName.rsplit(".", 1)[-1]
   entry = _GAME_PHASES.get(assetName)
   if entry is not None:
      return {"assetName": assetName, "phaseNumber": entry.get("phaseNumber"),
              "name": entry.get("displayName"), "cost": entry.get("cost", [])}
   numberMatch = re.search(r"Phase_(\d+)$", assetName)
   return {"assetName": assetName,
           "phaseNumber": int(numberMatch.group(1)) if numberMatch else None,
           "name": None, "cost": []}

def _collectSpaceElevatorState(levels) -> dict:
   spaceElevatorBuilt = False
   for level in levels:
      for header in level.actorAndComponentObjectHeaders:
         if isinstance(header, sav_parse.ActorHeader) and header.typePath == SPACE_ELEVATOR_TYPE_PATH:
            spaceElevatorBuilt = True

   phaseManager = _findObjectByTypePathSubstring(levels, GAME_PHASE_MANAGER_TYPE_PATH_SUBSTRING)
   if phaseManager is None:
      return {"built": spaceElevatorBuilt, "gameCompleted": False, "currentPhase": None,
              "targetPhase": None, "costMultiplier": 1.0, "targetCost": []}

   properties = phaseManager.properties
   currentPhase = _phaseInfo(sav_parse.getPropertyValue(properties, "mCurrentGamePhase"))
   targetPhase = _phaseInfo(sav_parse.getPropertyValue(properties, "mTargetGamePhase"))
   gameCompleted = bool(sav_parse.getPropertyValue(properties, "mIsGameCompleted"))

   # {itemShortName: amount already delivered toward the TARGET phase}. Items
   # not yet delivered at all simply don't appear in the list.
   paidOffByItem: dict[str, int] = {}
   paidOffCosts = sav_parse.getPropertyValue(properties, "mTargetGamePhasePaidOffCosts") or []
   for paidOffEntry in paidOffCosts:
      itemClass = sav_parse.getPropertyValue(paidOffEntry[0], "ItemClass")
      amount = sav_parse.getPropertyValue(paidOffEntry[0], "Amount")
      pathName = getattr(itemClass, "pathName", None)
      if pathName and amount is not None:
         paidOffByItem[pathName.rsplit(".", 1)[-1]] = amount

   # The "game mode" world-creation setting scaling Space Elevator phase
   # costs -- lives on BP_GameState_C next to the settings collectGameSettings
   # reads; absent (like every game-mode property) when left at its 1.0 default.
   costMultiplier = 1.0
   for level in levels:
      for object in level.objects:
         if GAME_STATE_TYPE_PATH_SUBSTRING in object.instanceName:
            multiplier = sav_parse.getPropertyValue(object.properties, "mSpacePartsCostMultiplier")
            if multiplier is not None:
               costMultiplier = multiplier

   # One row per required part of the target phase (base cost x multiplier),
   # overlaid with delivered amounts; delivered items the static table doesn't
   # know about (custom phase data, table gaps) still get their own row.
   targetCost = []
   knownItems = set()
   for cost in (targetPhase or {}).get("cost", []):
      itemShortName = cost.get("item")
      knownItems.add(itemShortName)
      targetCost.append({
         "item": itemShortName, "label": readableLabel(itemShortName),
         "required": round(cost.get("amount", 0) * costMultiplier),
         "imported": paidOffByItem.get(itemShortName, 0),
      })
   for (itemShortName, amount) in paidOffByItem.items():
      if itemShortName not in knownItems:
         targetCost.append({"item": itemShortName, "label": readableLabel(itemShortName),
                            "required": None, "imported": amount})
   if targetPhase is not None:
      targetPhase = {key: value for (key, value) in targetPhase.items() if key != "cost"}
   if currentPhase is not None:
      currentPhase = {key: value for (key, value) in currentPhase.items() if key != "cost"}

   return {
      "built": spaceElevatorBuilt,
      "gameCompleted": gameCompleted,
      "currentPhase": currentPhase,
      "targetPhase": targetPhase,
      "costMultiplier": costMultiplier,
      "targetCost": targetCost,
   }

def collectHardDrives(levels) -> dict:
   (_, notOpened, openWithDrive, openAndEmpty, dismantled) = sav_to_html.getCrashSiteState(levels)

   def bucketFor(instanceNames):
      points = []
      ids = []
      worldPositions = []
      # What a crash site demands before it'll hand over its hard drive --
      # either an item stack (sav_data.crashSites.CRASH_SITES' "cost" entry,
      # e.g. ("Steel Beam", 130)) or a power hookup ("power" entry, in MW);
      # the two never appear on the same site. None for crash sites with
      # neither. Parallels points/ids/worldPositions (same skip, same order)
      # so index i's requirement always matches index i's position everywhere else.
      requirements = []
      for instanceName in instanceNames:
         if instanceName in sav_data.crashSites.CRASH_SITES:
            crashSite = sav_data.crashSites.CRASH_SITES[instanceName]
            position = crashSite[2]
            (px, py) = projectXY(position)
            points.append(px)
            points.append(py)
            points.append(worldZToMeters(position[2]))
            ids.append(instanceName)
            # Raw world-space X/Y -- see _splitCollectableKind's comment;
            # same reasoning applies here once a hard drive is dismantled.
            worldPositions.append(position[0])
            worldPositions.append(position[1])
            info = crashSite[3]
            cost = info.get("cost")
            power = info.get("power")
            if cost:
               requirement = {"type": "cost", "item": cost[0], "quantity": cost[1]}
            elif power is not None:
               requirement = {"type": "power", "watts": power}
            else:
               requirement = None
            requirements.append(requirement)
      return (points, ids, worldPositions, requirements)

   # "Not yet opened" and "opened but the drive's still sitting there" are
   # both just "there's a drive here to go get" from a map-reading point of
   # view, so they're merged into one "hasDrive" bucket rather than kept as
   # separate categories -- only whether the drive is still gettable, and
   # what it costs to open, actually matters here.
   (hasDrivePoints, hasDriveIds, hasDriveWorldPositions, hasDriveRequirements) = bucketFor(notOpened + openWithDrive)
   (emptyPoints, emptyIds, emptyWorldPositions, emptyRequirements) = bucketFor(openAndEmpty)
   (dismantledPoints, dismantledIds, dismantledWorldPositions, dismantledRequirements) = bucketFor(dismantled)
   return {
      "hasDrive": hasDrivePoints, "hasDriveIds": hasDriveIds, "hasDriveWorldPositions": hasDriveWorldPositions, "hasDriveRequirements": hasDriveRequirements,
      "empty": emptyPoints, "emptyIds": emptyIds, "emptyWorldPositions": emptyWorldPositions, "emptyRequirements": emptyRequirements,
      "dismantled": dismantledPoints, "dismantledIds": dismantledIds, "dismantledWorldPositions": dismantledWorldPositions, "dismantledRequirements": dismantledRequirements,
   }

# Which real game item each collectable kind actually is once picked up --
# used only to fold "still left to collect" pickups into the item-search
# index below (collectCollectables/the map itself key these by "kind", e.g.
# "slugsBlue", not by item shortName).
COLLECTABLE_ITEM_SHORT_NAMES = {
   "slugsBlue": "Desc_Crystal_C",
   "slugsYellow": "Desc_Crystal_mk2_C",
   "slugsPurple": "Desc_Crystal_mk3_C",
   "somersloops": "Desc_WAT1_C",
   "mercerSpheres": "Desc_WAT2_C",
}
HARD_DRIVE_ITEM_SHORT_NAME = "Desc_HardDrive_C"

def _collectStaticItemLocations(levels) -> dict:
   # Power Slugs/Somersloops/Mercer Spheres/Hard Drives are static world
   # pickups, not held in any building's inventory -- and unlike a normal
   # building, they don't have a real ActorHeader to resolve a position from
   # either (their position data comes from sav_data.slug/somersloop/
   # mercerSphere/crashSites' hardcoded catalogs, the same source
   # collectCollectables/collectHardDrives already use to plot them on the
   # map), so they need their own position carried alongside the item-search
   # index here rather than resolving through saveIndex["headers"] the way
   # findItemLocations does for ordinary buildings. Only still-uncollected
   # pickups / still-present hard drives count -- a collected pickup or an
   # emptied/dismantled crash site no longer actually holds the item.
   # Returns itemShortName -> [{"instanceName":, "typePath":, "label":,
   # "count": 1, "position":, "worldPosition":}, ...].
   index: dict[str, list] = {}

   def addEntries(itemShortName, label, ids, points, worldPositions):
      for i in range(len(ids)):
         index.setdefault(itemShortName, []).append({
            "instanceName": ids[i],
            "typePath": None,
            "label": label,
            "count": 1,
            "position": [points[i * 3], points[i * 3 + 1], points[i * 3 + 2]],
            "worldPosition": [worldPositions[i * 2], worldPositions[i * 2 + 1]],
         })

   collectables = collectCollectables(levels)
   collectableLabels = {
      "slugsBlue": "Blue Power Slug", "slugsYellow": "Yellow Power Slug", "slugsPurple": "Purple Power Slug",
      "somersloops": "Somersloop", "mercerSpheres": "Mercer Sphere",
   }
   for (kind, itemShortName) in COLLECTABLE_ITEM_SHORT_NAMES.items():
      data = collectables[kind]
      addEntries(itemShortName, collectableLabels[kind], data["remainingIds"], data["remaining"], data["remainingWorldPositions"])

   hardDrives = collectHardDrives(levels)
   addEntries(HARD_DRIVE_ITEM_SHORT_NAME, "Hard Drive", hardDrives["hasDriveIds"], hardDrives["hasDrive"], hardDrives["hasDriveWorldPositions"])

   # World-spawned free item stacks in not-yet-generated map areas (see
   # _uncollectedCatalogDrops) -- real in-game, but with no actor in the save
   # for _collectItemLocationIndex's mPickupItems scan to find. Live actors
   # are that scan's job, so this only covers the catalog-only remainder --
   # no double counting. Same label findItemLocations gives live drops.
   for (shortName, quantity, position, instanceName) in _uncollectedCatalogDrops(levels):
      (px, py) = projectXY(position)
      index.setdefault(shortName, []).append({
         "instanceName": instanceName,
         "typePath": None,
         "label": "Dropped on the ground",
         "count": quantity,
         "position": [px, py, worldZToMeters(position[2])],
         "worldPosition": [position[0], position[1]],
      })

   return index

def _textPropertyValue(value):
   # Mirrors sav_parse.parseTextProperty()'s output shapes: [flags, historyType,
   # isTextCultureInvariant, s] for HistoryType.NONE, or [flags, historyType,
   # namespace, key, value] for HistoryType.BASE. Returns None for anything else.
   if isinstance(value, list) and len(value) == 4 and value[1] == 255:
      return value[3]
   if isinstance(value, list) and len(value) == 5 and value[1] == 0:
      return value[4]
   return None

# Every naming convention seen so far for the FGPipeConnectionComponent
# sub-objects that carry "mPipeNetworkID": plain pipe segments use
# ".PipelineConnection0"/"1", junctions/pumps/valves use ".Connection0".."3",
# and machine fluid ports use ".FGPipeConnectionFactory". Tried in order
# until one resolves (see buildSaveIndex / describeInstance /
# aggregateSelectionInventory).
PIPE_CONNECTOR_SUFFIXES = (
   ".PipelineConnection0", ".PipelineConnection1",
   ".Connection0", ".Connection1", ".Connection2", ".Connection3",
   ".FGPipeConnectionFactory",
)

def buildSaveIndex(parsedSave: sav_parse.ParsedSave) -> dict:
   # One-time O(n) pass so describeInstance() doesn't rescan the whole save
   # on every click. Cached by sav_map_server.py alongside the map payload.
   headersByInstanceName = {}
   objectsByInstanceName = {}
   instanceNamesByTypePath: dict[str, list] = {}
   for level in parsedSave.levels:
      for actorOrComponentObjectHeader in level.actorAndComponentObjectHeaders:
         headersByInstanceName[actorOrComponentObjectHeader.instanceName] = actorOrComponentObjectHeader
         if isinstance(actorOrComponentObjectHeader, sav_parse.ActorHeader):
            instanceNamesByTypePath.setdefault(actorOrComponentObjectHeader.typePath, []).append(
               actorOrComponentObjectHeader.instanceName)
      for object in level.objects:
         objectsByInstanceName[object.instanceName] = object

   # Lightweight buildables (foundations/walls/ramps/beams) have no real
   # ActorHeader of their own -- see _findLightweightBuildableGroups -- so the
   # loop above misses them entirely. Folding their synthetic
   # "LightweightBuildable:<typePath>:<idx>" ids in here too (collectBuildings
   # generates the same ones) means collectBuildingInfo's instance count for
   # e.g. a Foundation comes out right, even though every other per-instance
   # lookup it does (recipe/power/inventory) is a harmless no-op for these --
   # none of those concepts apply to a lightweight buildable anyway.
   for (typePath, instances) in _findLightweightBuildableGroups(parsedSave.levels):
      instanceNamesByTypePath.setdefault(typePath, []).extend(
         f"LightweightBuildable:{typePath}:{idx}" for idx in range(len(instances)))

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
   pipeNetworkIdToTotalFluid = {}
   pipeNetworkIdToMembers = {}
   for instanceName in headersByInstanceName:
      header = headersByInstanceName[instanceName]
      if getattr(header, "typePath", None) != "/Script/FactoryGame.FGPipeNetwork":
         continue
      networkActorObject = objectsByInstanceName.get(instanceName)
      if networkActorObject is None:
         continue
      # A network with no mFluidDescriptor (nothing has flowed through it
      # yet) still gets its member list indexed below -- the mark-mixing
      # bottleneck check (_pipeNetworkBottleneck) is about static capacity,
      # not current fluid, so it must work for empty networks too.
      fluidDescriptor = sav_parse.getPropertyValue(networkActorObject.properties, "mFluidDescriptor")
      fluidLabel = None
      if fluidDescriptor is not None and hasattr(fluidDescriptor, "pathName") and fluidDescriptor.pathName:
         fluidLabel = readableLabel(fluidDescriptor.pathName)
      members = sav_parse.getPropertyValue(networkActorObject.properties, "mFluidIntegrantScriptInterfaces") or []
      networkId = None
      totalFluid = 0.0
      memberNames = []
      for memberReference in members:
         if not hasattr(memberReference, "pathName") or not memberReference.pathName:
            continue
         memberNames.append(memberReference.pathName)
         # Everything in the network that holds fluid (pipes, pumps, valves,
         # junctions, tanks) carries its amount in its own mFluidBox (in m3),
         # so the network's total content is just the members' sum. The
         # member list is the authoritative source -- a brute-force scan of
         # all mFluidBox objects finds FEWER (some members' connector
         # sub-objects use other names), never more.
         memberObject = objectsByInstanceName.get(memberReference.pathName)
         if memberObject is not None:
            memberFluid = sav_parse.getPropertyValue(memberObject.properties, "mFluidBox")
            if memberFluid:
               totalFluid += memberFluid
         if networkId is not None:
            continue # All members share one ID -- resolving it once is enough.
         # Each reference points at the pipe/machine ACTOR itself, not its
         # connector sub-object -- mPipeNetworkID lives on the connector
         # (see PIPE_CONNECTOR_SUFFIXES).
         for connectorSuffix in PIPE_CONNECTOR_SUFFIXES:
            connectorObject = objectsByInstanceName.get(memberReference.pathName + connectorSuffix)
            if connectorObject is None:
               continue
            networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
            if networkId is not None:
               break
      if networkId is not None:
         pipeNetworkIdToMembers[networkId] = memberNames
         if fluidLabel is not None:
            pipeNetworkIdToFluid[networkId] = fluidLabel
            pipeNetworkIdToTotalFluid[networkId] = totalFluid

   # Lightweight buildables (see _findLightweightBuildableGroups) have no
   # real instanceName/Object of their own to look up at tooltip time -- this
   # indexes the synthetic "LightweightBuildable:<typePath>:<idx>" ids
   # collectBuildings() already generated against the same instance data.
   lightweightInstancesById = {}
   for (typePath, instances) in _findLightweightBuildableGroups(parsedSave.levels):
      for (idx, instance) in enumerate(instances):
         lightweightInstancesById[f"LightweightBuildable:{typePath}:{idx}"] = {"typePath": typePath, "position": instance[1]}

   # Train consists (BP_Train_C -> its locomotives/freight cars in physical
   # order), for describeInstance's whole-train tooltip. Orphan single-car
   # entries (see _trainConsistsFromMaps) are skipped: their id is the car's
   # own instanceName, which describeInstance's ordinary per-actor path
   # already describes in more detail than the train branch would.
   trainInfoByInstanceName = {}
   for train in _trainConsistsFromMaps(headersByInstanceName, objectsByInstanceName):
      trainHeader = headersByInstanceName.get(train["id"])
      if getattr(trainHeader, "typePath", None) == TRAIN_TYPE_PATH:
         trainInfoByInstanceName[train["id"]] = train

   return {
      "headers": headersByInstanceName,
      "objects": objectsByInstanceName,
      "trainInfoByInstanceName": trainInfoByInstanceName,
      "stationNameByStationInstance": stationNameByStationInstance,
      "pipeNetworkIdToFluid": pipeNetworkIdToFluid,
      "pipeNetworkIdToTotalFluid": pipeNetworkIdToTotalFluid,
      "pipeNetworkIdToMembers": pipeNetworkIdToMembers,
      "lightweightInstancesById": lightweightInstancesById,
      "instanceNamesByTypePath": instanceNamesByTypePath,
      "itemLocationIndex": _collectItemLocationIndex(objectsByInstanceName),
      "dimensionalDepotByItem": {
         entry["itemPath"]: entry["count"] for entry in collectDimensionalDepotContents(parsedSave.levels)
      },
      "staticItemLocations": _collectStaticItemLocations(parsedSave.levels),
   }

def _resolveComponentObject(saveIndex, properties, propertyName):
   reference = sav_parse.getPropertyValue(properties, propertyName)
   if reference is not None and hasattr(reference, "pathName"):
      return saveIndex["objects"].get(reference.pathName)
   return None

def _conveyorChainSegmentItemPaths(chainActorInfo, beltInstanceName) -> list:
   # A chain's serialized items (actorSpecificInfo[2], see sav_parse.py's
   # FGConveyorChainActor branch) are a ring-buffer window: serialized index j
   # holds the item at ring slot (chainLeadItemIndex + j) % maximumItems, and
   # the whole window runs lead-to-tail inclusive. Each belt in the chain
   # carries its own lead/tail ring indices (chainBelts[i][5]/[6]) whose
   # ranges exactly partition that window (verified across all 127 chains of
   # a real save: per-belt counts always sum to len(chainItems) with no
   # gaps/overlaps) -- so this segment's own items are the contiguous slice
   # below. A lead/tail of -1 means the segment holds nothing.
   chainBelts = chainActorInfo[1]
   chainItems = chainActorInfo[2]
   maximumItems = chainActorInfo[4]
   chainLeadItemIndex = chainActorInfo[5]
   if not chainItems or maximumItems <= 0 or chainLeadItemIndex < 0:
      return []
   for chainBelt in chainBelts:
      if getattr(chainBelt[0], "pathName", None) != beltInstanceName:
         continue
      (beltLeadItemIndex, beltTailItemIndex) = (chainBelt[5], chainBelt[6])
      if beltLeadItemIndex < 0 or beltTailItemIndex < 0:
         return []
      start = (beltLeadItemIndex - chainLeadItemIndex) % maximumItems
      count = (beltTailItemIndex - beltLeadItemIndex) % maximumItems + 1
      return [chainEntry[0] for chainEntry in chainItems[start:start + count]]
   return []

# Rated belt/lift throughput in items/min per mark. Static game data that
# isn't serialized in the save (same reasoning as RATED_POWER_MW below);
# sourced from the buildables' own descriptions in game_data/generated/
# buildings.json ("Transports up to N resources per minute").
_CONVEYOR_MARK_ITEMS_PER_MINUTE = {1: 60, 2: 120, 3: 270, 4: 480, 5: 780, 6: 1200}

def _conveyorItemsPerMinute(typePath):
   match = re.search(r"Conveyor(?:Belt|Lift)Mk(\d+)", typePath or "")
   return _CONVEYOR_MARK_ITEMS_PER_MINUTE.get(int(match.group(1))) if match else None

# Rated flow (m³/min) per pipe-network member class. Pipes are stated in
# their own descriptions ("Capacity: N m³ of fluid per minute"); the pumps'
# only mention head lift, but in-game a pump also caps flow at its mark's
# rate -- a Mk.1 pump on a Mk.2 pipeline is exactly the pipe counterpart of
# the forgotten low-mark lift in a belt line (wiki-sourced, same spirit as
# RATED_POWER_MW). Junctions/supports/machines have no rated flow of their
# own and Valves are user-configured, so none of them appear here.
_PIPE_FLOW_M3_PER_MINUTE_BY_CLASS = {
   "Build_Pipeline_C": 300,
   "Build_Pipeline_NoIndicator_C": 300,
   "Build_PipelineMK2_C": 600,
   "Build_PipelineMK2_NoIndicator_C": 600,
   "Build_PipelinePump_C": 300,
   "Build_PipelinePumpMk2_C": 600,
}

def _pipeFlowLimitPerMinute(typePath):
   return _PIPE_FLOW_M3_PER_MINUTE_BY_CLASS.get(_shortClassName(typePath) or "")

# Payload cap on the limiting-segment list below: a line/network can span
# hundreds of segments, and when roughly half of it is the slower mark this
# isn't a "one forgotten belt" bug worth plotting exhaustively anyway. The
# full count is still reported separately.
_BOTTLENECK_SEGMENT_LIMIT = 50

def _flowBottleneck(saveIndex, ratedInstanceNames, hoveredTypePath, rateOfTypePath, scope, unit) -> dict | None:
   # Shared mixed-mark detection for anything that moves at the speed of its
   # SLOWEST member: conveyor chains (belts + lifts) and pipe networks
   # (pipes + pumps). ratedInstanceNames names every member of the connected
   # line/network; rateOfTypePath maps a member's typePath to its rated
   # throughput (None = unrated member, e.g. a junction or a modded type --
   # skipped rather than ranked). If members' rates differ, the slowest ones
   # are exactly what holds the whole thing back, reported with positions so
   # the frontend can point at them (see tooltip.js's bottleneckSection and
   # bottleneck.js).
   headers = saveIndex["headers"]
   rankedSegments = []
   for segmentInstanceName in ratedInstanceNames:
      header = headers.get(segmentInstanceName)
      rate = rateOfTypePath(getattr(header, "typePath", None))
      if rate is None:
         continue
      rankedSegments.append((rate, segmentInstanceName, header))
   if not rankedSegments:
      return None
   slowestRate = min(entry[0] for entry in rankedSegments)
   fastestRate = max(entry[0] for entry in rankedSegments)
   if slowestRate >= fastestRate:
      return None # Uniform marks -- nothing is holding anything back.
   limitingSegments = [entry for entry in rankedSegments if entry[0] == slowestRate]
   result = {
      "scope": scope, # "line" (conveyor chain) or "network" (pipes) -- drives the tooltip's wording.
      "unit": unit,
      "limitPerMinute": slowestRate,
      "fastestPerMinute": fastestRate,
      "limitingSegmentCount": len(limitingSegments),
      # position is map-pixel space + altitude in meters (for plotting the
      # warning markers), worldPosition the raw coordinates (for the marker
      # tooltip's Coordinates row) -- same split as findItemLocations.
      "limitingSegments": [
         {
            "instanceName": segmentInstanceName,
            "label": readableLabel(header.typePath),
            "position": projectXY(header.position) + [worldZToMeters(header.position[2])],
            "worldPosition": [header.position[0], header.position[1]],
         }
         for (rate, segmentInstanceName, header) in limitingSegments[:_BOTTLENECK_SEGMENT_LIMIT]
      ],
   }
   hoveredRate = rateOfTypePath(hoveredTypePath)
   if hoveredRate is not None:
      result["hoveredPerMinute"] = hoveredRate
      result["hoveredIsLimiting"] = hoveredRate == slowestRate
   return result

def _conveyorChainBottleneck(saveIndex, chainActorInfo, hoveredTypePath) -> dict | None:
   # A conveyor line moves items at the speed of its slowest member, and
   # both belts AND lifts are chain members (lifts especially are easy to
   # overlook since they render as small boxes, not lines). The chain actor's
   # member list (chainActorInfo[1], same shape as in
   # _conveyorChainSegmentItemPaths) already names every segment of the
   # connected line.
   memberNames = [
      getattr(chainBelt[0], "pathName", None) for chainBelt in chainActorInfo[1]
      if getattr(chainBelt[0], "pathName", None)
   ]
   return _flowBottleneck(saveIndex, memberNames, hoveredTypePath, _conveyorItemsPerMinute, "line", "items/min")

def _pipeNetworkBottleneck(saveIndex, memberNames, hoveredTypePath) -> dict | None:
   # Pipe counterpart of _conveyorChainBottleneck, ranking the network's
   # members (see buildSaveIndex's pipeNetworkIdToMembers) by rated flow.
   # One caveat a belt chain doesn't have: a network is a GRAPH, not a line
   # -- a deliberately thin Mk.1 branch off a Mk.2 trunk also reads as
   # "mixed marks" here. Still worth flagging: the markers point at the
   # exact slow segments, so judging intent takes one look.
   return _flowBottleneck(saveIndex, memberNames, hoveredTypePath, _pipeFlowLimitPerMinute, "network", "m³/min")

# Rated power consumption in MW at 100% clock speed, straight from the game's
# own Docs.json via game_data/generated/buildings.json and recipes.json (see
# game_data/SCHEMA.md). The save itself only stores a *live* power draw
# (FGPowerInfoComponent.mTargetConsumption, see describeInstance below) which
# ramps down to 0.1MW whenever a machine is idle or output-blocked, so it
# can't answer "how much power does this use when actually running" -- that
# rated figure is static per building+recipe and only exists in the game data.
# Two layers, recipe first:
#   - recipes.json's variablePowerRangeMW: variable-power recipes (every
#     Particle Accelerator/Converter/Quantum Encoder recipe -- only those
#     machines apply recipe-driven power, see the extractor's
#     stripVariablePowerFromConstantPowerMachines) drive the machine's draw
#     themselves, overriding its base figure.
#   - buildings.json's powerConsumptionMW (steady draw) or
#     powerConsumptionRangeMW (the game's own min/max estimate across recipes,
#     for the three machines whose draw oscillates over the production cycle
#     -- used as the fallback when no recipe is set).
def _loadRatedPowerMWByClassName() -> dict:
   ratings = {}
   for (className, entry) in _RAW_BUILDINGS_JSON.items():
      powerRange = entry.get("powerConsumptionRangeMW")
      if powerRange:
         ratings[className] = (powerRange[0], powerRange[1])
      elif entry.get("powerConsumptionMW"):
         ratings[className] = entry["powerConsumptionMW"]
   return ratings

_RATED_POWER_MW_BY_CLASSNAME = _loadRatedPowerMWByClassName()

_VARIABLE_POWER_RANGE_MW_BY_RECIPE = {
   recipeClassName: (entry["variablePowerRangeMW"][0], entry["variablePowerRangeMW"][1])
   for (recipeClassName, entry) in _RECIPES_JSON.items()
   if entry.get("variablePowerRangeMW")
}

# Power consumption does NOT scale linearly with clock speed -- this exponent
# (changed from 1.6 to ~1.32 in patch 0.7) is Docs.json's own per-building
# mPowerConsumptionExponent, kept as one constant rather than extracted per
# building: it's 1.321929 on every overclockable consumer, and the buildings
# still carrying the legacy 1.6 (or nothing) are all non-overclockable
# (stations/lights/jump pads/...), so their clock never leaves 100% and the
# exponent never applies. Confirmed against the wiki's stated examples
# (50% clock -> 40% power, 200% clock -> 250% power).
POWER_CLOCK_SPEED_EXPONENT = 1.321929

def _ratedPowerForTypePath(typePath, recipePathName):
   if typePath is None:
      return None
   if recipePathName:
      recipeRange = _VARIABLE_POWER_RANGE_MW_BY_RECIPE.get(_shortClassName(recipePathName))
      if recipeRange is not None:
         return recipeRange
   return _RATED_POWER_MW_BY_CLASSNAME.get(_shortClassName(typePath))

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

# Every property name describeInstance resolves via _resolveComponentObject
# to reach an inventory's "mInventoryStacks" -- used generically here (unlike
# describeInstance, which checks each by name individually to label its
# per-field breakdown) since a global item search only cares about total
# quantity, not which specific inventory slot on a building holds it.
INVENTORY_PROPERTY_NAMES = (
   "mInventory", "mInputInventory", "mFuelInventory", "mOutputInventory",
   "mStorageInventory", "mBufferInventory", "mCouponInventory", "mShopInventory",
   "mInventoryPotential",
)

# Wheeled vehicles (Tractor/Truck/Fluid Truck/Explorer/Factory Cart) carry NO
# inventory reference property at all -- unlike drones and freight cars
# (mStorageInventory) or buildings (the names above), their cargo trunk and
# fuel slot exist only as child component objects tied to the actor by
# instance-name convention ("<vehicle>.StorageInventory" /
# "<vehicle>.FuelInventory" -- the same convention PIPE_CONNECTOR_SUFFIXES
# relies on). Without this fallback their contents were invisible to
# tooltips, item search, and selection totals alike.
VEHICLE_INVENTORY_COMPONENT_SUFFIXES = (".StorageInventory", ".FuelInventory")

def _inventoryComponentObjects(objectsByInstanceName: dict, instanceName: str, properties) -> list:
   # Every inventory component hanging off this instance: the ones its
   # reference properties point at, plus the name-convention vehicle ones --
   # deduped by pathName since a component can be BOTH referenced and
   # name-matching (a drone's mStorageInventory points at its own
   # ".StorageInventory" child).
   componentsByPathName = {}
   for propertyName in INVENTORY_PROPERTY_NAMES:
      reference = sav_parse.getPropertyValue(properties, propertyName)
      if reference is None or not hasattr(reference, "pathName"):
         continue
      componentObject = objectsByInstanceName.get(reference.pathName)
      if componentObject is not None:
         componentsByPathName[reference.pathName] = componentObject
   for componentSuffix in VEHICLE_INVENTORY_COMPONENT_SUFFIXES:
      pathName = instanceName + componentSuffix
      if pathName not in componentsByPathName:
         componentObject = objectsByInstanceName.get(pathName)
         if componentObject is not None:
            componentsByPathName[pathName] = componentObject
   return list(componentsByPathName.values())

def _collectItemLocationIndex(objectsByInstanceName: dict) -> dict:
   # One O(n) pass across every object in the save (not just known storage
   # buildings -- confirmed against a 762k-object save that checking all 9
   # INVENTORY_PROPERTY_NAMES on literally every object, then walking
   # matching inventories' stacks, takes ~2.6 seconds total), building a full
   # itemShortName -> [(instanceName, count), ...] index up front so a "find
   # item" search (see findItemLocations) becomes an O(1) dict lookup instead
   # of rescanning the whole save on every query.
   index: dict[str, list] = {}
   for (instanceName, object) in objectsByInstanceName.items():
      countByItem: dict[str, float] = {}
      for componentObject in _inventoryComponentObjects(objectsByInstanceName, instanceName, object.properties):
         stacks = sav_parse.getPropertyValue(componentObject.properties, "mInventoryStacks")
         if stacks is None:
            continue
         for stack in stacks:
            item = sav_parse.getPropertyValue(stack[0], "Item")
            numItems = sav_parse.getPropertyValue(stack[0], "NumItems")
            if not item or not numItems:
               continue
            itemPath = item[0] if isinstance(item, (list, tuple)) else item
            if itemPath:
               shortName = itemPath.rsplit(".", 1)[-1]
               countByItem[shortName] = countByItem.get(shortName, 0) + numItems
      # Items dropped loose on the ground (see collectDroppedItems) hold
      # theirs in an inline "mPickupItems" struct instead of a referenced
      # inventory component -- without this they'd be invisible to item search.
      pickupItems = sav_parse.getPropertyValue(object.properties, "mPickupItems")
      if pickupItems is not None:
         item = sav_parse.getPropertyValue(pickupItems[0], "Item")
         numItems = sav_parse.getPropertyValue(pickupItems[0], "NumItems")
         itemPath = item[0] if isinstance(item, (list, tuple)) else item
         if itemPath and numItems:
            shortName = itemPath.rsplit(".", 1)[-1]
            countByItem[shortName] = countByItem.get(shortName, 0) + numItems
      for (shortName, count) in countByItem.items():
         index.setdefault(shortName, []).append((instanceName, count))
   return index

def findItemLocations(saveIndex: dict, itemShortName: str) -> dict:
   entries = saveIndex.get("itemLocationIndex", {}).get(itemShortName, [])
   isFluid = _isFluidItemPath(itemShortName)
   scale = 1000.0 if isFluid else 1.0
   headers = saveIndex["headers"]

   locations = []
   totalCount = 0.0
   for (instanceName, count) in entries:
      totalCount += count
      header = headers.get(instanceName)
      if header is None:
         continue # Shouldn't happen (index is built from the same objects), but don't let one bad entry break the whole result.
      (px, py) = projectXY(header.position)
      typePath = getattr(header, "typePath", None)
      label = readableLabel(typePath) if typePath else instanceName
      if typePath == ITEM_PICKUP_TYPE_PATH:
         # readableLabel's generic fallback would render the engine class as
         # "FGItem Pickup, Spawnable"-style noise -- what matters to a search
         # result is that this stack is lying loose on the ground.
         label = "Dropped on the ground"
      if typePath == PLAYER_TYPE_PATH:
         # readableLabel's generic fallback renders this as the nonsensical
         # "Char, Player" (Char_Player_C isn't in the curated name table) --
         # same mCachedPlayerName lookup describeInstance uses for a player's
         # tooltip title.
         playerObject = saveIndex["objects"].get(instanceName)
         playerName = sav_parse.getPropertyValue(playerObject.properties, "mCachedPlayerName") if playerObject else None
         label = "Player: " + playerName if playerName else "Player"
      locations.append({
         "instanceName": instanceName,
         "typePath": typePath,
         "label": label,
         "count": round(count / scale, 1) if isFluid else count,
         # "position" is already map-pixel space (for plotting the point);
         # "worldPosition" is the raw, un-projected [x, y] (for the tooltip's
         # Coordinates row/copy button) -- same split as collectResourceNodes/
         # collectCollectables, since the two aren't interchangeable.
         "position": [px, py, worldZToMeters(header.position[2])],
         "worldPosition": [header.position[0], header.position[1]],
      })

   # The Dimensional Depot's contents aren't tied to any one building's
   # position (see collectDimensionalDepotContents) -- included as a
   # pseudo-location with no position/worldPosition, so the frontend lists
   # it in the sorted results alongside real buildings but skips it when
   # plotting map pins (there's nowhere on the map for it to point at).
   depotCount = saveIndex.get("dimensionalDepotByItem", {}).get(itemShortName)
   if depotCount:
      totalCount += depotCount
      locations.append({
         "instanceName": "dimensional-depot",
         "typePath": None,
         "label": "Dimensional Depot",
         "count": round(depotCount / scale, 1) if isFluid else depotCount,
         "position": None,
         "worldPosition": None,
      })

   # Power Slugs/Somersloops/Mercer Spheres still waiting to be picked up,
   # and Hard Drives still sitting in their crash site -- see
   # _collectStaticItemLocations. These already carry a real position (each
   # pickup is its own map location), unlike the Dimensional Depot above.
   for staticEntry in saveIndex.get("staticItemLocations", {}).get(itemShortName, []):
      totalCount += staticEntry["count"]
      locations.append(dict(staticEntry, count=round(staticEntry["count"] / scale, 1) if isFluid else staticEntry["count"]))

   locations.sort(key=lambda entry: entry["count"], reverse=True)

   return {
      "itemPath": itemShortName,
      "label": readableLabel(itemShortName),
      "isFluid": isFluid,
      "totalCount": round(totalCount / scale, 1) if isFluid else totalCount,
      "locations": locations,
   }

def listSearchableItems() -> list:
   # Every "Desc_*_C" entry in the game's own readable-name table is a real
   # inventory item (that prefix is Satisfactory's own convention for an
   # item descriptor class, as opposed to Build_/Char_/Recipe_/BP_...), so
   # this needs no separate curated list -- it's independent of any loaded
   # save (same catalog every time), used to populate the item search's
   # autocomplete list.
   items = [
      {"itemPath": shortName, "label": label}
      for (shortName, label) in sav_data.readableNames.READABLE_PATH_NAME_CORRECTIONS.items()
      if shortName.startswith("Desc_")
   ]
   items.sort(key=lambda entry: entry["label"])
   return items

def aggregateSelectionInventory(saveIndex: dict, instanceNames: list) -> list:
   # Sums everything held across the given selected instances (the
   # rectangle-selection "total inventory" -- see the frontend's
   # selection.js): building/player inventories, plus belt in-transit items
   # and pipe fluid. Instances with no object (lightweight foundations/walls,
   # resource nodes, etc.) or nothing stored simply contribute nothing.
   # Returns [{"item":, "label":, "count":, "isFluid":}, ...] sorted by count
   # descending. Solids are keyed/summed by short class name; fluids are
   # merged by readable label (so the same fluid from a tank inventory and a
   # pipe lands on one row) and all carry raw 1000x-m3 amounts until the
   # final /1000 (mFluidBox, natively m3, is scaled up on entry -- see below).
   objects = saveIndex["objects"]
   pipeNetworkIdToFluid = saveIndex.get("pipeNetworkIdToFluid", {})
   solidCountByShortName: dict[str, float] = {}
   fluidRawByLabel: dict[str, float] = {}

   def addItem(shortName, amount):
      if _isFluidItemPath(shortName):
         label = readableLabel(shortName)
         fluidRawByLabel[label] = fluidRawByLabel.get(label, 0) + amount
      else:
         solidCountByShortName[shortName] = solidCountByShortName.get(shortName, 0) + amount

   seenInstances = set() # A building can appear once per selection; guard against dupes in the id list.
   for instanceName in instanceNames:
      if instanceName in seenInstances:
         continue
      seenInstances.add(instanceName)
      object = objects.get(instanceName)
      if object is None:
         continue
      properties = object.properties

      # Building/player/vehicle inventories.
      for componentObject in _inventoryComponentObjects(objects, instanceName, properties):
         stacks = sav_parse.getPropertyValue(componentObject.properties, "mInventoryStacks")
         if stacks is None:
            continue
         for stack in stacks:
            item = sav_parse.getPropertyValue(stack[0], "Item")
            numItems = sav_parse.getPropertyValue(stack[0], "NumItems")
            if not item or not numItems:
               continue
            itemPath = item[0] if isinstance(item, (list, tuple)) else item
            if itemPath:
               addItem(itemPath.rsplit(".", 1)[-1], numItems)

      # Belt segments: in-transit items live on a shared FGConveyorChainActor,
      # but each chain belt records which slice of the chain's items sits on
      # it (see _conveyorChainSegmentItemPaths) -- so only the items
      # physically on THIS segment are counted. Selecting several segments of
      # one line sums exactly the selected stretch, never the whole line.
      chainReference = sav_parse.getPropertyValue(properties, "mConveyorChainActor")
      if chainReference is not None and getattr(chainReference, "pathName", None):
         chainActor = objects.get(chainReference.pathName)
         if chainActor is not None and getattr(chainActor, "actorSpecificInfo", None):
            for itemPath in _conveyorChainSegmentItemPaths(chainActor.actorSpecificInfo, instanceName):
               if itemPath:
                  addItem(itemPath.rsplit(".", 1)[-1], 1)

      # Pipe segments: current fluid amount is a per-segment mFluidBox float.
      # Unlike inventory-stack fluids (1000x-m3), mFluidBox is already in m3
      # -- an Industrial Fluid Buffer's box peaks at its 2400 m3 capacity, a
      # pipe segment's at its own few-m3 capacity -- so it's scaled up by
      # 1000 here to join fluidRawByLabel's 1000x-m3 convention. The fluid
      # type comes from the segment's pipe network (see buildSaveIndex's
      # pipeNetworkIdToFluid).
      fluidAmount = sav_parse.getPropertyValue(properties, "mFluidBox")
      if fluidAmount:
         for connectorSuffix in PIPE_CONNECTOR_SUFFIXES:
            connectorObject = objects.get(instanceName + connectorSuffix)
            if connectorObject is None:
               continue
            networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
            fluidLabel = pipeNetworkIdToFluid.get(networkId)
            if fluidLabel is not None:
               fluidRawByLabel[fluidLabel] = fluidRawByLabel.get(fluidLabel, 0) + fluidAmount * 1000
               break

   items = []
   for (shortName, count) in solidCountByShortName.items():
      items.append({"item": shortName, "label": readableLabel(shortName), "count": count, "isFluid": False})
   for (label, raw) in fluidRawByLabel.items():
      items.append({"item": label, "label": label, "count": round(raw / 1000, 1), "isFluid": True})
   items.sort(key=lambda entry: entry["count"], reverse=True)
   return items

def collectBuildingInfo(saveIndex: dict, typePaths: list) -> dict:
   # The save-wide counterpart to describeInstance: instead of one placed
   # instance, summarizes every instance of a building type -- or, for a
   # same-shape/different-material group the frontend merges into one search
   # entry (see filters.js's mergedMaterialLabel), every instance across all
   # of those typePaths combined. Reuses aggregateSelectionInventory for the
   # "shared inventory" (everything currently sitting in these buildings'
   # inventories, added up) and the same rated-power lookup describeInstance
   # uses for a single instance, just summed per instance here instead.
   instanceNamesByTypePath = saveIndex.get("instanceNamesByTypePath", {})
   objects = saveIndex["objects"]

   allInstanceNames = []
   recipeCounts: dict[str, int] = {}
   recipeOrder = []
   noRecipeCount = 0
   hasRecipeCapableInstance = False
   totalPowerMinMW = 0.0
   totalPowerMaxMW = 0.0
   hasPowerConsumer = False
   totalPowerProductionMW = 0.0
   hasPowerProducer = False

   for typePath in typePaths:
      isGenerator = "Generator" in typePath
      instanceNames = instanceNamesByTypePath.get(typePath, [])
      allInstanceNames.extend(instanceNames)
      for instanceName in instanceNames:
         object = objects.get(instanceName)
         if object is None:
            continue # Lightweight buildables (foundations/walls/...) -- no recipe/power/inventory concept for these.
         properties = object.properties

         recipe = sav_parse.getPropertyValue(properties, "mCurrentRecipe")
         recipePathName = recipe.pathName if recipe is not None and hasattr(recipe, "pathName") and recipe.pathName else None
         if recipePathName is not None:
            hasRecipeCapableInstance = True
            recipeName = recipeLabel(recipePathName)
            if recipeName not in recipeCounts:
               recipeCounts[recipeName] = 0
               recipeOrder.append(recipeName)
            recipeCounts[recipeName] += 1
         elif recipe is not None:
            # A recipe reference exists but couldn't be resolved to a name --
            # treat the same as "no recipe set" rather than dropping the instance.
            hasRecipeCapableInstance = True
            noRecipeCount += 1

         canOverclock = (
            recipe is not None or
            sav_parse.getPropertyValue(properties, "mExtractableResource") is not None or
            isGenerator
         )
         clockSpeedFraction = 1.0
         if canOverclock:
            clockSpeed = sav_parse.getPropertyValue(properties, "mCurrentPotential")
            clockSpeedFraction = clockSpeed if clockSpeed is not None else 1.0

         if isGenerator:
            powerComponent = _resolveComponentObject(saveIndex, properties, "mPowerInfo")
            if powerComponent is not None:
               production = sav_parse.getPropertyValue(powerComponent.properties, "mDynamicProductionCapacity")
               if production is None:
                  production = sav_parse.getPropertyValue(powerComponent.properties, "mBaseProduction")
               if production is not None:
                  hasPowerProducer = True
                  totalPowerProductionMW += production
         else:
            ratedPowerMW = _ratedPowerForTypePath(typePath, recipePathName)
            if ratedPowerMW is not None:
               scaled = _scaleRatedPowerForClockSpeed(ratedPowerMW, clockSpeedFraction)
               hasPowerConsumer = True
               if isinstance(scaled, tuple):
                  totalPowerMinMW += scaled[0]
                  totalPowerMaxMW += scaled[1]
               else:
                  totalPowerMinMW += scaled
                  totalPowerMaxMW += scaled

   result = {
      "count": len(allInstanceNames),
      "inventory": aggregateSelectionInventory(saveIndex, allInstanceNames),
   }
   if hasRecipeCapableInstance:
      recipeRows = [{"label": label, "count": recipeCounts[label]} for label in recipeOrder]
      if noRecipeCount:
         recipeRows.append({"label": "No recipe set", "count": noRecipeCount})
      recipeRows.sort(key=lambda row: row["count"], reverse=True)
      result["recipes"] = recipeRows
   if hasPowerConsumer:
      if round(totalPowerMinMW, 1) == round(totalPowerMaxMW, 1):
         result["powerConsumptionMW"] = round(totalPowerMinMW, 1)
      else:
         result["powerConsumptionRangeMW"] = [round(totalPowerMinMW, 1), round(totalPowerMaxMW, 1)]
   if hasPowerProducer:
      result["powerProductionMW"] = round(totalPowerProductionMW, 1)
   return result

def _sumInventoryComponentStacks(componentObjects: list) -> list:
   # Sums mInventoryStacks across an explicit list of inventory components
   # into the same row shape aggregateSelectionInventory produces (solids
   # merged by short class name, fluids by readable label at their 1000x
   # inventory-stack scale) so the frontend renders both identically.
   solidCountByShortName: dict[str, float] = {}
   fluidRawByLabel: dict[str, float] = {}
   for componentObject in componentObjects:
      stacks = sav_parse.getPropertyValue(componentObject.properties, "mInventoryStacks")
      if stacks is None:
         continue
      for stack in stacks:
         item = sav_parse.getPropertyValue(stack[0], "Item")
         numItems = sav_parse.getPropertyValue(stack[0], "NumItems")
         if not item or not numItems:
            continue
         itemPath = item[0] if isinstance(item, (list, tuple)) else item
         if not itemPath:
            continue
         shortName = itemPath.rsplit(".", 1)[-1]
         if _isFluidItemPath(shortName):
            label = readableLabel(shortName)
            fluidRawByLabel[label] = fluidRawByLabel.get(label, 0) + numItems
         else:
            solidCountByShortName[shortName] = solidCountByShortName.get(shortName, 0) + numItems
   items = [
      {"item": shortName, "label": readableLabel(shortName), "count": count, "isFluid": False}
      for (shortName, count) in solidCountByShortName.items()
   ]
   items.extend(
      {"item": label, "label": label, "count": round(raw / 1000, 1), "isFluid": True}
      for (label, raw) in fluidRawByLabel.items()
   )
   items.sort(key=lambda entry: entry["count"], reverse=True)
   return items

def _vehicleStorageComponent(objects: dict, instanceName: str, properties):
   # A vehicle's cargo trunk: drones reference it (mStorageInventory), wheeled
   # vehicles only have the name-linked child component.
   reference = sav_parse.getPropertyValue(properties, "mStorageInventory")
   if reference is not None and getattr(reference, "pathName", None):
      componentObject = objects.get(reference.pathName)
      if componentObject is not None:
         return componentObject
   return objects.get(instanceName + ".StorageInventory")

def collectVehicleInfo(saveIndex: dict, typePaths: list) -> dict:
   # The vehicle counterpart of collectBuildingInfo, for the search bar's
   # road-vehicle/drone entries: fleet size, everything sitting in their
   # cargo trunks summed, the fuel loaded across their fuel slots, and the
   # fleet-status counts the save can answer cheaply -- how many are
   # assigned to an autopilot route (wheeled), how many are currently docked
   # at a port (drones).
   instanceNamesByTypePath = saveIndex.get("instanceNamesByTypePath", {})
   objects = saveIndex["objects"]
   count = 0
   automatedCount = 0
   dockedCount = 0
   hasDockingState = False
   cargoComponents = []
   fuelComponents = []
   for typePath in typePaths:
      for instanceName in instanceNamesByTypePath.get(typePath, []):
         count += 1
         object = objects.get(instanceName)
         if object is None:
            continue
         properties = object.properties
         storageComponent = _vehicleStorageComponent(objects, instanceName, properties)
         if storageComponent is not None:
            cargoComponents.append(storageComponent)
         fuelComponent = objects.get(instanceName + ".FuelInventory")
         if fuelComponent is not None:
            fuelComponents.append(fuelComponent)
         # A wheeled vehicle put on a route keeps a live reference to the
         # path segment it's currently following; manually driven/parked
         # vehicles never carry it.
         if sav_parse.getPropertyValue(properties, "mCurrentVehiclePathSegment") is not None:
            automatedCount += 1
         # EDroneDockingState enum struct -- string-matched rather than
         # unpacking the parser's nested TextProperty-style struct layers.
         dockingState = sav_parse.getPropertyValue(properties, "mCurrentDockingState")
         if dockingState is not None:
            hasDockingState = True
            if "DS_DOCKED" in sav_parse.toString(dockingState):
               dockedCount += 1
   result = {
      "count": count,
      "inventory": _sumInventoryComponentStacks(cargoComponents),
   }
   fuelInventory = _sumInventoryComponentStacks(fuelComponents)
   if fuelInventory:
      result["fuelInventory"] = fuelInventory
   if automatedCount:
      result["automatedCount"] = automatedCount
   if hasDockingState:
      result["dockedCount"] = dockedCount
   return result

def collectTrainInfo(saveIndex: dict) -> dict:
   # The train counterpart of collectVehicleInfo, per assembled consist
   # rather than per actor (the search bar deliberately exposes one "Train"
   # entry, not Locomotive/Freight Car): train and car totals, the
   # locomotive/wagon split, how consists are composed ("1 loco + 4 wagons"
   # x3 -- includes stray uncoupled cars as single-car consists, same as the
   # map's Train pins), and every freight car's cargo summed.
   objects = saveIndex["objects"]
   trains = _trainConsistsFromMaps(saveIndex["headers"], objects)
   carCount = 0
   locomotiveCount = 0
   wagonCount = 0
   compositionCounts: dict[tuple, int] = {}
   cargoComponents = []
   for train in trains:
      locomotives = sum(1 for car in train["cars"] if car["typePath"] == LOCOMOTIVE_TYPE_PATH)
      wagons = len(train["cars"]) - locomotives
      carCount += len(train["cars"])
      locomotiveCount += locomotives
      wagonCount += wagons
      compositionCounts[(locomotives, wagons)] = compositionCounts.get((locomotives, wagons), 0) + 1
      for car in train["cars"]:
         if car["typePath"] != FREIGHT_WAGON_TYPE_PATH:
            continue
         carObject = objects.get(car["id"])
         if carObject is None:
            continue
         storageComponent = _vehicleStorageComponent(objects, car["id"], carObject.properties)
         if storageComponent is not None:
            cargoComponents.append(storageComponent)

   def compositionLabel(locomotives, wagons):
      parts = []
      if locomotives:
         parts.append(f"{locomotives} loco{'s' if locomotives != 1 else ''}")
      if wagons:
         parts.append(f"{wagons} wagon{'s' if wagons != 1 else ''}")
      return " + ".join(parts) if parts else "empty"

   consistBreakdown = [
      {"label": compositionLabel(locomotives, wagons), "count": compositionCounts[(locomotives, wagons)]}
      for (locomotives, wagons) in sorted(compositionCounts, key=lambda key: (key[0] + key[1], key[0]))
   ]
   return {
      "count": len(trains),
      "carCount": carCount,
      "locomotiveCount": locomotiveCount,
      "wagonCount": wagonCount,
      "consistBreakdown": consistBreakdown,
      "inventory": _sumInventoryComponentStacks(cargoComponents),
   }

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
      return {"instanceName": instanceName, "typePath": typePath, "label": readableLabel(typePath),
              "position": lightweightInfo["position"]}

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

   # A whole train consist (the pin the frontend draws once per train --
   # see collectTrains). The BP_Train_C actor itself sits at the world
   # origin holding nothing but links, so everything shown is aggregated
   # from its member cars: the consist composition in physical order, and
   # every freight car's cargo summed into one total.
   trainInfo = saveIndex.get("trainInfoByInstanceName", {}).get(instanceName)
   if trainInfo is not None:
      result["label"] = trainInfo["label"] or "Train"
      result["position"] = trainInfo["cars"][0]["position"] # The lead car's, not the consist actor's meaningless (0,0,0).
      result["trainCars"] = [
         {"kind": readableLabel(car["typePath"]), "instanceName": car["id"]}
         for car in trainInfo["cars"]
      ]
      totalByItem: dict[str, dict] = {}
      for car in trainInfo["cars"]:
         carObject = saveIndex["objects"].get(car["id"])
         if carObject is None:
            continue
         # Freight cars keep cargo in mStorageInventory; mInventory is tried
         # too in case the property name differs across game versions.
         carInventory = (
            _inventoryContents(_resolveComponentObject(saveIndex, carObject.properties, "mStorageInventory")) or
            _inventoryContents(_resolveComponentObject(saveIndex, carObject.properties, "mInventory"))
         )
         for entry in carInventory:
            merged = totalByItem.get(entry["item"])
            if merged is None:
               totalByItem[entry["item"]] = dict(entry)
            else:
               merged["count"] = round(merged["count"] + entry["count"], 1)
      if totalByItem:
         result["cargoInventory"] = sorted(totalByItem.values(), key=lambda entry: entry["count"], reverse=True)
      return result

   stationName = saveIndex.get("stationNameByStationInstance", {}).get(instanceName)
   if stationName:
      result["stationName"] = stationName

   object = saveIndex["objects"].get(instanceName)
   if object is None:
      return result
   properties = object.properties

   # mDisplayName is a TextProperty, exposed as [historyType, flags,
   # isCultureInvariant, actualString] -- the tamed Lizard Doggo's pet name,
   # if the player renamed it (untamed ones apparently aren't persisted in
   # the save at all, so this is only ever missing/empty in practice).
   if typePath == LIZARD_DOGGO_TYPE_PATH:
      displayName = sav_parse.getPropertyValue(properties, "mDisplayName")
      petName = displayName[-1] if isinstance(displayName, list) and displayName else None
      if petName:
         result["petName"] = petName

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
      result["recipe"] = recipeLabel(recipePathName)

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
   # float giving the current fluid amount in m3. The fluid *type* isn't on
   # the segment itself either, only its network ID (see buildSaveIndex's
   # pipeNetworkIdToFluid) -- resolved here via whichever connector
   # sub-object this instance happens to have. The same network ID also
   # gives the whole connected network's fluid total (the pipe counterpart
   # of a belt's itemsOnLine -- see pipeNetworkIdToTotalFluid).
   fluidContent = sav_parse.getPropertyValue(properties, "mFluidBox")
   if fluidContent is not None:
      result["fluidContent"] = round(fluidContent, 1)
      for connectorSuffix in PIPE_CONNECTOR_SUFFIXES:
         connectorObject = saveIndex["objects"].get(instanceName + connectorSuffix)
         if connectorObject is None:
            continue
         networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
         fluidLabel = saveIndex["pipeNetworkIdToFluid"].get(networkId)
         if fluidLabel is not None:
            result["fluidType"] = fluidLabel
            networkTotal = saveIndex.get("pipeNetworkIdToTotalFluid", {}).get(networkId)
            if networkTotal is not None:
               result["networkFluidContent"] = round(networkTotal, 1)
            break

   # Mixed-mark pipe network detection, for pipes and pumps only (the
   # members with a rated flow of their own) -- deliberately separate from
   # the mFluidBox block above, which only runs when the segment currently
   # holds fluid; an empty network is just as bottlenecked. See
   # _pipeNetworkBottleneck / the belt lineBottleneck below.
   if _pipeFlowLimitPerMinute(typePath) is not None:
      for connectorSuffix in PIPE_CONNECTOR_SUFFIXES:
         connectorObject = saveIndex["objects"].get(instanceName + connectorSuffix)
         if connectorObject is None:
            continue
         networkId = sav_parse.getPropertyValue(connectorObject.properties, "mPipeNetworkID")
         memberNames = saveIndex.get("pipeNetworkIdToMembers", {}).get(networkId) if networkId is not None else None
         if memberNames:
            networkBottleneck = _pipeNetworkBottleneck(saveIndex, memberNames, typePath)
            if networkBottleneck:
               result["lineBottleneck"] = networkBottleneck
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
   if not storageInventory:
      # Wheeled vehicles' cargo trunk: a name-linked child component with no
      # reference property (see VEHICLE_INVENTORY_COMPONENT_SUFFIXES).
      storageInventory = _inventoryContents(saveIndex["objects"].get(instanceName + ".StorageInventory"))
   if storageInventory:
      result["storageInventory"] = storageInventory

   # Wheeled vehicles' fuel slot, same name-linked convention. Gated on the
   # absence of mFuelInventory so a generator's fuel (already shown as part
   # of inputInventory above via that reference) isn't repeated here.
   if sav_parse.getPropertyValue(properties, "mFuelInventory") is None:
      fuelInventory = _inventoryContents(saveIndex["objects"].get(instanceName + ".FuelInventory"))
      if fuelInventory:
         result["fuelInventory"] = fuelInventory

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
   # referenced by this segment's "mConveyorChainActor" property. Each chain
   # belt records which slice of the chain's items sits on it (see
   # _conveyorChainSegmentItemPaths), so both granularities are reported:
   # itemsOnBelt = just this segment, itemsOnLine = the whole connected chain
   # (only emitted when the chain actually spans more than this one segment,
   # since otherwise it would just repeat itemsOnBelt).
   chainActor = _resolveComponentObject(saveIndex, properties, "mConveyorChainActor")
   if chainActor is not None and getattr(chainActor, "actorSpecificInfo", None):
      chainActorInfo = chainActor.actorSpecificInfo

      def countedItemList(itemPaths):
         countByItem: dict[str, int] = {}
         for itemPath in itemPaths:
            label = readableLabel(itemPath)
            countByItem[label] = countByItem.get(label, 0) + 1
         return [{"item": label, "count": countByItem[label]} for label in countByItem]

      segmentItems = countedItemList(_conveyorChainSegmentItemPaths(chainActorInfo, instanceName))
      if segmentItems:
         result["itemsOnBelt"] = segmentItems
      if len(chainActorInfo[1]) > 1:
         lineItems = countedItemList(chainEntry[0] for chainEntry in chainActorInfo[2])
         if lineItems:
            result["itemsOnLine"] = lineItems
            result["lineSegmentCount"] = len(chainActorInfo[1])
         # Mixed-mark line detection -- deliberately independent of whether
         # anything is currently in transit (an empty line is just as
         # bottlenecked, capacity is static). See _conveyorChainBottleneck.
         lineBottleneck = _conveyorChainBottleneck(saveIndex, chainActorInfo, typePath)
         if lineBottleneck:
            result["lineBottleneck"] = lineBottleneck

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

# The single canonical typePath used to place each whole-line kind in the
# build-menu tree (they render as one merged line bucket, not per-typePath, so
# they need one representative to look up their category/subcategory).
# Vehicle paths aren't here -- unlike power lines/railroads/hypertubes, the
# five tiers (Explorer/FactoryCart/Tractor/Truck/Universal) are genuinely
# distinct buildables with distinct names, so they're split per-tier via
# collectSplinePathGroups (like belts/pipes) instead of merged into one
# generically-labeled "Vehicle Path" bucket.
_LINE_KIND_TYPEPATH = {
   "powerLines": "/Game/FactoryGame/Buildable/Factory/PowerLine/Build_PowerLine.Build_PowerLine_C",
   "railroads": "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrack.Build_RailroadTrack_C",
   "hypertubes": "/Game/FactoryGame/Buildable/Factory/PipeHyper/Build_PipeHyper.Build_PipeHyper_C",
}

def _annotateLineKinds(lines: dict) -> dict:
   for (key, lineData) in lines.items():
      typePath = _LINE_KIND_TYPEPATH.get(key)
      lineData["category"] = categorizeTypePath(typePath) if typePath else OTHER_CATEGORY
      lineData["subcategory"] = categorizeSubcategory(None, typePath) if typePath else None
   return lines

def buildMapPayload(parsedSave: sav_parse.ParsedSave) -> dict:
   return {
      "mapSize": MAP_SIZE,
      "sessionName": parsedSave.saveFileInfo.sessionName,
      "saveDatetime": parsedSave.saveFileInfo.saveDatetime.strftime("%Y-%m-%d %H:%M:%S"),
      "buildingCategories": collectBuildings(parsedSave.levels),
      # Build-menu category/subcategory order for the frontend's filter tree.
      "menuOrder": BUILD_MENU_ORDER,
      "resourceNodes": collectResourceNodes(parsedSave.levels),
      "collectables": collectCollectables(parsedSave.levels),
      "hardDrives": collectHardDrives(parsedSave.levels),
      "players": collectPlayers(parsedSave.levels),
      "creatures": collectCreatures(parsedSave.levels),
      "vehicles": collectVehicles(parsedSave.levels),
      "trains": collectTrains(parsedSave.levels),
      "droppedItems": collectDroppedItems(parsedSave.levels),
      "hub": collectHub(parsedSave.levels),
      "gameSettings": collectGameSettings(parsedSave.levels),
      "itemCatalog": listSearchableItems(),
      "dimensionalDepot": collectDimensionalDepotContents(parsedSave.levels),
      # MAM/alternate-recipe/AWESOME-Shop/HUB-milestone/Space-Elevator
      # progression -- the top bar's progression buttons (see progression.js).
      "progression": collectProgression(parsedSave.levels),
      "lines": _annotateLineKinds({
         "powerLines": collectPowerLines(parsedSave.levels),
         "railroads": collectSplinePaths(parsedSave.levels, RAILROAD_SEGMENTS),
         "hypertubes": collectSplinePaths(parsedSave.levels, HYPERTUBE_SEGMENTS),
      }),
      # Belts, pipes, and vehicle paths are split per mark/tier (e.g. Belt
      # Mk.1..Mk.6, Truck/Explorer/Universal Vehicle Path) so each is
      # independently toggleable -- see the frontend's Conveyor Belts/Pipelines/
      # Vehicle Paths groups.
      "belts": collectSplinePathGroups(parsedSave.levels, CONVEYOR_BELT_ONLY_TYPE_PATHS),
      "pipes": collectSplinePathGroups(parsedSave.levels, PIPELINE_SEGMENTS),
      "vehiclePaths": collectSplinePathGroups(parsedSave.levels, VEHICLE_PATH_SEGMENTS, splinePropertyName="mSplinePoints"),
   }
