//! Phase 0 of the Modeler export: measure whether the design survives a real
//! save before any of it is built. Answers four questions:
//!
//!   1. How much of the belt/pipe graph does `mConnectedComponent` actually
//!      cover? (`mDirection` is never serialized, so this is the only edge
//!      source -- if it is sparse, the whole "contract logistics into
//!      transport networks" plan needs a geometric fallback.)
//!   2. After contracting logistics runs, how many transport networks are
//!      there and how big are they?
//!   3. Do machine ports actually land on a network, or are they orphaned?
//!   4. How many Modeler nodes would the (recipe, clock, sloops, in-nets,
//!      out-nets) aggregation key produce? Modeler's Full solver bogs down on
//!      large graphs, so this number is the feature's viability test.
//!
//!     cargo run --release --example modeler_probe -- save.sav

use sav_core::extract::find_prop;
use sav_core::level::parse_full_save_lean;
use sav_core::mapdata::props;
use sav_core::object::ClassTables;
use sav_core::store::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// What a buildable is, for the purpose of the collapse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// Contracted away into a transport network: belts, lifts, splitters,
    /// mergers, poles, pipes, junctions, pumps, valves, buffers.
    Logistics,
    /// Keeps its identity and attaches to networks through named ports.
    Boundary,
    /// Not part of the item graph at all.
    Other,
}

fn role_of(class: &str) -> Role {
    const LOGISTICS: [&str; 14] = [
        "Build_ConveyorBelt",
        "Build_ConveyorLift",
        "Build_ConveyorAttachment",
        "Build_ConveyorPole",
        "Build_ConveyorCeilingAttachment",
        "Build_Pipeline",
        "Build_PipeStorageTank",
        "Build_IndustrialTank",
        "Build_Valve",
        "Build_PipelineJunction",
        "Build_PipelinePump",
        "Build_StorageContainer",
        "Build_StorageIntegrated",
        "Build_StoragePlayer",
    ];
    const BOUNDARY: [&str; 22] = [
        "Build_ConstructorMk1",
        "Build_AssemblerMk1",
        "Build_ManufacturerMk1",
        "Build_SmelterMk1",
        "Build_FoundryMk1",
        "Build_OilRefinery",
        "Build_Packager",
        "Build_Blender",
        "Build_HadronCollider",
        "Build_Converter",
        "Build_QuantumEncoder",
        "Build_MinerMk",
        "Build_OilPump",
        "Build_WaterPump",
        "Build_FrackingExtractor",
        "Build_Generator",
        "Build_ResourceSink",
        "Build_SpaceElevator",
        "Build_TradingPost",
        "Build_TrainDockingStation",
        "Build_TruckStation",
        "Build_DroneStation",
    ];
    if LOGISTICS.iter().any(|p| class.starts_with(p)) {
        return Role::Logistics;
    }
    if BOUNDARY.iter().any(|p| class.starts_with(p)) {
        return Role::Boundary;
    }
    // Conveyor wall holes and similar pass-throughs carry SnapOnly factory
    // connections and must not break a run in half.
    if class.contains("Conveyor") && class.starts_with("Build_Wall") {
        return Role::Logistics;
    }
    Role::Other
}

/// Owner instance of a component object named `<owner>.<Suffix>`.
fn owner_of(component_path: &[u8]) -> &[u8] {
    match component_path.iter().rposition(|&b| b == b'.') {
        Some(dot) => &component_path[..dot],
        None => component_path,
    }
}

struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        DisjointSet { parent: (0..n).collect() }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: modeler_probe <save.sav>");
    let bytes = std::fs::read(&path).expect("read save");
    let started = std::time::Instant::now();
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);
    let data: &[u8] = &store.data;
    println!("parsed {path} in {:?}", started.elapsed());

    // -----------------------------------------------------------------
    // Pass 1: actor census. instance -> (dense id, short class, role).
    // -----------------------------------------------------------------
    let mut actor_id: HashMap<&[u8], usize> = HashMap::new();
    let mut actor_class: Vec<String> = Vec::new();
    let mut actor_role: Vec<Role> = Vec::new();
    let mut actor_slot: Vec<(usize, usize)> = Vec::new();
    let mut role_counts: BTreeMap<&str, usize> = BTreeMap::new();

    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Actor(a) = header else { continue };
            let class = props::lossy(props::short_name(a.type_path.bytes(data)));
            let role = role_of(&class);
            if role != Role::Other {
                let key = match role {
                    Role::Logistics => "logistics",
                    Role::Boundary => "boundary",
                    Role::Other => unreachable!(),
                };
                *role_counts.entry(key).or_insert(0) += 1;
            }
            actor_id.insert(a.instance_name.bytes(data), actor_class.len());
            actor_class.push(class);
            actor_role.push(role);
            actor_slot.push((li, oi));
        }
    }
    println!(
        "actors: {} total, {} logistics, {} boundary",
        actor_class.len(),
        role_counts.get("logistics").copied().unwrap_or(0),
        role_counts.get("boundary").copied().unwrap_or(0),
    );

    // -----------------------------------------------------------------
    // Pass 2: every saved factory/pipe connection component -> does it
    // carry a peer? This is the coverage question.
    // -----------------------------------------------------------------
    let mut conn_total = 0usize;
    let mut conn_with_peer = 0usize;
    let mut peer_unresolvable = 0usize;
    // (ownerA, portSuffix, ownerB) for every resolved link.
    let mut links: Vec<(usize, String, usize)> = Vec::new();
    // Per-owner-class: how many of its ports carry a peer.
    let mut class_peer: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Component(c) = header else { continue };
            let class_name = c.class_name.bytes(data);
            let is_factory = class_name.ends_with(b"FGFactoryConnectionComponent");
            let is_pipe = class_name.ends_with(b"FGPipeConnectionFactory")
                || class_name.ends_with(b"FGPipeConnectionComponent");
            if !is_factory && !is_pipe {
                continue;
            }
            conn_total += 1;

            let instance = c.instance_name.bytes(data);
            let owner_path = owner_of(instance);
            let owner_class = actor_id
                .get(owner_path)
                .map(|&i| actor_class[i].clone())
                .unwrap_or_else(|| "<unknown owner>".into());
            let entry = class_peer.entry(owner_class).or_insert((0, 0));
            entry.0 += 1;

            let Ok(object) = store.parse_object_at(li, oi) else { continue };
            let Some(peer) = props::object_ref(&object.properties, data, b"mConnectedComponent")
            else {
                continue;
            };
            let peer_path = peer.path_name.bytes(data);
            if peer_path.is_empty() {
                continue;
            }
            conn_with_peer += 1;
            entry.1 += 1;

            let (Some(&a), Some(&b)) =
                (actor_id.get(owner_path), actor_id.get(owner_of(peer_path)))
            else {
                peer_unresolvable += 1;
                continue;
            };
            let port = props::lossy(&instance[owner_path.len().min(instance.len())..])
                .trim_start_matches('.')
                .to_string();
            links.push((a, port, b));
        }
    }
    println!(
        "\nconnection components: {conn_total} saved, {conn_with_peer} carry mConnectedComponent \
         ({:.1}%), {peer_unresolvable} peers unresolvable",
        100.0 * conn_with_peer as f64 / conn_total.max(1) as f64,
    );
    println!("peer coverage by owner class (ports with peer / ports saved):");
    let mut rows: Vec<_> = class_peer.into_iter().collect();
    rows.sort_by_key(|(_, (total, _))| std::cmp::Reverse(*total));
    for (class, (total, with_peer)) in rows.into_iter().take(20) {
        println!("  {with_peer:7}/{total:<7} {:5.1}%  {class}", 100.0 * with_peer as f64 / total as f64);
    }

    // -----------------------------------------------------------------
    // Pass 3: contract logistics runs into transport networks.
    // -----------------------------------------------------------------
    let mut sets = DisjointSet::new(actor_class.len());
    let mut boundary_links: Vec<(usize, String, usize)> = Vec::new();
    for (a, port, b) in &links {
        match (actor_role[*a], actor_role[*b]) {
            (Role::Logistics, Role::Logistics) => sets.union(*a, *b),
            (Role::Boundary, Role::Logistics) => {
                boundary_links.push((*a, port.clone(), *b));
            }
            // The boundary side of this link shows up as the mirrored entry.
            _ => {}
        }
    }

    let mut net_size: HashMap<usize, usize> = HashMap::new();
    for id in 0..actor_class.len() {
        if actor_role[id] == Role::Logistics {
            *net_size.entry(sets.find(id)).or_insert(0) += 1;
        }
    }
    let mut sizes: Vec<usize> = net_size.values().copied().collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let singletons = sizes.iter().filter(|&&n| n == 1).count();
    println!(
        "\ntransport networks: {} ({} singletons -- a high count here means the \
         mConnectedComponent graph is fragmenting)",
        sizes.len(),
        singletons,
    );
    println!("  largest: {:?}", &sizes[..sizes.len().min(10)]);
    if !sizes.is_empty() {
        println!("  median: {}", sizes[sizes.len() / 2]);
    }

    // -----------------------------------------------------------------
    // Pass 4: boundary ports -- attached vs orphaned -- and the node count
    // the aggregation key would produce.
    // -----------------------------------------------------------------
    let mut ports_by_boundary: HashMap<usize, Vec<(String, usize)>> = HashMap::new();
    for (b, port, l) in &boundary_links {
        ports_by_boundary.entry(*b).or_default().push((port.clone(), sets.find(*l)));
    }

    let gd = sav_core::gamedata::get();
    // Recipe_X_C -> (ingredient short names, product short names).
    let recipe_io = |recipe: &str| -> (Vec<String>, Vec<String>) {
        let names = |v: Option<&serde_json::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.get("item").and_then(|i| i.as_str()))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        match gd.recipes.get(recipe) {
            Some(r) => (names(r.get("ingredients")), names(r.get("product"))),
            None => (Vec::new(), Vec::new()),
        }
    };

    // Component instance name -> slot, so a machine can reach its own
    // inventory component objects.
    let mut component_slot: HashMap<&[u8], (usize, usize)> = HashMap::new();
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            if let Header::Component(c) = header {
                component_slot.insert(c.instance_name.bytes(data), (li, oi));
            }
        }
    }
    let mut sloop_shape: BTreeMap<&str, usize> = BTreeMap::new();
    let mut sloop_slots: BTreeMap<(String, String), usize> = BTreeMap::new();

    let mut machines = 0usize;
    let mut machines_with_recipe = 0usize;
    let mut machines_attached = 0usize;
    let mut node_keys: HashSet<String> = HashSet::new();
    let mut recipes_seen: HashSet<String> = HashSet::new();
    let mut unknown_recipes: HashSet<String> = HashSet::new();
    // network -> item -> (producer node keys, consumer node keys)
    let mut flows: HashMap<(usize, String), (HashSet<String>, HashSet<String>)> = HashMap::new();

    for id in 0..actor_class.len() {
        if actor_role[id] != Role::Boundary {
            continue;
        }
        machines += 1;
        let (li, oi) = actor_slot[id];
        let Ok(object) = store.parse_object_at(li, oi) else { continue };
        let props_list = &object.properties;

        let recipe = match find_prop(props_list, data, b"mCurrentRecipe") {
            Some(PropertyValue::Object(r)) if !r.path_name.bytes(data).is_empty() => {
                props::lossy(props::short_name(r.path_name.bytes(data)))
            }
            _ => String::new(),
        };
        if recipe.is_empty() {
            continue;
        }
        machines_with_recipe += 1;
        recipes_seen.insert(recipe.clone());

        let clock = props::float(props_list, data, b"mCurrentPotential").unwrap_or(1.0);
        let sloops = props::float(props_list, data, b"mCurrentProductionBoost").unwrap_or(1.0);
        if sloops != 1.0 {
            // Modeler wants a somersloop COUNT; the save stores a boost
            // MULTIPLIER. Is the count recoverable from the machine's
            // potential inventory (where shards and sloops physically sit)?
            // Three outcomes have to stay distinguishable: no inventory
            // component at all, a component with no sloop stack, and a real
            // count. Conflating the first two is what made the last run
            // ambiguous.
            let mut sloop_items: Option<i64> = None;
            let mut had_component = false;
            if let Some(r) = props::object_ref(props_list, data, b"mInventoryPotential") {
                if let Some(&(cli, coi)) = component_slot.get(r.path_name.bytes(data)) {
                    if let Ok(component) = store.parse_object_at(cli, coi) {
                        had_component = true;
                        if let Some(stacks) =
                            props::array_structs(&component.properties, data, b"mInventoryStacks")
                        {
                            let mut total = 0i64;
                            for stack in stacks {
                                if let Some((item, n)) = sav_core::extract::stack_item(stack, data)
                                {
                                    if props::short_name(item) == b"Desc_WAT1_C" {
                                        total += n;
                                    }
                                }
                            }
                            sloop_items = Some(total);
                        }
                    }
                }
            }
            let bucket = match (had_component, sloop_items) {
                (false, _) => "no mInventoryPotential component",
                (true, None) => "component without mInventoryStacks",
                (true, Some(0)) => "component present, zero somersloops",
                (true, Some(_)) => "somersloop count present",
            };
            *sloop_shape.entry(bucket).or_insert(0) += 1;
            if let Some(count) = sloop_items.filter(|&n| n > 0) {
                // boost = 1 + sloops / maxSlots  =>  maxSlots = sloops / (boost - 1)
                let implied_slots = count as f64 / (sloops - 1.0);
                *sloop_slots
                    .entry((actor_class[id].clone(), format!("{implied_slots:.3}")))
                    .or_insert(0) += 1;
            }
        }

        let mut in_nets: Vec<usize> = Vec::new();
        let mut out_nets: Vec<usize> = Vec::new();
        for (port, net) in ports_by_boundary.get(&id).into_iter().flatten() {
            if port.starts_with("Input") || port.starts_with("PipeInput") {
                in_nets.push(*net);
            } else {
                out_nets.push(*net);
            }
        }
        if !in_nets.is_empty() || !out_nets.is_empty() {
            machines_attached += 1;
        }
        in_nets.sort_unstable();
        in_nets.dedup();
        out_nets.sort_unstable();
        out_nets.dedup();
        let key = format!("{recipe}|{:.4}|{:.4}|{in_nets:?}|{out_nets:?}", clock, sloops);
        node_keys.insert(key.clone());

        let (ingredients, products) = recipe_io(&recipe);
        if ingredients.is_empty() && products.is_empty() {
            unknown_recipes.insert(recipe.clone());
        }
        for net in &out_nets {
            for item in &products {
                flows.entry((*net, item.clone())).or_default().0.insert(key.clone());
            }
        }
        for net in &in_nets {
            for item in &ingredients {
                flows.entry((*net, item.clone())).or_default().1.insert(key.clone());
            }
        }
    }

    println!(
        "\nboundary buildings: {machines}, with a recipe: {machines_with_recipe}, \
         attached to >=1 network: {machines_attached} ({:.1}%)",
        100.0 * machines_attached as f64 / machines_with_recipe.max(1) as f64,
    );
    println!("distinct recipes in use: {}", recipes_seen.len());
    if !unknown_recipes.is_empty() {
        println!("recipes missing from game_data: {unknown_recipes:?}");
    }

    // Edges: per (network, item), every producer node feeds every consumer
    // node. Modeler's solver is more sensitive to edge count than node count.
    let mut edges = 0usize;
    let mut dangling = 0usize;
    let mut worst: Vec<(usize, String, usize, usize)> = Vec::new();
    let mut items_per_net: HashMap<usize, usize> = HashMap::new();
    // With a per-(network, item) hub node -- a Storage Container in
    // "Input = Output" mode -- a P x C bipartite block collapses to P + C.
    let mut hub_edges = 0usize;
    let mut hub_nodes = 0usize;
    for ((net, item), (producers, consumers)) in &flows {
        if producers.is_empty() || consumers.is_empty() {
            dangling += 1;
            continue;
        }
        *items_per_net.entry(*net).or_insert(0) += 1;
        let (p, c) = (producers.len(), consumers.len());
        let n = p * c;
        edges += n;
        if p >= 2 && c >= 2 && n > p + c {
            hub_edges += p + c;
            hub_nodes += 1;
        } else {
            hub_edges += n;
        }
        worst.push((n, item.clone(), p, c));
    }
    worst.sort_by_key(|(n, ..)| std::cmp::Reverse(*n));
    let mut diversity: Vec<usize> = items_per_net.values().copied().collect();
    diversity.sort_unstable_by(|a, b| b.cmp(a));

    println!(
        "\n==> Modeler nodes after aggregation: {} (from {machines_with_recipe} machines)",
        node_keys.len(),
    );
    println!("==> edges: {edges} ({dangling} item flows with no producer or no consumer)");
    println!(
        "==> with bus hub nodes: {} nodes / {hub_edges} edges  ({:.0}% fewer edges)",
        node_keys.len() + hub_nodes,
        100.0 * (1.0 - hub_edges as f64 / edges.max(1) as f64),
    );
    println!("items carried per network (sushi diversity): {:?}", &diversity[..diversity.len().min(12)]);
    println!("densest producer x consumer blocks:");
    for (n, item, p, c) in worst.into_iter().take(8) {
        println!("  {n:5} edges  {p:3} producers x {c:3} consumers  {item}");
    }
    if !sloop_shape.is_empty() {
        println!("\nsomersloop-boosted machines -- where does the count actually live?");
        for (bucket, n) in &sloop_shape {
            println!("  {n:5}  {bucket}");
        }
        println!("implied max somersloop slots per building -- count / (boost - 1):");
        for ((class, slots), n) in &sloop_slots {
            println!("  {n:5}  {class:<28} -> {slots}");
        }
    }
    // -----------------------------------------------------------------
    // Cross-check: the real `modeler::graph` must agree with the
    // independent implementation above. Two implementations disagreeing is
    // how a subtle contraction bug gets caught.
    // -----------------------------------------------------------------
    let graph_started = std::time::Instant::now();
    let graph = sav_core::modeler::graph::build(&store);
    let attached = graph.boundaries.iter().filter(|b| !b.ports.is_empty()).count();
    let with_recipe = graph.boundaries.iter().filter(|b| b.recipe.is_some()).count();
    let sloop_conflicts =
        graph.boundaries.iter().filter(|b| b.boost_disagrees_with_slots).count();
    println!("\nmodeler::graph cross-check (built in {:?}):", graph_started.elapsed());
    println!("  {:?}", graph.stats);
    println!(
        "  boundaries {} / with recipe {with_recipe} / attached {attached} / \
         somersloop-boost conflicts {sloop_conflicts}",
        graph.boundaries.len(),
    );
    // Only the classification-independent quantities have to match exactly.
    // The probe and graph.rs keep separate role tables on purpose, so their
    // logistics/boundary counts legitimately differ by the classes one knows
    // and the other does not.
    assert_eq!(
        graph.stats.connections_with_peer, conn_with_peer,
        "the two passes disagree on how many connections carry a peer",
    );
    assert_eq!(with_recipe, machines_with_recipe, "disagreement on recipe machines");
    println!(
        "  exact agreement on peers ({conn_with_peer}) and recipe machines ({with_recipe}); \
         networks {} vs {} (differs only by the role tables)",
        graph.stats.networks,
        sizes.len(),
    );

    // Unattached boundaries are expected (a freight platform has no factory
    // connection of its own), but a machine class showing up here would mean
    // the contraction is dropping real links.
    let mut unattached_by_class: BTreeMap<&str, usize> = BTreeMap::new();
    for boundary in graph.boundaries.iter().filter(|b| b.ports.is_empty()) {
        *unattached_by_class.entry(boundary.class.as_str()).or_insert(0) += 1;
    }
    let mut rows: Vec<_> = unattached_by_class.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  unattached boundaries by class:");
    for (class, n) in rows.into_iter().take(12) {
        println!("    {n:6}  {class}");
    }

    println!("total probe time: {:?}", started.elapsed());
}
