# sav_core / sav_wasm — Rust save parser + map-data builder

Cargo workspace:

- **`core/` (`sav_core`)** — the whole pipeline, pure Rust: chunked zlib
  decompression, save parsing into a compact `SaveStore` (the decompressed
  buffer is retained; strings are zero-copy `u32` ranges into it), and
  `mapdata/` — the map-payload builder, save index, and the six detail
  queries the frontend uses (`describeInstance`, `findItemLocations`,
  building/vehicle/train info, selection inventory). Static game data
  (`game_data/generated/*.json`, `game_data/sav_data/*.json`, the item-icon
  manifest) is embedded at compile time, so building requires the game data
  extracted first (see the root README).
- **`wasm/` (`sav_wasm`)** — the wasm-bindgen boundary. `SaveSession::load`
  parses a `.sav` from bytes and builds payload + index; the query methods
  return JSON strings. Loaded by `map/static/map/worker.js` inside a Web
  Worker (built with `wasm-pack --target no-modules` by
  `tools/build_site.py`).

## Design notes

- Single-threaded on wasm; the `parallel` feature enables rayon chunk
  decompression for native builds (same chunk order, identical output
  bytes).
- Progress reporting is a plain `FnMut(phase, current, total)` driven
  synchronously from inside the parse/build; the worker forwards it to the
  page as `postMessage` events.
- `mapdata/` is a behavioral port of the original Python
  `map/sav_map_data.py` (see git history / the `main` branch). It was landed
  collector-by-collector behind a bit-exact differential gate against the
  Python reference (order-strict JSON comparison, `float.hex()` equality),
  which is why the code deliberately reproduces Python quirks: dict
  insertion-order semantics (IndexMap; last-value-wins/first-position-kept),
  CPython's own `math.hypot` algorithm (`jsonval::py_hypot`), banker's
  rounding (`py_round`), exact Python `repr(float)` (`display.rs`),
  `rem_euclid` for Python `%`, absence-vs-null-vs-0 distinctions. Do not
  "simplify" these without understanding what they mirror.
- Decompressed-save size cap is **wasm32-only**: `StrRef`/`DataRef` and the
  store's span/offset fields use `usize` offsets, which is `u32` on wasm32
  (the browser build — a 4GB address space it can't exceed anyway) and 64-bit
  on native (the desktop app), so native has no cap. The guard in
  `decompress.rs` is `#[cfg(target_pointer_width = "32")]`.

## Rebuilding

```bash
cargo test -p sav_core            # unit tests (fixtures generated from CPython)
py tools/build_site.py            # wasm build + site assembly into dist/
```
