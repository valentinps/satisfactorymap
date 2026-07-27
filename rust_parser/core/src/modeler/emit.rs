//! Serialize the aggregated graph as a Modeler `.sfmd` file.
//!
//! The shape is pinned by `tests/modeler_names.rs`, which reads real files
//! saved out of the tool. Two details are easy to get wrong and produce a
//! file that loads but is silently incorrect rather than one that errors:
//!
//!  - `Max` is a mixed fraction (`"50 2/5"`) but `ClockSpeed` is a decimal
//!    (`"166.6666666"`). Different notations, different formatters.
//!  - `Inputs` has two shapes. Recipe nodes key by ingredient name; nodes
//!    that accept anything (`AWESOME Sink`, `Storage Container`,
//!    `Dimensional Depot`) carry the item on the edge inside a positional
//!    list, one group per incoming connection.

use super::aggregate::{ModelerGraph, Node};
use super::fraction;
use super::names;
use serde_json::{json, Map, Value};

/// Settings block Modeler writes at the top of every file.
fn document(data: Value, solver: &str) -> Value {
    json!({
        "Version": "1.0",
        "Language": "en-US",
        "Solver": solver,
        "Zoom": 1.0,
        "PanX": 0,
        "PanY": 0,
        "UseBuildingGrid": false,
        "BuildingGridX": "100",
        "BuildingGridY": "100",
        "UseConnectionGrid": false,
        "ConnectionGridX": "20",
        "ConnectionGridY": "20",
        "Path": "2D",
        "SpaceElevatorMultiplier": "1",
        "InputMultiplier": "2",
        "PowerMultiplier": "5",
        "Data": data,
    })
}

pub fn to_json(graph: &ModelerGraph) -> Value {
    let table = names::table();
    let data: Vec<Value> =
        graph.nodes.iter().map(|node| node_json(node, &graph.nodes, table)).collect();
    document(Value::Array(data), &graph.solver)
}

pub fn to_string(graph: &ModelerGraph) -> String {
    // Compact, like Modeler's own output -- these files are not hand-edited.
    serde_json::to_string(&to_json(graph)).expect("the graph is plain data")
}

fn node_json(node: &Node, nodes: &[Node], table: &names::NameTable) -> Value {
    debug_assert!(
        table.is_known_node(&node.name),
        "emitting a node name Modeler will reject: {}",
        node.name,
    );
    let mut object = Map::new();
    object.insert("Name".into(), Value::String(node.name.clone()));
    object.insert("X".into(), json!(node.position[0].round() as i64));
    object.insert("Y".into(), json!(node.position[1].round() as i64));

    if let Some(max) = node.max {
        object.insert("Max".into(), Value::String(fraction::format(max)));
    }
    // Omitted means 100 %, which is how Modeler's own files read.
    if (node.clock_percent - 100.0).abs() > 1e-9 {
        object.insert(
            "ClockSpeed".into(),
            Value::String(fraction::format_percent(node.clock_percent)),
        );
    }
    if node.somersloops > 0 {
        object.insert("ProductionShards".into(), json!(node.somersloops));
    }
    if let Some(capacity) = node.capacity {
        // Omitted Capacity means "Partially Full".
        if let Some(text) = capacity.as_str() {
            object.insert("Capacity".into(), Value::String(text.into()));
        }
        object.insert("ShowPpm".into(), Value::Bool(true));
    }

    if !node.inputs.is_empty() {
        object.insert("Inputs".into(), inputs_json(node, nodes, table));
    }
    Value::Object(object)
}

fn inputs_json(node: &Node, nodes: &[Node], table: &names::NameTable) -> Value {
    if node.generic_inputs {
        // The OUTER array is the node's input PORTS, not its connections. An
        // AWESOME Sink has exactly one port, so every source belongs in one
        // group; emitting one group per source made Modeler index a port that
        // does not exist and refuse the whole file
        // ("ArrayIndexOutOfBoundsException: Index 1 out of bounds for length 1").
        // Multi-port nodes -- Outposts -- are the exception, and are built
        // with their groups laid out explicitly elsewhere.
        Value::Array(vec![Value::Array(
            node.inputs.iter().map(|edge| source_json(edge.from, &edge.item, nodes, table)).collect(),
        )])
    } else {
        // Ingredient name -> every node supplying it.
        let mut by_item: Map<String, Value> = Map::new();
        for edge in &node.inputs {
            let key = item_name(table, &edge.item);
            let source = match nodes[edge.from].output_port {
                Some(port) => json!([edge.from, port]),
                None => json!(edge.from),
            };
            match by_item.entry(key).or_insert_with(|| Value::Array(Vec::new())) {
                Value::Array(sources) => sources.push(source),
                _ => unreachable!("entries are seeded as arrays"),
            }
        }
        Value::Object(by_item)
    }
}

/// One entry inside a generic-input group.
///
/// A port-addressed source (storage container, priority splitter) is named as
/// `[index, port]` -- the port already says which item flows, so no name is
/// carried. Everything else is `[index, "Item"]`, which is how the item gets
/// picked out of a recipe's several outputs.
fn source_json(from: usize, item: &str, nodes: &[Node], table: &names::NameTable) -> Value {
    match nodes[from].output_port {
        Some(port) => json!([from, port]),
        None => json!([from, item_name(table, item)]),
    }
}

