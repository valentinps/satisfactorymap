//! Step 2 of the collapse: turn boundary machines attached to transport
//! networks into Modeler nodes and edges.
//!
//! Two ideas do all the work.
//!
//! **Machines merge when they are interchangeable.** The group key is
//! `(identity, clock, somersloops, input networks, output networks)`. Eight
//! constructors hanging off one splitter tree share all five and become a
//! single node with `Max 8`; two Iron Plate lines with no belt between them
//! differ in their networks and stay two nodes. Independence is therefore
//! structural, not a heuristic.
//!
//! **Edges are derived per item, not per belt.** A network's item set comes
//! from what its producers make; a consumer gets an edge only for the items
//! its own recipe actually takes. That is what lets a "sushi" bus carrying
//! three item types feed a machine that wants only one of them, without
//! inventing connections that do not exist.

use super::graph::{Boundary, Direction, FactoryGraph, Kind};
use super::names::{self, Capacity};
use super::rates;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Options {
    /// Emit a `Storage Container` stub for vehicle stations. Without one, a
    /// factory fed only by train or truck has no producer at all and
    /// Modeler solves the entire chain downstream of it to zero.
    pub station_stubs: bool,
    /// Route dense producer x consumer blocks through a shared hub node.
    pub bus_hubs: bool,
    /// Collapse every extractor of the same resource into one node, instead
    /// of one per pipe/belt network.
    ///
    /// Off by default because it is a genuine loss of fidelity: a mature save
    /// has hundreds of independent water supplies, one per refinery cluster,
    /// and merging them lets the solver move water between clusters that
    /// share no pipe. It is offered because the alternative dominates the
    /// node count -- 306 of 951 nodes on the reference save were `Water` --
    /// and raw resources are the one case where "how much do I need in
    /// total" is usually the question being asked.
    pub merge_resource_nodes: bool,
    /// Which of Modeler's calculators the file asks for: "Full", "Basic",
    /// "Manual" or "None".
    ///
    /// **Basic**, not Modeler's own default of Full. Per Modeler's docs, Full
    /// "accounts for splitter/merger preference to divide flows evenly" and
    /// "can be quite slow on larger builds", while Basic "ignores splitter and
    /// merger evening preferences", "treats entered values as limits" and
    /// "should always be fast".
    ///
    /// Both of those differences are no-ops for an exported save. The export
    /// contains no splitters or mergers -- they are contracted into transport
    /// networks and the routing is resolved before anything is written -- and
    /// every number emitted already *is* a limit: `Max` is a building count,
    /// and a resource node's `Max` is items/min. So Full costs its full price
    /// and buys nothing, and on a whole-base graph it gives up entirely and
    /// leaves every value as "?".
    pub solver: String,
    /// Give every product nothing consumes somewhere to go.
    ///
    /// Modeler's "all zeros" page is blunt about why this matters: a backed-up
    /// output deadlocks its producer, and "if you made that in the game, it
    /// would also have locked up and come to a halt". One unconsumed byproduct
    /// therefore zeroes its whole upstream chain. Its prescribed fix is to
    /// "give a place for extra resources to go", which is what this does --
    /// an AWESOME Sink for parts, a Storage Container set to Empty for fluids,
    /// since fluids cannot be sunk in the game.
    pub overflow_sinks: bool,
    /// Group machines by the *items* they exchange rather than by the exact
    /// transport networks they sit on.
    ///
    /// The default keys on networks, which is the faithful reading: 164 coal
    /// generators on 164 separate water supplies really are 164 independent
    /// installations. This trades that away for readability -- every coal
    /// generator burning coal at the same clock becomes one node -- at the
    /// cost of implying flow can move between networks that share no pipe.
    /// It also merges genuinely independent production lines, so it is off by
    /// default.
    pub merge_by_item: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            station_stubs: true,
            bus_hubs: true,
            merge_resource_nodes: false,
            solver: "Basic".to_string(),
            overflow_sinks: true,
            merge_by_item: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// `Desc_X_C` short class of what flows along this edge.
    pub item: String,
    /// Index into [`ModelerGraph::nodes`].
    pub from: usize,
}

