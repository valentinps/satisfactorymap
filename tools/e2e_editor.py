"""End-to-end save-editor test: load -> move -> copy/paste -> delete ->
undo -> export -> re-import, asserting map state after every step.

Prereqs: dist/ built (tools/build_site.py) and served (tools/serve_site.py
8791), system Chrome, `pip install playwright`.

Usage: python tools/e2e_editor.py [save_path] [base_url]

Gate on the default small save. The 600k-object save
(BuildITBIIIIIG_*.sav) sits at the 4GB wasm memory ceiling: the first edit
succeeds (~80s), later edits may trip the out-of-memory recovery flow
(worker reset + reload + replay), which this script's strict step
assertions count as a failure. That's a known capacity limit, not a
regression -- see the editor notes in the save-editor branch history.
"""

import sys
import time
import tempfile
import pathlib
from playwright.sync_api import sync_playwright

REPO = pathlib.Path(__file__).resolve().parent.parent
SAVE = sys.argv[1] if len(sys.argv) > 1 else str(REPO / "map/uploads/All_autosave_0.sav")
BASE_URL = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8791/"
BUCKETS_READY = "window.MapApp && MapApp.layer && MapApp.layer.buckets && MapApp.layer.buckets.length > 0"


def bucket_info(page, fragment):
    return page.evaluate("""(frag) => {
      const b = MapApp.layer.buckets.find(b => b.key.indexOf('building:') === 0
          && b.key.indexOf(frag) !== -1 && b.ids && b.ids.length);
      if (!b) return null;
      return { key: b.key, n: b.ids.length, id: b.ids[0],
               x: b.points[0], y: b.points[1], ids: b.ids.slice() };
    }""", fragment)


def targets_js(info):
    return {
        "id": info["id"], "x": info["x"], "y": info["y"],
    }


def wait_idle(page, expected_ops):
    page.wait_for_function(
        f"window.EditorTool && EditorTool.opCount() === {expected_ops}"
        " && document.getElementById('loadProgressBar').style.display === 'none'",
        timeout=180000)
    time.sleep(0.8)


def main():
    with sync_playwright() as p:
        browser = p.chromium.launch(channel="chrome", headless=True)
        page = browser.new_page()
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(BASE_URL)
        page.set_input_files("#uploadFileInput", SAVE)
        page.wait_for_function(BUCKETS_READY, timeout=300000)
        time.sleep(1.5)

        # -- Move ------------------------------------------------------------
        smelter = bucket_info(page, "SmelterMk1")
        assert smelter, "test save needs a smelter"
        t0 = time.time()
        page.evaluate("""(t) => {
          const targets = { actorNames: [t.id], lightweight: [], skipped: 0,
                            bbox: { minX: t.x, minY: t.y, maxX: t.x, maxY: t.y } };
          EditorTool.startMove(targets);
          MapApp.map.fire('click', { latlng: L.latLng(t.y + 40, t.x + 80),
                                     originalEvent: new MouseEvent('click') });
        }""", targets_js(smelter))
        wait_idle(page, 1)
        moved = bucket_info(page, "SmelterMk1")
        assert abs(moved["x"] - (smelter["x"] + 80)) < 0.2
        assert abs(moved["y"] - (smelter["y"] + 40)) < 0.2
        print(f"move OK ({time.time() - t0:.1f}s)")

        # -- Copy/paste --------------------------------------------------------
        constructor = bucket_info(page, "ConstructorMk1")
        assert constructor, "test save needs a constructor"
        t0 = time.time()
        page.evaluate("""(t) => {
          const targets = { actorNames: [t.id], lightweight: [], skipped: 0,
                            bbox: { minX: t.x, minY: t.y, maxX: t.x, maxY: t.y } };
          EditorTool.copyTargets(targets);
          EditorTool.pasteAt(t.x + 150, t.y);
        }""", targets_js(constructor))
        wait_idle(page, 2)
        pasted = bucket_info(page, "ConstructorMk1")
        assert pasted["n"] == constructor["n"] + 1
        print(f"paste OK ({time.time() - t0:.1f}s)")

        # -- Delete ------------------------------------------------------------
        t0 = time.time()
        page.evaluate("""(t) => {
          const targets = { actorNames: [t.id], lightweight: [], skipped: 0,
                            bbox: { minX: t.x, minY: t.y, maxX: t.x, maxY: t.y } };
          EditorTool.deleteTargets(targets);
        }""", targets_js(smelter))
        wait_idle(page, 3)
        deleted = bucket_info(page, "SmelterMk1")
        deleted_n = deleted["n"] if deleted else 0
        assert deleted_n == smelter["n"] - 1
        print(f"delete OK ({time.time() - t0:.1f}s)")

        # -- Undo the delete ---------------------------------------------------
        t0 = time.time()
        page.evaluate("EditorTool.undo()")
        wait_idle(page, 2)
        restored = bucket_info(page, "SmelterMk1")
        assert restored["n"] == smelter["n"]
        print(f"undo OK ({time.time() - t0:.1f}s)")

        # -- Export + re-import ------------------------------------------------
        with page.expect_download() as dl_info:
            page.click("#downloadSaveBtn")
        out = pathlib.Path(tempfile.mkdtemp()) / dl_info.value.suggested_filename
        dl_info.value.save_as(str(out))
        page.set_input_files("#uploadFileInput", str(out))
        page.wait_for_function(
            "document.getElementById('loadStatus').textContent.indexOf('Loaded:') === 0",
            timeout=300000)
        time.sleep(1.5)
        persisted_smelter = bucket_info(page, "SmelterMk1")
        persisted_constructor = bucket_info(page, "ConstructorMk1")
        assert abs(persisted_smelter["x"] - moved["x"]) < 0.2
        assert persisted_constructor["n"] == constructor["n"] + 1
        assert page.evaluate("EditorTool.opCount()") == 0
        print("export/re-import OK")

        assert not errors, errors
        print("EDITOR E2E OK")
        browser.close()


if __name__ == "__main__":
    main()
