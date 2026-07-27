//! Compact dump of `modeler::graph` for one save: every boundary building
//! with the facts the aggregator keys on, plus the transport networks they
//! attach to. Small enough to eyeball against a purpose-built test save
//! (docs/modeler-test-saves.md), which is how the aggregation rules get
//! checked against a factory whose correct answer is known.
//!
//!     cargo run --release --example modeler_dump -- save.sav

use sav_core::level::parse_full_save_lean;
use sav_core::modeler::graph::{self, Direction, Kind};
use sav_core::object::ClassTables;
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: modeler_dump <save.sav>");
    let bytes = std::fs::read(&path).expect("read save");
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);

    let graph = graph::build(&store);
    let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy();
    println!("== {name} ==");
    println!("{:?}\n", graph.stats);

    // Only networks a boundary actually touches are interesting; a save has
    // thousands of stray singletons (unconnected poles, spare belt pieces).
    let mut used: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for boundary in &graph.boundaries {
        for port in &boundary.ports {
            let entry = used.entry(port.network).or_insert((0, 0));
            match port.direction {
                Direction::Out => entry.0 += 1,
                Direction::In => entry.1 += 1,
            }
        }
    }

    // Group identical boundaries so a 6-machine test save reads as 6 lines,
    // and so the aggregation the exporter will do is visible by eye.
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for boundary in &graph.boundaries {
        if boundary.ports.is_empty() && boundary.recipe.is_none() {
            continue; // unbuilt/decorative
        }
        let mut inputs: Vec<String> =
            ports(boundary, Direction::In).iter().map(|n| format!("n{n}")).collect();
        let mut outputs: Vec<String> =
            ports(boundary, Direction::Out).iter().map(|n| format!("n{n}")).collect();
        inputs.sort();
        outputs.sort();
        let sloops = if boundary.somersloops > 0 {
            format!(" sloops={}", boundary.somersloops)
        } else {
            String::new()
        };
        let conflict = if boundary.boost_disagrees_with_slots { "  [BOOST/SLOT CONFLICT]" } else { "" };
        let fuel = boundary.fuel.as_deref().map(|f| format!(" fuel={f}")).unwrap_or_default();
        let stock = if boundary.stock.is_empty() {
            String::new()
        } else {
            format!(" stock={}", boundary.stock.join("+"))
        };
        grouped
            .entry(format!(
                "{:<34} {:<16} clock={:<7}{sloops}{fuel}{stock}  in=[{}] out=[{}]{conflict}",
                boundary.recipe.clone().unwrap_or_else(|| boundary.class.clone()),
                kind_name(&boundary.kind),
                format!("{:.4}", boundary.clock),
                inputs.join(","),
                outputs.join(","),
            ))
            .and_modify(|n| *n += 1)
            .or_insert(1);
    }

    println!("boundaries (identical rows collapsed -- the count is what `Max` becomes):");
    for (row, count) in &grouped {
        println!("  {count:3}x  {row}");
    }

    explain_orphans(&graph);

    println!("\nnetworks touched by a boundary (id: producers -> consumers, members):");
    for (network, (producers, consumers)) in &used {
        println!(
            "  n{network}: {producers} feeding, {consumers} drawing, {} buildables",
            graph.network_members[*network],
        );
    }
}

/// Why does an extractor end up with nothing consuming it? Lists, for every
/// network an extractor feeds that has no recipe machine drawing from it,
/// what else is attached -- which separates a real dead end in the save from
/// something the exporter is dropping.
fn explain_orphans(graph: &graph::FactoryGraph) {
    // network -> boundaries drawing from it
    let mut drawers: BTreeMap<usize, Vec<&graph::Boundary>> = BTreeMap::new();
    for boundary in &graph.boundaries {
        for port in &boundary.ports {
            if port.direction == Direction::In {
                drawers.entry(port.network).or_default().push(boundary);
            }
        }
    }

    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for boundary in &graph.boundaries {
        if boundary.kind != Kind::Extractor {
            continue;
        }
        for port in ports(boundary, Direction::Out) {
            let consumers = drawers.get(&port).map(Vec::as_slice).unwrap_or(&[]);
            let has_recipe_machine =
                consumers.iter().any(|c| c.kind == Kind::Machine && c.recipe.is_some());
            if has_recipe_machine {
                continue;
            }
            let reason = if consumers.is_empty() {
                "nothing at all draws from this network".to_string()
            } else {
                let mut kinds: Vec<String> = consumers
                    .iter()
                    .map(|c| {
                        if c.kind == Kind::Machine {
                            format!("{} (no recipe set)", c.class)
                        } else {
                            format!("{} [{}]", c.class, kind_name(&c.kind))
                        }
                    })
                    .collect();
                kinds.sort();
                kinds.dedup();
                kinds.join(", ")
            };
            *reasons.entry(format!("{} -> {reason}", boundary.class)).or_insert(0) += 1;
        }
    }

    println!("\nextractor outputs with no recipe machine consuming them:");
    let mut rows: Vec<_> = reasons.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (reason, count) in rows.into_iter().take(20) {
        println!("  {count:5}  {reason}");
    }
}

fn ports(boundary: &graph::Boundary, direction: Direction) -> Vec<usize> {
    let mut networks: Vec<usize> = boundary
        .ports
        .iter()
        .filter(|p| p.direction == direction)
        .map(|p| p.network)
        .collect();
    networks.sort_unstable();
    networks.dedup();
    networks
}

fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Machine => "machine",
        Kind::Extractor => "extractor",
        Kind::Generator => "generator",
        Kind::Sink => "sink",
        Kind::SpaceElevator => "space-elevator",
        Kind::DimensionalDepot => "depot",
        Kind::VehicleStation => "vehicle-station",
    }
}
