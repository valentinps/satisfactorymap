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
    let store = load("All_autosave_0.sav");
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
    let store = load("All_autosave_0.sav");
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
    let store = load("All_autosave_0.sav");

    // Find a belt that appears in some chain actor.
    let mut target: Option<(String, Vec<[[f64; 3]; 3]>)> = None;
    'outer: for level in &store.levels {
        for object in &level.objects {
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
        for object in &level.objects {
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
    let store = load("All_autosave_0.sav");

    let mut target: Option<(String, [f64; 3])> = None;
    for level in &store.levels {
        for object in &level.objects {
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
        for object in &level.objects {
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
fn replay_is_deterministic() {
    let store = load("All_autosave_0.sav");
    let (_, _, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let tables = ClassTables::embedded();
    let ops = vec![
        EditOp::MoveActors { names: vec![name.clone()], delta: [100.0, 0.0, 0.0], rotate_yaw_deg: 0.0, pivot: None },
        EditOp::MoveActors { names: vec![name], delta: [0.0, 100.0, 0.0], rotate_yaw_deg: 0.0, pivot: None },
    ];
    let pristine = effective_body(&store).to_vec();
    let a = session::rebuild(&pristine, &store.file_header, &store.info, &tables, &ops, None).unwrap();
    let b = session::rebuild(&pristine, &store.file_header, &store.info, &tables, &ops, None).unwrap();
    assert_eq!(a.data, b.data);
    // And rebuilding with no ops returns the pristine body exactly.
    let c = session::rebuild(&pristine, &store.file_header, &store.info, &tables, &[], None).unwrap();
    assert_eq!(effective_body(&c), &pristine[..]);
}
