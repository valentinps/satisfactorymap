//! Items with per-item state riding a conveyor: a jetpack's fuel level, a
//! weapon's magazine, a gas mask's filter. Their state record sits inline in
//! the conveyor's item slot, and a parser that assumes it away walks off the
//! end of the object and fails the whole save (issue #22) -- so a gallery of
//! gear on a belt is the exact shape these tests keep working.

use sav_core::editor::ops::EditOp;
use sav_core::editor::{effective_body, export_sav, session};
use sav_core::level::parse_full_save;
use sav_core::mapdata::scan::SaveScan;
use sav_core::mapdata::{self, index::MapIndex};
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, SaveStore};
use std::path::PathBuf;

/// A save with equipment parked on a belt: jetpacks, gas masks, hazmat suits.
const SAVE: &str = "belt_item_state.sav";

fn load(name: &str) -> SaveStore {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = std::fs::read(path).expect("test save present");
    parse_full_save(&bytes, &ClassTables::embedded(), None)
        .unwrap_or_else(|e| panic!("{name}: parse failed: {}", e.msg))
}

/// Every state record on a conveyor, chain-owned or belt-owned, as raw bytes.
fn conveyor_item_states(store: &SaveStore) -> Vec<&[u8]> {
    let data: &[u8] = &store.data;
    let mut states = Vec::new();
    for level in &store.levels {
        for object in level.parsed_objects() {
            match &object.actor_specific {
                ActorSpecific::ConveyorChain { items, .. } => {
                    states.extend(items.iter().filter_map(|i| i.state).map(|s| s.bytes(data)));
                }
                ActorSpecific::ConveyorBelt { items, .. } => {
                    states.extend(items.iter().filter_map(|i| i.state).map(|s| s.bytes(data)));
                }
                _ => {}
            }
        }
    }
    states
}

/// A state record is an empty level name, the state's type path, and its
/// length-prefixed properties -- and the span we kept covers exactly that,
/// no more and no less. (Reading it back is what a writer does with these
/// bytes, so if the span is off by one the write path corrupts a save.)
fn assert_well_formed_state(state: &[u8]) {
    assert!(state.len() > 12, "state record too short: {} bytes", state.len());
    assert_eq!(&state[..4], &0u32.to_le_bytes(), "state level name is not empty");
    let type_len = u32::from_le_bytes(state[4..8].try_into().unwrap()) as usize;
    let type_end = 8 + type_len;
    assert!(type_end + 4 <= state.len(), "state type path overruns the record");
    let type_path = String::from_utf8_lossy(&state[8..type_end - 1]).into_owned();
    assert!(
        type_path.starts_with("/Script/FactoryGame.FG") || type_path.starts_with("/Game/"),
        "implausible state type path {type_path:?}"
    );
    let size = u32::from_le_bytes(state[type_end..type_end + 4].try_into().unwrap()) as usize;
    assert_eq!(
        type_end + 4 + size,
        state.len(),
        "state span does not end where its property block does ({type_path})"
    );
}

/// The save parses at all -- the issue-#22 crash -- and every state record it
/// holds round-trips as the span we hand a writer.
#[test]
fn stated_items_on_conveyors_parse() {
    let store = load(SAVE);
    let states = conveyor_item_states(&store);
    assert!(!states.is_empty(), "{SAVE} has no stated conveyor items left to gate on");
    for state in &states {
        assert_well_formed_state(state);
    }
}

/// An item search must find the geared-up items too, listed against the belt
/// they ride. (They reach the index through the same path as plain items, so
/// this is really "state does not hide an item from the search".)
#[test]
fn stated_belt_items_are_searchable() {
    let store = load(SAVE);
    let (_payload, index): (_, MapIndex) = mapdata::build_all_json(&store, None).unwrap();
    let mut found = 0;
    for (short, entries) in &index.item_location_index {
        if !short.starts_with(b"BP_EquipmentDescriptor") {
            continue;
        }
        for (instance_name, count) in entries {
            if String::from_utf8_lossy(instance_name).contains("Build_Conveyor") {
                assert!(*count > 0);
                found += 1;
            }
        }
    }
    assert!(found > 0, "no stateful equipment found on a belt by the item search");
}

/// Deleting the belt that carries stated items takes the whole line with it
/// (a chain actor cannot outlive its belts), and the resulting save must
/// still export and re-parse -- the write-back path runs over these records
/// on the way out. The record layout it emits for a stated item is pinned by
/// editor::apply's belt_item_record_writes_the_item_state_slot.
#[test]
fn deleting_a_belt_of_stated_items_still_round_trips() {
    let store = load(SAVE);
    let tables = ClassTables::embedded();
    let data: &[u8] = &store.data;

    let mut belt = None;
    for level in &store.levels {
        for object in level.parsed_objects() {
            let ActorSpecific::ConveyorChain { belts, items, .. } = &object.actor_specific else {
                continue;
            };
            if items.iter().any(|i| i.state.is_some()) {
                belt = Some(belts[0].belt.path_name.to_string(data));
                break;
            }
        }
    }
    let belt = belt.expect("no chain carrying stated items");

    let store2 = session::step(&store, &EditOp::DeleteActors { names: vec![belt.clone()] }, &tables)
        .unwrap();
    assert!(!SaveScan::new(&store2).by_instance_name.contains_key(belt.as_bytes()));

    let exported = export_sav(&store2.file_header, effective_body(&store2));
    let reparsed = parse_full_save(&exported, &tables, None)
        .unwrap_or_else(|e| panic!("exported save failed to re-parse: {}", e.msg));
    for state in conveyor_item_states(&reparsed) {
        assert_well_formed_state(state);
    }
}
