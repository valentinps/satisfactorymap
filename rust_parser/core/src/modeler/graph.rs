//! Step 1 of the collapse: turn a save's buildables into *boundary machines*
//! attached to *transport networks*.
//!
//! A save wires machines together through belts, lifts, splitters, mergers,
//! poles, pipes, junctions, pumps and valves. None of those exist in Modeler,
//! and enumerating them would produce a graph with hundreds of thousands of
//! edges. So every such run is contracted, by union-find, into one opaque
//! **transport network**; a machine then simply "draws from" or "feeds" a
//! network. A splitter tree with 200 belts becomes one network id, which is
//! what makes the whole export tractable.
//!
//! Edges come from `mConnectedComponent` on the saved connection components.
//! Measured coverage on a 367 k-actor save: 99 %+ on every belt, lift and
//! pipe class, and 8 171 of 8 173 recipe machines land on a network. The
//! classes that fall short of 100 % are splitters and machines with genuinely
//! *unused* ports, so no geometric fallback is needed.
//!
//! `mDirection` is never serialized, but that does not matter: direction is
//! only needed where a machine meets a network, and there the component's own
//! name (`Input0`, `Output2`, `PipeInputFactory`) carries it.

use crate::extract::{find_prop, stack_item};
use crate::mapdata::props;
use crate::store::{Header, PropertyValue, SaveStore};
use std::collections::HashMap;

/// Dense index into [`FactoryGraph::boundaries`].
pub type BoundaryId = usize;
/// Dense transport-network id.
pub type NetworkId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// The machine draws items from the network.
    In,
    /// The machine feeds items into the network.
    Out,
}

#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub direction: Direction,
    pub network: NetworkId,
}

/// What kind of boundary building this is. Drives which Modeler node the
/// aggregator emits for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Runs `mCurrentRecipe`.
    Machine,
    /// Miner, oil pump, water extractor, resource-well satellite.
    Extractor,
    /// Burns fuel for power.
    Generator,
    /// AWESOME Sink.
    Sink,
    /// Absorbs items and never runs: the Space Elevator, per the export's
    /// treatment of it as a big storage system rather than a machine.
    SpaceElevator,
    /// Dimensional Depot uploads.
    DimensionalDepot,
    /// Train platform, truck station or drone port. Vehicular transport is
    /// deliberately not traversed, so these terminate a network.
    VehicleStation,
}

