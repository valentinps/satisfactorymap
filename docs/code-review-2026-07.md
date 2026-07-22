# Full codebase review — July 22, 2026

Seven parallel deep-review passes over the whole repo (~35k lines): Rust
parser core, save editor, mapdata collectors, Tauri & WASM shells, frontend
render core, frontend UI, and Python/build/CI. Every finding was verified
against source. Severity: **H**igh / **M**edium / **L**ow. Items marked ✅
were fixed on the `review-fixes` branch alongside this document.

## Verdict

The codebase is in good shape: deliberate architecture, comments that match
reality, and real byte-level round-trip tests where they matter most. The
hardest engineering (byte-splice editing, wasm 4 GB memory discipline,
WebView2 IPC truncation workarounds, float32 anchor math) is the best code
in the repo. Verified clean: no structural offset bug in the editor's splice
bookkeeping, no XSS path from save-file strings to the DOM, no GPU/WASM
memory leaks, tight Tauri capability surface, tight git tracking hygiene.

The weaknesses cluster in four themes, systematic rather than sloppy:

1. **Trusting data at the boundaries** — corrupt saves and regenerated
   game-data tables could crash the parser/worker instead of erroring.
2. **No CI** in front of an auto-deploying `main` and a signed updater.
3. **Hand-synchronized duplication** — several subsystems stay consistent
   only via "keep in sync" comments.
4. **The Python bit-exact parity gate is now vestigial** and freezes real
   payload wins (2–3× shrink available: float precision, instance-path
   interning, derivable world positions).

## Fix-first list

1. ✅ **Commit the regenerated `Cargo.lock`** — v0.1.4 was published from a
   lock state that existed in no commit; with loose version reqs the signed
   updater artifact was not reproducible from its tag.
2. ✅ **Add CI** — `.github/` had no workflows at all; nothing ran
   `cargo test` or the wasm build before Cloudflare deployed every push to
   `main`. CI now fetches the public test-saves corpus (release
   `test-saves-v1`, via `tools/fetch_test_saves.py`) and runs the Rust suite
   plus the wasm build. The Rust tests now reference the published corpus
   (`All_080726-163150.sav`, `solo_autosave_1.sav`,
   `BuildITBIIIIIG_210726-231135.sav`), so a clean clone is testable.