#[derive(Clone, Debug)]
pub struct Node {
    /// The Modeler node name; always one `names::table()` can produce.
    pub name: String,
    /// Building count for machines and sinks, items/min for resource nodes,
    /// absent where Modeler should decide for itself.
    pub max: Option<f64>,
    pub clock_percent: f64,
    pub somersloops: u32,
    /// Storage Container mode. `Some` only on storage-shaped nodes.
    pub capacity: Option<Capacity>,
    /// Modeler keys a recipe node's inputs by ingredient name, but nodes that
    /// accept anything (`AWESOME Sink`, `Storage Container`, `Dimensional
    /// Depot`) carry the item on the edge instead. This picks the shape.
    pub generic_inputs: bool,
    /// World centroid of the machines behind this node; layout input.
    pub position: [f64; 3],
    pub inputs: Vec<Edge>,
    /// How many save buildings collapsed into this node (0 for synthesized
    /// hub nodes).
    pub building_count: usize,
    /// When another node draws from this one, which output port it must
    /// name. `None` means a bare node index is enough.
    ///
    /// Storage containers and priority splitters have addressable outputs and
    /// have to be referenced as `[index, port]`; Modeler silently draws no
    /// connection at all for a bare index into one, which is how a truck
    /// station stub ended up supplying nothing.
    pub output_port: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Recipes in use that have no Modeler node name.
    pub unmapped_recipes: BTreeSet<String>,
    /// Buildings dropped from the export, by class and why.
    pub dropped: BTreeMap<String, String>,
    /// `(node name, item)` a machine needs but nothing on its input networks
    /// produces -- Modeler will treat these as externally supplied.
    pub unsupplied_inputs: BTreeSet<(String, String)>,
    /// `(node name, item)` produced into a network nothing draws from.
    pub unconsumed_outputs: BTreeSet<(String, String)>,
    /// Vehicle stations turned into stubs, with the item they offer.
    pub station_stubs: BTreeSet<(String, String)>,
    /// Extractor nodes and the rate assumed for them, so a wrong entry in the
    /// hardcoded extraction table is visible rather than silent.
    pub extractor_rates: BTreeSet<(String, String)>,
    /// Somersloop boost recorded with no somersloop installed.
    pub stale_somersloop_boosts: usize,
    pub hub_nodes: usize,
    pub edges_saved_by_hubs: usize,
    /// Surplus outputs given a destination so they do not deadlock.
    pub overflow_sinks: usize,
}

pub struct ModelerGraph {
    pub nodes: Vec<Node>,
    pub report: Report,
    pub solver: String,
}

/// Identity half of the group key: what makes two buildings the same *kind*
/// of node, before clock and networks are considered.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum Identity {
    Recipe(String),
    /// Class and purity both matter: they set the extraction rate.
    Resource { item: String, class: String, purity: String },
    Sink,
    SpaceElevator,
    Depot,
    /// Stations are never merged with each other -- two stations feeding the
    /// same belt are two independent supplies.
    Station(String),
    /// The Modeler node name already encodes the building AND the fuel, so
    /// one string is the whole identity.
    Generator { node: String, building: String, fuel: String },
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
struct GroupKey {
    identity: Identity,
    /// Clock in thousandths of a percent, so the key stays hashable and two
    /// machines set to the same slider position always land together.
    clock_milli: u64,
    somersloops: u32,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
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

/// A node that takes whatever reaches it, held back until the items arriving
/// on its networks are known.
struct Absorber {
    identity: Identity,
    inputs: Vec<usize>,
    position: [f64; 3],
    count: usize,
}

/// A single-part `Storage Container`, addressable as a source through port 0.
fn storage_node(capacity: Capacity, position: [f64; 3], count: usize) -> Node {
    Node {
        name: names::virtual_nodes::STORAGE_CONTAINER.to_string(),
        max: None,
        clock_percent: 100.0,
        somersloops: 0,
        capacity: Some(capacity),
        generic_inputs: true,
        position,
        inputs: Vec::new(),
        building_count: count,
        output_port: Some(0),
    }
}

/// A terminal node that swallows one part type: a sink or a depot upload.
/// No `Max` -- a limit would make it stop absorbing.
fn generic_node(name: &str, position: [f64; 3], count: usize) -> Node {
    Node {
        name: name.to_string(),
        max: None,
        clock_percent: 100.0,
        somersloops: 0,
        capacity: None,
        generic_inputs: true,
        position,
        inputs: Vec::new(),
        building_count: count,
        output_port: None,
    }
}

fn centroid(members: &[&Boundary]) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    for member in members {
        for axis in 0..3 {
            sum[axis] += member.position[axis] as f64;
        }
    }
    let n = members.len().max(1) as f64;
    [sum[0] / n, sum[1] / n, sum[2] / n]
}