#[derive(Clone, Debug)]
pub struct Boundary {
    pub instance_name: String,
    pub class: String,
    pub kind: Kind,
    pub position: [f32; 3],
    pub ports: Vec<Port>,
    /// `Recipe_X_C` short class, when the building runs one.
    pub recipe: Option<String>,
    /// Overclock as a fraction (2.5 == 250 %), from `mPendingPotential` if
    /// present, else `mCurrentPotential`, else 1.0.
    ///
    /// The order matters. `mPendingPotential` is the value the *player set*;
    /// `mCurrentPotential` is where the machine has ramped to, and is only
    /// written once it has actually run. A machine built and overclocked but
    /// never powered has the pending property alone -- reading only
    /// `mCurrentPotential` silently exported every such machine at 100 %.
    /// (Caught by `modeler_clocks.sav`, where all three 250 % constructors
    /// carry `mPendingPotential: 2.5` and no current potential whatsoever.)
    pub clock: f64,
    /// Somersloops physically installed, counted from `Desc_WAT1_C` stacks in
    /// `mInventoryPotential`. This is deliberately *not* derived from
    /// `mCurrentProductionBoost`: that is a multiplier, needs a per-building
    /// max-slot table to invert, and goes stale when a sloop is pulled out
    /// (48 boosted machines on the reference save, 16 of them with an empty
    /// sloop slot). The physical count is the ground truth Modeler wants.
    pub somersloops: u32,
    /// `mCurrentProductionBoost != 1` while no somersloop is installed, or
    /// vice versa -- surfaced in the report rather than silently resolved.
    pub boost_disagrees_with_slots: bool,
    /// Generators: `mCurrentFuelClass` short name.
    pub fuel: Option<String>,
    /// Extractors: the resource node instance this is mining, resolved
    /// against the purity tables by the caller.
    pub extractable_resource: Option<String>,
    /// Vehicle stations only: distinct item classes sitting in the station's
    /// cargo inventory. Since vehicular transport is not traversed, this is
    /// the only evidence of what the station actually moves, and it is what
    /// the stub node offers to the rest of the graph.
    pub stock: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GraphStats {
    pub actors: usize,
    pub logistics: usize,
    pub connection_components: usize,
    pub connections_with_peer: usize,
    /// Peers naming an instance that is not in the save. Always 0 on healthy
    /// saves; non-zero means a mod or a corrupt reference.
    pub dangling_peers: usize,
    pub networks: usize,
    /// Boundary buildings that resolved to no network at all.
    pub unattached_boundaries: usize,
    /// Ports on a boundary whose direction could be resolved neither from
    /// the component name nor from the building's kind. Should stay 0.
    pub undirected_ports: usize,
}

pub struct FactoryGraph {
    pub boundaries: Vec<Boundary>,
    /// Buildable count per network, indexed by [`NetworkId`].
    pub network_members: Vec<usize>,
    pub stats: GraphStats,
}

/// Role a buildable plays in the collapse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Logistics,
    Boundary(KindTag),
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KindTag {
    Machine,
    Extractor,
    Generator,
    Sink,
    SpaceElevator,
    DimensionalDepot,
    VehicleStation,
}

/// Buildables contracted into transport networks. Storage containers and
/// fluid buffers are here on purpose: a container spliced into a belt line is
/// a pass-through, and treating it as a node would cut the line in two.
const LOGISTICS_PREFIXES: [&str; 14] = [
    "Build_ConveyorBelt",
    "Build_ConveyorLift",
    "Build_ConveyorAttachment",
    "Build_ConveyorPole",
    "Build_ConveyorCeilingAttachment",
    "Build_Pipeline",
    "Build_PipeStorageTank",
    "Build_IndustrialTank",
    "Build_Valve",
    "Build_StorageContainer",
    "Build_StorageIntegrated",
    "Build_StoragePlayer",
    "Build_StorageBlueprint",
    "Build_StorageMedkit",
];

const MACHINE_PREFIXES: [&str; 12] = [
    "Build_ConstructorMk",
    "Build_AssemblerMk",
    "Build_ManufacturerMk",
    "Build_SmelterMk",
    "Build_FoundryMk",
    "Build_OilRefinery",
    "Build_Packager",
    "Build_Blender",
    "Build_HadronCollider",
    "Build_Converter",
    "Build_QuantumEncoder",
    "Build_AutomatedWorkBench",
];

const EXTRACTOR_PREFIXES: [&str; 5] = [
    "Build_MinerMk",
    "Build_OilPump",
    "Build_WaterPump",
    "Build_FrackingExtractor",
    "Build_FrackingSmasher",
];

const VEHICLE_STATION_PREFIXES: [&str; 5] = [
    "Build_TrainDockingStation",
    "Build_TrainPlatformCargo",
    "Build_TruckStation",
    "Build_FluidTruckStation",
    "Build_DroneStation",
];

fn role_of(class: &str) -> Role {
    if LOGISTICS_PREFIXES.iter().any(|p| class.starts_with(p)) {
        return Role::Logistics;
    }
    if MACHINE_PREFIXES.iter().any(|p| class.starts_with(p)) {
        return Role::Boundary(KindTag::Machine);
    }
    if EXTRACTOR_PREFIXES.iter().any(|p| class.starts_with(p)) {
        return Role::Boundary(KindTag::Extractor);
    }
    if VEHICLE_STATION_PREFIXES.iter().any(|p| class.starts_with(p)) {
        return Role::Boundary(KindTag::VehicleStation);
    }
    if class.starts_with("Build_Generator") {
        return Role::Boundary(KindTag::Generator);
    }
    if class.starts_with("Build_ResourceSink") && !class.starts_with("Build_ResourceSinkShop") {
        return Role::Boundary(KindTag::Sink);
    }
    if class.starts_with("Build_SpaceElevator") {
        return Role::Boundary(KindTag::SpaceElevator);
    }
    if class.starts_with("Build_DimensionalDepot") || class.starts_with("Build_CentralStorage") {
        return Role::Boundary(KindTag::DimensionalDepot);
    }
    // Conveyor wall holes: pass-throughs carrying SnapOnly connections. They
    // must not cut a belt run in half.
    if class.starts_with("Build_Wall") && class.contains("Conveyor") {
        return Role::Logistics;
    }
    Role::Other
}

fn kind_of(tag: KindTag) -> Kind {
    match tag {
        KindTag::Machine => Kind::Machine,
        KindTag::Extractor => Kind::Extractor,
        KindTag::Generator => Kind::Generator,
        KindTag::Sink => Kind::Sink,
        KindTag::SpaceElevator => Kind::SpaceElevator,
        KindTag::DimensionalDepot => Kind::DimensionalDepot,
        KindTag::VehicleStation => Kind::VehicleStation,
    }
}

/// Owner instance of a component object named `<owner>.<Suffix>`.
fn owner_of(component_path: &[u8]) -> &[u8] {
    match component_path.iter().rposition(|&b| b == b'.') {
        Some(dot) => &component_path[..dot],
        None => component_path,
    }
}

/// `Input0` / `PipeInputFactory` / `ConveyorInput0_0` -> In;
/// `Output2` / `PipeOutputFactory` -> Out. Belt ends (`ConveyorAny0`) and
/// attachment ports (`Connection1`) are ambiguous, but those owners are
/// logistics, so their direction is never consulted.
fn direction_of(port_suffix: &str) -> Option<Direction> {
    if port_suffix.contains("Input") {
        Some(Direction::In)
    } else if port_suffix.contains("Output") {
        Some(Direction::Out)
    } else {
        None
    }
}

/// Some single-port fluid buildings name their connection after the
/// component class instead of its direction -- a water pump's only port is
/// `.FGPipeConnectionFactory`. Left unresolved that silently orphaned 4 870
/// water pumps, 311 fuel generators and every oil pump on the reference
/// save, so fall back to what the building can only possibly do.
fn direction_from_kind(tag: KindTag) -> Option<Direction> {
    match tag {
        // Extractors only ever push resource out.
        KindTag::Extractor => Some(Direction::Out),
        // These only ever consume.
        KindTag::Generator
        | KindTag::Sink
        | KindTag::SpaceElevator
        | KindTag::DimensionalDepot => Some(Direction::In),
        // Machines and stations do both, and always name their ports
        // explicitly, so guessing here would be wrong rather than helpful.
        KindTag::Machine | KindTag::VehicleStation => None,
    }
}

struct DisjointSet {
    parent: Vec<u32>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        DisjointSet { parent: (0..n as u32).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] as usize != root {
            root = self.parent[root] as usize;
        }
        // Path compression, iteratively -- these chains can be long on a
        // 200 k-belt save and recursion would blow the stack.
        let mut cursor = x;
        while self.parent[cursor] as usize != root {
            let next = self.parent[cursor] as usize;
            self.parent[cursor] = root as u32;
            cursor = next;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb as u32;
        }
    }
}

