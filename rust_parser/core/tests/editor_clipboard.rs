//! Cross-save clipboard tests: extract a blob from one save and paste it
//! into a DIFFERENT parsed save (the two-tabs scenario), with version gates.

use sav_core::editor::clipboard::extract_clipboard;
use sav_core::editor::ops::{parse_ops_json, LwRef};
use sav_core::editor::{effective_body, export_sav, session};
use sav_core::level::parse_full_save;
use sav_core::mapdata::scan::SaveScan;
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, Header, SaveStore};
use serde_json::json;
use std::path::PathBuf;

fn load(name: &str) -> SaveStore {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = std::fs::read(path).expect("test save present");
    parse_full_save(&bytes, &ClassTables::embedded(), None).unwrap()
}

fn find_actor(store: &SaveStore, prefix: &str) -> (usize, usize, String) {
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                if a.type_path.to_string(&store.data).starts_with(prefix) {
                    return (li, oi, a.instance_name.to_string(&store.data));
                }
            }
        }
    }
    panic!("no actor with type prefix {prefix}");
}

fn count_of_type(store: &SaveStore, prefix: &str) -> usize {
    store
        .levels
        .iter()
        .flat_map(|l| &l.headers)
        .filter(|h| matches!(h, Header::Actor(a) if a.type_path.to_string(&store.data).starts_with(prefix)))
        .count()
}

/// Blob JSON -> a pasteExternal op at anchor+delta.
fn op_from_blob(blob_json: &str, delta: [f64; 3]) -> sav_core::editor::EditOp {
    let blob: serde_json::Value = serde_json::from_str(blob_json).unwrap();
    let op = json!([{
        "op": "pasteExternal",
        "saveVersion": blob["saveVersion"],
        "objectVersion": blob["objectVersion"],
        "lightweightVersion": blob["lightweightVersion"],
        "actors": blob["actors"],
        "lightweight": blob["lightweight"],
        "anchor": blob["anchor"],
        "delta": delta,
        "seed": 4242u64,
    }]);
    parse_ops_json(&op.to_string()).unwrap().remove(0)
}

#[test]
fn paste_constructor_into_another_save() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();
    let prefix = "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/";
    let (_, _, name) = find_actor(&source, prefix);

    let blob_json = extract_clipboard(&source, &[name], &[]).unwrap();
    let blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    assert!(blob["actors"].as_array().unwrap().len() > 1, "components travel with the actor");

    let before = count_of_type(&target, prefix);
    let op = op_from_blob(&blob_json, [5000.0, -3000.0, 0.0]);
    let target2 = session::step(&target, &op, &tables).unwrap();

    assert_eq!(count_of_type(&target2, prefix), before + 1);
    // The pasted constructor sits at the source anchor + delta.
    let anchor = blob["anchor"].as_array().unwrap();
    let (ax, ay) = (anchor[0].as_f64().unwrap(), anchor[1].as_f64().unwrap());
    let scan_before = SaveScan::new(&target);
    let mut found = false;
    for (li, level) in target2.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                if a.type_path.to_string(&target2.data).starts_with(prefix)
                    && !scan_before
                        .by_instance_name
                        .contains_key(a.instance_name.bytes(&target2.data))
                {
                    // (source actor is at the anchor: it's the only actor in
                    // the blob, so bbox center == its position)
                    assert!((a.position[0] as f64 - (ax + 5000.0)).abs() < 1.0, "x {}", a.position[0]);
                    assert!((a.position[1] as f64 - (ay - 3000.0)).abs() < 1.0, "y {}", a.position[1]);
                    // Its components exist in the target.
                    let scan2 = SaveScan::new(&target2);
                    let object = target2.parse_object_at(li, oi).unwrap();
                    if let Some((_, comps)) = &object.actor_reference_associations {
                        assert!(!comps.is_empty());
                        for comp in comps {
                            assert!(
                                scan2.by_instance_name.contains_key(comp.path_name.bytes(&target2.data)),
                                "component missing in target"
                            );
                        }
                    }
                    found = true;
                }
            }
        }
    }
    assert!(found, "pasted constructor not found");

    // The edited target round-trips through export.
    let exported = export_sav(&target2.file_header, effective_body(&target2));
    parse_full_save(&exported, &tables, None).unwrap();
}

#[test]
fn paste_lightweight_into_another_save() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();

    // First lightweight type that exists in BOTH saves.
    let groups_of = |s: &SaveStore| -> Vec<String> {
        for level in &s.levels {
            for object in level.parsed_objects() {
                if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                    return items.iter().map(|g| g.type_path.to_string(&s.data)).collect();
                }
            }
        }
        Vec::new()
    };
    let source_groups = groups_of(&source);
    let target_groups = groups_of(&target);
    let Some(type_path) = source_groups.iter().find(|t| target_groups.contains(t)) else {
        eprintln!("no shared lightweight type; skipping");
        return;
    };

    let lw = vec![LwRef { type_path: type_path.clone(), index: 0 }];
    let blob_json = extract_clipboard(&source, &[], &lw).unwrap();

    let count_in = |s: &SaveStore| -> usize {
        for level in &s.levels {
            for object in level.parsed_objects() {
                if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                    return items
                        .iter()
                        .find(|g| g.type_path.eq_ascii(&s.data, type_path))
                        .map(|g| g.instances.len())
                        .unwrap_or(0);
                }
            }
        }
        0
    };
    let before = count_in(&target);
    let op = op_from_blob(&blob_json, [2000.0, 2000.0, 0.0]);
    let target2 = session::step(&target, &op, &tables).unwrap();
    assert_eq!(count_in(&target2), before + 1);
}

#[test]
fn paste_refuses_version_mismatch() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();
    let (_, _, name) = find_actor(&source, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let blob_json = extract_clipboard(&source, &[name], &[]).unwrap();

    // Corrupt the save version.
    let mut blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    blob["saveVersion"] = json!(13);
    let op = op_from_blob(&blob.to_string(), [0.0, 0.0, 0.0]);
    let err = match session::step(&target, &op, &tables) {
        Ok(_) => panic!("version mismatch should be refused"),
        Err(e) => e,
    };
    assert!(err.msg.contains("not byte-compatible"), "{}", err.msg);
}

#[test]
fn paste_is_deterministic_for_replay() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();
    let (_, _, name) = find_actor(&source, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let blob_json = extract_clipboard(&source, &[name], &[]).unwrap();
    let op = op_from_blob(&blob_json, [1234.0, 0.0, 0.0]);

    let pristine = effective_body(&target).to_vec();
    let a = session::rebuild(pristine.clone(), &target.file_header, &target.info, &tables, std::slice::from_ref(&op), None).unwrap();
    let b = session::rebuild(pristine, &target.file_header, &target.info, &tables, std::slice::from_ref(&op), None).unwrap();
    assert_eq!(a.data, b.data);
}
