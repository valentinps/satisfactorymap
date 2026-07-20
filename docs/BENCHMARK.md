# Benchmarks

How much faster this map handles a mega-factory save than the incumbent web
tool
([SCIM — satisfactory-calculator.com](https://satisfactory-calculator.com/en/interactive-map)),
measured reproducibly: **load time** (one-time cost per save) and
**interaction responsiveness** (what you feel for the whole session).

## Headline results

**Loading:** this tool loads a 600k-object mega-factory save in ~8 seconds;
SCIM takes ~110 seconds on the same machine — a **13×** difference.
(2026-07-19, both tools' public production sites, median of 3 cold runs.)

**Interacting:** during an identical scripted pan/zoom session at overview
zoom with every object visible, this tool **never stalled once** (worst
frame of the whole session: 24 ms); SCIM **froze 20 times for a combined
69 seconds**, the single longest freeze lasting 15.3 s. Comparing worst
frames: **~630×**. (2026-07-20, same save, same machine, headed Chrome.)

| interaction | worst frame (ours) | stalls >100 ms (ours) | worst frame (SCIM) | stalls >100 ms (SCIM) | total frozen (SCIM) |
| --- | --- | --- | --- | --- | --- |
| 4 drag-pans, overview | 24.4 ms | 0 | 15,342 ms | 15 | 47.2 s |
| zoom cycle (2 in / 2 out) | 24.3 ms | 0 | 9,330 ms | 5 | 21.9 s |

| | run 1 | run 2 | run 3 | median |
| --- | --- | --- | --- | --- |
| satisfactorymap.net | 8.20 s | 8.18 s | 8.20 s | **8.2 s** |
| SCIM interactive map | 130.85 s | 109.85 s | 109.13 s | **109.9 s** |

Page load (excluded from the above, see below): ~0.5 s for both tools.
Raw report: `benchmark_result.json` emitted by the runner.

## What is measured: interaction

`python tools/benchmark.py path/to/save.sav --frames` — after loading the
save, both tools receive the **identical scripted mouse input** (four
drag-pans, then a two-in/two-out wheel-zoom cycle) while a
`requestAnimationFrame`-gap recorder captures every frame time. Details
that matter:

- **Camera normalization first** (not recorded): SCIM deliberately loads
  zoomed into the player — a full overview is exactly what a DOM-based
  renderer can't afford — while this tool fits the whole map. Both are
  wheel-zoomed fully out before measurement so they compare the same
  "everything visible" camera. (This tool renders through a WebGL layer, so
  overview zoom is its *heaviest* case too — there's no culling trick
  favoring it.)
- **The median frame time is deliberately NOT the headline**: rAF ticks at
  monitor rate whenever the app is idle between gestures, so idle frames
  dominate the count and the median reads "smooth" (~6 ms on both tools)
  even when every gesture triggers a multi-second freeze. What users feel
  is the stalls — so the report counts frames over 100 ms, their total,
  and the single worst. Raw per-frame data is saved in
  `benchmark_frames.json` for scrutiny.
- Headed (visible) Chrome, since headless compositing isn't representative
  for frame timing. One run per tool — with zero stalls on one side and
  tens of seconds on the other, run-to-run noise is not the issue.

## What is measured: loading

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