pub fn build(graph: &FactoryGraph, options: &Options) -> ModelerGraph {
    let table = names::table();
    let mut report = Report::default();

    // -- Group interchangeable buildings. --------------------------------
    let mut groups: BTreeMap<GroupKey, Vec<&Boundary>> = BTreeMap::new();
    for boundary in &graph.boundaries {
        if boundary.boost_disagrees_with_slots {
            report.stale_somersloop_boosts += 1;
        }
        let Some(identity) = identity_of(boundary, table, &mut report) else { continue };
        if matches!(identity, Identity::Station(_)) && !options.station_stubs {
            continue;
        }
        // Merging resource nodes means dropping the network half of the key.
        // The flows are still taken from the members' real networks below, so
        // the merged node still feeds exactly the pipes its extractors do.
        let merged = options.merge_by_item
            || (options.merge_resource_nodes && matches!(identity, Identity::Resource { .. }));
        groups
            .entry(GroupKey {
                identity,
                clock_milli: (boundary.clock * 100_000.0).round() as u64,
                somersloops: boundary.somersloops,
                inputs: if merged { Vec::new() } else { networks(boundary, Direction::In) },
                outputs: if merged { Vec::new() } else { networks(boundary, Direction::Out) },
            })
            .or_default()
            .push(boundary);
    }

    // -- One node per group. ---------------------------------------------
    let mut nodes: Vec<Node> = Vec::new();
    // Parallel to `nodes`: what each one puts on / takes off which network.
    let mut produces: Vec<Vec<(usize, String)>> = Vec::new();
    let mut consumes: Vec<Vec<(usize, String)>> = Vec::new();
    // Nodes that accept whatever reaches them. They cannot be built in this
    // pass: each becomes several nodes, one per item that turns out to
    // arrive, which is only known once every producer exists.
    let mut pending_absorbers: Vec<Absorber> = Vec::new();

    for (key, members) in &groups {
        let clock_percent = key.clock_milli as f64 / 1000.0;
        let count = members.len();
        let position = centroid(members);

        // Taken from the members rather than the key, so that a merged
        // resource node -- whose key deliberately carries no networks -- still
        // feeds every pipe its extractors actually reach.
        let union = |direction: Direction| -> Vec<usize> {
            let mut all: Vec<usize> =
                members.iter().flat_map(|b| networks(b, direction)).collect();
            all.sort_unstable();
            all.dedup();
            all
        };
        let member_inputs = union(Direction::In);
        let member_outputs = union(Direction::Out);

        let (node, out_flows, in_flows) = match &key.identity {
            Identity::Recipe(recipe) => {
                let name = table.recipe_node(recipe).unwrap_or_default().to_string();
                let io = rates::recipe_io(recipe);
                let outs = io
                    .map(|io| {
                        member_outputs
                            .iter()
                            .flat_map(|n| io.products.iter().map(|p| (*n, p.item.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                let ins = io
                    .map(|io| {
                        member_inputs
                            .iter()
                            .flat_map(|n| io.ingredients.iter().map(|i| (*n, i.item.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    Node {
                        name,
                        max: Some(count as f64),
                        clock_percent,
                        somersloops: key.somersloops,
                        capacity: None,
                        generic_inputs: false,
                        position,
                        inputs: Vec::new(),
                        building_count: count,
                        output_port: None,
                    },
                    outs,
                    ins,
                )
            }
            Identity::Resource { item, class, purity } => {
                let name = table.resource_node(item).unwrap_or_default().to_string();
                let purity_enum = rates::Purity::parse(purity);
                let per_extractor = rates::extraction_rate(class, purity_enum).unwrap_or(0.0);
                // A resource node's Max is items/min, not a building count --
                // it is the one node type where that is true.
                let rate = per_extractor * count as f64 * clock_percent / 100.0;
                report
                    .extractor_rates
                    .insert((name.clone(), format!("{count} x {class} {purity} -> {rate}/min")));
                (
                    Node {
                        name,
                        max: Some(rate),
                        clock_percent: 100.0, // folded into the rate above
                        somersloops: 0,
                        capacity: None,
                        generic_inputs: false,
                        position,
                        inputs: Vec::new(),
                        building_count: count,
                        output_port: None,
                    },
                    member_outputs.iter().map(|n| (*n, item.clone())).collect(),
                    Vec::new(),
                )
            }
            Identity::Generator { node, building, fuel } => {
                // Fuel and coolant in, byproduct out. A generator produces
                // power, which Modeler tracks separately from parts, so the
                // only item flows are the physical ones.
                let io = rates::generator_io(building, fuel);
                let mut inputs: Vec<String> =
                    io.extra_inputs.iter().map(|s| s.to_string()).collect();
                if !fuel.is_empty() {
                    inputs.push(fuel.clone());
                }
                (
                    Node {
                        name: node.clone(),
                        max: Some(count as f64),
                        clock_percent,
                        somersloops: 0,
                        capacity: None,
                        generic_inputs: false,
                        position,
                        inputs: Vec::new(),
                        building_count: count,
                        output_port: None,
                    },
                    member_outputs
                        .iter()
                        .flat_map(|n| io.outputs.iter().map(|o| (*n, o.to_string())))
                        .collect(),
                    member_inputs
                        .iter()
                        .flat_map(|n| inputs.iter().map(|i| (*n, i.clone())))
                        .collect(),
                )
            }
            Identity::Sink | Identity::SpaceElevator | Identity::Depot => {
                // Deferred: a generic-input node in Modeler handles exactly
                // ONE part type -- its own sample files use four separate
                // AWESOME Sink nodes for four items -- and which items reach
                // these is only knowable once every producer is placed.
                pending_absorbers.push(Absorber {
                    identity: key.identity.clone(),
                    inputs: member_inputs.clone(),
                    position,
                    count,
                });
                continue;
            }
            Identity::Station(_) => {
                // Unloading onto a belt makes the station a supply the rest of
                // the map cannot otherwise see; loading from one makes it a
                // drain. Either way it is local -- no edge ever crosses the
                // map through it.
                //
                // The two sides are independent. A truck station commonly has
                // belts on both: ore in from the mine, something else out to
                // the factory. Treating it as unload-only silently discarded
                // the loading side whenever its cargo hold happened to be
                // empty, which orphaned the miners feeding it.
                let mut stock: Vec<String> =
                    members.iter().flat_map(|b| b.stock.iter().cloned()).collect();
                stock.sort();
                stock.dedup();
                for item in &stock {
                    report
                        .station_stubs
                        .insert((members[0].class.clone(), display(table, item)));
                }

                // Drain side: whatever it takes off a belt leaves the map.
                if !member_inputs.is_empty() {
                    pending_absorbers.push(Absorber {
                        identity: key.identity.clone(),
                        inputs: member_inputs.clone(),
                        position,
                        count,
                    });
                }

                // Supply side: one container per stocked item, for the
                // one-part-per-node rule. With an empty hold there is no
                // evidence of what arrives by vehicle, so nothing is invented.
                if !member_outputs.is_empty() {
                    if stock.is_empty() {
                        report.dropped.insert(
                            members[0].class.clone(),
                            "station unloads onto a belt but its cargo hold is empty, so \
                             there is no evidence of what it carries"
                                .into(),
                        );
                    }
                    for item in &stock {
                        // A Storage Container with no inputs has no part
                        // Modeler can infer, and connecting one to a typed
                        // input is rejected outright ("Cannot connect two
                        // different parts"). A raw resource has a node that
                        // needs no input and carries its own identity, so
                        // ore arriving by truck exports as an ore node.
                        // Anything else has no untyped-source equivalent, so
                        // it is reported rather than emitted broken.
                        let Some(name) = table.resource_node(item) else {
                            report.dropped.insert(
                                format!("{} carrying {}", members[0].class, display(table, item)),
                                "no way to express a non-raw part arriving by vehicle without \
                                 a source Modeler can type"
                                    .into(),
                            );
                            continue;
                        };
                        nodes.push(Node {
                            name: name.to_string(),
                            // Unknown: the vehicles decide, not the save.
                            max: None,
                            clock_percent: 100.0,
                            somersloops: 0,
                            capacity: None,
                            generic_inputs: false,
                            position,
                            inputs: Vec::new(),
                            building_count: count,
                            output_port: None,
                        });
                        produces
                            .push(member_outputs.iter().map(|n| (*n, item.clone())).collect());
                        consumes.push(Vec::new());
                    }
                }
                continue;
            }
        };

        if node.name.is_empty() {
            continue; // unmapped; already reported
        }
        nodes.push(node);
        produces.push(out_flows);
        consumes.push(in_flows);
    }

    // -- Per (network, item) producer sets. -------------------------------
    let mut producers: BTreeMap<(usize, String), BTreeSet<usize>> = BTreeMap::new();
    for (index, flows) in produces.iter().enumerate() {
        for (network, item) in flows {
            producers.entry((*network, item.clone())).or_default().insert(index);
        }
    }

    // -- Absorbers, now that the items reaching them are known. -----------
    // One node per item: Modeler rejects a file in which a single generic
    // node is wired to two different parts ("IllegalArgumentException: Cannot
    // connect two different parts").
    let mut consumers: BTreeMap<(usize, String), BTreeSet<usize>> = BTreeMap::new();
    for absorber in &pending_absorbers {
        let mut arriving: BTreeSet<String> = BTreeSet::new();
        for network in &absorber.inputs {
            arriving.extend(
                producers
                    .range((*network, String::new())..)
                    .take_while(|((n, _), _)| n == network)
                    .map(|((_, item), _)| item.clone()),
            );
        }
        for item in &arriving {
            let index = nodes.len();
            let node = match absorber.identity {
                Identity::Sink => generic_node(
                    names::virtual_nodes::AWESOME_SINK,
                    absorber.position,
                    absorber.count,
                ),
                Identity::Depot => generic_node(
                    names::virtual_nodes::DIMENSIONAL_DEPOT,
                    absorber.position,
                    absorber.count,
                ),
                // The Space Elevator absorbs parts until full and never runs
                // as a machine, so it exports as a storage container rather
                // than Modeler's own Space Elevator node.
                _ => storage_node(Capacity::Empty, absorber.position, absorber.count),
            };
            nodes.push(node);
            for network in &absorber.inputs {
                if producers.contains_key(&(*network, item.clone())) {
                    consumers.entry((*network, item.clone())).or_default().insert(index);
                }
            }
        }
    }

    // Recipe ingredients.
    for (index, flows) in consumes.iter().enumerate() {
        for (network, item) in flows {
            consumers.entry((*network, item.clone())).or_default().insert(index);
        }
    }

    // -- Edges. ----------------------------------------------------------
    let mut edges: BTreeSet<(usize, String, usize)> = BTreeSet::new(); // (to, item, from)
    // Hubs and overflow sinks share ONE list. They used to be two, with
    // indices computed as `nodes.len() + hubs.len()` and
    // `nodes.len() + hubs.len() + overflow.len()` -- both evaluated mid-loop,
    // so as soon as a hub was created after an overflow sink the two collided
    // and edges landed on the wrong node, giving a bus hub two different
    // parts. One list makes the index arithmetic impossible to get wrong.
    let mut extra: Vec<Node> = Vec::new();
    for ((network, item), from_set) in &producers {
        let to_set = match consumers.get(&(*network, item.clone())) {
            Some(set) => set,
            None => {
                for &producer in from_set {
                    report
                        .unconsumed_outputs
                        .insert((nodes[producer].name.clone(), display(table, item)));
                }
                if !options.overflow_sinks {
                    continue;
                }
                // Somewhere for the surplus to go, so the producers do not
                // deadlock and take their whole upstream chain to zero.
                let index = nodes.len() + extra.len();
                extra.push(if rates::is_fluid(item) {
                    // Fluids cannot go in an AWESOME Sink; a container set to
                    // Empty collects them instead, which is what Modeler's own
                    // docs suggest for surplus.
                    storage_node(Capacity::Empty, nodes[*from_set.iter().next().unwrap()].position, 0)
                } else {
                    generic_node(
                        names::virtual_nodes::AWESOME_SINK,
                        nodes[*from_set.iter().next().unwrap()].position,
                        0,
                    )
                });
                for &producer in from_set {
                    edges.insert((index, item.clone(), producer));
                }
                report.overflow_sinks += 1;
                continue;
            }
        };
        // A node feeding a network it also draws from is not its own supplier.
        let sources: Vec<usize> = from_set.iter().copied().collect();
        let sinks: Vec<usize> = to_set.iter().copied().filter(|t| !from_set.contains(t)).collect();
        if sinks.is_empty() {
            continue;
        }

        let direct = sources.len() * sinks.len();
        let through_hub = sources.len() + sinks.len();
        if options.bus_hubs && sources.len() >= 2 && sinks.len() >= 2 && direct > through_hub {
            // One shared bus stands in for the complete bipartite block. This
            // is both far smaller and closer to the truth: those producers
            // really do all dump onto one belt that those consumers draw from.
            let hub_index = nodes.len() + extra.len();
            let members: Vec<[f64; 3]> =
                sources.iter().chain(sinks.iter()).map(|i| nodes[*i].position).collect();
            let mut position = [0.0; 3];
            for point in &members {
                for axis in 0..3 {
                    position[axis] += point[axis] / members.len() as f64;
                }
            }
            extra.push(Node {
                name: names::virtual_nodes::STORAGE_CONTAINER.to_string(),
                max: None,
                clock_percent: 100.0,
                somersloops: 0,
                capacity: Some(Capacity::InputEqualsOutput),
                generic_inputs: true,
                position,
                inputs: sources.iter().map(|&from| Edge { item: item.clone(), from }).collect(),
                building_count: 0,
                output_port: Some(0),
            });
            for &sink in &sinks {
                edges.insert((sink, item.clone(), hub_index));
            }
            report.hub_nodes += 1;
            report.edges_saved_by_hubs += direct - through_hub;
        } else {
            for &sink in &sinks {
                for &source in &sources {
                    edges.insert((sink, item.clone(), source));
                }
            }
        }
    }
    nodes.extend(extra);

    for (to, item, from) in edges {
        nodes[to].inputs.push(Edge { item, from });
    }

    // -- Ingredients nothing on the input networks supplies. --------------
    for (index, flows) in consumes.iter().enumerate() {
        for (network, item) in flows {
            if !producers.contains_key(&(*network, item.clone())) {
                report
                    .unsupplied_inputs
                    .insert((nodes[index].name.clone(), display(table, item)));
            }
        }
    }

    ModelerGraph { nodes, report, solver: options.solver.clone() }
}

fn display(table: &names::NameTable, item: &str) -> String {
    table.item(item).unwrap_or(item).to_string()
}

fn identity_of(
    boundary: &Boundary,
    table: &names::NameTable,
    report: &mut Report,
) -> Option<Identity> {
    match boundary.kind {
        Kind::Machine => {
            let recipe = boundary.recipe.as_ref()?;
            if table.recipe_node(recipe).is_none() {
                report.unmapped_recipes.insert(recipe.clone());
                return None;
            }
            Some(Identity::Recipe(recipe.clone()))
        }
        Kind::Extractor => {
            let node_name = boundary.extractable_resource.as_deref().unwrap_or("");
            // A water extractor has no resource node; its class alone says
            // what it pulls.
            let item = rates::node_resource(node_name).unwrap_or_else(|| {
                if boundary.class.starts_with("Build_WaterPump") {
                    "Desc_Water_C".to_string()
                } else {
                    String::new()
                }
            });
            if item.is_empty() || table.resource_node(&item).is_none() {
                report
                    .dropped
                    .insert(boundary.class.clone(), "extractor with no resolvable resource".into());
                return None;
            }
            let purity = format!("{:?}", rates::node_purity(node_name)).to_uppercase();
            Some(Identity::Resource { item, class: boundary.class.clone(), purity })
        }
        Kind::Sink => Some(Identity::Sink),
        Kind::SpaceElevator => Some(Identity::SpaceElevator),
        Kind::DimensionalDepot => Some(Identity::Depot),
        Kind::VehicleStation => Some(Identity::Station(boundary.class.clone())),
        Kind::Generator => {
            // The node name depends on what is burning, not on the building:
            // one Build_GeneratorFuel_C is five different Modeler nodes.
            let fuel = boundary.fuel.clone().unwrap_or_default();
            match table.generator_node(&boundary.class, &fuel) {
                Some(node) => Some(Identity::Generator {
                    node: node.to_string(),
                    building: boundary.class.clone(),
                    fuel,
                }),
                None => {
                    report.dropped.insert(
                        boundary.class.clone(),
                        if fuel.is_empty() {
                            // Never fuelled, so there is no way to tell which
                            // of the five fuel-generator nodes it should be.
                            "generator with no fuel selected".into()
                        } else {
                            // Modeler has no biomass burner node.
                            format!("no Modeler node for this generator burning {fuel}")
                        },
                    );
                    None
                }
            }
        }
    }
}
