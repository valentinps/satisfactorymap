"""One-time/offline converter: dumps the static Python data tables from the
parser/ submodule (GreyHak's sat_sav_parse sav_data package) to JSON under
game_data/sav_data/, where the Rust payload builder embeds them. Unlike
game_data/generated/ (gitignored, regenerable from your game install), these
files are committed: they are world data curated upstream/by SCIM, and they
must outlive the parser/ submodule's removal.

Regeneration requires the parser/ git submodule, which is removed from the
repo once the client-side port is complete -- re-add it (or check out an old
commit) to regenerate after a game update:

    git submodule add https://github.com/GreyHak/sat_sav_parse parser
    py game_data/extract_sav_data_tables.py

Run with --check to verify the committed JSON matches the Python literals
without rewriting anything.

Shapes (tuples become JSON arrays, Purity enums become their names):
  resourcePurity.json     {pathName: [descClass, purityName, [x,y,z], coreName|null]}
  powerSlugs.json         {"blue"|"yellow"|"purple": {pathName: [x,y,z]}}
  somersloops.json        {pathName: [id, [qx,qy,qz,qw], [x,y,z], metadata]}
  mercerSpheres.json      same shape as somersloops.json
  crashSites.json         same shape as somersloops.json
  freeDroppedItems.json   {itemFullPath: [[count, [x,y,z], instanceName], ...]}
  readableNameCorrections.json  {shortName: displayName}
  typePaths.json          {"conveyorBelts"|"miners"|"minedResources"|"powerLine": [typePath...],
                           "crashSite": typePath}

Key order in every file preserves Python dict insertion order -- the payload
builder's output ordering depends on it.
"""

import enum
import json
import os
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUTPUT_DIR = os.path.join(REPO_ROOT, "game_data", "sav_data")

sys.path.insert(0, os.path.join(REPO_ROOT, "parser"))

from sav_data import crashSites, data, freeStuff, mercerSphere, readableNames, resourcePurity, slug, somersloop  # noqa: E402


def jsonable(value):
    """Tuples -> lists, enums -> names, recursively; keys stay strings."""
    if isinstance(value, enum.Enum):
        return value.name
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if isinstance(value, dict):
        return {k: jsonable(v) for k, v in value.items()}
    return value


def buildTables():
    return {
        "resourcePurity.json": jsonable(resourcePurity.RESOURCE_PURITY),
        "powerSlugs.json": {
            "blue": jsonable(slug.POWER_SLUGS_BLUE),
            "yellow": jsonable(slug.POWER_SLUGS_YELLOW),
            "purple": jsonable(slug.POWER_SLUGS_PURPLE),
        },
        "somersloops.json": jsonable(somersloop.SOMERSLOOPS),
        "mercerSpheres.json": jsonable(mercerSphere.MERCER_SPHERES),
        "crashSites.json": jsonable(crashSites.CRASH_SITES),
        "freeDroppedItems.json": jsonable(freeStuff.FREE_DROPPED_ITEMS),
        "readableNameCorrections.json": jsonable(readableNames.READABLE_PATH_NAME_CORRECTIONS),
        "typePaths.json": {
            "conveyorBelts": jsonable(data.CONVEYOR_BELTS),
            "miners": jsonable(data.MINERS),
            "minedResources": jsonable(data.MINED_RESOURCES),
            "powerLine": jsonable(data.POWER_LINE),
            "crashSite": data.CRASH_SITE,
        },
    }


def main():
    check = "--check" in sys.argv
    tables = buildTables()
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    failures = []
    for filename, table in tables.items():
        path = os.path.join(OUTPUT_DIR, filename)
        if check:
            with open(path, encoding="utf-8") as f:
                onDisk = json.load(f)
            # Compare values and key order (order is load-bearing downstream).
            if onDisk != table or _keyOrders(onDisk) != _keyOrders(table):
                failures.append(filename)
            continue
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(table, f, ensure_ascii=False, indent=1)
            f.write("\n")
        print(f"wrote {path}")
    if check:
        if failures:
            print(f"MISMATCH: {', '.join(failures)}")
            sys.exit(1)
        print(f"OK: all {len(tables)} files match the Python literals")


def _keyOrders(value, prefix=""):
    """Flat list of (path, keys-in-order) for every dict in the tree."""
    orders = []
    if isinstance(value, dict):
        orders.append((prefix, list(value.keys())))
        for k, v in value.items():
            orders.extend(_keyOrders(v, f"{prefix}/{k}"))
    elif isinstance(value, list):
        for i, v in enumerate(value):
            orders.extend(_keyOrders(v, f"{prefix}[{i}]"))
    return orders


if __name__ == "__main__":
    main()
