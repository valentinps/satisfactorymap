//! Move-engine tests: translate/rotate actors, chained belts, and
//! lightweight buildables on a real save, re-parse, and check exactly the
//! intended values changed.

use sav_core::editor::ops::{EditOp, LwRef};
use sav_core::editor::{effective_body, export_sav, session};
use sav_core::level::parse_full_save;
use sav_core::mapdata::geometry::yaw_from_quaternion;
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, Header, SaveStore};
use std::path::PathBuf;

fn load(name: &str) -> SaveStore {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = std::fs::read(path).expect("test save present");
    parse_full_save(&bytes, &ClassTables::embedded(), None).unwrap()
}

/// First actor whose type path starts with the given prefix.
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

fn actor_position(store: &SaveStore, li: usize, oi: usize) -> [f32; 3] {
    match &store.levels[li].headers[oi] {
        Header::Actor(a) => a.position,
        _ => panic!("not an actor"),
    }
}

#[test]
fn move_actor_translates_header_position() {
    let store = load("All_080726-163150.sav");
    let (li, oi, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let before = actor_position(&store, li, oi);

    let tables = ClassTables::embedded();
    let op = EditOp::MoveActors {
        names: vec![name.clone()],
        delta: [800.0, -400.0, 100.0],
        rotate_yaw_deg: 0.0,
        pivot: None,
    };
    let store2 = session::step(&store, &op, &tables).unwrap();

    let (li2, oi2, _) = find_actor(&store2, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let after = actor_position(&store2, li2, oi2);
    assert_eq!(after[0], before[0] + 800.0);
    assert_eq!(after[1], before[1] - 400.0);
    assert_eq!(after[2], before[2] + 100.0);

    // Only the three floats changed: bodies are same length, and the diff is
    // exactly 12 bytes inside the transform block.
    let b1 = effective_body(&store);
    let b2 = effective_body(&store2);
    assert_eq!(b1.len(), b2.len());
    let diff: Vec<usize> = (0..b1.len()).filter(|&i| b1[i] != b2[i]).collect();
    let t = match &store.levels[li].headers[oi] {
        Header::Actor(a) => a.transform_off as usize,
        _ => unreachable!(),
    };
    assert!(!diff.is_empty());
    assert!(diff.iter().all(|&i| i >= t + 16 && i < t + 28), "unexpected diff offsets: {diff:?}");

    // Exported file re-parses.
    let exported = export_sav(&store2.file_header, effective_body(&store2));
    parse_full_save(&exported, &tables, None).unwrap();
}

#[test]
fn rotate_actor_about_own_position_changes_only_quat() {
    let store = load("All_080726-163150.sav");
    let (li, oi, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let (before_pos, before_rot) = match &store.levels[li].headers[oi] {
        Header::Actor(a) => (a.position, a.rotation),
        _ => unreachable!(),
    };

    let tables = ClassTables::embedded();
    let op = EditOp::MoveActors {
        names: vec![name],
        delta: [0.0, 0.0, 0.0],
        rotate_yaw_deg: 90.0,
        pivot: Some([before_pos[0] as f64, before_pos[1] as f64]),
    };
    let store2 = session::step(&store, &op, &tables).unwrap();
    let (li2, oi2, _) = find_actor(&store2, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let (after_pos, after_rot) = match &store2.levels[li2].headers[oi2] {
        Header::Actor(a) => (a.position, a.rotation),
        _ => unreachable!(),
    };

    // Pivot at own position: position unchanged (f32-exact since dx=dy=0).
    assert_eq!(before_pos, after_pos);
    let yaw_before = yaw_from_quaternion([before_rot[0] as f64, before_rot[1] as f64, before_rot[2] as f64, before_rot[3] as f64]);
    let yaw_after = yaw_from_quaternion([after_rot[0] as f64, after_rot[1] as f64, after_rot[2] as f64, after_rot[3] as f64]);
    let mut d = (yaw_after - yaw_before).to_degrees();
    while d < -180.0 { d += 360.0; }
    while d > 180.0 { d -= 360.0; }
    assert!((d - 90.0).abs() < 0.01, "yaw delta {d}");
}

#[test]
fn move_chained_belt_moves_chain_splines() {
    let store = load("All_080726-163150.sav");

    // Find a belt that appears in some chain actor.
    let mut target: Option<(String, Vec<[[f64; 3]; 3]>)> = None;
    'outer: for level in &store.levels {
        for object in level.parsed_objects() {
            if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
                let cb = &belts[0];
                target = Some((
                    cb.belt.path_name.to_string(&store.data),
                    cb.elements.clone(),
                ));
                break 'outer;
            }
        }
    }
    let Some((belt_name, elements_before)) = target else {
        eprintln!("save has no conveyor chains; skipping");
        return;
    };

    let tables = ClassTables::embedded();
    let op = EditOp::MoveActors {
        names: vec![belt_name.clone()],
        delta: [1000.0, 2000.0, 300.0],
        rotate_yaw_deg: 0.0,
        pivot: None,
    };
    let store2 = session::step(&store, &op, &tables).unwrap();

    let mut found = false;
    'outer2: for level in &store2.levels {
        for object in level.parsed_objects() {
            if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
                for cb in belts {
                    if cb.belt.path_name.eq_ascii(&store2.data, &belt_name) {
                        for (e, before) in cb.elements.iter().zip(&elements_before) {
                            // Row 0 = location: translated.
                            assert_eq!(e[0][0], before[0][0] + 1000.0);
                            assert_eq!(e[0][1], before[0][1] + 2000.0);
                            assert_eq!(e[0][2], before[0][2] + 300.0);
                            // Rows 1-2 = tangents: untouched by pure translation.
                            assert_eq!(e[1], before[1]);
                            assert_eq!(e[2], before[2]);
                        }
                        found = true;
                        break 'outer2;
                    }
                }
            }
        }
    }
    assert!(found, "moved belt no longer found in chains");
}

