//! A body this parser cannot read must not sink the whole save.
//!
//! Modded saves serialise property shapes sav_core has never seen (the case
//! that prompted this: Modular Load Balancers stores a native 12-byte struct
//! as a MapProperty value, where the parser expects a tagged property list).
//! The build skips such an object and reports it instead of failing, and the
//! object still draws on the map because buildings render off the header.

use sav_core::level::parse_full_save_lean;
use sav_core::mapdata;
use sav_core::mapdata::index::MapIndex;
use sav_core::mapdata::scan::ParseFailures;
use sav_core::object::ClassTables;
use sav_core::store::{Header, SaveStore};
use std::path::PathBuf;

fn load_lean(name: &str) -> SaveStore {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = std::fs::read(path).expect("test save present");
    parse_full_save_lean(&bytes, &ClassTables::embedded(), None).unwrap()
}

const SAVE: &str = "All_080726-163150.sav";

/// Find a buildable whose body parses today, and return its slot, type path
/// and instance name. Buildable so the "still renders" half is testable.
fn a_parsable_buildable(store: &SaveStore) -> ((usize, usize), String, String) {
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Actor(actor) = header else { continue };
            let type_path = actor.type_path.to_string(&store.data);
            if !type_path.contains("/Build_") || store.parse_object_at(li, oi).is_err() {
                continue;
            }
            return ((li, oi), type_path, actor.instance_name.to_string(&store.data));
        }
    }
    panic!("no parsable buildable in {SAVE}");
}

/// 0xFF over the whole body span: every length prefix in it becomes absurd,
/// so the property walk fails wherever it first looks. The headers (and the
/// recorded spans) are untouched -- exactly the shape of the real bug, where
/// the header is fine and only the body is unreadable.
fn clobber_body(store: &mut SaveStore, (li, oi): (usize, usize)) {
    let (off, len) = store.levels[li].object_spans[oi];
    store.data[off..off + len as usize].fill(0xFF);
    assert!(
        store.parse_object_at(li, oi).is_err(),
        "clobbered body still parsed -- the test corrupts nothing",
    );
}

#[test]
fn clean_save_reports_no_unreadable_objects() {
    let store = load_lean(SAVE);
    let (payload, index) = mapdata::build_all_json(&store, None).expect("build");
    assert!(index.parse_failures.is_empty());
    // Absent entirely rather than a zero count: nearly every save is clean,
    // and the frontend keys the notice off the key's presence.
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert!(payload.get("unreadableObjects").is_none());
}

#[test]
fn unreadable_body_is_skipped_and_reported_not_fatal() {
    let mut store = load_lean(SAVE);
    let (slot, type_path, instance_name) = a_parsable_buildable(&store);
    clobber_body(&mut store, slot);

    // The whole point: this used to be Err.
    let (payload, index) = mapdata::build_all_json(&store, None).expect("build must not fail");

    assert_eq!(index.parse_failures.len(), 1);
    assert!(index.parse_failures.names.contains(instance_name.as_bytes()));

    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    let report = &payload["unreadableObjects"];
    assert_eq!(report["count"], 1);
    assert!(
        report["samples"][0].as_str().unwrap().contains(&type_path),
        "sample should name the class: {report}",
    );
}

/// The map keeps the building: position/rotation/typePath live in the header,
/// which parses fine -- only the body-derived detail is lost.
#[test]
fn unreadable_buildable_still_renders() {
    let store = load_lean(SAVE);
    let (slot, type_path, instance_name) = a_parsable_buildable(&store);
    let short_name = instance_name.rsplit('.').next().unwrap().to_string();

    let mut broken = load_lean(SAVE);
    clobber_body(&mut broken, slot);
    let (payload, _) = mapdata::build_all_json(&broken, None).expect("build");
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();

    let rendered = payload["buildingCategories"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|category| category["types"].as_array().unwrap())
        .filter(|bucket| bucket["typePath"] == type_path.as_str())
        .any(|bucket| {
            bucket["ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id.as_str() == Some(short_name.as_str()))
        });
    assert!(rendered, "{instance_name} vanished from the map after its body failed to parse");
}

/// Edits stay strictly checked: an object that existed and parsed before an
/// edit must still parse after it. Pre-existing failures and objects the
/// edit itself created are not that.
#[test]
fn edit_gate_rejects_only_newly_broken_existing_objects() {
    // A save holding two objects, one of them unreadable modded content.
    let mut before = MapIndex::build(&load_lean(SAVE));
    before.by_instance_name.clear();
    for name in [b"modded_thing".as_slice(), b"was_fine".as_slice()] {
        before.by_instance_name.insert(name.to_vec(), (0, 0));
    }
    before.parse_failures = ParseFailures::default();
    before.parse_failures.names.insert(b"modded_thing".to_vec());

    let after = |failures: &[&[u8]]| {
        let mut index = MapIndex::build(&load_lean(SAVE));
        index.parse_failures = ParseFailures::default();
        for name in failures {
            index.parse_failures.names.insert(name.to_vec());
        }
        index
    };

    // Unchanged, and "the edit removed the broken object", are both fine.
    assert!(after(&[b"modded_thing"]).reject_new_parse_failures(Some(&before)).is_ok());
    assert!(after(&[]).reject_new_parse_failures(Some(&before)).is_ok());

    // An object that parsed before this edit and no longer does: corruption.
    let err = after(&[b"modded_thing", b"was_fine"])
        .reject_new_parse_failures(Some(&before))
        .unwrap_err();
    assert!(err.contains("was_fine"), "{err}");
    assert!(!err.contains("modded_thing"), "pre-existing failure reported as new: {err}");

    // Pasting an unreadable buildable splices its bytes under a fresh name --
    // a new unreadable object, not damage to anything that existed.
    assert!(after(&[b"modded_thing", b"pasted_copy"])
        .reject_new_parse_failures(Some(&before))
        .is_ok());

    // No baseline (a session recovering from a failed edit) must not read as
    // "everything is new".
    assert!(after(&[b"was_fine"]).reject_new_parse_failures(None).is_ok());
}
