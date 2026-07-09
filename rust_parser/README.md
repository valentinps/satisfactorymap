# sav_parse_rs — Rust save parser

Rust (PyO3) rewrite of the Satisfactory save parser. Drop-in backend for the
map server behind `map/sav_parse_shim.py`; the pure-Python reference
(`patches/sav_parse.py`) remains the source of truth for the format and the
runtime fallback.

## Design

- **Data stays on the Rust side.** `readFullSaveFile` parses into a compact
  `SaveStore` (the decompressed buffer is retained; strings are zero-copy
  ranges into it). Python sees thin handle classes (`ParsedSave`, `Level`,
  `ActorHeader`, `ComponentHeader`, `Object`, `PropertyList`,
  `ObjectReference`), each holding an `Arc<SaveStore>` — the store lives as
  long as any handle, so the server's cached `saveIndex` (which stores live
  header/object handles) is safe.
- **Lazy conversion.** `Object.properties` is a Rust-backed `PropertyList`.
  `sav_parse_shim.getPropertyValue()` resolves lookups in Rust and converts
  only the matched value to Python (memoized per property). Converted values
  are exact reproductions of the Python parser's shapes (nested
  `[props, propTypes]` pairs, `Int8` as 1-byte `bytes`, TextProperty history
  lists, etc.), so all `sav_map_data.py` consumers work unchanged.
- **Parallel decompression** (rayon) of the zlib chunk stream.
- **Progress**: the parse thread updates atomics; a GIL-side poller invokes
  the Python callback ~10×/s. The shim adapts this to `ProgressBar` (looked
  up at call time, so the server's `_ProgressBarHook` monkey-patch works).
- Class dispatch tables (conveyor belt classes) are passed in from
  `sav_data.data` at call time — Python stays the single source of truth.

## Building

One-time setup (already done on this machine): VS Build Tools 2022 C++
workload, rustup (MSVC toolchain), `pip install maturin` in the server's
Python environment. Then:

```
cd rust_parser
maturin develop --release
```

Backend selection: `SAV_PARSE_IMPL=rust|py` (default: Rust when importable,
with stderr notice on fallback).

## Regression gates — run after touching either parser

```
python tools/diff_parsers.py            # structural parity, every local save
python tools/diff_payload.py <saves>    # buildMapPayload/buildSaveIndex/
                                        # describeInstance/findItemLocations parity
python tools/bench_parse.py <saves>     # timings for both backends
```

`diff_parsers.py` canonicalizes every header, object (properties,
propertyTypes, actorSpecificInfo), collectable and quirk marker from both
parsers and compares SHA-256 digests piece-by-piece; floats are compared via
`float.hex()` (bit-exact). Any future change to `patches/sav_parse.py` must
be mirrored here and validated with these tools.

## Benchmarks (this machine, 16 cores, Anaconda Python 3.12)

Full server load cycle = readFullSaveFile + buildMapPayload + buildSaveIndex
+ json.dumps.

| Save | Backend | parse | payload | index | jsonify | total |
|---|---|---|---|---|---|---|
| solo_autosave_1.sav (15MB, 332MB decompressed) | Python | 20.1s | 1.5s | 1.4s | 0.6s | 23.7s |
| | Rust | **0.8s** | 4.4s | 2.8s | 0.6s | **8.6s** |
| BuildITBIIIIIG_autosave_8.sav (50MB, 1.05GB decompressed, ~600k objects) | Python | 60.8s | 5.5s | 4.6s | 2.7s | 73.6s |
| | Rust | **2.4s** | 13.0s | 8.5s | 2.4s | **26.3s** |

Parse speedup ~25×; full request cycle ~2.8×.

(The payload/index phases are slower under the Rust backend because property
values convert on demand there — under Python that cost is paid inside the
parse phase. Totals are the comparable number.)
