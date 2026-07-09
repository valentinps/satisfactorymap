#!/usr/bin/env python3
"""Benchmark the two parser backends: readFullSaveFile alone, and the full
server request cycle (parse + buildMapPayload + buildSaveIndex + json.dumps).

Backend selection happens at import time, so each backend runs in a
subprocess.

Usage: python tools/bench_parse.py save.sav [save2.sav ...]
"""

import json
import os
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def worker(sav):
    sys.path.insert(0, os.path.join(REPO, "map"))
    import sav_parse_shim as sav_parse
    import sav_map_data

    t0 = time.perf_counter()
    parsed = sav_parse.readFullSaveFile(sav)
    t1 = time.perf_counter()
    payload, index = sav_map_data.buildAll(parsed)
    t2 = time.perf_counter()
    blob = json.dumps(payload)
    t3 = time.perf_counter()
    print(json.dumps({
        "impl": sav_parse.ACTIVE_IMPL,
        "parse": round(t1 - t0, 2),
        "payloadIndex": round(t2 - t1, 2),
        "jsonify": round(t3 - t2, 2),
        "total": round(t3 - t0, 2),
        "payloadMB": round(len(blob) / 1e6, 1),
    }))


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "--worker":
        worker(sys.argv[2])
        return 0
    saves = sys.argv[1:]
    if not saves:
        print(__doc__)
        return 2
    for sav in saves:
        name = os.path.basename(sav)
        print(f"== {name} ==")
        for impl in ("rust", "py"):
            env = dict(os.environ, SAV_PARSE_IMPL=impl)
            subprocess.run(
                [sys.executable, os.path.abspath(__file__), "--worker", sav],
                env=env, cwd=REPO,
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
