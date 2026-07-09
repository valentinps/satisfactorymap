#!/usr/bin/env python3
# Parser backend selector: exposes the sav_parse API surface the map code
# consumes, backed by the Rust parser (sav_parse_rs) when available, else the
# pure-Python reference (patches/sav_parse.py). Force with SAV_PARSE_IMPL=py
# or SAV_PARSE_IMPL=rust.
#
# The Rust ParsedSave keeps the parsed data on the Rust side; property access
# goes through getPropertyValue() below, which converts only the requested
# value. tools/diff_parsers.py is the parity regression gate between the two
# backends -- run it after touching either parser.

import os
import sys

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for _sub in ("parser", "patches"):
   _p = os.path.join(_REPO_ROOT, _sub)
   if _p not in sys.path:
      sys.path.insert(0, _p)
# patches/ must shadow parser/ (same contract as sav_map_server's own setup).
_patches = os.path.join(_REPO_ROOT, "patches")
if sys.path.index(_patches) > sys.path.index(os.path.join(_REPO_ROOT, "parser")):
   sys.path.remove(_patches)
   sys.path.insert(0, _patches)

import sav_parse as _py  # the patches copy

# Always from the Python module: these operate on plain strings / converted
# values identically for both backends. ProgressBar lives here so the server's
# monkey-patch (sav_parse.ProgressBar = hook) affects the Rust path too.
toString = _py.toString
pathNameToReadableName = _py.pathNameToReadableName
ProgressBar = _py.ProgressBar
readSaveFileInfo = _py.readSaveFileInfo

_impl = os.environ.get("SAV_PARSE_IMPL", "").strip().lower()
_rs = None
if _impl != "py":
   try:
      import sav_parse_rs as _rs
   except ImportError:
      if _impl == "rust":
         raise
      print("sav_parse_shim: sav_parse_rs not importable, falling back to the Python parser",
            file=sys.stderr)

ACTIVE_IMPL = "python" if _rs is None else "rust"

# Mirrors the reference module's global; refreshed on every readFullSaveFile.
satisfactoryCalculatorInteractiveMapExtras = []

if _rs is None:
   ParseError = _py.ParseError
   ActorHeader = _py.ActorHeader
   ComponentHeader = _py.ComponentHeader
   ObjectReference = _py.ObjectReference
   ParsedSave = _py.ParsedSave
   getPropertyValue = _py.getPropertyValue

   def readFullSaveFile(filename, decompressedOutputFilename=None):
      global satisfactoryCalculatorInteractiveMapExtras
      parsed = _py.readFullSaveFile(filename, decompressedOutputFilename)
      satisfactoryCalculatorInteractiveMapExtras = _py.satisfactoryCalculatorInteractiveMapExtras
      return parsed

else:
   import sav_data.data as _sav_data

   ParseError = _rs.ParseError
   ActorHeader = _rs.ActorHeader
   ComponentHeader = _rs.ComponentHeader
   ObjectReference = _rs.ObjectReference
   ParsedSave = _rs.ParsedSave
   PropertyList = _rs.PropertyList

   # Dispatch lives in Rust (sav_parse_rs.get_property_value): fast path for
   # Rust-backed top-level property lists (converts only the matched value),
   # reference-equivalent loop for nested plain-list property lists.
   getPropertyValue = _rs.get_property_value

   def readFullSaveFile(filename, decompressedOutputFilename=None):
      global satisfactoryCalculatorInteractiveMapExtras
      if decompressedOutputFilename is not None:
         # Debug-only path; the Rust backend doesn't write the decompressed
         # dump, so use the reference implementation.
         parsed = _py.readFullSaveFile(filename, decompressedOutputFilename)
         satisfactoryCalculatorInteractiveMapExtras = _py.satisfactoryCalculatorInteractiveMapExtras
         return parsed

      # Progress bridge: phase 0 = decompression (file bytes), phase 1 =
      # parsing (level bytes) -- same prefixes as the reference parser so the
      # server's ProgressBar hook reports the same phases. ProgressBar is
      # looked up on this module at call time to honor monkey-patching.
      bars = {}

      def _progress(phase, current, total):
         prefix = "Decompression: " if phase == 0 else "      Parsing: "
         bar = bars.get(phase)
         if bar is None or bar.total != total:
            bar = bars[phase] = ProgressBar(total, prefix)
         bar.set(current)

      parsed = _rs.read_full_save_file(filename, list(_sav_data.CONVEYOR_BELTS), _progress)
      for bar in bars.values():
         bar.complete()
      satisfactoryCalculatorInteractiveMapExtras = list(parsed.calculatorExtras)
      if len(satisfactoryCalculatorInteractiveMapExtras) > 0:
         print(f"File suspected of having been saved by satisfactory-calculator.com/en/interactive-map for {len(satisfactoryCalculatorInteractiveMapExtras)} reasons.",
               file=sys.stderr)
      return parsed
