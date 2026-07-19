//! Lazy-object-model tests: span reparse equivalence (the proof that queries
//! can re-parse single objects on demand after drop_object_model), the drop
//! itself, and edit-after-drop rehydration.

use sav_core::editor::effective_body;
use sav_core::editor::ops::EditOp;
use sav_core::editor::session;
use sav_core::level::{parse_body_bytes_lean, parse_full_save, parse_full_save_lean};
use sav_core::object::ClassTables;
use sav_core::store::{Header, SaveStore};
use std::path::PathBuf;

fn save_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name)
}

fn load(name: &str) -> SaveStore {
    let bytes = std::fs::read(save_path(name)).expect("test save present");
    parse_full_save(&bytes, &ClassTables::embedded(), None).unwrap()
}

fn load_lean(name: &str) -> SaveStore {
    let bytes = std::fs::read(save_path(name)).expect("test save present");
    parse_full_save_lean(&bytes, &ClassTables::embedded(), None).unwrap()
}

/// Every object re-parsed from its recorded span must equal the eagerly
/// parsed one. Debug output covers every field (all types derive Debug and
/// StrRef/DataRef offsets are deterministic for identical spans), so string
/// equality is full structural equality. Once the query layer always
/// re-parses, this equivalence is what guarantees query parity.
#[test]
fn span_reparse_matches_eager_parse() {
    let store = load("All_autosave_0.sav");
    let mut checked = 0usize;
    for (li, level) in store.levels.iter().enumerate() {
        let objects = level.parsed_objects();
        for oi in 0..objects.len() {
            let reparsed = store
                .parse_object_at(li, oi)
                .unwrap_or_else(|e| panic!("reparse failed at ({li},{oi}): {}", e.msg));
            let eager = format!("{:?}", objects[oi]);
            let lazy = format!("{:?}", reparsed);
            assert_eq!(eager, lazy, "object mismatch at ({li},{oi})");
            checked += 1;
        }
    }
    assert!(checked > 1000, "suspiciously few objects checked: {checked}");
}

/// drop_object_model frees the model; headers/spans/data stay usable and
/// re-parse still works.
#[test]
fn drop_then_reparse_on_demand() {
    let mut store = load("All_autosave_0.sav");
    assert!(store.has_object_model());

    // Remember one actor with associations to spot-check after the drop.
    let mut probe: Option<(usize, usize, String)> = None;
    'outer: for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Actor(a) = header {
                if level.parsed_objects()[oi].actor_reference_associations.is_some() {
                    probe = Some((li, oi, format!("{:?}", level.parsed_objects()[oi])));
                    break 'outer;
                }
                let _ = a;
            }
        }
    }
    let (li, oi, eager) = probe.expect("save has an actor with associations");

    store.drop_object_model();
    assert!(!store.has_object_model());
    for level in &store.levels {
        assert!(level.objects.is_none());
        assert_eq!(level.headers.len(), level.object_spans.len());
    }

    let reparsed = store.parse_object_at(li, oi).unwrap();
    assert_eq!(eager, format!("{:?}", reparsed));
}

/// The lean parse (skip mode) must produce identical headers, spans and body
/// to the full parse, with every Level.objects None -- and objects re-parsed
/// from those spans must match the eager ones.
#[test]
fn lean_parse_spans_match_full_parse() {
    let full = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();

    // Feed the lean parser the full parse's own body (bytes start at the u64
    // uncompressedSize, no quirk pad), exactly like load_lean does.
    let mut body = effective_body(&full).to_vec();
    // effective_body strips the pad; parse_body_bytes expects the raw
    // decompressed shape, which effective_body already matches.
    let lean = parse_body_bytes_lean(
        std::mem::take(&mut body),
        full.file_header.clone(),
        full.info.clone(),
        &tables,
        None,
    )
    .unwrap();

    assert!(!lean.has_object_model());
    assert_eq!(lean.data, full.data, "bodies diverge");
    assert_eq!(lean.levels.len(), full.levels.len());
    for (ll, fl) in lean.levels.iter().zip(&full.levels) {
        assert!(ll.objects.is_none());
        assert_eq!(ll.object_ue5_version, fl.object_ue5_version);
        assert_eq!(ll.header_spans, fl.header_spans);
        assert_eq!(ll.object_spans, fl.object_spans);
        assert_eq!(format!("{:?}", ll.spans), format!("{:?}", fl.spans));
        assert_eq!(format!("{:?}", ll.headers), format!("{:?}", fl.headers));
    }

    // Objects re-parsed from the lean store's spans == the eager objects.
    for (li, fl) in full.levels.iter().enumerate() {
        for (oi, eager) in fl.parsed_objects().iter().enumerate() {
            let reparsed = lean.parse_object_at(li, oi).unwrap();
            assert_eq!(
                format!("{:?}", eager),
                format!("{:?}", reparsed),
                "object mismatch at ({li},{oi})"
            );
        }
    }
}

