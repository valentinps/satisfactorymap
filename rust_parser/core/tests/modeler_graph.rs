//! `modeler::graph` against purpose-built saves whose correct answer is known
//! by construction (see docs/modeler-test-saves.md). PhantomTorture proves the
//! export scales; these prove it is right.
//!
//! The saves live in `map/uploads/` like the rest of the corpus. They are not
//! in the published `test-saves-v1` release yet, so a missing file skips
//! rather than fails -- unlike the older fixtures, which hard-fail.

use sav_core::level::parse_full_save_lean;
use sav_core::modeler::graph::{self, Boundary, Direction, FactoryGraph, Kind};
use sav_core::object::ClassTables;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn load(name: &str) -> Option<FactoryGraph> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("skipping: {name} not in map/uploads (see docs/modeler-test-saves.md)");
            return None;
        }
    };
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    Some(graph::build(&store))
}

fn networks(boundary: &Boundary, direction: Direction) -> Vec<usize> {
    let mut ids: Vec<usize> = boundary
        .ports
        .iter()
        .filter(|p| p.direction == direction)
        .map(|p| p.network)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn with_recipe<'a>(graph: &'a FactoryGraph, recipe: &str) -> Vec<&'a Boundary> {
    graph.boundaries.iter().filter(|b| b.recipe.as_deref() == Some(recipe)).collect()
}

/// The save that caught the clock bug: three constructors at 100 %, two at
/// 250 %, one at 100 % with a somersloop, all between one splitter tree and
/// one merger tree.
///
/// The 250 % machines have never run, so they carry `mPendingPotential` and
/// no `mCurrentPotential` at all. Reading only the latter reported every one
/// of them as 100 %, which is exactly the silent wrongness this save exists
/// to catch.
#[test]
fn clock_and_somersloop_groups_split_but_stay_interchangeable() {
    let Some(graph) = load("modeler_clocks.sav") else { return };
    let plates = with_recipe(&graph, "Recipe_IronPlate_C");
    assert_eq!(plates.len(), 6, "six Iron Plate constructors were built");

    // (clock as permille to keep it hashable, somersloops) -> count
    let mut groups: BTreeMap<(u64, u32), usize> = BTreeMap::new();
    for plate in &plates {
        *groups.entry(((plate.clock * 1000.0).round() as u64, plate.somersloops)).or_insert(0) += 1;
    }
    assert_eq!(
        groups,
        BTreeMap::from([((1000, 0), 3), ((2500, 0), 2), ((1000, 1), 1)]),
        "expected 3 at 100%, 2 at 250%, 1 at 100% with one somersloop",
    );

    // Every one of them draws from the same network and feeds the same one,
    // so only clock/sloops may split them apart. If the chained splitters had
    // failed to contract, these would differ and the aggregation would
    // fragment for the wrong reason.
    let inputs = networks(plates[0], Direction::In);
    let outputs = networks(plates[0], Direction::Out);
    assert_eq!(inputs.len(), 1);
    assert_eq!(outputs.len(), 1);
    assert_ne!(inputs, outputs, "the input and output runs must stay separate networks");
    for plate in &plates {
        assert_eq!(networks(plate, Direction::In), inputs);
        assert_eq!(networks(plate, Direction::Out), outputs);
    }

    // A somersloop physically installed on a machine that has never
    // recomputed its boost is normal, not a conflict.
    assert!(!plates.iter().any(|p| p.boost_disagrees_with_slots));

    // The standalone Assembler carries ONE of its two somersloop slots, so
    // its boost multiplier is 1.5. That half-boost is the sharpest form of
    // this assertion: reading the multiplier instead of the physical slot
    // count cannot even produce a whole number here, let alone the right one.
    let assemblers = with_recipe(&graph, "Recipe_IronPlateReinforced_C");
    assert_eq!(assemblers.len(), 1);
    assert_eq!(assemblers[0].somersloops, 1, "one sloop installed, not two");
}

/// Two Iron Plate lines with no belt between them must stay two groups.
#[test]
fn unconnected_lines_making_the_same_thing_do_not_merge() {
    let Some(graph) = load("modeler_independent.sav") else { return };
    let plates = with_recipe(&graph, "Recipe_IronPlate_C");
    assert_eq!(plates.len(), 8, "two lines of 6 and 2 constructors");

    let mut by_line: BTreeMap<(Vec<usize>, Vec<usize>), usize> = BTreeMap::new();
    for plate in &plates {
        *by_line
            .entry((networks(plate, Direction::In), networks(plate, Direction::Out)))
            .or_insert(0) += 1;
    }
    let mut sizes: Vec<usize> = by_line.values().copied().collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![2, 6], "the two lines must key differently, not merge into one 8");
}

