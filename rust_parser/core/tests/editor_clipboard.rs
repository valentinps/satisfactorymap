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

/// Blob JSON -> a pasteExternal op at anchor+delta (v2 compressed payload).
fn op_from_blob(blob_json: &str, delta: [f64; 3]) -> sav_core::editor::EditOp {
    let blob: serde_json::Value = serde_json::from_str(blob_json).unwrap();
    let op = json!([{
        "op": "pasteExternal",
        "saveVersion": blob["saveVersion"],
        "objectVersion": blob["objectVersion"],
        "lightweightVersion": blob["lightweightVersion"],
        "z": blob["z"],
        "zLen": blob["zLen"],
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
    let payload = sav_core::editor::clipboard::inflate_payload(
        blob["z"].as_str().unwrap(),
        blob["zLen"].as_u64().unwrap(),
    )
    .unwrap();
    assert!(payload.actors.len() > 1, "components travel with the actor");

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

/// Six-figure copy/paste on the 600k-object save: run explicitly with
/// `cargo test --release --test editor_clipboard -- --ignored`.
#[test]
#[ignore]
fn paste_100k_objects_scale() {
    let t0 = std::time::Instant::now();
    let source = load("BuildITBIIIIIG_autosave_0.sav");
    println!("source parsed: {:?}", t0.elapsed());

    // Every buildable actor, capped at 100k selected names (component
    // expansion multiplies that).
    let mut names: Vec<String> = Vec::new();
    for level in &source.levels {
        for header in &level.headers {
            if let Header::Actor(a) = header {
                let tp = a.type_path.to_string(&source.data);
                if tp.contains("/Buildable/") && !tp.contains("/Vehicle/") {
                    names.push(a.instance_name.to_string(&source.data));
                    if names.len() >= 100_000 {
                        break;
                    }
                }
            }
        }
    }
    println!("selected {} actors", names.len());
    assert!(names.len() >= 100_000, "save too small for the scale test");

    let t1 = std::time::Instant::now();
    let blob_json = extract_clipboard(&source, &names, &[]).unwrap();
    println!("extracted in {:?}: blob {} MB", t1.elapsed(), blob_json.len() / 1_000_000);
    let blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    let n_records = sav_core::editor::clipboard::inflate_payload(
        blob["z"].as_str().unwrap(),
        blob["zLen"].as_u64().unwrap(),
    )
    .unwrap()
    .actors
    .len();
    println!("blob holds {} records (incl components)", n_records);

    let target = load("BuildITBIIIIIG_autosave_0.sav");
    let count_before: usize = target.levels.iter().map(|l| l.headers.len()).sum();
    let op = op_from_blob(&blob_json, [50000.0, 0.0, 0.0]);
    let tables = ClassTables::embedded();
    let t2 = std::time::Instant::now();
    let target2 = session::step(&target, &op, &tables).unwrap();
    println!("pasted + revalidated in {:?}", t2.elapsed());
    let count_after: usize = target2.levels.iter().map(|l| l.headers.len()).sum();
    assert_eq!(count_after, count_before + n_records);
    println!("total {:?}", t0.elapsed());
}

// First actor with `prefix` whose object carries an mExtractableResource
// ref: (instance name, node path).
fn find_miner_on_node(store: &SaveStore, prefix: &str) -> (String, String) {
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                if !a.type_path.to_string(&store.data).starts_with(prefix) {
                    continue;
                }
                let object = store.parse_object_at(li, oi).unwrap();
                if let Some(r) = sav_core::mapdata::props::object_ref(
                    &object.properties,
                    &store.data,
                    b"mExtractableResource",
                ) {
                    return (
                        a.instance_name.to_string(&store.data),
                        r.path_name.to_string(&store.data),
                    );
                }
            }
        }
    }
    panic!("no {prefix} actor with a node ref in the test save");
}

// A pasted extractor's mExtractableResource after `op` runs against `target`:
// (ref path of the newly created miner-type actor, its instance count delta).
fn pasted_miner_node_ref(
    target: &SaveStore,
    target2: &SaveStore,
    prefix: &str,
) -> Option<String> {
    let scan_before = SaveScan::new(target);
    for (li, level) in target2.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                if a.type_path.to_string(&target2.data).starts_with(prefix)
                    && !scan_before
                        .by_instance_name
                        .contains_key(a.instance_name.bytes(&target2.data))
                {
                    let object = target2.parse_object_at(li, oi).unwrap();
                    return sav_core::mapdata::props::object_ref(
                        &object.properties,
                        &target2.data,
                        b"mExtractableResource",
                    )
                    .map(|r| r.path_name.to_string(&target2.data));
                }
            }
        }
    }
    None
}