/// Contract the save's logistics runs and return the boundary machines
/// attached to the resulting networks.
pub fn build(store: &SaveStore) -> FactoryGraph {
    let data: &[u8] = &store.data;
    let mut stats = GraphStats::default();

    // -- Pass 1: actors, their role, and a name -> dense id map. ----------
    let mut actor_id: HashMap<&[u8], u32> = HashMap::new();
    let mut roles: Vec<Role> = Vec::new();
    let mut slots: Vec<(u32, u32)> = Vec::new();
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Actor(actor) = header else { continue };
            let class = props::short_name(actor.type_path.bytes(data));
            let role = role_of(&String::from_utf8_lossy(class));
            if role == Role::Logistics {
                stats.logistics += 1;
            }
            actor_id.insert(actor.instance_name.bytes(data), roles.len() as u32);
            roles.push(role);
            slots.push((li as u32, oi as u32));
        }
    }
    stats.actors = roles.len();

    // -- Pass 2: connection components -> links. -------------------------
    let mut sets = DisjointSet::new(roles.len());
    // Boundary side only: (boundary actor id, port suffix, logistics actor id).
    let mut boundary_links: Vec<(u32, String, u32)> = Vec::new();
    let mut component_slot: HashMap<&[u8], (u32, u32)> = HashMap::new();
    let mut station_components: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();

    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Component(component) = header else { continue };
            let instance = component.instance_name.bytes(data);
            component_slot.insert(instance, (li as u32, oi as u32));

            // A vehicle station's cargo inventory is a child component that
            // no saved property on the station points at, so it can only be
            // reached parent-first. Collected for stations alone -- keeping
            // this map for all 500 k components would cost far more memory
            // than the handful of stations is worth.
            let parent = component.parent_actor_name.bytes(data);
            if let Some(&parent_id) = actor_id.get(parent) {
                if roles[parent_id as usize] == Role::Boundary(KindTag::VehicleStation) {
                    station_components
                        .entry(parent_id)
                        .or_insert_with(Vec::new)
                        .push((li as u32, oi as u32));
                }
            }

            let class_name = component.class_name.bytes(data);
            let is_connection = class_name.ends_with(b"FGFactoryConnectionComponent")
                || class_name.ends_with(b"FGPipeConnectionFactory")
                || class_name.ends_with(b"FGPipeConnectionComponent");
            if !is_connection {
                continue;
            }
            stats.connection_components += 1;

            let Ok(object) = store.parse_object_at(li, oi) else { continue };
            let Some(peer) = props::object_ref(&object.properties, data, b"mConnectedComponent")
            else {
                continue;
            };
            let peer_path = peer.path_name.bytes(data);
            if peer_path.is_empty() {
                continue;
            }
            stats.connections_with_peer += 1;

            let owner_path = owner_of(instance);
            let (Some(&owner), Some(&other)) =
                (actor_id.get(owner_path), actor_id.get(owner_of(peer_path)))
            else {
                stats.dangling_peers += 1;
                continue;
            };
            match (roles[owner as usize], roles[other as usize]) {
                (Role::Logistics, Role::Logistics) => sets.union(owner as usize, other as usize),
                (Role::Boundary(_), Role::Logistics) => {
                    let suffix = String::from_utf8_lossy(&instance[owner_path.len()..])
                        .trim_start_matches('.')
                        .to_string();
                    boundary_links.push((owner, suffix, other));
                }
                // A machine wired straight into another machine has no
                // network between them; Modeler cannot express that either,
                // so it is reported as an unattached port rather than faked.
                _ => {}
            }
        }
    }

    // -- Pass 3: dense network ids. --------------------------------------
    let mut dense: HashMap<usize, NetworkId> = HashMap::new();
    let mut network_members: Vec<usize> = Vec::new();
    for id in 0..roles.len() {
        if roles[id] != Role::Logistics {
            continue;
        }
        let root = sets.find(id);
        let next = network_members.len();
        let network = *dense.entry(root).or_insert(next);
        if network == next {
            network_members.push(0);
        }
        network_members[network] += 1;
    }
    stats.networks = network_members.len();

    // -- Pass 4: boundary facts and ports. -------------------------------
    let mut ports_by_actor: HashMap<u32, Vec<Port>> = HashMap::new();
    let mut undirected_ports = 0usize;
    for (owner, suffix, other) in &boundary_links {
        let Role::Boundary(tag) = roles[*owner as usize] else { continue };
        let Some(direction) = direction_of(suffix).or_else(|| direction_from_kind(tag)) else {
            undirected_ports += 1;
            continue;
        };
        let root = sets.find(*other as usize);
        let Some(&network) = dense.get(&root) else { continue };
        ports_by_actor.entry(*owner).or_default().push(Port { direction, network });
    }
    stats.undirected_ports = undirected_ports;

    let mut boundaries = Vec::new();
    for id in 0..roles.len() {
        let Role::Boundary(tag) = roles[id] else { continue };
        let (li, oi) = slots[id];
        let (li, oi) = (li as usize, oi as usize);
        let Header::Actor(actor) = &store.levels[li].headers[oi] else { continue };

        let mut ports = ports_by_actor.remove(&(id as u32)).unwrap_or_default();
        ports.sort_by_key(|p| (p.network, p.direction == Direction::Out));
        ports.dedup_by_key(|p| (p.network, p.direction));
        if ports.is_empty() {
            stats.unattached_boundaries += 1;
        }

        let mut boundary = Boundary {
            instance_name: actor.instance_name.to_string(data),
            class: props::lossy(props::short_name(actor.type_path.bytes(data))),
            kind: kind_of(tag),
            position: actor.position,
            ports,
            recipe: None,
            clock: 1.0,
            somersloops: 0,
            boost_disagrees_with_slots: false,
            fuel: None,
            extractable_resource: None,
            stock: Vec::new(),
        };

        if boundary.kind == Kind::VehicleStation {
            boundary.stock = station_stock(store, data, station_components.get(&(id as u32)));
        }

        if let Ok(object) = store.parse_object_at(li, oi) {
            let p = &object.properties;
            if let Some(PropertyValue::Object(r)) = find_prop(p, data, b"mCurrentRecipe") {
                let path = r.path_name.bytes(data);
                if !path.is_empty() {
                    boundary.recipe = Some(props::lossy(props::short_name(path)));
                }
            }
            boundary.clock = props::float(p, data, b"mPendingPotential")
                .or_else(|| props::float(p, data, b"mCurrentPotential"))
                .unwrap_or(1.0);
            if let Some(PropertyValue::Object(r)) = find_prop(p, data, b"mCurrentFuelClass") {
                let path = r.path_name.bytes(data);
                if !path.is_empty() {
                    boundary.fuel = Some(props::lossy(props::short_name(path)));
                }
            }
            if let Some(r) = props::object_ref(p, data, b"mExtractableResource") {
                let path = r.path_name.bytes(data);
                if !path.is_empty() {
                    boundary.extractable_resource = Some(props::lossy(path));
                }
            }
            boundary.somersloops =
                count_somersloops(store, data, &component_slot, p).unwrap_or(0);
            // Same pending-before-current rule as the clock. Only one
            // direction is worth reporting: a boost recorded with no
            // somersloop physically present means a stale multiplier, which
            // would overstate output. The reverse (a sloop installed on a
            // machine that has not recomputed its boost yet) is the normal
            // state of a freshly built machine, and the slot count is right.
            let boost = props::float(p, data, b"mPendingProductionBoost")
                .or_else(|| props::float(p, data, b"mCurrentProductionBoost"))
                .unwrap_or(1.0);
            boundary.boost_disagrees_with_slots = boost != 1.0 && boundary.somersloops == 0;
        }

        boundaries.push(boundary);
    }

    FactoryGraph { boundaries, network_members, stats }
}

