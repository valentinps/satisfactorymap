# Save-load benchmark

How much faster this map loads a save than the incumbent web tool
([SCIM — satisfactory-calculator.com](https://satisfactory-calculator.com/en/interactive-map)),
measured reproducibly.

## Headline result

**This tool loads a 600k-object mega-factory save in ~8 seconds; SCIM takes
~110 seconds on the same machine — a 13× difference.** (2026-07-19, both
tools' public production sites, median of 3 cold runs.)

| | run 1 | run 2 | run 3 | median |
| --- | --- | --- | --- | --- |
| satisfactorymap.net | 8.20 s | 8.18 s | 8.20 s | **8.2 s** |
| SCIM interactive map | 130.85 s | 109.85 s | 109.13 s | **109.9 s** |

Page load (excluded from the above, see below): ~0.5 s for both tools.
Raw report: `benchmark_result.json` emitted by the runner.

## What is measured

**Wall time from "save file handed to the page" to "map rendered."** Page
load time is measured separately and excluded from the headline: it depends
on the host/CDN, not the parser, and both pages are fully loaded (plus a 2s
settle) before the save is fed in. This is the number a player experiences
every time they load their save, and it is the one that scales with factory
size.

Completion signals:

- **This tool** — the parsed payload has been applied and the WebGL layer's
  buckets exist (`MapApp.layer.buckets.length > 0`); the frame renders
  immediately after.
- **SCIM** — its loading overlay has appeared and then disappeared. The
  overlay stays up through the entire parse and every "Adding map layers
  (…)" pass, so overlay-gone means the map is fully populated.

Both tools parse fully client-side, so this is a like-for-like comparison
of the same job: read the `.sav` in the browser, build the map.

## Benchmark save

`BuildITBIIIIIG_autosave_8.sav` — a 50 MB endgame mega-factory save with
~600,000 objects. Large saves are where load time actually hurts;
a small save loads in about a second in either tool.

## Protocol

- One **fresh headless Chrome instance per run** (system Chrome via
  Playwright) — no cache or JIT warm-up carries over between runs.
- 3 runs per tool, **median** reported.
- Both tools loaded from their public production URLs.
- Runner: `tools/benchmark.py` in this repo —
  `python tools/benchmark.py path/to/save.sav` reproduces the whole table,
  including hardware detection. Requires Python 3.10+, `pip install
  playwright`, and an installed Chrome.

## Hardware

AMD Ryzen 7 5800X (8 cores), 32 GB RAM, Windows 11, Chrome (headless,
system install). NVMe-local save file.

## Caveats

- Headless Chrome; headed numbers are within noise on this workload
  (CPU-bound parse, not compositor-bound).
- SCIM does more than render on load (it is also a save editor with
  per-object markers); the comparison is of the user-facing wait, not of
  internal design choices.
- Numbers vary with hardware; the multiple is far more stable than the
  absolute seconds.
