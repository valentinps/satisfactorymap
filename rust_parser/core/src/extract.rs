//! Bulk extractors: whole-save scans that sav_map_data.py used to run in
//! Python over converted property values, ported to run directly against the
//! Rust store. Each is an exact behavioral port of its Python reference
//! (named in the doc comment); tools/diff_payload.py compares the two
//! implementations and is the regression gate.

use crate::mapdata::queries::conveyor_chain_segment_window;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use std::collections::{HashMap, HashSet};

/// sav_map_data.py INVENTORY_PROPERTY_NAMES -- keep in sync.
pub const INVENTORY_PROPERTY_NAMES: [&[u8]; 9] = [
    b"mInventory",
    b"mInputInventory",
    b"mFuelInventory",
    b"mOutputInventory",
    b"mStorageInventory",
    b"mBufferInventory",
    b"mCouponInventory",
    b"mShopInventory",
    b"mInventoryPotential",
];

/// sav_map_data.py VEHICLE_INVENTORY_COMPONENT_SUFFIXES -- keep in sync.
pub const VEHICLE_INVENTORY_COMPONENT_SUFFIXES: [&[u8]; 2] =
    [b".StorageInventory", b".FuelInventory"];

pub fn find_prop<'a>(props: &'a PropList, data: &[u8], name: &[u8]) -> Option<&'a PropertyValue> {
    props
        .props
        .iter()
        .find(|p| !p.name.wide && p.name.bytes(data) == name)
        .map(|p| &p.value)
}

pub fn short_name(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Item/NumItems from one stack-shaped PropList; mirrors the Python guards
/// (`if not item or not numItems`, empty itemPath skipped).
pub fn stack_item<'a>(pl: &PropList, data: &'a [u8]) -> Option<(&'a [u8], i64)> {
    let item_path: &[u8] = match find_prop(pl, data, b"Item")? {
        PropertyValue::Struct(StructValue::InventoryItem { item_name, .. }) => item_name.bytes(data),
        PropertyValue::Str(s) => s.bytes(data),
        _ => return None,
    };
    let num_items = match find_prop(pl, data, b"NumItems") {
        Some(PropertyValue::Int(n)) => *n as i64,
        _ => return None,
    };
    if item_path.is_empty() || num_items == 0 {
        return None;
    }
    Some((item_path, num_items))
}

/// (slots, slotIndexByName) -- the shared instance-name index consumed by
/// the bulk extractors. Build once per save (SaveScan caches it) and pass
/// to every extractor call.
pub type InstanceSlots<'a> = (Vec<(&'a [u8], (usize, usize))>, HashMap<&'a [u8], usize>);

struct ItemIndex<'a> {
    /// shortName -> rows, insertion-ordered.
    order: Vec<(&'a [u8], Vec<(usize, i64)>)>, // (shortName, [(objectSlot, count)])
    by_name: HashMap<&'a [u8], usize>,
}

impl<'a> ItemIndex<'a> {
    fn push(&mut self, short: &'a [u8], slot: usize, count: i64) {
        match self.by_name.get(short) {
            Some(&idx) => self.order[idx].1.push((slot, count)),
            None => {
                self.by_name.insert(short, self.order.len());
                self.order.push((short, vec![(slot, count)]));
            }
        }
    }
}

/// `countByItem[short] += n` over one object's insertion-ordered assoc list
/// (a handful of distinct items per object -- a linear scan beats a map).
fn add_item_count<'a>(counts: &mut Vec<(&'a [u8], i64)>, short: &'a [u8], n: i64) {
    match counts.iter_mut().find(|(s, _)| *s == short) {
        Some((_, total)) => *total += n,
        None => counts.push((short, n)),
    }
}

