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


def payload_worker(sav, out_path, steps):
    sys.path.insert(0, os.path.join(REPO, "map"))
    import sav_parse_shim as sav_parse
    import sav_map_data

    parsed = sav_parse.readFullSaveFile(sav)
    impl = os.environ.get("PAYLOAD_IMPL", "py")
    if impl == "rust":
        import sav_parse_rs
        raw = sav_parse_rs.build_map_payload_json(parsed, list(steps) if steps else None)
        payload = json.loads(bytes(raw))
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
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump({"impl": impl, "payload": canonical_strict(payload)}, f, sort_keys=True)
    print(f"  [payload={impl}] {len(payload)} payload keys")


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
    # --payload [--steps a,b,c] save.sav ... : parser held at rust, Python
    # payload builder vs Rust payload builder.
    steps = None
    if argv and argv[0] == "--steps":
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
                       ",".join(steps) if steps else "*"]
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
        payload_worker(sys.argv[2], sys.argv[3], None if steps_arg == "*" else steps_arg.split(","))
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
