#!/usr/bin/env python3
"""Layer-2 differential test: run sav_map_data.buildMapPayload +
buildSaveIndex (+ sampled describeInstance / findItemLocations calls) under
both parser backends and diff the results. Backend selection happens at
import time via SAV_PARSE_IMPL, so each backend runs in a subprocess.

Usage: python tools/diff_payload.py save.sav [save2.sav ...]
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


def worker(sav, out_path):
    sys.path.insert(0, os.path.join(REPO, "map"))
    import sav_parse_shim as sav_parse
    import sav_map_data

    parsed = sav_parse.readFullSaveFile(sav)
    payload = sav_map_data.buildMapPayload(parsed)
    save_index = sav_map_data.buildSaveIndex(parsed)

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


def main():
    if len(sys.argv) >= 4 and sys.argv[1] == "--worker":
        worker(sys.argv[2], sys.argv[3])
        return 0

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
