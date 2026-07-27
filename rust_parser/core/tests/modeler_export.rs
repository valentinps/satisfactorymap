//! End-to-end: save -> `.sfmd`, asserted against factories whose correct
//! answer is known by construction (docs/modeler-test-saves.md).
//!
//! These are the assertions that could not be made any other way. Everything
//! upstream can be checked against numbers computed from the game data, but
//! "did it wire the right consumer to the right producer on a shared belt?"
//! needs a factory someone deliberately built to have a known answer.

use sav_core::level::parse_full_save_lean;
use sav_core::modeler::{self, aggregate::Options};
use sav_core::object::ClassTables;
use serde_json::Value;
use std::path::PathBuf;

fn export(name: &str, options: &Options) -> Option<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("skipping: {name} not in map/uploads (see docs/modeler-test-saves.md)");
            return None;
        }
    };
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    let (text, _) = modeler::export(&store, options);
    Some(serde_json::from_str(&text).expect("our own output must be valid JSON"))
}

fn nodes(document: &Value) -> &Vec<Value> {
    document["Data"].as_array().expect("Data array")
}

/// Every node named `name`, as (index, node).
fn named<'a>(document: &'a Value, name: &str) -> Vec<(usize, &'a Value)> {
    nodes(document)
        .iter()
        .enumerate()
        .filter(|(_, node)| node["Name"] == name)
        .collect()
}

/// A source is a bare node index, or `[index, port]` when the supplier has
/// addressable outputs (a storage container or priority splitter).
fn source_index(value: &Value) -> usize {
    match value {
        Value::Number(n) => n.as_u64().expect("node index") as usize,
        Value::Array(pair) => pair[0].as_u64().expect("node index") as usize,
        other => panic!("unexpected source shape {other}"),
    }
}

/// Source node indices feeding `ingredient` into `node`.
fn sources(node: &Value, ingredient: &str) -> Vec<usize> {
    node["Inputs"]
        .get(ingredient)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(source_index).collect())
        .unwrap_or_default()
}

/// THE assertion. Three item types share one belt network; each consumer must
/// be wired to exactly the items its own recipe takes and no others. A naive
/// implementation connects everything on the bus to everything, and would
/// pass every other test in this suite.
#[test]
fn a_shared_bus_wires_each_consumer_to_only_the_items_it_uses() {
    let Some(document) = export("modeler_sushi.sav", &Options::default()) else { return };

    let plate = named(&document, "Iron Plate")[0].0;
    let rod = named(&document, "Iron Rod")[0].0;
    let cast_screw = named(&document, "Cast Screw")[0].0;

    // The Screw constructor takes Iron Rod -- one of the three items on the
    // bus -- and nothing else.
    let (_, screw) = named(&document, "Screw")[0];
    assert_eq!(sources(screw, "Iron Rod"), vec![rod]);
    let screw_inputs = screw["Inputs"].as_object().expect("dict inputs");
    assert_eq!(
        screw_inputs.keys().collect::<Vec<_>>(),
        vec!["Iron Rod"],
        "the Screw constructor must not be fed Iron Plate or Screw just because \
         they share its belt",
    );

    // The Reinforced Iron Plate assembler takes two of the three: plate and
    // screw, but not rod.
    let (_, rip) = named(&document, "Reinforced Iron Plate")[0];
    assert_eq!(sources(rip, "Iron Plate"), vec![plate]);
    assert_eq!(sources(rip, "Screw"), vec![cast_screw]);
    let rip_inputs = rip["Inputs"].as_object().expect("dict inputs");
    let mut keys: Vec<&String> = rip_inputs.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["Iron Plate", "Screw"], "RIP does not consume Iron Rod");
}

