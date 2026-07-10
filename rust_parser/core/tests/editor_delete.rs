//! Delete-engine tests: remove a machine (components go with it), attached
//! wires follow deleted poles, chained belts are refused, lightweight
//! instances splice out, and delete round-trips through export.

use sav_core::editor::ops::{EditOp, LwRef};
use sav_core::editor::{effective_body, export_sav, session};
use sav_core::level::parse_full_save;
use sav_core::mapdata::scan::SaveScan;
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, Header, SaveStore};
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

#[test]
fn delete_machine_removes_actor_and_components() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();
    let (li, oi, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let components: Vec<String> = match &store.levels[li].objects[oi].actor_reference_associations {
        Some((_, comps)) => comps.iter().map(|r| r.path_name.to_string(&store.data)).collect(),
        None => panic!(),
    };
    let count_before: usize = store.levels.iter().map(|l| l.headers.len()).sum();

    let op = EditOp::DeleteActors { names: vec![name.clone()] };
    let store2 = session::step(&store, &op, &tables).unwrap();

    let scan2 = SaveScan::new(&store2);
    assert!(!scan2.by_instance_name.contains_key(name.as_bytes()));
    for comp in &components {
        assert!(!scan2.by_instance_name.contains_key(comp.as_bytes()), "component {} survived", comp);
    }
    let count_after: usize = store2.levels.iter().map(|l| l.headers.len()).sum();
    assert_eq!(count_after, count_before - 1 - components.len());

    let exported = export_sav(&store2.file_header, effective_body(&store2));
    parse_full_save(&exported, &tables, None).unwrap();
}

#[test]
fn delete_pole_removes_attached_wires() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();
    let data: &[u8] = &store.data;

    // Find a wire and one of its endpoint owners.
    let mut target: Option<String> = None;
    for level in &store.levels {
        for object in &level.objects {
            if let ActorSpecific::PowerLine(a, _) = &object.actor_specific {
                let pa = a.path_name.to_string(data);
                if let Some(dot) = pa.rfind('.') {
                    let owner = pa[..dot].to_string();
                    let scan = SaveScan::new(&store);
                    if scan.by_instance_name.contains_key(owner.as_bytes()) {
                        target = Some(owner);
                        break;
                    }
                }
            }
        }
        if target.is_some() {
            break;
        }
    }
    let Some(owner) = target else {
        eprintln!("no wire with resolvable owner; skipping");
        return;
    };

    let count_wires = |s: &SaveStore| -> usize {
        s.levels
            .iter()
            .flat_map(|l| &l.objects)
            .filter(|o| matches!(o.actor_specific, ActorSpecific::PowerLine(..)))
            .count()
    };
    let wires_before = count_wires(&store);

    let op = EditOp::DeleteActors { names: vec![owner.clone()] };
    let store2 = session::step(&store, &op, &tables).unwrap();
    assert!(count_wires(&store2) < wires_before, "attached wire not deleted");
    let scan2 = SaveScan::new(&store2);
    assert!(!scan2.by_instance_name.contains_key(owner.as_bytes()));
}

#[test]
fn delete_chained_belt_is_refused() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();

    let mut belt: Option<String> = None;
    for level in &store.levels {
        for object in &level.objects {
            if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
                belt = Some(belts[0].belt.path_name.to_string(&store.data));
            }
        }
    }
    let Some(belt) = belt else {
        eprintln!("no conveyor chains; skipping");
        return;
    };

    let op = EditOp::DeleteActors { names: vec![belt] };
    let err = match session::step(&store, &op, &tables) {
        Ok(_) => panic!("deleting a chained belt should be refused"),
        Err(e) => e,
    };
    assert!(err.msg.contains("conveyor chain"), "{}", err.msg);
}

#[test]
fn delete_lightweight_instance() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();

    let mut target: Option<(String, usize, [f64; 3])> = None;
    for level in &store.levels {
        for object in &level.objects {
            if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                for group in items {
                    if group.instances.len() >= 2 {
                        target = Some((
                            group.type_path.to_string(&store.data),
                            group.instances.len(),
                            group.instances[1].position,
                        ));
                        break;
                    }
                }
            }
        }
    }
    let Some((type_path, count_before, survivor_pos)) = target else {
        eprintln!("no lightweight group with 2+ instances; skipping");
        return;
    };

    // Delete instance 0; instance 1 becomes instance 0.
    let op = EditOp::DeleteLightweight {
        items: vec![LwRef { type_path: type_path.clone(), index: 0 }],
    };
    let store2 = session::step(&store, &op, &tables).unwrap();

    for level in &store2.levels {
        for object in &level.objects {
            if let ActorSpecific::Lightweight { items, .. } = &object.actor_specific {
                let group = items.iter().find(|g| g.type_path.eq_ascii(&store2.data, &type_path)).unwrap();
                assert_eq!(group.instances.len(), count_before - 1);
                assert_eq!(group.instances[0].position, survivor_pos);
                return;
            }
        }
    }
    panic!("lightweight subsystem missing after delete");
}

#[test]
fn delete_then_undo_replay_restores_pristine() {
    let store = load("All_autosave_0.sav");
    let tables = ClassTables::embedded();
    let (_, _, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let pristine = effective_body(&store).to_vec();

    // Apply a delete, then "undo" by replaying an empty op list.
    let op = EditOp::DeleteActors { names: vec![name] };
    let _deleted = session::step(&store, &op, &tables).unwrap();
    let restored = session::rebuild(pristine.clone(), &store.file_header, &store.info, &tables, &[], None).unwrap();
    assert_eq!(effective_body(&restored), &pristine[..]);
}
