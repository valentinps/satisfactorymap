//! The hard gate on `modeler::names`: every node name and every `Inputs` key
//! in a real Modeler file must resolve through our table. A name Modeler does
//! not recognise makes it reject the whole `.sfmd`, so a silent mapping drift
//! after a game update has to fail here rather than in the user's hands.
//!
//! `tests/fixtures/modeler/*.sfmd` are real files saved out of Satisfactory
//! Modeler -- five mature factory plans plus five minimal graphs built to
//! pin down the schema of features the plans do not use (outposts,
//! somersloops, storage container modes, generators, the dimensional depot).
//! Between them they exercise 123 distinct node names and 92 item names.

use sav_core::modeler::fraction;
use sav_core::modeler::names;
use serde_json::Value;
use std::collections::BTreeSet;

fn fixtures() -> Vec<(String, Value)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/modeler");
    let mut files: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "sfmd"))
        .map(|entry| {
            let text = std::fs::read_to_string(entry.path()).expect("read fixture");
            let json = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", entry.path().display()));
            (entry.file_name().to_string_lossy().into_owned(), json)
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(files.len() >= 10, "expected the sample .sfmd fixtures, found {}", files.len());
    files
}

fn nodes(doc: &Value) -> &Vec<Value> {
    doc.get("Data").and_then(Value::as_array).expect("`Data` array")
}

/// Item names appear as `Inputs` dict keys on recipe nodes, and as the second
/// element of a `[index, "Item"]` source on nodes with generic inputs
/// (`AWESOME Sink`, `Storage Container`, `Dimensional Depot`, `Outpost`).
fn item_names_in(node: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for key in ["Inputs", "InteriorInputs"] {
        match node.get(key) {
            Some(Value::Object(map)) => found.extend(map.keys().cloned()),
            Some(Value::Array(groups)) => {
                for group in groups {
                    for source in group.as_array().into_iter().flatten() {
                        if let Some(name) = source.get(1).and_then(Value::as_str) {
                            found.push(name.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    found
}

#[test]
fn every_sample_node_name_is_one_we_can_emit() {
    let table = names::table();
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let mut seen = 0usize;
    for (file, doc) in fixtures() {
        for node in nodes(&doc) {
            let name = node.get("Name").and_then(Value::as_str).expect("node `Name`");
            seen += 1;
            if !table.is_known_node(name) {
                unknown.insert(format!("{name}  (in {file})"));
            }
        }
    }
    assert!(seen > 400, "fixtures look truncated: only {seen} nodes");
    assert!(unknown.is_empty(), "node names our table cannot produce:\n  {}", join(&unknown));
}

#[test]
fn every_sample_item_name_is_one_we_can_emit() {
    let table = names::table();
    // Reverse index: Modeler item name -> is it reachable from some item?
    let emittable: BTreeSet<&str> = sav_core::gamedata::get()
        .items
        .keys()
        .chain(sav_core::gamedata::get().resources.keys())
        .filter_map(|short_class| table.item(short_class))
        .collect();

    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let mut seen = 0usize;
    for (file, doc) in fixtures() {
        for node in nodes(&doc) {
            for item in item_names_in(node) {
                seen += 1;
                if !emittable.contains(item.as_str()) {
                    unknown.insert(format!("{item}  (in {file})"));
                }
            }
        }
    }
    assert!(seen > 200, "fixtures look truncated: only {seen} item references");
    assert!(unknown.is_empty(), "item names our table cannot produce:\n  {}", join(&unknown));
}

/// `Max` is byte-stable: Modeler always writes the canonical mixed fraction,
/// so ours must match character for character.
#[test]
fn every_sample_max_round_trips_byte_identically() {
    let mut checked = 0usize;
    for (file, doc) in fixtures() {
        for node in nodes(&doc) {
            let Some(text) = node.get("Max").and_then(Value::as_str) else { continue };
            let value = fraction::parse(text)
                .unwrap_or_else(|| panic!("{file}: cannot parse Max {text:?}"));
            assert_eq!(
                fraction::format(value),
                text,
                "{file}: Max {text:?} did not survive the round trip",
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "expected plenty of Max values, saw {checked}");
}

/// `ClockSpeed` is *not* byte-stable -- its precision is whatever the user
/// typed (`"166.6666666"` vs `"133.3333333333333"` in the same corpus). Only
/// the value has to survive.
#[test]
fn every_sample_clock_speed_round_trips_numerically() {
    let mut checked = 0usize;
    for (file, doc) in fixtures() {
        for node in nodes(&doc) {
            let Some(text) = node.get("ClockSpeed").and_then(Value::as_str) else { continue };
            let typed = fraction::parse(text)
                .unwrap_or_else(|| panic!("{file}: cannot parse ClockSpeed {text:?}"));
            let ours = fraction::parse(&fraction::format_percent(typed))
                .expect("our own output must parse");
            assert!(
                (ours - typed).abs() / typed < 1e-6,
                "{file}: ClockSpeed {text:?} became {ours}",
            );
            checked += 1;
        }
    }
    assert!(checked > 40, "expected plenty of ClockSpeed values, saw {checked}");
}

/// Guards the assumptions the emitter is built on -- if a future Modeler
/// version changes any of these shapes, this fails loudly instead of us
/// writing files it silently mis-reads.
#[test]
fn sample_schema_matches_what_the_emitter_assumes() {
    let by_name: std::collections::HashMap<String, Value> =
        fixtures().into_iter().map(|(n, d)| (n, d)).collect();

    // Outpost: children are in the same flat Data array with `Parent` set,
    // and reference the outpost's ports as [outpostIndex, portIndex].
    let outpost = &by_name["outpost.sfmd"];
    let data = nodes(outpost);
    let outpost_index = data
        .iter()
        .position(|n| n["Name"] == "Outpost")
        .expect("an Outpost node");
    let children: Vec<&Value> = data
        .iter()
        .filter(|n| n.get("Parent").and_then(Value::as_u64) == Some(outpost_index as u64))
        .collect();
    assert_eq!(children.len(), 2, "outpost.sfmd should have two interior nodes");
    assert!(data[outpost_index].get("InteriorInputs").is_some(), "outposts carry InteriorInputs");
    let steel_ingot = children.iter().find(|n| n["Name"] == "Steel Ingot").unwrap();
    let coal_source = &steel_ingot["Inputs"]["Coal"][0];
    assert_eq!(coal_source[0].as_u64(), Some(outpost_index as u64));
    assert!(coal_source[1].is_u64(), "interior nodes address outpost ports by index");

    // Somersloops ride on ProductionShards, independently of ClockSpeed.
    let sloops = nodes(&by_name["sloops.sfmd"]);
    let shards: Vec<u64> =
        sloops.iter().filter_map(|n| n.get("ProductionShards").and_then(Value::as_u64)).collect();
    assert_eq!(shards, vec![1, 2], "sloops.sfmd pins ProductionShards 1 and 2");
    assert!(sloops.iter().all(|n| n["ClockSpeed"] == "250"));

    // Storage containers carry a Capacity string we have a variant for.
    let storage = nodes(&by_name["storage.sfmd"]);
    let capacities: BTreeSet<&str> = storage
        .iter()
        .filter_map(|n| n.get("Capacity").and_then(Value::as_str))
        .collect();
    for capacity in &capacities {
        assert!(
            [
                names::Capacity::Full,
                names::Capacity::Empty,
                names::Capacity::InputEqualsOutput,
            ]
            .iter()
            .any(|c| c.as_str() == Some(capacity)),
            "unhandled Storage Container Capacity {capacity:?}",
        );
    }
    assert_eq!(capacities.len(), 3, "storage.sfmd covers Full, Empty and Input = Output");

    // Generators are named per fuel, and every one of those names is ours.
    let table = names::table();
    for node in nodes(&by_name["generators.sfmd"]) {
        let name = node["Name"].as_str().unwrap();
        assert!(
            names_generator_exists(table, name),
            "{name} is not produced by any (building, fuel) pair",
        );
    }
}

fn names_generator_exists(table: &names::NameTable, name: &str) -> bool {
    // Exhaustive over the fuels a save can present.
    const BUILDINGS: [&str; 4] = [
        "Build_GeneratorCoal_C",
        "Build_GeneratorFuel_C",
        "Build_GeneratorNuclear_C",
        "Build_GeneratorGeoThermal_C",
    ];
    const FUELS: [&str; 12] = [
        "Desc_Coal_C",
        "Desc_CompactedCoal_C",
        "Desc_PetroleumCoke_C",
        "Desc_LiquidFuel_C",
        "Desc_LiquidTurboFuel_C",
        "Desc_RocketFuel_C",
        "Desc_IonizedFuel_C",
        "Desc_LiquidBiofuel_C",
        "Desc_NuclearFuelRod_C",
        "Desc_PlutoniumFuelRod_C",
        "Desc_FicsoniumFuelRod_C",
        "",
    ];
    BUILDINGS
        .iter()
        .any(|b| FUELS.iter().any(|f| table.generator_node(b, f) == Some(name)))
}

fn join(items: &BTreeSet<String>) -> String {
    items.iter().cloned().collect::<Vec<_>>().join("\n  ")
}
