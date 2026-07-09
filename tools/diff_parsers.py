#!/usr/bin/env python3
"""Differential harness: parse a save with both the Python reference parser
(patches/sav_parse.py) and the Rust parser (sav_parse_rs), canonicalize both
object graphs, and diff them. Used as the regression gate for the Rust port.

Usage: python tools/diff_parsers.py [save.sav ...]
       (no args: every .sav under map/uploads/ and sftp_saves/)
"""

import datetime
import glob
import hashlib
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, "parser"))
sys.path.insert(0, os.path.join(REPO, "patches"))

import sav_parse  # noqa: E402  (patches copy shadows parser copy)
import sav_data.data  # noqa: E402
import sav_parse_rs  # noqa: E402

OBJECT_REF_TYPES = (sav_parse.ObjectReference, sav_parse_rs.ObjectReference)


def canon(v):
    """Normalize a parsed value into a JSON-able tree that is bit-exact for
    floats and distinguishes tuples from lists (toString renders them
    differently, so shape class matters)."""
    if v is None or isinstance(v, (str, int)):
        return v
    if isinstance(v, bool):
        return v
    if isinstance(v, float):
        return ["F", v.hex()]
    if isinstance(v, (bytes, bytearray)):
        return ["B", bytes(v).hex()]
    if isinstance(v, tuple):
        return ["T"] + [canon(x) for x in v]
    if isinstance(v, list):
        return [canon(x) for x in v]
    if isinstance(v, OBJECT_REF_TYPES):
        return ["OR", v.levelName, v.pathName]
    if isinstance(v, datetime.datetime):
        return ["DT", v.isoformat()]
    if isinstance(v, sav_parse_rs.PropertyList):
        return [canon(x) for x in v]
    raise TypeError(f"canon: unhandled type {type(v)!r}: {v!r}")


def dump_header(h):
    if hasattr(h, "typePath"):
        return ["A", h.typePath, h.rootObject, h.instanceName, h.flags,
                h.needTransform, canon(h.rotation), canon(h.position),
                canon(h.scale), h.wasPlacedInLevel]
    return ["C", h.className, h.rootObject, h.instanceName, h.flags,
            h.parentActorName]


def dump_object(o):
    return [
        o.instanceName,
        o.objectGameVersion,
        o.shouldMigrateObjectRefsToPersistentFlag,
        canon(o.perObjectVersionData),
        canon(o.actorReferenceAssociations),
        canon(list(o.properties)),
        canon(o.propertyTypes),
        canon(o.actorSpecificInfo),
    ]


def dump_info(info):
    return {f: canon(getattr(info, f)) for f in (
        "saveHeaderType", "saveVersion", "buildVersion", "saveName", "mapName",
        "mapOptions", "sessionName", "playDurationInSeconds",
        "saveDateTimeInTicks", "saveDatetime", "sessionVisibility",
        "editorObjectVersion", "modMetadata", "isModdedSave", "saveIdentifier",
        "saveDataHash", "isCreativeModeEnabled")}