/// Cross-save miner paste keeps its resource-node ref: node actors are
/// level-placed with identical names in every save of the map, and both
/// saves agree on the node's resource, so the blob's extRefs entry passes
/// the relink gate instead of being tombstoned.
#[test]
fn pasted_miner_relinks_to_the_same_node() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();
    // Matches MinerMK1/MinerMk2/MinerMk3 paths, not other buildings.
    let prefix = "/Game/FactoryGame/Buildable/Factory/MinerM";
    let (name, node_path) = find_miner_on_node(&source, prefix);

    let blob_json = extract_clipboard(&source, &[name], &[]).unwrap();
    let blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    let payload = sav_core::editor::clipboard::inflate_payload(
        blob["z"].as_str().unwrap(),
        blob["zLen"].as_u64().unwrap(),
    )
    .unwrap();
    let ext = payload.ext_refs.get(&node_path).expect("extRefs records the node");
    assert!(ext.cls.contains("BP_ResourceNode") || ext.cls.contains("BP_Fracking"), "{}", ext.cls);
    assert!(!ext.res.is_empty(), "resource resolved from override or static table");
    assert_eq!(blob["count"], json!(1));

    let op = op_from_blob(&blob_json, [800.0, 0.0, 0.0]);
    let target2 = session::step(&target, &op, &tables).unwrap();
    let pasted_ref = pasted_miner_node_ref(&target, &target2, prefix).expect("pasted miner found");
    assert_eq!(pasted_ref, node_path, "node ref relinked, not tombstoned");

    // The edited target still round-trips.
    let exported = export_sav(&target2.file_header, effective_body(&target2));
    parse_full_save(&exported, &tables, None).unwrap();
}

/// The relink gate severs the ref when the recorded resource does not match
/// what the target save's node yields (the randomized-nodes scenario):
/// tamper the blob's extRefs entry and check the pasted miner's node ref is
/// a tombstone, not the real node.
#[test]
fn miner_ref_is_severed_when_node_resource_differs() {
    let source = load("All_autosave_0.sav");
    let target = load("All_autosave_1.sav");
    let tables = ClassTables::embedded();
    let prefix = "/Game/FactoryGame/Buildable/Factory/MinerM";
    let (name, node_path) = find_miner_on_node(&source, prefix);

    let blob_json = extract_clipboard(&source, &[name], &[]).unwrap();
    let mut blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    let mut payload = sav_core::editor::clipboard::inflate_payload(
        blob["z"].as_str().unwrap(),
        blob["zLen"].as_u64().unwrap(),
    )
    .unwrap();
    payload.ext_refs.get_mut(&node_path).expect("recorded").res = "Desc_SomethingElse_C".into();
    let raw = serde_json::to_vec(&payload).unwrap();
    let (compressed, raw_len) = sav_core::editor::session::compress_body(&raw);
    blob["z"] = json!(base64_std(&compressed));
    blob["zLen"] = json!(raw_len as u64);

    let op = op_from_blob(&blob.to_string(), [800.0, 0.0, 0.0]);
    let target2 = session::step(&target, &op, &tables).unwrap();
    let pasted_ref = pasted_miner_node_ref(&target, &target2, prefix).expect("pasted miner found");
    assert_ne!(pasted_ref, node_path, "mismatched resource must not relink");
    assert_eq!(pasted_ref.len(), node_path.len(), "tombstone is same-length");
}

fn base64_std(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
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