3. ✅ **`release.py` refuses a dirty/out-of-sync tree** — it built from the
   local tree but tagged remote `main` HEAD, so uncommitted or unpushed
   state silently shipped (this is exactly how #1 happened).
4. ✅ **`filters.js` "Check all/Uncheck all" scoping** — the query matched
   every checkbox under `#sidebar`, including the server-fetch "Remember
   password" box: bulk-toggling could silently opt the user into storing
   the dedicated-server admin password (or visually clear it without
   deleting the stored copy).
5. ✅ **Tauri CSP** — `tauri.conf.json` had `"csp": null` next to a `load`
   IPC command that reads arbitrary paths; any future DOM injection would
   have been arbitrary-file-read + exfiltration. Now `default-src 'self'`.
6. ✅ **Parser: clamp untrusted counts** — ~25 sites fed raw u32 counts into
   `Vec::with_capacity`; one corrupted count requested 100+ GB and aborted
   the process instead of returning a parse error. Wrapping arithmetic in
   the `reader.rs` string paths could also trap wasm32 release builds.
7. ✅ **Mapdata: NaN-safe sorts** — NaN fluid amounts from modded/corrupt
   saves panicked `sort_by(...partial_cmp().expect())` comparators and
   killed the worker (`queries.rs`, `geometry.rs` hull).
8. **WebGL alpha compositing mismatch** — `webgl_layer.js` uses
   `premultipliedAlpha:false` with straight-alpha blending, so translucent
   fills over bare map render at ~0.30 effective opacity instead of 0.55
   (alpha applied twice at page composite). Fix: premultiplied output +
   `(ONE, ONE_MINUS_SRC_ALPHA)`; A/B against `?renderer=canvas`.
9. **Tauri chunked-payload generation token** — `payload_slice(offset,len)`
   serves whatever the stash currently holds; a load/edit completing
   between slice pulls can interleave two payloads. Guarded today only by
   UI-level flags in another layer.
10. **Dedicated-server password protection** — no trust-on-first-use cert
    pinning over the (necessarily) unverified TLS, and plaintext
    localStorage persistence where the OS keyring is available natively.

## Remaining findings by area

### Rust parser core (`rust_parser/core/src/`) — solid

- **M** `level.rs:477` — the "missing final array count" quirk path copies
  the entire decompressed body (2× peak memory, wasm OOM risk for
  calculator-resaved files); an 8-byte tail buffer would do.
- **M** Per-property `String` allocations to drive the type `match` in the
  hottest parse loop (`properties.rs`); match on bytes instead.
- **M** No adversarial-input tests: nothing asserts "corrupt input returns
  `Err`, never panics" for `reader.rs`/`properties.rs`/`decompress.rs`.
  A small corpus of truncated/bit-flipped fixtures would be high value.
- **L** `u64→usize as` truncation on wasm32 size fields (misleading error,
  not silent success); dead `decompressed_size` and `build_instance_slots`;
  `store.rs` `parsed_objects()` is a pub `.expect()` accessor.

### Save editor (`rust_parser/core/src/editor/`) — solid

- **M** `apply_plan` (`apply.rs:36-100`) enforces none of its documented
  invariants (sorted, disjoint, patches outside removes). Violations mean
  either silent wrong bytes (same-length writes are invisible to the strict
  re-parse gate) or underflow panics. Cheap `debug_assert`s close it.
- **M** Wire "Locations" rewrite finds the vector by 24-byte f64 pattern
  search (`apply.rs:335`); a numerically-equal other vector (zero-length
  wire) gets silently rewritten — the one corruption class the re-parse
  gate cannot catch.
- **M** `plan_duplicate_actors` / `plan_paste_external` are ~70% the same
  algorithm and have drifted once already (the sign soft-ref exemption
  exists only in the paste path). Unify.
- **M** Every `MoveActors` op parses *all* conveyor chains (full item
  rings) and all power wires even when irrelevant; gate on the moved set.
- **L** No pre-flight bounds pass before mutation (mid-apply error forces a
  full multi-second replay-from-pristine); delete errors say "Cannot copy";
  machines with missing component records can't be deleted on modded saves;
  tombstone names aren't checked against concurrently generated names.

### Mapdata (`rust_parser/core/src/mapdata/`) — good, fragile edges

- **M** Four collectors run twice per load (depot contents, collectables,
  hard drives, drops — payload pass then index pass), plus per-kind rescans
  of huge collectable lists: a few hundred ms of duplication per load.
- **M** Resource nodes missing from the static tables are silently dropped
  from the map (the extracted tables have ~704 known gaps); bucket unknown
  types instead of `continue`.
- **M** Lightweight instance-name strings are materialized 3–4× in the map
  index (formatted IDs, owned keys, decoded Strings) — tens of MB near the
  4 GB wasm ceiling; intern type paths.
- **M** Payload is 2–3× larger than needed: 17-digit floats, full 65-char
  instance paths, world positions derivable client-side — all frozen by
  the retired-Python parity gate. Deciding to end parity unlocks all three.
- **L** Malformed regenerated `buildings.json` (clearance/power-range
  shapes) panics table init for *all* saves — degrade per-class instead;
  hardcoded belt/pipe-rate tables silently exclude modded marks; belt
  ring-buffer slice truncates wrapped windows (inherited Python quirk);
  `find_subslice`/camel-split/`f3`/`f4` utilities triplicated;
  `POWER_CLOCK_SPEED_EXPONENT = 1.321929` deserves a "log₂ 2.5" comment.

### Tauri & WASM shells — good, security gaps closed above

- **M** Session orchestration (~200 lines: load, apply_edits dry-run,
  pristine capture, teardown ordering) is maintained twice — natively in
  `tauri/src/session.rs` and in `wasm/src/lib.rs`. Currently in step and
  documented, but nothing enforces parity.
- **L** wasm query-path traps leave a possibly-corrupt session serving
  (edit paths are defended via `store.take()`); treat any `RuntimeError`
  as fatal and respawn the worker. Failed chunk pulls leak the Rust-side
  stash until the next load; `load`/`apply_edits` run minutes-long work on
  tokio workers without `spawn_blocking`; clipboard slots grow unbounded
  per copy; server-supplied save names aren't checked for Windows reserved
  device names.

### Frontend render core (`map.js`, `webgl_layer.js`) — good

- **H** (accepted) The 2D canvas fallback still carries the unchunked ~2s
  full-map redraw at zoom −1; after a real GL context loss users land on
  it with no warning.
- **M** `_redrawHighlight` does O(n) `ids.indexOf` per frame with a pinned
  tooltip (store the hit index); hitTest's line phase walks every polyline
  per hover tick (grid-index line bboxes); no devicePixelRatio handling in
  the 2D layer, and the pin-sprite DPR bake is negated by a CSS-resolution
  canvas; `depthMask` left false after the outline pass makes next frame's
  depth clear a no-op (correct today only by coincidence); GL stream
  rebuild is the synchronous per-edit latency floor.
- **L** Stale comments that invite bugs (`map.js:92` "filters.js pushes
  buckets directly" — false; `filters.js:313` documents the wrong stride-7
  layout); renderer constants duplicated between canvas and GL with "keep
  in sync" notes; script-tag load order is a silent perf cliff; probe GL
  contexts never released; altitude "Reset" persists a concrete range
  instead of a no-filter sentinel.

### Frontend UI (filters/finditem/editor/selection/…) — solid

- **M** Train "Show only this on map" reads pin buckets with stride 4 but
  the train pin bucket is stride 3 → NaN/misplaced pins whose ids don't
  match coordinates (`finditem.js:607`).
- **M** Find-item searches carry no request generation: a slow older query
  overwrites a newer result and can re-open the modal with the *previous
  save's* data (`tooltip.js` already implements the right guard pattern).
- **M** Sidebar toggles made while a highlight is active are reverted by
  the snapshot restore without resyncing checkboxes.
- **M** After a partial session recovery, `editor.js` leaves `redoStack`
  referencing a state that no longer exists.
- **L** Five uncoordinated document-level Escape handlers dismiss several
  layers per press; label-keyed catalogs collide on duplicate labels;
  `savedVisibility` localStorage grows unboundedly; `el()` helper
  duplicated in six files; lightweight-ID logic duplicated between
  editor.js and selection.js; accent-button CSS recipe copy-pasted ~6×.

### Python / build / CI — weakest layer, largely addressed above

- **M** Fresh-setup breakage: the tile-cache stamp is mtime-based and can
  never survive the `game_data.zip` round-trip, so every fresh setup
  re-cuts all tiles and hits Pillow — which no requirements file declares
  for building. The zip also ships ~1400 guaranteed-dead tile PNGs.
- **M** `build.sh` pipes two unpinned remote installers to `sh` and
  fetches `game_data.zip` without a checksum inside the production deploy
  path; no `set -o pipefail`.
- **L** `package_game_data.py`'s traversal-guard comment is wrong (safety
  actually rests on `ZipFile.extract`); updater signing key sits
  unencrypted at a well-known path; `requirements.txt` incomplete
  (playwright) with a stale Pillow comment; `extract_map_image.py` tells
  you to update a deleted file; two scripts hardcode the local Windows
  username as defaults; CONTRIBUTING points at a Google Drive
  `game_data.zip` while production pulls the GitHub release; 19 upstream
  v1.x tags sit locally (an accidental `git push --tags` would spray
  them); no local `v0.1.4` tag exists despite the published release;
  `.gitignore` says `LAUNCH.md` but the file is `LAUNCH.MD`
  (case-sensitive clones won't ignore it).

## Repo hygiene notes

- Tracking is tight (183 files; all artifact classes gitignored), but the
  working tree carries ~250 MB of ignored residue: personal saves in
  `map/uploads/`, `__pycache__` remnants of the deleted Flask server, and
  `sftp_config.json` with live credentials.
- Local branches `client-side`, `rust-rewrite`, `save-editor`, `tauri-app`,
  `webgl` are fully merged into `main` and deletable; only
  `sav-data-from-save` diverges (1 commit).
