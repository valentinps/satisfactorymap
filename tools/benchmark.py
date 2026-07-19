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