def first_diff(a, b, path="$"):
    """Walk two canonical trees, return the first divergent path."""
    if type(a) is not type(b):
        return f"{path}: type {type(a).__name__} != {type(b).__name__} ({a!r:.120} vs {b!r:.120})"
    if isinstance(a, dict):
        for k in a:
            if k not in b:
                return f"{path}.{k}: missing on rust side"
            d = first_diff(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: len {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_diff(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    if a != b:
        return f"{path}: {a!r:.200} != {b!r:.200}"
    return None


def digest(tree):
    return hashlib.sha256(json.dumps(tree, sort_keys=True).encode()).hexdigest()


def digest_save(parsed, extras):
    """Streaming per-piece digests so two 600k-object graphs never coexist
    fully expanded in memory. Returns {piece_key: sha}."""
    out = {"preamble": digest({
        "info": dump_info(parsed.saveFileInfo),
        "persistentVersionData": canon(parsed.persistentLevelSaveObjectVersionData),
        "partitions": canon(parsed.partitions),
        "aLevelName": parsed.aLevelName,
        "dropPods": canon(parsed.dropPodObjectReferenceList),
        "extraRefs": canon(parsed.extraObjectReferenceList),
        "calculatorExtras": list(extras),
    })}
    for li, lv in enumerate(parsed.levels):
        out[f"L{li}.misc"] = digest({
            "name": lv.levelName,
            "persistentFlag": lv.levelPersistentFlag,
            "saveVersion": lv.levelSaveVersion,
            "collectables1": canon(lv.collectables1),
            "collectables2": canon(lv.collectables2),
            "versionData": canon(lv.saveObjectVersionData),
        })
        for hi, h in enumerate(lv.actorAndComponentObjectHeaders):
            out[f"L{li}.h{hi}"] = digest(dump_header(h))
        for oi, o in enumerate(lv.objects):
            out[f"L{li}.o{oi}"] = digest(dump_object(o))
    return out


def piece_detail(parsed, extras, key):
    """Re-dump a single piece for failure reporting."""
    if key == "preamble":
        return {
            "info": dump_info(parsed.saveFileInfo),
            "persistentVersionData": canon(parsed.persistentLevelSaveObjectVersionData),
            "partitions": canon(parsed.partitions),
            "aLevelName": parsed.aLevelName,
            "dropPods": canon(parsed.dropPodObjectReferenceList),
            "extraRefs": canon(parsed.extraObjectReferenceList),
            "calculatorExtras": list(extras),
        }
    lpart, rest = key.split(".", 1)
    lv = parsed.levels[int(lpart[1:])]
    if rest == "misc":
        return {
            "name": lv.levelName,
            "persistentFlag": lv.levelPersistentFlag,
            "saveVersion": lv.levelSaveVersion,
            "collectables1": canon(lv.collectables1),
            "collectables2": canon(lv.collectables2),
            "versionData": canon(lv.saveObjectVersionData),
        }
    if rest.startswith("h"):
        return dump_header(lv.actorAndComponentObjectHeaders[int(rest[1:])])
    return dump_object(lv.objects[int(rest[1:])])


def run(sav):
    name = os.path.basename(sav)
    py_parsed = sav_parse.readFullSaveFile(sav)
    py_digests = digest_save(py_parsed, sav_parse.satisfactoryCalculatorInteractiveMapExtras)
    del py_parsed

    rs_parsed = sav_parse_rs.read_full_save_file(sav, list(sav_data.data.CONVEYOR_BELTS))
    rs_digests = digest_save(rs_parsed, rs_parsed.calculatorExtras)

    if py_digests == rs_digests:
        print(f"OK   {name}  ({len(py_digests)} pieces)")
        return True

    keys = sorted(set(py_digests) | set(rs_digests))
    bad = [k for k in keys if py_digests.get(k) != rs_digests.get(k)]
    print(f"FAIL {name}: {len(bad)} divergent pieces, first: {bad[0]}")
    # Pinpoint: re-parse Python side for the first divergent piece only.
    py_parsed = sav_parse.readFullSaveFile(sav)
    a = piece_detail(py_parsed, sav_parse.satisfactoryCalculatorInteractiveMapExtras, bad[0])
    b = piece_detail(rs_parsed, rs_parsed.calculatorExtras, bad[0])
    print("  ", first_diff(a, b, bad[0]))
    return False


def main():
    saves = sys.argv[1:] or sorted(
        glob.glob(os.path.join(REPO, "map", "uploads", "*.sav"))
        + glob.glob(os.path.join(REPO, "sftp_saves", "*.sav"))
    )
    ok = True
    for sav in saves:
        try:
            ok &= run(sav)
        except Exception as e:
            import traceback
            traceback.print_exc()
            print(f"ERR  {os.path.basename(sav)}: {type(e).__name__}: {e}")
            ok = False
    print("RESULT:", "GREEN" if ok else "RED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
