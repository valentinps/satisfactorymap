"""Cold-load benchmark: this project's map vs SCIM's interactive map.

Measures the user-facing number: wall time from "save file handed to the
page" to "map rendered", in a fresh headless Chrome per run (no cache
carry-over between runs). Page-load time is measured separately and NOT
included — it depends on the host/CDN, not the tool, and both pages are
fully loaded before the save is fed in.

Completion signals:
  - ours: window.MapApp.layer.buckets non-empty (the payload is applied and
    the WebGL layer has its buckets; the frame renders immediately after).
  - SCIM: its loading overlay (.loader / #saveGameLoader) has appeared and
    then gone — it stays up through every "Adding map layers (...)" pass.

Usage:
  python tools/benchmark.py path/to/save.sav [--runs 3]
      [--our-url https://satisfactorymap.net/]
      [--scim-url https://satisfactory-calculator.com/en/interactive-map]
      [--skip-scim] [--timeout 1800]

Requires: pip install playwright (uses the installed system Chrome).
"""

import argparse
import json
import os
import platform
import statistics
import subprocess
import sys
import time

from playwright.sync_api import sync_playwright


OUR_READY = ("window.MapApp && MapApp.layer && MapApp.layer.buckets"
             " && MapApp.layer.buckets.length > 0")
SCIM_LOADER = """() => {
  const els = document.querySelectorAll('.loader, #saveGameLoader, #loaderProgressBar');
  return [...els].some(el => el.offsetWidth || el.offsetHeight);
}"""


def hardware_summary():
    info = {"os": f"{platform.system()} {platform.release()}",
            "machine": platform.machine()}
    try:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "(Get-CimInstance Win32_Processor).Name; "
             "[math]::Round((Get-CimInstance Win32_ComputerSystem)"
             ".TotalPhysicalMemory / 1GB)"],
            capture_output=True, text=True, timeout=30).stdout.split("\n")
        lines = [l.strip() for l in out if l.strip()]
        if len(lines) >= 2:
            info["cpu"] = lines[0]
            info["ram_gb"] = int(lines[1])
    except Exception:
        pass
    return info


def time_our_tool(pw, url, save, timeout_s):
    browser = pw.chromium.launch(channel="chrome", headless=True)
    page = browser.new_page(viewport={"width": 1600, "height": 900})
    t = time.perf_counter()
    page.goto(url, wait_until="load", timeout=120000)
    page.wait_for_selector("#uploadFileInput", state="attached", timeout=60000)
    page_load = time.perf_counter() - t
    page.wait_for_timeout(2000)  # let the worker/wasm settle

    page.set_input_files("#uploadFileInput", save)
    t = time.perf_counter()
    page.wait_for_function(OUR_READY, timeout=timeout_s * 1000)
    load = time.perf_counter() - t
    browser.close()
    return {"page_load_s": round(page_load, 2), "save_load_s": round(load, 2)}


def time_scim(pw, url, save, timeout_s):
    browser = pw.chromium.launch(channel="chrome", headless=True)
    page = browser.new_page(viewport={"width": 1600, "height": 900})
    result = {}
    try:
        t = time.perf_counter()
        page.goto(url, wait_until="load", timeout=120000)
        page.wait_for_selector("#saveGameFileInput", state="attached",
                               timeout=60000)
        result["page_load_s"] = round(time.perf_counter() - t, 2)
        page.wait_for_timeout(2000)

        page.set_input_files("#saveGameFileInput", save)
        t = time.perf_counter()
        # Loader must appear first, so "no loader" can't fire on the gap
        # before SCIM's JS reacts to the file.
        page.wait_for_function(SCIM_LOADER, timeout=60000)
        page.wait_for_function(f"!({SCIM_LOADER})()",
                               timeout=timeout_s * 1000, polling=250)
        result["save_load_s"] = round(time.perf_counter() - t, 2)
    except Exception as exc:  # timeout or tab crash (OOM) is itself a result
        result["error"] = f"{type(exc).__name__}: {str(exc).splitlines()[0][:200]}"
        result["gave_up_after_s"] = round(time.perf_counter() - t, 1)
    finally:
        browser.close()
    return result


# ---- Interaction (frame-time) benchmark -------------------------------------
# Same idea as the load benchmark but for the whole session after it: with
# every layer visible, drive an identical scripted mouse interaction (four
# drag-pans, four wheel zoom-ins, four zoom-outs) on both tools and record
# requestAnimationFrame gaps -- the user-felt frame time. Runs HEADED:
# headless compositing is not representative for framerate.

FRAME_RECORDER_START = """() => {
  window.__ft = []; window.__ftStop = false;
  let last = performance.now();
  const loop = t => {
    window.__ft.push(t - last); last = t;
    if (!window.__ftStop) requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
}"""


