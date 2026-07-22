"""Download the public test-save corpus into map/uploads/.

The Rust test suite (cargo test -p sav_core), tools/e2e_editor.py, and CI all
read real .sav files from map/uploads/, which is gitignored (saves are tens of
MB). The corpus lives as assets on the `test-saves-v1` GitHub release -- same
distribution mechanism as game_data.zip -- so a clean clone becomes testable
with:

  py tools/fetch_test_saves.py

Files already present with the expected size are skipped, so this is cheap to
re-run (and CI caches map/uploads keyed on the release tag).
"""

import os
import sys
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEST = os.path.join(REPO, "map", "uploads")
BASE = "https://github.com/valentinps/satisfactorymap/releases/download/test-saves-v1/"

# name -> expected size (skip check; also catches truncated downloads)
SAVES = {
    "All_080726-163150.sav": 263_439,
    "solo_autosave_1.sav": 15_068_544,
    "BuildITBIIIIIG_210726-231135.sav": 50_186_116,
}


def main():
    os.makedirs(DEST, exist_ok=True)
    for name, size in SAVES.items():
        path = os.path.join(DEST, name)
        if os.path.isfile(path) and os.path.getsize(path) == size:
            print(f"{name}: present ({size} bytes), skipping")
            continue
        print(f"{name}: downloading {size} bytes...", flush=True)
        tmp = path + ".part"
        urllib.request.urlretrieve(BASE + name, tmp)
        got = os.path.getsize(tmp)
        if got != size:
            os.remove(tmp)
            sys.exit(f"{name}: downloaded {got} bytes, expected {size}")
        os.replace(tmp, path)
        print(f"{name}: done")
    print(f"test saves ready in {DEST}")


if __name__ == "__main__":
    main()