/// One belt network carrying several item types, with consumers hanging off
/// it. The aggregator has to route per item; this asserts the raw material it
/// works from.
#[test]
fn a_mixed_bus_is_one_network_with_several_producers_and_consumers() {
    let Some(graph) = load("modeler_sushi.sav") else { return };

    let bus: Vec<usize> = graph
        .boundaries
        .iter()
        .flat_map(|b| b.ports.iter().map(|p| p.network))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(bus.len(), 1, "producers and consumers all share one bus");
    let bus = bus[0];

    let feeding: Vec<&str> = graph
        .boundaries
        .iter()
        .filter(|b| b.ports.iter().any(|p| p.network == bus && p.direction == Direction::Out))
        .filter_map(|b| b.recipe.as_deref())
        .collect();
    let drawing: Vec<&str> = graph
        .boundaries
        .iter()
        .filter(|b| b.ports.iter().any(|p| p.network == bus && p.direction == Direction::In))
        .filter_map(|b| b.recipe.as_deref())
        .collect();

    // Three different items go onto one belt: Iron Plate, Iron Rod, and
    // Screw (via the Cast Screw alternate, straight from ingots).
    for producer in ["Recipe_IronPlate_C", "Recipe_IronRod_C", "Recipe_Alternate_Screw_C"] {
        assert!(feeding.contains(&producer), "{producer} should feed the bus");
    }
    // Two consumers hang off that same bus, and they want different things:
    // the Screw constructor takes Iron Rod only (1 of the 3 items present),
    // the Reinforced Iron Plate assembler takes Iron Plate and Screw but not
    // Iron Rod (2 of 3). That asymmetry is what makes this save able to tell
    // correct per-item routing apart from wiring the whole bus to everything;
    // the routing itself is asserted in the aggregator's own tests.
    assert!(drawing.contains(&"Recipe_Screw_C"), "the Screw constructor draws from the bus");
    assert!(drawing.contains(&"Recipe_IronPlateReinforced_C"), "RIP draws from the bus");
    assert_eq!(drawing.len(), 2, "exactly two consumers on the bus");
}

/// Pipes contract exactly like belts, and a water pump's only port is named
/// `.FGPipeConnectionFactory` -- no Input/Output in the name. Before the
/// kind-based direction fallback, that orphaned every pump in the save.
#[test]
fn fluid_buildings_attach_despite_undirected_port_names() {
    let Some(graph) = load("modeler_fluids.sav") else { return };
    let pumps: Vec<&Boundary> =
        graph.boundaries.iter().filter(|b| b.kind == Kind::Extractor).collect();
    assert_eq!(pumps.len(), 2, "two water extractors");
    for pump in &pumps {
        assert_eq!(networks(pump, Direction::Out).len(), 1, "pump must reach the pipe network");
        assert!(networks(pump, Direction::In).is_empty(), "an extractor only ever outputs");
    }
    assert_eq!(graph.stats.undirected_ports, 0, "every port resolved a direction");

    // Both pumps and both packagers share one contracted pipe network.
    let shared = networks(pumps[0], Direction::Out)[0];
    assert_eq!(networks(pumps[1], Direction::Out)[0], shared);
    let packagers = graph
        .boundaries
        .iter()
        .filter(|b| b.class.starts_with("Build_Packager"))
        .filter(|b| networks(b, Direction::In).contains(&shared))
        .count();
    assert_eq!(packagers, 2, "both packagers draw from the same pipe network");
}

/// A material cycle must not hang the graph build, and both directions of the
/// loop have to survive contraction.
#[test]
fn a_material_loop_survives_contraction() {
    let Some(graph) = load("modeler_loop.sav") else { return };
    let pack = with_recipe(&graph, "Recipe_PackagedWater_C");
    let unpack = with_recipe(&graph, "Recipe_UnpackageWater_C");
    assert_eq!((pack.len(), unpack.len()), (1, 1), "one packager each way");

    // Each one feeds a network the other draws from -- water out through the
    // pipes, packaged water back over the belt. That is the cycle the layout
    // pass has to break without spinning.
    let packed = networks(pack[0], Direction::Out);
    assert!(
        packed.iter().any(|n| networks(unpack[0], Direction::In).contains(n)),
        "packaged water must reach the unpackager",
    );
    assert!(
        networks(unpack[0], Direction::Out)
            .iter()
            .any(|n| networks(pack[0], Direction::In).contains(n)),
        "and the water must flow back into the packager",
    );
}

/// Vehicular transport is deliberately not traversed, so the station is a
/// terminal: it feeds a network but nothing feeds it.
#[test]
fn a_vehicle_station_terminates_the_graph() {
    let Some(graph) = load("modeler_vehicle.sav") else { return };
    let stations: Vec<&Boundary> =
        graph.boundaries.iter().filter(|b| b.kind == Kind::VehicleStation).collect();
    assert_eq!(stations.len(), 1, "one truck station");
    let unloads_onto = networks(stations[0], Direction::Out);
    assert_eq!(unloads_onto.len(), 1, "it unloads onto a belt");
    assert!(
        networks(stations[0], Direction::In).is_empty(),
        "nothing on this map feeds the station -- its supply arrives by truck",
    );

    // The chain behind it is real, and its first machine draws from the very
    // network the station feeds. Without a stub node standing in for the
    // station, that smelter has no producer and Modeler solves this whole
    // chain to zero.
    let smelter = with_recipe(&graph, "Recipe_IngotIron_C");
    assert_eq!(smelter.len(), 1);
    assert_eq!(networks(smelter[0], Direction::In), unloads_onto);
    assert_eq!(with_recipe(&graph, "Recipe_IronPlate_C").len(), 1);
}