/// The lean-worker handoff pair: CBOR round-trip of the MapIndex + lean
/// re-parse of the body must answer queries identically to the loaded
/// session.
#[test]
fn cbor_index_roundtrip_preserves_queries() {
    let full = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();
    let (_payload, index) = sav_core::mapdata::build_all_json(&full, None).unwrap();

    let lean_store = parse_body_bytes_lean(
        effective_body(&full).to_vec(),
        full.file_header.clone(),
        full.info.clone(),
        &tables,
        None,
    )
    .unwrap();
    let lean_index =
        sav_core::mapdata::index::MapIndex::from_cbor(&index.to_cbor().unwrap(), &lean_store)
            .unwrap();

    let mut checked = 0usize;
    for (i, name) in index.by_instance_name.keys().enumerate() {
        if i % 97 != 0 {
            continue;
        }
        let name = String::from_utf8_lossy(name).into_owned();
        let a = sav_core::mapdata::describe::describe_instance(&full, &index, &name);
        let b = sav_core::mapdata::describe::describe_instance(&lean_store, &lean_index, &name);
        assert_eq!(a.to_string(), b.to_string(), "describe diverges for {name}");
        checked += 1;
    }
    assert!(checked > 20, "suspiciously few instances checked: {checked}");

    let a = sav_core::mapdata::queries::collect_train_info(&full, &index);
    let b = sav_core::mapdata::queries::collect_train_info(&lean_store, &lean_index);
    assert_eq!(a.to_string(), b.to_string());
}

/// build_all_json must produce byte-identical payload + index whether the
/// store holds the full object model or is lean (objects re-parsed on demand
/// from spans). This is the executable proof that the builder never depends
/// on the resident model.
#[test]
fn build_all_json_on_lean_store_matches_full() {
    let full = load("All_autosave_0.sav");
    let (payload_full, index_full) = sav_core::mapdata::build_all_json(&full, None).unwrap();

    let lean = load_lean("All_autosave_0.sav");
    assert!(!lean.has_object_model(), "lean parse must not build the object model");
    let (payload_lean, index_lean) = sav_core::mapdata::build_all_json(&lean, None).unwrap();

    assert_eq!(payload_full, payload_lean, "payload diverges on lean store");
    assert_eq!(
        index_full.dump(&full).to_string(),
        index_lean.dump(&lean).to_string(),
        "index diverges on lean store"
    );
}

/// A dropped (model-free) store can be edited directly -- planning re-parses
/// on demand from spans -- and the result matches the never-dropped path byte
/// for byte.
#[test]
fn edit_after_drop_matches_direct_edit() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();

    // First movable actor of a machine class.
    let mut name = None;
    'outer: for level in &store.levels {
        for header in &level.headers {
            if let Header::Actor(a) = header {
                let tp = a.type_path.to_string(&store.data);
                if tp.starts_with("/Game/FactoryGame/Buildable/Factory/ConstructorMk1/") {
                    name = Some(a.instance_name.to_string(&store.data));
                    break 'outer;
                }
            }
        }
    }
    let name = name.expect("save has a constructor");
    let op = EditOp::MoveActors {
        names: vec![name],
        delta: [1234.0, -500.0, 250.0],
        rotate_yaw_deg: 0.0,
        pivot: None,
    };

    // Reference: edit without ever dropping.
    let direct = session::step(&store, &op, &tables).unwrap();

    // Dropped store: edit directly, without ever rebuilding the model.
    let mut dropped = load("All_autosave_0.sav");
    dropped.drop_object_model();
    assert!(!dropped.has_object_model());
    let edited = session::step_owned(dropped, &op, &tables).unwrap();

    assert_eq!(direct.data, edited.data, "edited bodies diverge");
}