/// Distinct item classes in a vehicle station's cargo inventory.
///
/// `FuelInventory` (the truck's own fuel) and `InventoryPotential` (power
/// shards and somersloops) are excluded -- neither is cargo, and counting
/// them would have the station offering coal or somersloops to the factory.
fn station_stock(
    store: &SaveStore,
    data: &[u8],
    components: Option<&Vec<(u32, u32)>>,
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for &(li, oi) in components.into_iter().flatten() {
        let Header::Component(header) = &store.levels[li as usize].headers[oi as usize] else {
            continue;
        };
        let name = header.instance_name.bytes(data);
        let suffix = props::short_name(name);
        if suffix == b"FuelInventory" || suffix == b"InventoryPotential" {
            continue;
        }
        let Ok(object) = store.parse_object_at(li as usize, oi as usize) else { continue };
        let Some(stacks) = props::array_structs(&object.properties, data, b"mInventoryStacks")
        else {
            continue;
        };
        for stack in stacks {
            if let Some((item_path, count)) = stack_item(stack, data) {
                if count > 0 && !item_path.is_empty() {
                    let item = props::lossy(props::short_name(item_path));
                    if !items.contains(&item) {
                        items.push(item);
                    }
                }
            }
        }
    }
    items.sort();
    items
}

/// Somersloops installed in a machine, from its `mInventoryPotential`
/// component (power shards live there too, so the item class matters).
fn count_somersloops(
    store: &SaveStore,
    data: &[u8],
    component_slot: &HashMap<&[u8], (u32, u32)>,
    properties: &crate::store::PropList,
) -> Option<u32> {
    let reference = props::object_ref(properties, data, b"mInventoryPotential")?;
    let &(li, oi) = component_slot.get(reference.path_name.bytes(data))?;
    let component = store.parse_object_at(li as usize, oi as usize).ok()?;
    let stacks = props::array_structs(&component.properties, data, b"mInventoryStacks")?;
    let mut total = 0i64;
    for stack in stacks {
        if let Some((item_path, count)) = stack_item(stack, data) {
            if props::short_name(item_path) == b"Desc_WAT1_C" {
                total += count;
            }
        }
    }
    Some(total.max(0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_names_carry_their_direction() {
        assert_eq!(direction_of("Input0"), Some(Direction::In));
        assert_eq!(direction_of("Input3"), Some(Direction::In));
        assert_eq!(direction_of("Output1"), Some(Direction::Out));
        assert_eq!(direction_of("PipeInputFactory"), Some(Direction::In));
        assert_eq!(direction_of("PipeOutputFactory"), Some(Direction::Out));
        // The Portal's oddly-named conveyor port still reads as an input.
        assert_eq!(direction_of("ConveyorInput0_0"), Some(Direction::In));
        // Belt ends and attachment ports are ambiguous by design.
        assert_eq!(direction_of("ConveyorAny0"), None);
        assert_eq!(direction_of("Connection1"), None);
        assert_eq!(direction_of("SnapOnly0"), None);
        // A water pump's only port is named after its component class.
        assert_eq!(direction_of("FGPipeConnectionFactory"), None);
    }

    #[test]
    fn single_port_fluid_buildings_fall_back_to_their_kind() {
        // Water/oil pumps name their only port `.FGPipeConnectionFactory`;
        // without this fallback every one of them is orphaned.
        assert_eq!(direction_from_kind(KindTag::Extractor), Some(Direction::Out));
        assert_eq!(direction_from_kind(KindTag::Generator), Some(Direction::In));
        assert_eq!(direction_from_kind(KindTag::Sink), Some(Direction::In));
        assert_eq!(direction_from_kind(KindTag::SpaceElevator), Some(Direction::In));
        // Guessing for two-way buildings would be wrong, not helpful.
        assert_eq!(direction_from_kind(KindTag::Machine), None);
        assert_eq!(direction_from_kind(KindTag::VehicleStation), None);
    }

    #[test]
    fn logistics_and_boundary_classification() {
        assert_eq!(role_of("Build_ConveyorBeltMk3_C"), Role::Logistics);
        assert_eq!(role_of("Build_ConveyorAttachmentSplitterSmart_C"), Role::Logistics);
        assert_eq!(role_of("Build_PipelineJunction_Cross_C"), Role::Logistics);
        // A container spliced into a belt line is a pass-through, not a node.
        assert_eq!(role_of("Build_StorageContainerMk2_C"), Role::Logistics);
        // Wall conveyor holes must not cut a run in half.
        assert_eq!(role_of("Build_Wall_Conveyor_8x4_01_C"), Role::Logistics);

        assert_eq!(role_of("Build_ManufacturerMk1_C"), Role::Boundary(KindTag::Machine));
        assert_eq!(role_of("Build_MinerMk3_C"), Role::Boundary(KindTag::Extractor));
        assert_eq!(role_of("Build_GeneratorNuclear_C"), Role::Boundary(KindTag::Generator));
        assert_eq!(role_of("Build_SpaceElevator_C"), Role::Boundary(KindTag::SpaceElevator));
        assert_eq!(role_of("Build_TruckStation_C"), Role::Boundary(KindTag::VehicleStation));
        // The sink SHOP is a coupon kiosk, not an item sink.
        assert_eq!(role_of("Build_ResourceSink_C"), Role::Boundary(KindTag::Sink));
        assert_eq!(role_of("Build_ResourceSinkShop_C"), Role::Other);

        assert_eq!(role_of("Build_Foundation_8x1_01_C"), Role::Other);
    }

    #[test]
    fn disjoint_set_compresses_long_chains() {
        // A 100 k belt run is a realistic chain length; this must not
        // recurse or go quadratic.
        let mut sets = DisjointSet::new(100_000);
        for i in 1..100_000 {
            sets.union(i - 1, i);
        }
        let root = sets.find(0);
        assert!((0..100_000).all(|i| sets.find(i) == root));
    }
}