def zoom_fully_out(page, cx, cy):
    """Camera normalization, NOT recorded: SCIM loads already zoomed into
    the player position (a world view with everything visible is exactly
    what a DOM renderer can't afford), while this tool fits the whole map.
    Walking both fully out puts them in the same camera state -- ours just
    clamps at min zoom. Generous pacing: on SCIM each step out can take
    many seconds as more of the factory enters view."""
    for _ in range(8):
        page.mouse.move(cx, cy)
        page.mouse.wheel(0, 400)
        page.wait_for_timeout(1500)
    page.wait_for_timeout(3000)


def drive_pans(page, cx, cy):
    """Four drag-pans at the current (overview) zoom -- the 'all objects in
    view' case, and the half both tools are guaranteed to experience with an
    identical camera."""
    for dx, dy in [(320, 0), (-320, 0), (0, 220), (0, -220)]:
        page.mouse.move(cx, cy)
        page.mouse.down()
        for i in range(8):
            page.mouse.move(cx + dx * (i + 1) / 8, cy + dy * (i + 1) / 8)
            page.wait_for_timeout(40)
        page.mouse.up()
        page.wait_for_timeout(800)


def drive_zooms(page, cx, cy):
    """A shallow zoom cycle: two wheel ticks in, two out. Deliberately
    shallow -- wheel sensitivity differs per site, and deep zoom broke
    SCIM outright in testing."""
    for direction in (-1, -1, 1, 1):
        page.mouse.move(cx, cy)
        page.mouse.wheel(0, 400 * direction)
        page.wait_for_timeout(1200)


def harvest_frames(page):
    """Fetch and reset the recorder between phases."""
    return page.evaluate(
        "() => { const f = window.__ft; window.__ft = []; return f; }")


STALL_THRESHOLD_MS = 100.0


def frame_stats(frames):
    # The median is a TRAP here: rAF ticks at monitor rate whenever the app
    # is idle (including the scripted waits between gestures), so idle
    # frames dominate the count and the median reads "smooth" even when
    # every gesture triggers a multi-second freeze. The story lives in the
    # stalls: frames over STALL_THRESHOLD_MS -- how many, how long in
    # total, and the single worst.
    frames = frames[5:]  # recorder warm-up
    if not frames:
        return None
    stalls = [f for f in frames if f > STALL_THRESHOLD_MS]
    return {
        "frames": len(frames),
        "median_ms": round(statistics.median(frames), 1),
        "worst_ms": round(max(frames), 1),
        "stalls_over_100ms": len(stalls),
        "stall_total_ms": round(sum(stalls), 1),
        "raw_ms": [round(f, 1) for f in frames],
    }


def measure_phases(page):
    """Pans first and harvested first, so the overview numbers are banked
    even if the (riskier) zoom phase wedges the tab. Any phase failing --
    including a main thread so blocked the harvest itself times out --
    records an error for that phase instead of losing the run."""
    result = {}
    zoom_fully_out(page, 1000, 450)
    page.evaluate(FRAME_RECORDER_START)
    try:
        drive_pans(page, 1000, 450)
        result["pan"] = frame_stats(harvest_frames(page))
    except Exception as exc:
        result["pan"] = {"error": f"{type(exc).__name__}: {str(exc).splitlines()[0][:160]}"}
        return result
    try:
        drive_zooms(page, 1000, 450)
        result["zoom"] = frame_stats(harvest_frames(page))
    except Exception as exc:
        result["zoom"] = {"error": f"{type(exc).__name__}: {str(exc).splitlines()[0][:160]}"}
    return result


def frames_our_tool(pw, url, save, timeout_s):
    browser = pw.chromium.launch(channel="chrome", headless=False)
    page = browser.new_page(viewport={"width": 1600, "height": 900})
    page.set_default_timeout(300000)  # a wedged tab should error, not hang
    try:
        page.goto(url, wait_until="load", timeout=120000)
        page.wait_for_selector("#uploadFileInput", state="attached",
                               timeout=60000)
        # A previous visit's persisted filter selection could hide layers --
        # this run is "everything shown".
        page.evaluate("() => localStorage.clear()")
        page.reload(wait_until="load")
        page.wait_for_selector("#uploadFileInput", state="attached",
                               timeout=60000)
        page.wait_for_timeout(2000)
        page.set_input_files("#uploadFileInput", save)
        page.wait_for_function(OUR_READY, timeout=timeout_s * 1000)
        page.wait_for_timeout(3000)
        return measure_phases(page)
    except Exception as exc:
        return {"error": f"{type(exc).__name__}: {str(exc).splitlines()[0][:200]}"}
    finally:
        browser.close()


