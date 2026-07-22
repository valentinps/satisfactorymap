//! Golden dump of the post-load query endpoints (describeInstance, findItem,
//! buildingInfo, vehicleInfo, trainInfo, selectionInventory) so refactors of
//! the query layer that must not alter behavior can be byte-diffed, same idea
//! as dump_payload.
//!
//!     cargo run --release --example dump_queries -- save.sav out.json [--drop]
//!
//! --drop frees the parsed object model after the payload/index build (the
//! wasm session's post-load state); the output must match the no-drop run.

use sav_core::level::parse_full_save;
use sav_core::mapdata::{self, describe, queries};
use sav_core::object::ClassTables;
use serde_json::{Map, Value};

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let drop_model = if let Some(i) = args.iter().position(|a| a == "--drop") {
        args.remove(i);
        true
    } else {
        false
    };
    let [_, sav, out_path] = &args[..] else {
        eprintln!("usage: dump_queries <save.sav> <out.json> [--drop]");
        std::process::exit(2);
    };
    let bytes = std::fs::read(sav).expect("read save");
    let mut store = parse_full_save(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);
    let (_payload, index) = mapdata::build_all_json(&store, None).expect("build");
    if drop_model {
        store.drop_object_model();
    }
    let store = store;

    let mut out = Map::new();

    // describeInstance: the first instance of every distinct type path (one
    // per class covers every describe branch) plus a stride sample over all
    // instance names (odd one-off objects, components).
    let mut describe_names: Vec<String> = Vec::new();
    for tp in index.instance_slots_by_type_path.keys() {
        if let Some(first) = index.instance_names_for_type_path(&store, tp).into_iter().next() {
            describe_names.push(first);
        }
    }
    for (i, name) in index.by_instance_name.keys().enumerate() {
        if i % 997 == 0 {
            describe_names.push(String::from_utf8_lossy(name).into_owned());
        }
    }
    let mut describes = Map::new();
    for name in &describe_names {
        describes.insert(name.clone(), describe::describe_instance(&store, &index, name));
    }
    out.insert("describe".into(), Value::Object(describes));

    // findItem for every item the index knows about.
    let mut item_keys: Vec<String> = index
        .item_location_index
        .keys()
        .map(|k| String::from_utf8_lossy(k).into_owned())
        .collect();
    for k in index.dimensional_depot_by_item.keys() {
        if !item_keys.contains(k) {
            item_keys.push(k.clone());
        }
    }
    for k in index.static_item_locations.keys() {
        if !item_keys.contains(k) {
            item_keys.push(k.clone());
        }
    }
    let mut find_items = Map::new();
    for key in &item_keys {
        find_items.insert(key.clone(), queries::find_item_locations(&store, &index, key));
    }
    out.insert("findItem".into(), Value::Object(find_items));

    // buildingInfo per type path: the 100 first type paths in index order
    // plus the 20 most-populated ones (recipes/power/inventory coverage).
    let mut building_tps: Vec<String> =
        index.instance_slots_by_type_path.keys().take(100).cloned().collect();
    let mut by_count: Vec<(&String, usize)> = index
        .instance_slots_by_type_path
        .iter()
        .map(|(tp, b)| (tp, b.actor_slots.len() + b.lightweight_count))
        .collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (tp, _) in by_count.into_iter().take(20) {
        if !building_tps.contains(tp) {
            building_tps.push(tp.clone());
        }
    }
    let mut buildings = Map::new();
    for tp in &building_tps {
        buildings
            .insert(tp.clone(), queries::collect_building_info(&store, &index, &[tp.clone()]));
    }
    out.insert("buildingInfo".into(), Value::Object(buildings));

    // vehicleInfo over the vehicle classes present; trainInfo is global.
    let vehicle_tps: Vec<String> = index
        .instance_slots_by_type_path
        .keys()
        .filter(|tp| tp.contains("/Buildable/Vehicle/"))
        .cloned()
        .collect();
    out.insert("vehicleInfo".into(), queries::collect_vehicle_info(&store, &index, &vehicle_tps));
    out.insert("trainInfo".into(), queries::collect_train_info(&store, &index));

    // selectionInventory over the whole describe sample at once.
    let refs: Vec<&str> = describe_names.iter().map(String::as_str).collect();
    out.insert(
        "selectionInventory".into(),
        queries::aggregate_selection_inventory(&store, &index, &refs),
    );

    std::fs::write(out_path, serde_json::to_string_pretty(&Value::Object(out)).unwrap())
        .expect("write output");
    eprintln!("dumped {} describes, {} items, {} building types", describe_names.len(), item_keys.len(), building_tps.len());
}