fn item_name(table: &names::NameTable, item_short_class: &str) -> String {
    table.item(item_short_class).unwrap_or(item_short_class).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::aggregate::{Edge, ModelerGraph, Report};

    fn node(name: &str, generic: bool, inputs: Vec<Edge>) -> Node {
        Node {
            name: name.into(),
            max: Some(3.0),
            clock_percent: 100.0,
            somersloops: 0,
            capacity: None,
            generic_inputs: generic,
            position: [0.0, 0.0, 0.0],
            inputs,
            building_count: 3,
            output_port: None,
        }
    }

    #[test]
    fn recipe_nodes_key_inputs_by_ingredient_and_merge_sources() {
        let graph = ModelerGraph {
            nodes: vec![
                node("Iron Plate", false, vec![]),
                node("Iron Plate", false, vec![]),
                node(
                    "Reinforced Iron Plate",
                    false,
                    vec![
                        Edge { item: "Desc_IronPlate_C".into(), from: 0 },
                        Edge { item: "Desc_IronPlate_C".into(), from: 1 },
                        Edge { item: "Desc_IronScrew_C".into(), from: 1 },
                    ],
                ),
            ],
            report: Report::default(),
            solver: "Full".into(),
        };
        let value = to_json(&graph);
        let inputs = &value["Data"][2]["Inputs"];
        // Two suppliers of the same ingredient share one array.
        assert_eq!(inputs["Iron Plate"], json!([0, 1]));
        // And the item name is Modeler's, not the save's class.
        assert_eq!(inputs["Screw"], json!([1]));
    }

    #[test]
    fn generic_input_nodes_carry_the_item_on_the_edge() {
        let graph = ModelerGraph {
            nodes: vec![
                node("Iron Plate", false, vec![]),
                node(
                    "AWESOME Sink",
                    true,
                    vec![Edge { item: "Desc_IronPlate_C".into(), from: 0 }],
                ),
            ],
            report: Report::default(),
            solver: "Full".into(),
        };
        let value = to_json(&graph);
        assert_eq!(value["Data"][1]["Inputs"], json!([[[0, "Iron Plate"]]]));
    }

    /// Regression: Modeler rejected a whole file with
    /// "ArrayIndexOutOfBoundsException: Index 1 out of bounds for length 1"
    /// because the outer array is the node's input PORTS. A sink has one.
    #[test]
    fn a_generic_node_puts_every_source_in_its_single_port() {
        let graph = ModelerGraph {
            nodes: vec![
                node("Iron Plate", false, vec![]),
                node("Iron Plate", false, vec![]),
                node(
                    "AWESOME Sink",
                    true,
                    vec![
                        Edge { item: "Desc_IronPlate_C".into(), from: 0 },
                        Edge { item: "Desc_IronPlate_C".into(), from: 1 },
                    ],
                ),
            ],
            report: Report::default(),
            solver: "Full".into(),
        };
        let value = to_json(&graph);
        assert_eq!(
            value["Data"][2]["Inputs"],
            json!([[[0, "Iron Plate"], [1, "Iron Plate"]]]),
            "one port holding two connections, not two ports",
        );
    }

    /// Regression: a bare index into a storage container draws no connection
    /// at all in Modeler, which left a truck-station stub supplying nothing.
    #[test]
    fn storage_shaped_sources_are_addressed_by_output_port() {
        let mut stub = node("Storage Container", true, vec![]);
        stub.capacity = Some(names::Capacity::Full);
        stub.output_port = Some(0);
        stub.max = None;
        let graph = ModelerGraph {
            nodes: vec![
                stub,
                node("Iron Ingot", false, vec![Edge { item: "Desc_OreIron_C".into(), from: 0 }]),
                node("AWESOME Sink", true, vec![Edge { item: "Desc_OreIron_C".into(), from: 0 }]),
            ],
            report: Report::default(),
            solver: "Full".into(),
        };
        let value = to_json(&graph);
        // Dict form: [index, port] instead of a bare index.
        assert_eq!(value["Data"][1]["Inputs"]["Iron Ore"], json!([[0, 0]]));
        // Positional form: the port replaces the item name entirely.
        assert_eq!(value["Data"][2]["Inputs"], json!([[[0, 0]]]));
    }

    #[test]
    fn max_is_a_fraction_but_clock_speed_is_a_decimal() {
        let mut plate = node("Iron Plate", false, vec![]);
        plate.max = Some(50.4);
        plate.clock_percent = 500.0 / 3.0; // 166.66...
        plate.somersloops = 2;
        let graph = ModelerGraph { nodes: vec![plate], report: Report::default(), solver: "Full".into() };
        let value = to_json(&graph);
        assert_eq!(value["Data"][0]["Max"], json!("50 2/5"));
        assert_eq!(value["Data"][0]["ClockSpeed"], json!("166.6666666667"));
        assert_eq!(value["Data"][0]["ProductionShards"], json!(2));
    }

    #[test]
    fn a_hundred_percent_clock_is_omitted_entirely() {
        let graph =
            ModelerGraph {
            nodes: vec![node("Iron Plate", false, vec![])],
            report: Report::default(),
            solver: "Full".into(),
        };
        let value = to_json(&graph);
        assert!(value["Data"][0].get("ClockSpeed").is_none());
        assert!(value["Data"][0].get("ProductionShards").is_none());
        assert!(value["Data"][0].get("Inputs").is_none());
    }

    #[test]
    fn the_document_header_matches_what_modeler_writes() {
        let graph = ModelerGraph { nodes: Vec::new(), report: Report::default(), solver: "Full".into() };
        let value = to_json(&graph);
        assert_eq!(value["Version"], "1.0");
        assert_eq!(value["Solver"], "Full");
        assert_eq!(value["Path"], "2D");
        assert_eq!(value["Data"], json!([]));
    }
}