def frames_scim(pw, url, save, timeout_s):
    browser = pw.chromium.launch(channel="chrome", headless=False)
    page = browser.new_page(viewport={"width": 1600, "height": 900})
    page.set_default_timeout(300000)
    try:
        page.goto(url, wait_until="load", timeout=120000)
        page.wait_for_selector("#saveGameFileInput", state="attached",
                               timeout=60000)
        page.wait_for_timeout(2000)
        page.set_input_files("#saveGameFileInput", save)
        page.wait_for_function(SCIM_LOADER, timeout=60000)
        page.wait_for_function(f"!({SCIM_LOADER})()",
                               timeout=timeout_s * 1000, polling=250)
        page.wait_for_timeout(3000)
        return measure_phases(page)
    except Exception as exc:
        return {"error": f"{type(exc).__name__}: {str(exc).splitlines()[0][:200]}"}
    finally:
        browser.close()


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("save", help="path to the .sav to benchmark with")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--our-url", default="https://satisfactorymap.net/")
    ap.add_argument("--scim-url",
                    default="https://satisfactory-calculator.com/en/interactive-map")
    ap.add_argument("--skip-scim", action="store_true")
    ap.add_argument("--timeout", type=int, default=1800,
                    help="per-run save-load timeout, seconds")
    ap.add_argument("--frames", action="store_true",
                    help="measure interaction frame times instead of load "
                         "time (headed Chrome, one run per tool)")
    args = ap.parse_args()

    save = os.path.abspath(args.save)
    report = {
        "save": {"name": os.path.basename(save),
                 "size_mb": round(os.path.getsize(save) / 1e6, 1)},
        "hardware": hardware_summary(),
        "urls": {"ours": args.our_url, "scim": args.scim_url},
        "runs": {"ours": [], "scim": []},
    }
    print(f"save: {report['save']['name']} ({report['save']['size_mb']} MB)")
    print(f"hardware: {report['hardware']}")

    if args.frames:
        with sync_playwright() as pw:
            r = frames_our_tool(pw, args.our_url, save, args.timeout)
            report["frames_ours"] = r
            print(f"  ours  frames: {r}")
            if not args.skip_scim:
                r = frames_scim(pw, args.scim_url, save, args.timeout)
                report["frames_scim"] = r
                print(f"  scim  frames: {r}")
        fo, fs = report.get("frames_ours") or {}, report.get("frames_scim") or {}
        for phase in ("pan", "zoom"):
            po, ps = fo.get(phase) or {}, fs.get(phase) or {}
            if "worst_ms" in po and "worst_ms" in ps:
                multiple = round(ps["worst_ms"] / po["worst_ms"], 1)
                report[f"worst_frame_multiple_{phase}"] = multiple
                print(f"\n{phase}: worst frame ours {po['worst_ms']}ms "
                      f"(stalls: {po['stalls_over_100ms']}), SCIM "
                      f"{ps['worst_ms']}ms (stalls: {ps['stalls_over_100ms']},"
                      f" {ps['stall_total_ms']}ms frozen total) "
                      f"-> {multiple}x")
        out = os.path.join(os.path.dirname(save), "benchmark_frames.json")
        with open(out, "w", encoding="utf-8") as f:
            json.dump(report, f, indent=2)
        print(f"full report: {out}")
        return

    with sync_playwright() as pw:
        for i in range(args.runs):
            r = time_our_tool(pw, args.our_url, save, args.timeout)
            report["runs"]["ours"].append(r)
            print(f"  ours  run {i + 1}: {r}")
        for i in range(args.runs):
            if args.skip_scim:
                break
            r = time_scim(pw, args.scim_url, save, args.timeout)
            report["runs"]["scim"].append(r)
            print(f"  scim  run {i + 1}: {r}")

    ours = [r["save_load_s"] for r in report["runs"]["ours"]
            if "save_load_s" in r]
    scim = [r["save_load_s"] for r in report["runs"]["scim"]
            if "save_load_s" in r]
    if ours:
        report["median_ours_s"] = statistics.median(ours)
    if scim:
        report["median_scim_s"] = statistics.median(scim)
    if ours and scim:
        report["multiple"] = round(report["median_scim_s"]
                                   / report["median_ours_s"], 1)
        print(f"\nmedian save-load: ours {report['median_ours_s']}s, "
              f"SCIM {report['median_scim_s']}s "
              f"-> {report['multiple']}x")

    out = os.path.join(os.path.dirname(save), "benchmark_result.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2)
    print(f"full report: {out}")


if __name__ == "__main__":
    main()
