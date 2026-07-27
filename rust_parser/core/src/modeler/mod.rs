//! Export a parsed save as a Satisfactory Modeler `.sfmd` planning graph.
//!
//! Modeler models a factory as a few hundred *recipe* nodes carrying a
//! machine count, connected by direct producer -> consumer edges. A save
//! holds the opposite: thousands of individual machines wired through belts,
//! lifts, splitters, mergers and pipes. Bridging the two is a collapse, in
//! three steps:
//!
//!   1. `graph`     -- contract every belt/pipe/splitter/merger run into a
//!                     single *transport network*, so a split/merge tree
//!                     stops being thousands of edges.
//!   2. `aggregate` -- merge machines that are interchangeable through those
//!                     networks into one node with `Max` = machine count.
//!   3. `layout`/`emit` -- place and serialize.
//!
//! Measured on a 1.24 GB save (`examples/modeler_probe.rs`): 8 173 machines
//! and ~222 k logistics buildables collapse to 377 nodes and 498 edges.
//!
//! Modeler's own docs warn that its Full solver "can slow down a lot on
//! larger builds" and recommend minimal connections between sections, so
//! that collapse is the point of the whole module, not an optimization.

pub mod aggregate;
pub mod emit;
pub mod fraction;
pub mod graph;
pub mod layout;
pub mod names;
pub mod rates;

use crate::store::SaveStore;

/// The whole pipeline: contract the save's logistics, merge interchangeable
/// machines, lay the result out, serialize it.
///
/// Returns the `.sfmd` text and a report of everything that could not be
/// represented -- unmapped recipes, ingredients nothing supplies, buildings
/// dropped. The report is not an error channel; a healthy export still has
/// entries in it (a factory fed by train genuinely has unsupplied inputs),
/// which is exactly why it is returned rather than logged.
pub fn export(store: &SaveStore, options: &aggregate::Options) -> (String, aggregate::Report) {
    let factory = graph::build(store);
    let mut graph = aggregate::build(&factory, options);
    layout::apply(&mut graph.nodes);
    (emit::to_string(&graph), graph.report)
}