#[test]
fn move_lightweight_instance() {
    let store = load("All_080726-163150.sav");

    let mut target: Option<(String, [f64; 3])> = None;
    for level in &store.levels {
        for object in level.parsed_objects() {
            if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                if let Some(group) = items.first() {
                    target = Some((
                        group.type_path.to_string(&store.data),
                        group.instances[0].position,
                    ));
                }
            }
        }
    }
    let Some((type_path, before)) = target else {
        eprintln!("save has no lightweight buildables; skipping");
        return;
    };

    let tables = ClassTables::embedded();
    let op = EditOp::MoveLightweight {
        items: vec![LwRef { type_path: type_path.clone(), index: 0 }],
        delta: [500.0, 600.0, 700.0],
        rotate_yaw_deg: 0.0,
        pivot: None,
    };
    let store2 = session::step(&store, &op, &tables).unwrap();

    for level in &store2.levels {
        for object in level.parsed_objects() {
            if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                let group = items
                    .iter()
                    .find(|g| g.type_path.eq_ascii(&store2.data, &type_path))
                    .unwrap();
                let after = group.instances[0].position;
                assert_eq!(after, [before[0] + 500.0, before[1] + 600.0, before[2] + 700.0]);
                return;
            }
        }
    }
    panic!("lightweight subsystem missing after edit");
}

