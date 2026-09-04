//! Item-location index tests, focused on the items riding conveyors -- the
//! one class of stock that lives outside an inventory component and so has to
//! be gathered from the belt lines themselves (extract::item_location_index).

use sav_core::level::parse_full_save;
use sav_core::mapdata::queries::conveyor_chain_segment_window;
use sav_core::mapdata::{self, index::MapIndex, queries};
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, SaveStore};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

fn load(name: &str) -> (SaveStore, MapIndex) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = std::fs::read(path).expect("test save present");
    let store = parse_full_save(&bytes, &ClassTables::embedded(), None).unwrap();
    let (_payload, index) = mapdata::build_all_json(&store, None).unwrap();
    (store, index)
}

fn belt_instance_names(store: &SaveStore, index: &MapIndex) -> Vec<String> {
    let belt_types: HashSet<&str> =
        sav_core::gamedata::get().type_paths.conveyor_belts.iter().map(String::as_str).collect();
    let mut names = Vec::new();
    for type_path in index.instance_slots_by_type_path.keys() {
        if belt_types.contains(type_path.as_str()) {
            names.extend(index.instance_names_for_type_path(store, type_path));
        }
    }
    names
}

/// The item search must agree, belt by belt, with the independent per-belt
/// path the selection panel and the belt tooltip already use -- and must
/// actually find items there, not quietly contribute nothing.
#[test]
fn belt_items_match_the_per_belt_inventory_path() {
    for name in ["All_080726-163150.sav", "solo_autosave_1.sav"] {
        let (store, index) = load(name);
        let belts = belt_instance_names(&store, &index);
        let belt_set: HashSet<&str> = belts.iter().map(String::as_str).collect();

        // (belt, itemShortName) -> count, as the item search sees it.
        let mut from_index: HashMap<(String, String), i64> = HashMap::new();
        for (short, entries) in &index.item_location_index {
            let short = String::from_utf8_lossy(short).into_owned();
            for (instance_name, count) in entries {
                let instance_name = String::from_utf8_lossy(instance_name).into_owned();
                if belt_set.contains(instance_name.as_str()) {
                    *from_index.entry((instance_name, short.clone())).or_insert(0) += count;
                }
            }
        }

        // ... and as aggregateSelectionInventory sees it.
        let mut from_selection: HashMap<(String, String), i64> = HashMap::new();
        for belt in &belts {
            for row in queries::aggregate_selection_inventory(&store, &index, &[belt.as_str()])
                .as_array()
                .unwrap()
            {
                let item = row["item"].as_str().unwrap().to_string();
                let count = row["count"].as_f64().unwrap() as i64;
                *from_selection.entry((belt.clone(), item)).or_insert(0) += count;
            }
        }

        assert_eq!(from_index, from_selection, "{name}: belt contents disagree");
        let total: i64 = from_index.values().sum();
        assert!(total > 0, "{name}: the index found no items on any belt at all");
    }
}

/// Since 1.0 a belt line's items live in one ring buffer on the shared chain
/// actor, split between its member belts by index arithmetic. Attributing
/// each item to the belt it rides is only correct if those windows tile the
/// ring exactly: an overlap would double-count an item into two belts, a gap
/// would drop it from the search entirely.
#[test]
fn chain_item_windows_tile_the_ring_exactly() {
    for name in ["All_080726-163150.sav", "solo_autosave_1.sav"] {
        let (store, _index) = load(name);
        let mut chains_with_items = 0usize;
        for level in &store.levels {
            for object in level.parsed_objects() {
                let ActorSpecific::ConveyorChain {
                    belts, items, maximum_items, chain_lead_item_index, ..
                } = &object.actor_specific
                else {
                    continue;
                };
                if items.is_empty() {
                    continue;
                }
                chains_with_items += 1;
                let mut claims = vec![0u32; items.len()];
                for chain_belt in belts {
                    let Some((start, count)) =
                        conveyor_chain_segment_window(chain_belt, *maximum_items, *chain_lead_item_index)
                    else {
                        continue; // Belt holding nothing (game writes -1 indices).
                    };
                    assert!(
                        start + count <= items.len(),
                        "{name}: belt window {start}+{count} overruns a {}-slot ring",
                        items.len()
                    );
                    for claim in claims.iter_mut().skip(start).take(count) {
                        *claim += 1;
                    }
                }
                assert!(
                    claims.iter().all(|&c| c == 1),
                    "{name}: ring slots claimed by {:?} belts, expected exactly one each",
                    claims.iter().collect::<HashSet<_>>()
                );
            }
        }
        assert!(chains_with_items > 0, "{name}: no loaded belt lines to check");
    }
}
