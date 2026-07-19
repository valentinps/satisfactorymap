# Streaming payload builder — kill the last full-model materialization

## Why

The viewer/editor already keeps its *standing* memory low: after the map
payload is built, the parsed per-object model is dropped
(`SaveStore::drop_object_model`), queries re-parse single objects on demand
from their byte spans (`SaveStore::parse_object_at`), and a lean-worker
handoff returns freed memory to the browser. A 600k-object save idles at
~1.8 GB of tab memory instead of ~3.6 GB.

What remains is the *transient* peak. `mapdata::build_all_json` — which
produces the map payload JSON and the `MapIndex` — requires the **entire
parsed object model resident** (`Level::parsed_objects()`). So both at load
and after **every edit**, the pipeline materializes all ~600k parsed objects
(~1.7 GB) one more time, builds, then drops them again. During that window
the wasm heap holds:

```
body (~850 MB) + full model (~1.7 GB) + builder transients (serde_json
Values, index maps) ≈ 3.5–4.0 GB
```

wasm32 is hard-capped at 4 GB (4.29e9 bytes). Normal edits on the big save
fit (barely — heap grows to ~3.9e9). Big *pastes* into the big save (the
cross-save clipboard supports 100k+ objects, whose decoded blobs add
~0.7 GB of their own) can cross the ceiling and abort into the (graceful,
but slow) recovery flow.

**Goal: build the payload + index without ever holding the full model.**
Peak becomes ~body + collector outputs (~1.2–1.5 GB). That removes the 4 GB
ceiling from every operation, makes mega-pastes into the biggest saves
routine, and as a bonus likely speeds edits up (less allocation pressure
near the ceiling).

## Current architecture (read these first)

- `rust_parser/core/src/mapdata/mod.rs` — `build_all_json(store, progress)`:
  runs ~17 collectors + `MapIndex::build`, all sharing one
  `SaveScan` (`mapdata/scan.rs`).
- `SaveScan::new` walks **headers only** (already model-free). The choke
  point is `SaveScan::object(slot) -> &'a Object`, which indexes
  `Level::parsed_objects()` — the resident model. Collectors and
  `index.rs` call it for the objects they care about; several also iterate
  `level.parsed_objects()` directly (grep for `parsed_objects`).
- On-demand infrastructure that already exists and is gated by tests:
  - `SaveStore::parse_object_at(li, oi)` (object.rs) — re-parses one object
    from its span; identical result, microseconds each.
  - `mapdata/describe.rs` + `queries.rs` were already converted to owned
    on-demand objects when the lazy model landed — use them as the pattern.
- The editor pipeline (`editor/session.rs`): `fold_ops` runs lean
  intermediate re-parses and ONE final **full** parse, solely because
  `finish_edit` (wasm/src/lib.rs) feeds `build_all_json`. If the builder no
  longer needs the model, that final parse can become
  `parse_body_bytes_lean` too, and `SaveSession::load` can drop
  `parse_full_save`'s eager model as well (or parse lean from the start).

## Recommended shape

Option B first (least invasive), fall back to A only if B measures too slow:

- **B — owned objects at the access points.** Change `SaveScan::object` (and
  the direct `parsed_objects()` iterations in collectors/index.rs) to
  produce owned `Object`s via `parse_object_at`, holding them only for the
  duration of the loop body. Most collectors touch small type-filtered
  subsets (via `actors_of_type`); the big one is the buildings collector,
  which touches most objects once. Total re-parse cost ≈ 1–2× a full parse
  spread across collectors — measure it (native `dump_payload` timing before
  vs after; budget: ≤ 1.5× current build time).
- **A — single-pass visitor.** If B is too slow: one pass over all slots in
  save order, parse each object once, hand `&Object` to every collector's
  per-object hook, drop. Much more invasive (restructures every collector's
  control flow); only do this with evidence B isn't good enough.

Then:
1. Make `build_all_json` work on a lean store (assert it never calls
   `parsed_objects()`).
2. Switch `fold_ops`'s final parse and `finish_edit` to lean; delete the
   `rehydrate` path if nothing uses it.
3. Switch `SaveSession::load` to a lean parse (the load-time high-water
   drops too; the lean-worker handoff may become mostly redundant — keep it
   unless measurements say otherwise, it also frees browser-side memory).

## Hard constraints — do not regress these

- **Byte-exact payload.** The payload JSON is gated bit-for-bit against the
  old Python implementation. Iteration ORDER is load-bearing everywhere
  (Python dict semantics: insertion order, last-value-wins). Any change
  must keep `cargo run --release --features parallel --example dump_payload
  -- <save> out.json idx.json` **byte-identical** before/after, on BOTH
  `map/uploads/All_autosave_0.sav` and
  `map/uploads/BuildITBIIIIIG_autosave_0.sav` (stash-dance to capture the
  baseline first). Same for `dump_queries.rs`.
- **Quirk parity:** `parse_object` appends to `calculator_extras` in object
  order during eager parses. If nothing consumes them at build time, fine —
  but check (grep `calculator_extras`) before changing when objects parse.
- Existing gates: `cargo test -p sav_core --release` (includes
  `tests/lazy_objects.rs` span-reparse equality and all `tests/editor_*`),
  `python tools/e2e_editor.py` (small save; big save documented in the
  script), the ignored `paste_100k_objects_scale` test
  (`-- --ignored`).

## Verification of the actual goal

Measure wasm heap during an edit on `BuildITBIIIIIG_autosave_0.sav`
(worker progress events carry `memBytes` = linear memory high-water and
`liveBytes` = live allocations; `SaveClient.memStats()` reads both):
- Before: first edit grows the heap to ~3.9e9.
- Target: edit-time high-water ≤ ~2.5e9, and the 100k-object cross-save
  paste into the big save completes WITHOUT tripping the sessionLost
  recovery (drive it via the two-tab flow in the git history's
  `e2e_crosstab.py` pattern, or natively via the ignored scale test).

## Build loop

```
cd rust_parser && cargo test -p sav_core --release
python tools/build_site.py          # FULL build whenever Rust changed
python tools/serve_site.py 8791
python tools/e2e_editor.py          # must stay green
```
`py` is not on bash PATH — use `python`. Piped `| tail` masks exit codes.