#[test]
fn move_both_wire_owners_moves_the_wire() {
    use sav_core::mapdata::scan::SaveScan;
    use sav_core::store::{PropertyValue, StructValue};

    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();
    let data: &[u8] = &store.data;

    let wire_locations = |s: &SaveStore, li: usize, oi: usize| -> Vec<[f64; 3]> {
        let object = s.parse_object_at(li, oi).unwrap();
        let mut out = Vec::new();
        if let Some(entries) =
            sav_core::mapdata::props::array_structs(&object.properties, &s.data, b"mWireInstances")
        {
            for entry in entries {
                for prop in &entry.props {
                    if !prop.name.wide && prop.name.bytes(&s.data) == b"Locations" {
                        if let PropertyValue::Struct(StructValue::Vector(v)) = &prop.value {
                            out.push(*v);
                        }
                    }
                }
            }
        }
        out
    };

    // A wire with two distinct resolvable owners.
    let scan = SaveScan::new(&store);
    let mut target: Option<(String, String, String)> = None;
    'outer: for level in &store.levels {
        for (oi, object) in level.parsed_objects().iter().enumerate() {
            if let ActorSpecific::PowerLine(a, b) = &object.actor_specific {
                let (pa, pb) = (a.path_name.to_string(data), b.path_name.to_string(data));
                let (Some(da), Some(db)) = (pa.rfind('.'), pb.rfind('.')) else { continue };
                let (owner_a, owner_b) = (pa[..da].to_string(), pb[..db].to_string());
                if owner_a != owner_b
                    && scan.by_instance_name.contains_key(owner_a.as_bytes())
                    && scan.by_instance_name.contains_key(owner_b.as_bytes())
                {
                    target = Some((owner_a, owner_b, level.headers[oi].instance_name().to_string(data)));
                    break 'outer;
                }
            }
        }
    }
    let Some((owner_a, owner_b, wire)) = target else {
        eprintln!("save has no two-owner wire; skipping");
        return;
    };
    let &(wli, woi) = scan.by_instance_name.get(wire.as_bytes()).unwrap();
    let before_locations = wire_locations(&store, wli, woi);
    assert!(!before_locations.is_empty(), "wire has no cached Locations");
    let before_pos = match &store.levels[wli].headers[woi] {
        Header::Actor(a) => a.position,
        _ => unreachable!(),
    };

    let op = EditOp::MoveActors {
        names: vec![owner_a, owner_b],
        delta: [4000.0, -2500.0, 100.0],
        rotate_yaw_deg: 0.0,
        pivot: None,
    };
    let store2 = session::step(&store, &op, &tables).unwrap();

    let scan2 = SaveScan::new(&store2);
    let &(wli2, woi2) = scan2.by_instance_name.get(wire.as_bytes()).unwrap();
    // The wire actor itself followed (it wasn't in `names`).
    let after_pos = match &store2.levels[wli2].headers[woi2] {
        Header::Actor(a) => a.position,
        _ => unreachable!(),
    };
    assert_eq!(after_pos[0], before_pos[0] + 4000.0);
    assert_eq!(after_pos[1], before_pos[1] - 2500.0);
    assert_eq!(after_pos[2], before_pos[2] + 100.0);
    // And so did its cached mesh-endpoint positions.
    let after_locations = wire_locations(&store2, wli2, woi2);
    assert_eq!(after_locations.len(), before_locations.len());
    for (after, before) in after_locations.iter().zip(&before_locations) {
        assert_eq!(after[0], before[0] + 4000.0);
        assert_eq!(after[1], before[1] - 2500.0);
        assert_eq!(after[2], before[2] + 100.0);
    }
}

#[test]
fn replay_is_deterministic() {
    let store = load("All_080726-163150.sav");
    let (_, _, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let tables = ClassTables::embedded();
    let ops = vec![
        EditOp::MoveActors { names: vec![name.clone()], delta: [100.0, 0.0, 0.0], rotate_yaw_deg: 0.0, pivot: None },
        EditOp::MoveActors { names: vec![name], delta: [0.0, 100.0, 0.0], rotate_yaw_deg: 0.0, pivot: None },
    ];
    let pristine = effective_body(&store).to_vec();
    let a = session::rebuild(pristine.clone(), &store.file_header, &store.info, &tables, &ops, None).unwrap();
    let b = session::rebuild(pristine.clone(), &store.file_header, &store.info, &tables, &ops, None).unwrap();
    assert_eq!(a.data, b.data);
    // And rebuilding with no ops returns the pristine body exactly.
    let c = session::rebuild(pristine.clone(), &store.file_header, &store.info, &tables, &[], None).unwrap();
    assert_eq!(effective_body(&c), &pristine[..]);
}