/// Port of sav_map_data._collectItemLocationIndex (+ its helpers
/// _inventoryComponentObjects / the stack walk), extended with the items
/// riding the belt lines -- stock the Python version never counted, so an
/// item search under-reported a running factory by everything in transit.
/// Returns [(itemShortName, [(instanceName, count)])] in the same insertion
/// order the Python implementation's dict produces.
pub fn item_location_index(scan: &SaveScan) -> Vec<(Vec<u8>, Vec<(Vec<u8>, i64)>)> {
    let store = scan.store;
    let data: &[u8] = &store.data;
    let (slots, slot_by_name) = scan.instance_slots();

    let mut index = ItemIndex { order: Vec::new(), by_name: HashMap::new() };
    let mut suffix_key: Vec<u8> = Vec::new();

    for (slot, (instance_name, loc)) in slots.iter().enumerate() {
        // Owned re-parse; every value we keep (component paths, item short
        // names) borrows store.data or is copied, so the object drops at the
        // end of this iteration.
        let Some(object) = scan.parse_object(*loc) else { continue };

        // _inventoryComponentObjects: referenced components first (only
        // ObjectProperty references -- mirrors hasattr(ref, "pathName")),
        // then the vehicle name-convention ones; dedupe by pathName
        // keeping the first.
        let mut components: Vec<(&[u8], (usize, usize))> = Vec::new();
        for prop_name in INVENTORY_PROPERTY_NAMES {
            if let Some(PropertyValue::Object(r)) = find_prop(&object.properties, data, prop_name) {
                let path = r.path_name.bytes(data);
                if let Some(&idx) = slot_by_name.get(path) {
                    if !components.iter().any(|(p, _)| *p == path) {
                        components.push((path, slots[idx].1));
                    }
                }
            }
        }
        for suffix in VEHICLE_INVENTORY_COMPONENT_SUFFIXES {
            suffix_key.clear();
            suffix_key.extend_from_slice(instance_name);
            suffix_key.extend_from_slice(suffix);
            if let Some(&idx) = slot_by_name.get(suffix_key.as_slice()) {
                let path = slots[idx].0;
                if !components.iter().any(|(p, _)| *p == path) {
                    components.push((path, slots[idx].1));
                }
            }
        }

        // countByItem, insertion-ordered per object.
        let mut count_by_item: Vec<(&[u8], i64)> = Vec::new();
        for (_, comp_loc) in &components {
            let Some(comp) = scan.parse_object(*comp_loc) else { continue };
            let stacks = match find_prop(&comp.properties, data, b"mInventoryStacks") {
                Some(PropertyValue::Array(ArrayValue::Structs(v))) => v,
                _ => continue,
            };
            for stack in stacks {
                if let Some((item_path, n)) = stack_item(stack, data) {
                    add_item_count(&mut count_by_item, short_name(item_path), n);
                }
            }
        }

        // mPickupItems: inline struct on dropped-item actors.
        if let Some(PropertyValue::Struct(StructValue::Props(pl))) =
            find_prop(&object.properties, data, b"mPickupItems")
        {
            if let Some((item_path, n)) = stack_item(pl, data) {
                add_item_count(&mut count_by_item, short_name(item_path), n);
            }
        }

        // Items riding a conveyor are real stock too -- a late-game belt
        // network holds a lot of it -- so they count towards the item search
        // exactly like a machine's or a container's contents. Pre-1.0 saves
        // (and the chained-belt delete write-back, see editor::apply) keep a
        // belt's items on the belt itself, one entity per record.
        if let ActorSpecific::ConveyorBelt { items, .. } = &object.actor_specific {
            for item in items {
                let path = item.item_path.bytes(data);
                if !path.is_empty() {
                    add_item_count(&mut count_by_item, short_name(path), 1);
                }
            }
        }

        for (short, count) in count_by_item {
            index.push(short, slot, count);
        }

        // Since 1.0 a whole belt line's items instead live in one ring buffer
        // on the shared FGConveyorChainActor, each member belt owning a
        // contiguous window of it. Counted against the belt the item is
        // physically on -- not against the chain actor, which is an invisible
        // bookkeeping object with no footprint -- so every location the search
        // lists is a real placed building at a real position. Same split the
        // belt tooltip's "items on this segment" and the selection panel's
        // combined inventory already show.
        if let ActorSpecific::ConveyorChain {
            belts, items, maximum_items, chain_lead_item_index, ..
        } = &object.actor_specific
        {
            // The member belts' windows tile `items` exactly -- no slot
            // counted twice, none missed -- so a line's total across its
            // belts is exactly the length of the ring.
            let mut belt_counts: Vec<(&[u8], i64)> = Vec::new();
            for chain_belt in belts {
                let Some((start, count)) = conveyor_chain_segment_window(
                    chain_belt,
                    *maximum_items,
                    *chain_lead_item_index,
                ) else {
                    continue;
                };
                let Some(&belt_slot) = slot_by_name.get(chain_belt.belt.path_name.bytes(data))
                else {
                    continue;
                };
                belt_counts.clear();
                for item in items.iter().skip(start).take(count) {
                    let path = item.item_path.bytes(data);
                    if !path.is_empty() {
                        add_item_count(&mut belt_counts, short_name(path), 1);
                    }
                }
                for &(short, n) in &belt_counts {
                    index.push(short, belt_slot, n);
                }
            }
        }
    }

    // Owned rows so the borrow of store.data ends here.
    index
        .order
        .into_iter()
        .map(|(short, entries)| {
            (
                short.to_vec(),
                entries
                    .into_iter()
                    .map(|(slot, count)| (slots[slot].0.to_vec(), count))
                    .collect(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Spline polylines
// ---------------------------------------------------------------------------

/// Projection constants. Passed in from sav_map_data.py (as an 8-tuple) so
/// the two sides can't drift; the wasm/native payload builder constructs it
/// from the same embedded constants. Order: (worldToPixelScale, offsetX,
/// offsetY, oldDescale, cropLo, scaleToHighres, mapSize, pixelsPerWorldUnit).
pub struct Proj {
    pub scale: f64,
    pub off_x: f64,
    pub off_y: f64,
    pub descale: f64,
    pub crop_lo: f64,
    pub to_highres: f64,
    pub map_size: f64,
    pub ppwu: f64,
}

impl Proj {
    /// sav_map_data._adjPos(_adjPosBlankMap20(...)) -- identical op order.
    #[inline]
    pub fn adj_pos(&self, pos: f64, off: f64) -> f64 {
        ((pos / self.scale + off) / self.descale - self.crop_lo) * self.to_highres
    }
    /// sav_map_data.projectXY
    #[inline]
    pub fn project_xy(&self, x: f64, y: f64) -> (f64, f64) {
        (self.adj_pos(x, self.off_x), self.map_size - self.adj_pos(y, self.off_y))
    }
    /// sav_map_data.projectVectorXY
    #[inline]
    pub fn project_vector_xy(&self, x: f64, y: f64) -> (f64, f64) {
        (x * self.ppwu, -y * self.ppwu)
    }
}

/// sav_map_data.rotateVectorByQuaternion -- identical expression structure.
#[inline]
pub fn rotate_vector_by_quaternion(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let (qx, qy, qz, qw) = (q[0], q[1], q[2], q[3]);
    let (vx, vy, vz) = (v[0], v[1], v[2]);
    let tx = 2.0 * (qy * vz - qz * vy);
    let ty = 2.0 * (qz * vx - qx * vz);
    let tz = 2.0 * (qx * vy - qy * vx);
    [
        vx + qw * tx + (qy * tz - qz * ty),
        vy + qw * ty + (qz * tx - qx * tz),
        vz + qw * tz + (qx * ty - qy * tx),
    ]
}

pub fn vector_prop(pl: &PropList, data: &[u8], name: &[u8]) -> Option<[f64; 3]> {
    match find_prop(pl, data, name) {
        Some(PropertyValue::Struct(StructValue::Vector(v))) => Some(*v),
        _ => None,
    }
}

/// Exact port of sav_map_data._splineSegmentPolyline over one whole-save
/// scan: for every object whose actor header's typePath is in `type_paths`
/// (in global save order), the flat
/// [px, py, arriveX, arriveY, leaveX, leaveY, zMeters, ...] vertex list.
/// Objects with fewer than 2 points are skipped, mirroring the `>= 14`
/// length check. Returns [(instanceName, typePath, flatPoints)].
pub fn spline_polylines(
    scan: &SaveScan,
    type_paths: &[String],
    spline_property: &str,
    proj: &Proj,
) -> Vec<(String, String, Vec<f64>)> {
    let store = scan.store;
    let data: &[u8] = &store.data;
    let wanted: HashSet<&[u8]> = type_paths.iter().map(|s| s.as_bytes()).collect();
    let spline_prop = spline_property.as_bytes();
    let (slots, slot_by_name) = scan.instance_slots();
    let zero = [0.0f64; 3];

    let mut out: Vec<(String, String, Vec<f64>)> = Vec::new();
    for level in &store.levels {
        for header in &level.headers {
            let actor = match header {
                Header::Actor(a) => a,
                Header::Component(_) => continue,
            };
            if actor.type_path.wide || !wanted.contains(actor.type_path.bytes(data)) {
                continue;
            }
            let name = actor.instance_name.bytes(data);
            let Some(&idx) = slot_by_name.get(name) else { continue };
            let Some(object) = scan.parse_object(slots[idx].1) else { continue };

            // (location, arriveTangent, leaveTangent) triples, actor-local.
            let mut local_points: Vec<([f64; 3], [f64; 3], [f64; 3])> = Vec::new();
            match find_prop(&object.properties, data, spline_prop) {
                Some(PropertyValue::Array(ArrayValue::Structs(points))) => {
                    for point in points {
                        if let Some(location) = vector_prop(point, data, b"Location") {
                            let arrive = vector_prop(point, data, b"ArriveTangent").unwrap_or(zero);
                            let leave = vector_prop(point, data, b"LeaveTangent").unwrap_or(zero);
                            local_points.push((location, arrive, leave));
                        }
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(PropertyValue::Struct(StructValue::Props(pl))) =
                        find_prop(&object.properties, data, b"mTopTransform")
                    {
                        if let Some(translation) = vector_prop(pl, data, b"Translation") {
                            local_points.push((zero, zero, zero));
                            local_points.push((translation, zero, zero));
                        }
                    }
                }
            }

            if local_points.len() < 2 {
                continue;
            }
            let rotation = [
                actor.rotation[0] as f64,
                actor.rotation[1] as f64,
                actor.rotation[2] as f64,
                actor.rotation[3] as f64,
            ];
            let position = [
                actor.position[0] as f64,
                actor.position[1] as f64,
                actor.position[2] as f64,
            ];
            let mut flat: Vec<f64> = Vec::with_capacity(local_points.len() * 7);
            for (location, arrive_tangent, leave_tangent) in local_points {
                let world_offset = rotate_vector_by_quaternion(rotation, location);
                let (px, py_) =
                    proj.project_xy(position[0] + world_offset[0], position[1] + world_offset[1]);
                let arrive = rotate_vector_by_quaternion(rotation, arrive_tangent);
                let (arrive_x, arrive_y) = proj.project_vector_xy(arrive[0], arrive[1]);
                let leave = rotate_vector_by_quaternion(rotation, leave_tangent);
                let (leave_x, leave_y) = proj.project_vector_xy(leave[0], leave[1]);
                let z = (position[2] + world_offset[2]) / 100.0; // worldZToMeters
                flat.extend_from_slice(&[px, py_, arrive_x, arrive_y, leave_x, leave_y, z]);
            }
            out.push((
                String::from_utf8_lossy(name).into_owned(),
                String::from_utf8_lossy(actor.type_path.bytes(data)).into_owned(),
                flat,
            ));
        }
    }
    out
}
