#!/usr/bin/env python3
"""Layer-2 differential test: run sav_map_data.buildMapPayload +
buildSaveIndex (+ sampled describeInstance / findItemLocations calls) under
both parser backends and diff the results. Backend selection happens at
import time via SAV_PARSE_IMPL, so each backend runs in a subprocess.

Usage: python tools/diff_payload.py save.sav [save2.sav ...]

Payload-port gating mode (the Rust sav_core::mapdata port of this module):
    python tools/diff_payload.py --payload [--steps key1,key2] save.sav ...
holds the parser constant (SAV_PARSE_IMPL=rust) and compares the payload
built by Python sav_map_data against sav_parse_rs.build_map_payload_json,
restricted to the given payload-step keys (all Rust-ported steps must be
listed; omitting --steps compares the full payload). Canonicalization in
this mode is ORDER-STRICT: dict key order differences fail the diff, since
the frontend iterates payload objects in insertion order.
"""

import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def canonical(v):
    if isinstance(v, dict):
        return {str(k): canonical(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [canonical(x) for x in v]
    if isinstance(v, float):
        return ["F", v.hex()]
    if isinstance(v, (set, frozenset)):
        return sorted(canonical(x) for x in v)
    return v


def canonical_strict(v):
    # Order-strict variant: a dict becomes ["D", [keys in order], {values}]
    # so insertion-order drift fails the comparison.
    if isinstance(v, dict):
        return ["D", [str(k) for k in v.keys()],
                {str(k): canonical_strict(x) for k, x in v.items()}]
    if isinstance(v, (list, tuple)):
        return [canonical_strict(x) for x in v]
    if isinstance(v, float):
        return ["F", v.hex()]
    if isinstance(v, (set, frozenset)):
        return sorted(canonical_strict(x) for x in v)
    return v


PAYLOAD_HEADER_KEYS = ("mapSize", "sessionName", "saveDatetime", "menuOrder", "itemCatalog")


def _index_samples(save_index_dump):
    # Deterministic query samples, computed from each side's OWN index dump
    # (if the dumps differ, the saveIndex comparison fails first anyway).
    import sav_map_data

    items = sorted(save_index_dump["itemLocationIndex"].keys())[:25]
    building_types = [tp for tp in sorted(save_index_dump["instanceNamesByTypePath"])
                      if "/Build_" in tp][:20]
    vehicle_types = [tp for tp in sav_map_data.VEHICLE_ICONS_BY_TYPE_PATH
                     if tp not in sav_map_data.RAILCAR_TYPE_PATHS]
    selection_names = list(save_index_dump["objects"])[:50]  # dump lists are pre-sorted
    # describeInstance samples: worker()'s spread over all objects, plus the
    # index-only id spaces (lightweight buildables, train consists) and the
    # not-found error branch.
    names = list(save_index_dump["objects"])  # pre-sorted
    step = max(1, len(names) // 100)
    describe_names = names[::step][:100]
    describe_names += sorted(save_index_dump["lightweightInstancesById"])[:10]
    describe_names += sorted(save_index_dump["trainInfoByInstanceName"])[:5]
    describe_names.append("__no_such_instance__")
    return items, building_types, vehicle_types, selection_names, describe_names


def _index_dump_py(sav_map_data, scan):
    # Same dump shape as worker() below: headers/objects as sorted name
    # lists, everything else canonical().
    save_index = sav_map_data._buildSaveIndex(scan)
    index_dump = {}
    for key, val in save_index.items():
        if key in ("headers", "objects"):
            index_dump[key] = sorted(val.keys())
        else:
            index_dump[key] = canonical(val)
    # Query results use canonical_strict (the frontend iterates those dicts
    # in insertion order, so key-order drift must fail the diff); the
    # saveIndex dump itself stays order-blind canonical() -- describeInstance
    # & friends look keys up by name, so its dict order is not load-bearing.
    (items, building_types, vehicle_types, selection_names,
     describe_names) = _index_samples(index_dump)
    return {
        "saveIndex": index_dump,
        "describeInstance": {
            name: canonical_strict(sav_map_data.describeInstance(save_index, name))
            for name in describe_names},
        "findItemLocations": {
            item: canonical_strict(sav_map_data.findItemLocations(save_index, item))
            for item in items},
        "buildingInfo": {
            tp: canonical_strict(sav_map_data.collectBuildingInfo(save_index, [tp]))
            for tp in building_types},
        "vehicleInfo": {
            tp: canonical_strict(sav_map_data.collectVehicleInfo(save_index, [tp]))
            for tp in vehicle_types},
        "trainInfo": canonical_strict(sav_map_data.collectTrainInfo(save_index)),
        "selectionInventory": canonical_strict(
            sav_map_data.aggregateSelectionInventory(save_index, selection_names)),
    }


def _index_dump_rust(parsed):
    import sav_parse_rs

    session = sav_parse_rs.build_map_session(parsed)
    index_dump = {k: canonical(v) for k, v in json.loads(session.index_dump_json()).items()}
    (items, building_types, vehicle_types, selection_names,
     describe_names) = _index_samples(index_dump)
    return {
        "saveIndex": index_dump,
        "describeInstance": {
            name: canonical_strict(json.loads(session.describe_instance_json(name)))
            for name in describe_names},
        "findItemLocations": {
            item: canonical_strict(json.loads(session.find_item_locations_json(item)))
            for item in items},
        "buildingInfo": {
            tp: canonical_strict(json.loads(session.building_info_json([tp])))
            for tp in building_types},
        "vehicleInfo": {
            tp: canonical_strict(json.loads(session.vehicle_info_json([tp])))
            for tp in vehicle_types},
        "trainInfo": canonical_strict(json.loads(session.train_info_json())),
        "selectionInventory": canonical_strict(
            json.loads(session.selection_inventory_json(selection_names))),
    }


def payload_worker(sav, out_path, steps, with_index=False):
    sys.path.insert(0, os.path.join(REPO, "map"))
    import sav_parse_shim as sav_parse
    import sav_map_data

    parsed = sav_parse.readFullSaveFile(sav)
    impl = os.environ.get("PAYLOAD_IMPL", "py")
    dump = {"impl": impl}
    if impl == "rust":
        import sav_parse_rs
        raw = sav_parse_rs.build_map_payload_json(parsed, list(steps) if steps else None)
        payload = json.loads(bytes(raw))
        if with_index:
            dump.update(_index_dump_rust(parsed))
    else:
        scan = sav_map_data.SaveScan(parsed)
        payload = {
            "mapSize": sav_map_data.MAP_SIZE,
            "sessionName": parsed.saveFileInfo.sessionName,
            "saveDatetime": parsed.saveFileInfo.saveDatetime.strftime("%Y-%m-%d %H:%M:%S"),
            "menuOrder": sav_map_data.BUILD_MENU_ORDER,
            "itemCatalog": sav_map_data.listSearchableItems(),
        }
        for key, compute in sav_map_data._payloadSteps(parsed, scan):
            if steps is None or key in steps:
                payload[key] = compute()
        if with_index:
            dump.update(_index_dump_py(sav_map_data, scan))
    dump["payload"] = canonical_strict(payload)
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(dump, f, sort_keys=True)
    print(f"  [payload={impl}] {len(payload)} payload keys"
          + (" + saveIndex/queries" if with_index else ""))


def worker(sav, out_path):
    sys.path.insert(0, os.path.join(REPO, "map"))
    import sav_parse_shim as sav_parse
    import sav_map_data

    parsed = sav_parse.readFullSaveFile(sav)
    payload, save_index = sav_map_data.buildAll(parsed)

    index_dump = {}
    for key, val in save_index.items():
        if key in ("headers", "objects"):
            index_dump[key] = sorted(val.keys())  # live handles: names only
        else:
            index_dump[key] = canonical(val)

    # Sampled per-request endpoints (deterministic sample).
    names = sorted(save_index["objects"].keys())
    step = max(1, len(names) // 100)
    described = {}
    for name in names[::step][:100]:
        described[name] = canonical(sav_map_data.describeInstance(save_index, name))

    items = sorted(save_index["itemLocationIndex"].keys())
    found = {}
    for item in items[: 25]:
        found[item] = canonical(sav_map_data.findItemLocations(save_index, item))

    dump = {
        "impl": sav_parse.ACTIVE_IMPL,
        "payload": canonical(payload),
        "saveIndex": index_dump,
        "describeInstance": described,
        "findItemLocations": found,
    }
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(dump, f, sort_keys=True)
    print(f"  [{sav_parse.ACTIVE_IMPL}] dumped {len(names)} objects")


def first_diff(a, b, path="$"):
    if type(a) is not type(b):
        return f"{path}: type {type(a).__name__} != {type(b).__name__}"
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return f"{path}.{k}: only in {'rust' if k in b else 'python'} dump"
            d = first_diff(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: len {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_diff(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    if a != b:
        return f"{path}: {a!r:.200} != {b!r:.200}"
    return None


def compare_files(path_a, path_b):
    with open(path_a, encoding="utf-8") as f:
        a = json.load(f)
    with open(path_b, encoding="utf-8") as f:
        b = json.load(f)
    a.pop("impl", None), b.pop("impl", None)
    if a == b:
        print(f"OK   {os.path.basename(path_a)} == {os.path.basename(path_b)}")
        return True
    print(f"FAIL {path_a} vs {path_b}: {first_diff(a, b)}")
    return False


def payload_main(argv):
    # --payload [--steps a,b,c] [--with-index] save.sav ... : parser held at
    # rust, Python payload builder vs Rust payload builder. --with-index also
    # compares the saveIndex dump + sampled query endpoints (findItemLocations
    # / buildingInfo / vehicleInfo / trainInfo / selectionInventory).
    steps = None
    with_index = False
    while argv and argv[0] in ("--steps", "--with-index"):
        if argv[0] == "--with-index":
            with_index = True
            argv = argv[1:]
        else:
            steps = [s for s in argv[1].split(",") if s]
            argv = argv[2:]
    saves = argv
    if not saves:
        print(__doc__)
        return 2
    ok = True
    for sav in saves:
        name = os.path.basename(sav)
        with tempfile.TemporaryDirectory() as td:
            outs = {}
            for impl in ("py", "rust"):
                out = os.path.join(td, f"{impl}.json")
                env = dict(os.environ, SAV_PARSE_IMPL="rust", PAYLOAD_IMPL=impl)
                cmd = [sys.executable, os.path.abspath(__file__), "--payload-worker", sav, out,
                       ",".join(steps) if steps else "*",
                       "with-index" if with_index else "no-index"]
                r = subprocess.run(cmd, env=env, cwd=REPO)
                if r.returncode != 0:
                    print(f"FAIL {name}: payload={impl} worker exited {r.returncode}")
                    ok = False
                    break
                outs[impl] = out
            else:
                with open(outs["py"], encoding="utf-8") as f:
                    a = json.load(f)
                with open(outs["rust"], encoding="utf-8") as f:
                    b = json.load(f)
                a.pop("impl"), b.pop("impl")
                if a == b:
                    print(f"OK   {name}")
                else:
                    print(f"FAIL {name}: {first_diff(a, b)}")
                    ok = False
    print("RESULT:", "GREEN" if ok else "RED")
    return 0 if ok else 1


def main():
    if len(sys.argv) >= 4 and sys.argv[1] == "--worker":
        worker(sys.argv[2], sys.argv[3])
        return 0
    if len(sys.argv) >= 5 and sys.argv[1] == "--payload-worker":
        steps_arg = sys.argv[4]
        with_index = len(sys.argv) >= 6 and sys.argv[5] == "with-index"
        payload_worker(sys.argv[2], sys.argv[3],
                       None if steps_arg == "*" else steps_arg.split(","),
                       with_index=with_index)
        return 0
    if len(sys.argv) == 4 and sys.argv[1] == "--compare":
        return 0 if compare_files(sys.argv[2], sys.argv[3]) else 1
    if len(sys.argv) >= 2 and sys.argv[1] == "--payload":
        return payload_main(sys.argv[2:])

    saves = sys.argv[1:]
    if not saves:
        print(__doc__)
        return 2
    ok = True
    for sav in saves:
        name = os.path.basename(sav)
        with tempfile.TemporaryDirectory() as td:
            outs = {}
            for impl in ("py", "rust"):
                out = os.path.join(td, f"{impl}.json")
                env = dict(os.environ, SAV_PARSE_IMPL=impl)
                r = subprocess.run(
                    [sys.executable, os.path.abspath(__file__), "--worker", sav, out],
                    env=env, cwd=REPO,
                )
                if r.returncode != 0:
                    print(f"FAIL {name}: {impl} worker exited {r.returncode}")
                    ok = False
                    break
                outs[impl] = out
            else:
                with open(outs["py"], encoding="utf-8") as f:
                    a = json.load(f)
                with open(outs["rust"], encoding="utf-8") as f:
                    b = json.load(f)
                a.pop("impl"), b.pop("impl")
                if a == b:
                    print(f"OK   {name}")
                else:
                    print(f"FAIL {name}: {first_diff(a, b)}")
                    ok = False
    print("RESULT:", "GREEN" if ok else "RED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