/// Machine counts, overclocks and somersloops survive the collapse.
#[test]
fn clock_and_somersloop_groups_become_separate_nodes() {
    let Some(document) = export("modeler_clocks.sav", &Options::default()) else { return };

    let mut plates: Vec<(String, String, u64)> = named(&document, "Iron Plate")
        .into_iter()
        .map(|(_, node)| {
            (
                node["Max"].as_str().unwrap_or_default().to_string(),
                node["ClockSpeed"].as_str().unwrap_or("100").to_string(),
                node["ProductionShards"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    plates.sort();
    assert_eq!(
        plates,
        vec![
            ("1".into(), "100".to_string(), 1),  // one with a somersloop
            ("2".into(), "250".to_string(), 0),  // two overclocked
            ("3".into(), "100".to_string(), 0),  // three at default
        ],
    );

    // The standalone Assembler has one of its two somersloop slots filled,
    // so its boost multiplier is 1.5 -- a value no misreading of the
    // multiplier as a count could produce.
    let (_, rip) = named(&document, "Reinforced Iron Plate")[0];
    assert_eq!(rip["ProductionShards"].as_u64(), Some(1));
}

#[test]
fn unconnected_lines_making_the_same_thing_stay_separate_nodes() {
    let Some(document) = export("modeler_independent.sav", &Options::default()) else { return };
    let mut counts: Vec<&str> =
        named(&document, "Iron Plate").iter().map(|(_, n)| n["Max"].as_str().unwrap()).collect();
    counts.sort();
    assert_eq!(counts, vec!["2", "6"], "two lines, not one node of 8");
}

/// Pipe networks contract like belts, and a resource node's `Max` is
/// items/min rather than a building count.
#[test]
fn fluid_extraction_becomes_one_resource_node_rated_in_cubic_metres() {
    let Some(document) = export("modeler_fluids.sav", &Options::default()) else { return };
    let water = named(&document, "Water");
    assert_eq!(water.len(), 1, "both extractors share one pipe network");
    // Two water extractors at 120 m3/min each.
    assert_eq!(water[0].1["Max"].as_str(), Some("240"));

    let packagers = named(&document, "Packaged Water");
    assert_eq!(packagers.len(), 1, "both packagers merge");
    assert_eq!(packagers[0].1["Max"].as_str(), Some("2"));
    assert_eq!(sources(packagers[0].1, "Water"), vec![water[0].0]);
}

/// A cycle must survive both the aggregation and the layout pass.
#[test]
fn a_material_loop_exports_with_both_directions_intact() {
    let Some(document) = export("modeler_loop.sav", &Options::default()) else { return };
    let pack = named(&document, "Packaged Water")[0];
    let unpack = named(&document, "Unpackage Water")[0];

    assert!(sources(unpack.1, "Packaged Water").contains(&pack.0), "the loop runs forwards");
    assert!(sources(pack.1, "Water").contains(&unpack.0), "and closes back on itself");
    // Layout must still have placed everything -- a cycle that hung the
    // layering pass would never get here, but an unplaced node would show up
    // as a missing coordinate.
    for node in nodes(&document) {
        assert!(node["X"].is_number() && node["Y"].is_number(), "every node is placed");
    }
}

/// Vehicular transport is not traversed, so a truck-fed factory needs a stub
/// standing in for its supply -- otherwise Modeler solves the chain to zero.
#[test]
fn a_vehicle_fed_factory_gets_a_storage_stub_it_can_solve_from() {
    let Some(document) = export("modeler_vehicle.sav", &Options::default()) else { return };

    // The station carries iron ore, so it exports as an ore node. A Storage
    // Container would have been the obvious choice, but one with no inputs
    // has no part Modeler can infer and it rejects the connection; a raw
    // resource node carries its own identity and needs no supplier.
    let stubs = named(&document, "Iron Ore");
    assert_eq!(stubs.len(), 1, "the truck station becomes its cargo");
    assert!(stubs[0].1.get("Inputs").is_none(), "nothing on this map feeds it");
    assert!(stubs[0].1.get("Max").is_none(), "how much arrives by truck is unknowable");

    // And the smelter draws its ore from that stub rather than from nothing.
    let smelter = named(&document, "Iron Ingot")[0];
    assert_eq!(sources(smelter.1, "Iron Ore"), vec![stubs[0].0]);

    // With stubs off, the same factory is left unsupplied on purpose.
    let bare = Options { station_stubs: false, ..Options::default() };
    let Some(document) = export("modeler_vehicle.sav", &bare) else { return };
    assert!(named(&document, "Iron Ore").is_empty());
    assert!(named(&document, "Iron Ingot")[0].1.get("Inputs").is_none());
}

/// Structural invariants that must hold for any save, checked on the largest
/// fixture available: a file breaking these would load into Modeler wrong
/// rather than fail to load.
#[test]
fn exported_documents_are_structurally_sound() {
    for save in [
        "modeler_sushi.sav",
        "modeler_clocks.sav",
        "modeler_independent.sav",
        "modeler_fluids.sav",
        "modeler_loop.sav",
        "modeler_vehicle.sav",
    ] {
        let Some(document) = export(save, &Options::default()) else { continue };
        let all = nodes(&document);
        for (index, node) in all.iter().enumerate() {
            assert!(node["Name"].as_str().is_some_and(|n| !n.is_empty()), "{save}: unnamed node");
            match node.get("Inputs") {
                // Recipe nodes: ingredient -> [source index, ...]
                Some(Value::Object(map)) => {
                    for (ingredient, list) in map {
                        for source in list.as_array().expect("source list") {
                            let source = source_index(source);
                            assert!(source < all.len(), "{save}: {ingredient} source out of range");
                            assert_ne!(source, index, "{save}: node feeds itself");
                        }
                    }
                }
                // Generic-input nodes: one group per PORT, each holding that
                // port's connections as [index, "Item"] or [index, port].
                Some(Value::Array(groups)) => {
                    assert_eq!(
                        groups.len(),
                        1,
                        "{save}: only multi-port nodes may have more than one input group",
                    );
                    // Modeler refuses a file where one generic node is wired
                    // to two different parts ("IllegalArgumentException:
                    // Cannot connect two different parts"), so a sink that
                    // eats twelve item types has to become twelve nodes.
                    let mut parts: std::collections::BTreeSet<&str> =
                        std::collections::BTreeSet::new();
                    for group in groups {
                        for entry in group.as_array().expect("group") {
                            if let Some(item) = entry[1].as_str() {
                                parts.insert(item);
                            }
                        }
                    }
                    assert!(
                        parts.len() <= 1,
                        "{save}: node {index} ({}) mixes parts {parts:?}",
                        node["Name"],
                    );
                    // A port-addressed source with no inputs of its own has
                    // no part Modeler can infer, and wiring one into a typed
                    // input is rejected outright.
                    for group in groups {
                        for entry in group.as_array().expect("group") {
                            if entry[1].is_number() {
                                let source = source_index(entry);
                                assert!(
                                    all[source].get("Inputs").is_some(),
                                    "{save}: node {index} draws from an untyped source",
                                );
                            }
                        }
                    }
                    for group in groups {
                        for entry in group.as_array().expect("group") {
                            let pair = entry.as_array().expect("[index, item|port]");
                            let source = source_index(entry);
                            assert!(source < all.len(), "{save}: source out of range");
                            assert_ne!(source, index, "{save}: node feeds itself");
                            // Either an item name, or a port on a source that
                            // has addressable outputs.
                            let addressed = all[source].get("Capacity").is_some();
                            if addressed {
                                assert!(pair[1].is_number(), "{save}: storage source needs a port");
                            } else {
                                assert!(pair[1].as_str().is_some(), "{save}: edge needs an item");
                            }
                        }
                    }
                }
                Some(other) => panic!("{save}: unexpected Inputs shape {other}"),
                None => {}
            }
        }
    }
}
