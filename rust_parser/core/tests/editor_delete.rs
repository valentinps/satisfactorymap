//! Delete-engine tests: remove a machine (components go with it), attached
//! wires follow deleted poles, chained belts drag their chain actor along,
//! lightweight instances splice out, and delete round-trips through export.

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
    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();
    let (li, oi, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    let components: Vec<String> = match &store.levels[li].parsed_objects()[oi].actor_reference_associations {
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
    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();
    let data: &[u8] = &store.data;

    // Find a wire and one of its endpoint owners.
    let mut target: Option<String> = None;
    for level in &store.levels {
        for object in level.parsed_objects() {
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
            .flat_map(|l| l.parsed_objects())
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

/// Deleting one belt of a conveyor chain deletes that chain ACTOR too (its
/// packed belt list cannot point at a gone belt), and every SURVIVING belt
/// of the line inherits its own slice of the chain's items as per-belt
/// records -- only the deleted segment's items are lost, like cutting a
/// line in game.
#[test]
fn delete_chained_belt_removes_chain_actor() {
    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();

    // A chain with 2+ belts: delete one, the other must survive. Prefer a
    // chain whose surviving belt actually carries items so the write-back
    // is exercised, not just the zero case.
    let mut target: Option<(String, String, String, usize)> = None;
    'outer: for (li, level) in store.levels.iter().enumerate() {
        for (oi, object) in level.parsed_objects().iter().enumerate() {
            if let ActorSpecific::ConveyorChain { belts, .. } = &object.actor_specific {
                if belts.len() < 2 {
                    continue;
                }
                let Header::Actor(a) = &store.levels[li].headers[oi] else { continue };
                let other_belt = belts[1].belt.path_name.to_string(&store.data);
                let expected_items = sav_core::mapdata::queries::conveyor_chain_segment_item_paths(
                    &object.actor_specific,
                    &store.data,
                    other_belt.as_bytes(),
                )
                .into_iter()
                .filter(|p| !p.is_empty())
                .count();
                if target.is_none() || expected_items > 0 {
                    target = Some((
                        a.instance_name.to_string(&store.data),
                        belts[0].belt.path_name.to_string(&store.data),
                        other_belt,
                        expected_items,
                    ));
                    if expected_items > 0 {
                        break 'outer;
                    }
                }
            }
        }
    }
    let Some((chain_name, belt, other_belt, expected_items)) = target else {
        eprintln!("no conveyor chain with 2+ belts; skipping");
        return;
    };
    if expected_items == 0 {
        eprintln!("note: surviving belt has no items in this save; write-back untested");
    }

    let op = EditOp::DeleteActors { names: vec![belt.clone()] };
    let store2 = session::step(&store, &op, &tables).unwrap();

    let scan2 = SaveScan::new(&store2);
    assert!(!scan2.by_instance_name.contains_key(belt.as_bytes()), "belt survived");
    assert!(!scan2.by_instance_name.contains_key(chain_name.as_bytes()), "chain actor survived");
    assert!(
        scan2.by_instance_name.contains_key(other_belt.as_bytes()),
        "other belt of the line was deleted"
    );

    // The surviving belt inherited exactly its slice of the chain's items.
    let &(bli, boi) = scan2.by_instance_name.get(other_belt.as_bytes()).unwrap();
    let belt_object = store2.parse_object_at(bli, boi).unwrap();
    let ActorSpecific::ConveyorBelt { items, .. } = &belt_object.actor_specific else {
        panic!("surviving belt lost its belt-specific data");
    };
    assert_eq!(items.len(), expected_items, "written-back item count");

    let exported = export_sav(&store2.file_header, effective_body(&store2));
    parse_full_save(&exported, &tables, None).unwrap();
}

#[test]
fn delete_lightweight_instance() {
    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();

    let mut target: Option<(String, usize, [f64; 3])> = None;
    for level in &store.levels {
        for object in level.parsed_objects() {
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
        for object in level.parsed_objects() {
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
    let store = load("All_080726-163150.sav");
    let tables = ClassTables::embedded();
    let (_, _, name) = find_actor(&store, "/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let pristine = effective_body(&store).to_vec();

    // Apply a delete, then "undo" by replaying an empty op list.
    let op = EditOp::DeleteActors { names: vec![name] };
    let _deleted = session::step(&store, &op, &tables).unwrap();
    let restored = session::rebuild(pristine.clone(), &store.file_header, &store.info, &tables, &[], None).unwrap();
    assert_eq!(effective_body(&restored), &pristine[..]);
}
