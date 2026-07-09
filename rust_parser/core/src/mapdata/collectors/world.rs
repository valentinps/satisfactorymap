//! The static-world collectors: resourceNodes, collectables, hardDrives,
//! droppedItems (sav_map_data.py lines ~1211-1496, 1942-2044). All four
//! match save actors/collectables against the embedded static world tables
//! (gamedata::get()); insertion order of every bucket dict is load-bearing.

use crate::extract::find_prop;
use crate::gamedata;
use crate::mapdata::geometry::{project_xy, world_z_to_meters};
use crate::mapdata::jsonval::jnum;
use crate::mapdata::names::readable_label;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn f3(v: [f32; 3]) -> [f64; 3] {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

/// Table lookups keyed by String against save-side byte strings. Wide
/// (UTF-16) or invalid-UTF-8 names decode to "" and simply miss, exactly as
/// they could never equal an ASCII table key in Python either.
fn utf8(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or("")
}

// ---------------------------------------------------------------------------
// collectResourceNodes (sav_map_data.py 1211-1339)
// ---------------------------------------------------------------------------

/// See FRACKING_CORE_TYPE_PATH's comment in sav_map_data.py: the well's own
/// core actor is never a real extraction point, so it's always excluded.
const FRACKING_CORE_TYPE_PATH: &str =
    "/Game/FactoryGame/Resource/BP_FrackingCore.BP_FrackingCore_C";
const FRACKING_SATELLITE_TYPE_PATH: &str =
    "/Game/FactoryGame/Resource/BP_FrackingSatellite.BP_FrackingSatellite_C";

/// _PURITY_OVERRIDE_NAME_TO_ENUM applied to a raw mPurityOverride value.
/// Python duck-types `isinstance(purityOverride, list) and len(...) == 2`
/// then looks up element [1]: Byte and Enum properties both convert to
/// 2-lists; a Byte's numeric payload (an int) never matches the str keys.
/// The engine misspells impure as "RP_Inpure"; both spellings map to IMPURE.
fn purity_override_name(value: Option<&PropertyValue>, data: &[u8]) -> Option<&'static str> {
    let raw: &[u8] = match value? {
        PropertyValue::Byte { value: ByteVal::Str(s), .. } => s.bytes(data),
        PropertyValue::Enum { value, .. } => value.bytes(data),
        _ => return None,
    };
    match raw {
        b"RP_Inpure" | b"RP_Impure" => Some("IMPURE"),
        b"RP_Normal" => Some("NORMAL"),
        b"RP_Pure" => Some("PURE"),
        _ => None,
    }
}

struct PurityBucket {
    points: Vec<Value>,
    ids: Vec<Value>,
    world_positions: Vec<Value>,
}

impl PurityBucket {
    fn new() -> Self {
        PurityBucket { points: Vec::new(), ids: Vec::new(), world_positions: Vec::new() }
    }
}

/// {purityName: {"points": ..., "ids": ..., "worldPositions": ...}} in
/// insertion order.
fn purity_map(buckets: IndexMap<String, PurityBucket>) -> Value {
    let mut map = Map::new();
    for (purity_name, b) in buckets {
        map.insert(
            purity_name,
            json!({"points": b.points, "ids": b.ids, "worldPositions": b.world_positions}),
        );
    }
    Value::Object(map)
}

pub fn collect_resource_nodes(scan: &SaveScan) -> Value {
    let data = scan.data();
    let gd = gamedata::get();

    let miner_type_paths: Vec<&str> = gd.type_paths.miners.iter().map(String::as_str).collect();
    let mut miner_objects: Vec<&Object> = Vec::new();
    for (_, obj_slot) in scan.actors_of_type(&miner_type_paths) {
        if let Some(slot) = obj_slot {
            miner_objects.push(scan.object(slot));
        }
    }

    let node_type_paths: Vec<&str> = gd
        .type_paths
        .mined_resources
        .iter()
        .map(String::as_str)
        .filter(|p| *p != FRACKING_CORE_TYPE_PATH)
        .collect();
    // instanceName -> (position, typePath); Python dict semantics on a
    // duplicate name (last value wins, first position kept) == IndexMap.
    let mut mined_resource_actors: IndexMap<&[u8], ([f32; 3], &[u8])> = IndexMap::new();
    let mut node_objects_by_instance_name: IndexMap<&[u8], &Object> = IndexMap::new();
    for (actor_slot, obj_slot) in scan.actors_of_type(&node_type_paths) {
        let actor = scan.actor(actor_slot);
        let name = actor.instance_name.bytes(data);
        mined_resource_actors.insert(name, (actor.position, actor.type_path.bytes(data)));
        if let Some(slot) = obj_slot {
            node_objects_by_instance_name.insert(name, scan.object(slot));
        }
    }

    let mut mined_resource_instance_names: HashSet<&[u8]> = HashSet::new();
    for object in &miner_objects {
        if let Some(r) = props::object_ref(&object.properties, data, b"mExtractableResource") {
            mined_resource_instance_names.insert(r.path_name.bytes(data));
        }
    }

    // Per-node game-mode overrides (see the long Python comment): when
    // present they're authoritative, the static table is only a fallback.
    // instanceName -> (overrideResourceType, overridePurity).
    let mut overrides_by_instance_name: HashMap<&[u8], (Option<String>, Option<&'static str>)> =
        HashMap::new();
    for (&name, object) in &node_objects_by_instance_name {
        let mut override_resource_type: Option<String> = None;
        if let Some(r) = props::object_ref(&object.properties, data, b"mResourceClassOverride") {
            // getattr(..., "pathName", None) truthiness: empty pathName skipped.
            let path = r.path_name.bytes(data);
            if !path.is_empty() {
                override_resource_type = Some(props::lossy(props::short_name(path)));
            }
        }
        let override_purity =
            purity_override_name(find_prop(&object.properties, data, b"mPurityOverride"), data);
        if override_resource_type.is_some() || override_purity.is_some() {
            overrides_by_instance_name.insert(name, (override_resource_type, override_purity));
        }
    }

    struct NodeBucket {
        label: String,
        resource_type: String,
        is_well: bool,
        mined: IndexMap<String, PurityBucket>,
        unmined: IndexMap<String, PurityBucket>,
    }
    let mut resource_buckets: IndexMap<String, NodeBucket> = IndexMap::new();
    for (&instance_name, &(position, type_path)) in &mined_resource_actors {
        let static_entry = gd.resource_purity.get(utf8(instance_name));
        let (override_resource_type, override_purity): (Option<&str>, Option<&'static str>) =
            match overrides_by_instance_name.get(instance_name) {
                Some((rt, p)) => (rt.as_deref(), *p),
                None => (None, None),
            };
        // Python `or`: an empty-string override falls back to the static
        // entry too; no override and no static entry -> not a real node.
        let resource_type: &str = match override_resource_type {
            Some(rt) if !rt.is_empty() => rt,
            _ => match static_entry {
                Some(entry) => entry.0.as_str(),
                None => continue,
            },
        };
        // Purity: the override wins outright; the static table's enum NAME
        // otherwise; _purityName(None) == "UNKNOWN".
        let purity_name: &str = match override_purity {
            Some(p) => p,
            None => match static_entry {
                Some(entry) => entry.1.as_str(),
                None => "UNKNOWN",
            },
        };
        // Well/non-well is read straight from the actor's own typePath (a
        // fixed physical-world fact, never touched by either game mode).
        let is_well = type_path == FRACKING_SATELLITE_TYPE_PATH.as_bytes();
        let bucket_key =
            format!("{}{}", resource_type, if is_well { ":well" } else { "" });
        let bucket = match resource_buckets.get_mut(&bucket_key) {
            Some(b) => b,
            None => {
                let label = format!(
                    "{}{}",
                    readable_label(resource_type),
                    if is_well { " (Resource Well)" } else { "" }
                );
                resource_buckets.insert(
                    bucket_key.clone(),
                    NodeBucket {
                        label,
                        resource_type: resource_type.to_string(),
                        is_well,
                        mined: IndexMap::new(),
                        unmined: IndexMap::new(),
                    },
                );
                resource_buckets.get_mut(&bucket_key).unwrap()
            }
        };
        let mined_flag = mined_resource_instance_names.contains(instance_name);
        let state_buckets = if mined_flag { &mut bucket.mined } else { &mut bucket.unmined };
        let purity_bucket =
            state_buckets.entry(purity_name.to_string()).or_insert_with(PurityBucket::new);
        let pos = f3(position);
        let [px, py] = project_xy(pos[0], pos[1]);
        purity_bucket.points.push(jnum(px));
        purity_bucket.points.push(jnum(py));
        purity_bucket.points.push(jnum(world_z_to_meters(pos[2])));
        purity_bucket.ids.push(Value::String(props::lossy(instance_name)));
        // Raw world-space X/Y for the tooltip (see the Python comment).
        purity_bucket.world_positions.push(jnum(pos[0]));
        purity_bucket.world_positions.push(jnum(pos[1]));
    }

    let mut by_resource_type: Vec<Value> = Vec::new();
    for (_, bucket) in resource_buckets {
        by_resource_type.push(json!({
            "resourceType": bucket.resource_type,
            "label": bucket.label,
            "isWell": bucket.is_well,
            "mined": {"byPurity": purity_map(bucket.mined)},
            "unmined": {"byPurity": purity_map(bucket.unmined)},
        }));
    }
    json!({"byResourceType": by_resource_type})
}

// ---------------------------------------------------------------------------
// collectCollectables (sav_map_data.py 1341-1396)
// ---------------------------------------------------------------------------

/// _splitCollectableKind: `entries` is the static table flattened to
/// (instanceName, position) in table order. Both collectables lists are
/// checked (their UNION -- see the Python comment on why neither list alone
/// is complete).
fn split_collectable_kind(scan: &SaveScan, entries: &[(&str, [f64; 3])]) -> Value {
    let data = scan.data();
    let static_keys: HashSet<&[u8]> = entries.iter().map(|(k, _)| k.as_bytes()).collect();
    let mut collected_instance_names: HashSet<&[u8]> = HashSet::new();
    for level in &scan.store.levels {
        for list in [level.collectables1.as_deref(), Some(level.collectables2.as_slice())] {
            let Some(list) = list else { continue };
            for collectable in list {
                let path = collectable.path_name.bytes(data);
                if static_keys.contains(path) {
                    collected_instance_names.insert(path);
                }
            }
        }
    }

    let mut remaining = PurityBucket::new(); // same points/ids/worldPositions triple
    let mut collected = PurityBucket::new();
    for &(instance_name, position) in entries {
        let [px, py] = project_xy(position[0], position[1]);
        let bucket = if collected_instance_names.contains(instance_name.as_bytes()) {
            &mut collected
        } else {
            &mut remaining
        };
        bucket.points.push(jnum(px));
        bucket.points.push(jnum(py));
        bucket.points.push(jnum(world_z_to_meters(position[2])));
        bucket.ids.push(Value::String(instance_name.to_string()));
        // Raw world-space X/Y -- available unconditionally from the static
        // reference data (a collected pickup's actor is removed from the save).
        bucket.world_positions.push(jnum(position[0]));
        bucket.world_positions.push(jnum(position[1]));
    }

    json!({
        "remaining": remaining.points,
        "remainingIds": remaining.ids,
        "remainingWorldPositions": remaining.world_positions,
        "collected": collected.points,
        "collectedIds": collected.ids,
        "collectedWorldPositions": collected.world_positions,
    })
}

pub fn collect_collectables(scan: &SaveScan) -> Value {
    let gd = gamedata::get();
    // _positionFromSlugEntry: the entry IS the position.
    fn slug_entries(m: &'static gamedata::SlugMap) -> Vec<(&'static str, [f64; 3])> {
        m.iter().map(|(k, v)| (k.as_str(), *v)).collect()
    }
    // _positionFromDetailedEntry: (id, rotationQuat, position, detailsDict)[2].
    fn detailed_entries(m: &'static gamedata::CollectibleMap) -> Vec<(&'static str, [f64; 3])> {
        m.iter().map(|(k, v)| (k.as_str(), v.2)).collect()
    }
    json!({
        "slugsBlue": split_collectable_kind(scan, &slug_entries(&gd.power_slugs.blue)),
        "slugsYellow": split_collectable_kind(scan, &slug_entries(&gd.power_slugs.yellow)),
        "slugsPurple": split_collectable_kind(scan, &slug_entries(&gd.power_slugs.purple)),
        "somersloops": split_collectable_kind(scan, &detailed_entries(&gd.somersloops)),
        "mercerSpheres": split_collectable_kind(scan, &detailed_entries(&gd.mercer_spheres)),
    })
}

// ---------------------------------------------------------------------------
// _getCrashSiteState + collectHardDrives (sav_map_data.py 1942-2044)
// ---------------------------------------------------------------------------

/// Python list.remove(item): drop the first occurrence. (A miss raises
/// ValueError there -- never reachable on a save Python itself can serve --
/// and is a no-op here.)
fn remove_first(list: &mut Vec<&[u8]>, item: &[u8]) {
    if let Some(i) = list.iter().position(|&x| x == item) {
        list.remove(i);
    }
}

const HARD_DRIVE_ITEM_PATH: &[u8] =
    b"/Game/FactoryGame/Resource/Environment/CrashSites/Desc_HardDrive.Desc_HardDrive_C";

/// _getCrashSiteState, minus crashSitesInSave (built by the Python original
/// but never consumed by any caller -- collectHardDrives discards it).
/// Returns (notOpened, openWithDrive, openAndEmpty, dismantled).
#[allow(clippy::type_complexity)]
fn crash_site_state<'a>(
    scan: &SaveScan<'a>,
) -> (Vec<&'a [u8]>, Vec<&'a [u8]>, Vec<&'a [u8]>, Vec<&'a [u8]>) {
    let data = scan.data();
    let crash_sites = &gamedata::get().crash_sites;
    let mut not_opened: Vec<&[u8]> = crash_sites.keys().map(|k| k.as_bytes()).collect();
    let mut dismantled: Vec<&[u8]> = Vec::new();
    for level in &scan.store.levels {
        // collectables1 has all dismantled sites; collectables2 can be a
        // subset of collectables1 (Python checks collectables1 only here).
        if let Some(collectables1) = &level.collectables1 {
            for collectable in collectables1 {
                let path = collectable.path_name.bytes(data);
                if crash_sites.contains_key(utf8(path)) {
                    dismantled.push(path);
                    remove_first(&mut not_opened, path);
                }
            }
        }
    }

    let mut open_and_empty: Vec<&[u8]> = Vec::new();
    let mut open_with_drive: Vec<&[u8]> = Vec::new();
    // Inventory instance path name -> crash site instance path name.
    let mut crash_site_inventory_path_name: HashMap<Vec<u8>, &[u8]> = HashMap::new();
    for level in &scan.store.levels {
        for (header, object) in level.headers.iter().zip(&level.objects) {
            let instance_name = header.instance_name().bytes(data);
            if !crash_sites.contains_key(utf8(instance_name)) {
                continue;
            }
            // `hasBeenOpened is not None and hasBeenOpened`.
            if props::boolean(&object.properties, data, b"mHasBeenOpened") != Some(true) {
                continue;
            }
            remove_first(&mut not_opened, instance_name);
            let has_been_looted = match props::boolean(&object.properties, data, b"mHasBeenLooted")
            {
                Some(v) => v,
                None => {
                    // v1.0 doesn't use "mInventory" anymore: check the
                    // "<site>.Inventory2" object in the pass below; until
                    // proven otherwise the droppod counts as looted.
                    let mut inventory_name = instance_name.to_vec();
                    inventory_name.extend_from_slice(b".Inventory2");
                    crash_site_inventory_path_name.insert(inventory_name, instance_name);
                    true
                }
            };
            if has_been_looted {
                open_and_empty.push(instance_name);
            } else {
                // This case has not been observed.
                open_with_drive.push(instance_name);
            }
        }
    }

    for level in &scan.store.levels {
        for (header, object) in level.headers.iter().zip(&level.objects) {
            let instance_name = header.instance_name().bytes(data);
            let Some(&site) = crash_site_inventory_path_name.get(instance_name) else { continue };
            let Some(stacks) = props::array_structs(&object.properties, data, b"mInventoryStacks")
            else {
                continue;
            };
            // Python indexes inventoryStacks[0][0] unconditionally (an empty
            // array would IndexError; not observed on real saves).
            let Some(first) = stacks.first() else { continue };
            // `len(item) == 2 and isinstance(item[0], str)` is always true of
            // an InventoryItem's [itemName, props] conversion, and item[1] is
            // 1, 2 or a props list -- never 0 -- so only the path matters.
            if let Some(PropertyValue::Struct(StructValue::InventoryItem { item_name, .. })) =
                find_prop(first, data, b"Item")
            {
                if item_name.bytes(data) == HARD_DRIVE_ITEM_PATH {
                    remove_first(&mut open_and_empty, site);
                    open_with_drive.push(site);
                }
            }
        }
    }

    (not_opened, open_with_drive, open_and_empty, dismantled)
}

pub fn collect_hard_drives(scan: &SaveScan) -> Value {
    let (not_opened, open_with_drive, open_and_empty, dismantled) = crash_site_state(scan);
    let crash_sites = &gamedata::get().crash_sites;

    // bucketFor: points/ids/worldPositions/requirements, all parallel (same
    // skip, same order).
    let bucket_for = |instance_names: &[&[u8]]| -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
        let mut points: Vec<Value> = Vec::new();
        let mut ids: Vec<Value> = Vec::new();
        let mut world_positions: Vec<Value> = Vec::new();
        let mut requirements: Vec<Value> = Vec::new();
        for &instance_name in instance_names {
            let Some((_, _, position, info)) = crash_sites.get(utf8(instance_name)) else {
                continue;
            };
            let [px, py] = project_xy(position[0], position[1]);
            points.push(jnum(px));
            points.push(jnum(py));
            points.push(jnum(world_z_to_meters(position[2])));
            ids.push(Value::String(props::lossy(instance_name)));
            // Raw world-space X/Y -- see _splitCollectableKind's comment.
            world_positions.push(jnum(position[0]));
            world_positions.push(jnum(position[1]));
            // Either an item stack ("cost": ["Item Label", qty]) or a power
            // hookup ("power", in MW), never both; None for sites with
            // neither. `if cost:` -- Python truthiness on the list.
            let requirement = match info.get("cost") {
                Some(Value::Array(cost)) if !cost.is_empty() => {
                    json!({"type": "cost", "item": cost[0], "quantity": cost[1]})
                }
                _ => match info.get("power") {
                    Some(power) if !power.is_null() => json!({"type": "power", "watts": power}),
                    _ => Value::Null,
                },
            };
            requirements.push(requirement);
        }
        (points, ids, world_positions, requirements)
    };

    // notOpened + openWithDrive merge into one "hasDrive" bucket (see the
    // Python comment).
    let has_drive_names: Vec<&[u8]> =
        not_opened.iter().chain(open_with_drive.iter()).copied().collect();
    let (has_drive_points, has_drive_ids, has_drive_world, has_drive_reqs) =
        bucket_for(&has_drive_names);
    let (empty_points, empty_ids, empty_world, empty_reqs) = bucket_for(&open_and_empty);
    let (dismantled_points, dismantled_ids, dismantled_world, dismantled_reqs) =
        bucket_for(&dismantled);
    json!({
        "hasDrive": has_drive_points, "hasDriveIds": has_drive_ids,
        "hasDriveWorldPositions": has_drive_world, "hasDriveRequirements": has_drive_reqs,
        "empty": empty_points, "emptyIds": empty_ids,
        "emptyWorldPositions": empty_world, "emptyRequirements": empty_reqs,
        "dismantled": dismantled_points, "dismantledIds": dismantled_ids,
        "dismantledWorldPositions": dismantled_world, "dismantledRequirements": dismantled_reqs,
    })
}

// ---------------------------------------------------------------------------
// _uncollectedCatalogDrops + collectDroppedItems (sav_map_data.py 1398-1496)
// ---------------------------------------------------------------------------

/// Items dropped loose on the ground -- each is its own actor of this one
/// engine class.
use crate::mapdata::consts::ITEM_PICKUP_TYPE_PATH;

/// _itemIconFilename: the ClassName-keyed icon file under
/// map/static/map/icons/items/ (see game_data/copy_icons.py), or null when no
/// icon was extracted. Python checked os.path.exists per call; here the
/// icons dir is snapshotted at compile time (core/build.rs) so the check
/// also works on wasm.
fn item_icon_filename(item_short_name: &str) -> Value {
    if gamedata::has_item_icon(item_short_name) {
        Value::String(format!("{}.png", item_short_name))
    } else {
        Value::Null
    }
}

/// Python's `itemFullPath.rsplit(".", 1)[-1]` over a str.
fn short_name_str(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// _uncollectedCatalogDrops: every FREE_DROPPED_ITEMS catalog stack that
/// isn't live in the save as an actor (visited areas) and isn't recorded as
/// collected in either collectables list. Yields (itemShortName, quantity,
/// position, instanceName) in catalog order.
pub(crate) fn uncollected_catalog_drops(
    scan: &SaveScan,
) -> Vec<(&'static str, i64, [f64; 3], &'static str)> {
    let data = scan.data();
    let free_dropped_items = &gamedata::get().free_dropped_items;
    let present_actors: HashSet<&[u8]> = scan
        .actor_slots_of_type(&[ITEM_PICKUP_TYPE_PATH])
        .into_iter()
        .map(|slot| scan.actor(slot).instance_name.bytes(data))
        .collect();
    let mut catalog_instance_names: HashSet<&'static [u8]> = HashSet::new();
    for entries in free_dropped_items.values() {
        for (_, _, instance_name) in entries {
            catalog_instance_names.insert(instance_name.as_bytes());
        }
    }
    let mut collected_instance_names: HashSet<&[u8]> = HashSet::new();
    for level in &scan.store.levels {
        // Both lists' union, same reasoning as _splitCollectableKind.
        for list in [level.collectables1.as_deref(), Some(level.collectables2.as_slice())] {
            let Some(list) = list else { continue };
            for collectable in list {
                let path = collectable.path_name.bytes(data);
                if catalog_instance_names.contains(path) {
                    collected_instance_names.insert(path);
                }
            }
        }
    }

    let mut drops: Vec<(&'static str, i64, [f64; 3], &'static str)> = Vec::new();
    for (item_full_path, entries) in free_dropped_items {
        let short_name = short_name_str(item_full_path);
        for (quantity, position, instance_name) in entries {
            if !present_actors.contains(instance_name.as_bytes())
                && !collected_instance_names.contains(instance_name.as_bytes())
            {
                drops.push((short_name, *quantity, *position, instance_name.as_str()));
            }
        }
    }
    drops
}

struct DropBucket {
    item_path: String,
    label: String,
    icon: Value,
    points: Vec<Value>,
    ids: Vec<Value>,
    counts: Vec<Value>,
    world_positions: Vec<Value>,
}

fn append_drop(
    buckets: &mut IndexMap<String, DropBucket>,
    short_name: &str,
    position: [f64; 3],
    instance_name: &[u8],
    num_items: Value,
) {
    let bucket = match buckets.get_mut(short_name) {
        Some(b) => b,
        None => {
            buckets.insert(
                short_name.to_string(),
                DropBucket {
                    item_path: short_name.to_string(),
                    label: readable_label(short_name),
                    icon: item_icon_filename(short_name),
                    points: Vec::new(),
                    ids: Vec::new(),
                    counts: Vec::new(),
                    world_positions: Vec::new(),
                },
            );
            buckets.get_mut(short_name).unwrap()
        }
    };
    let [px, py] = project_xy(position[0], position[1]);
    bucket.points.push(jnum(px));
    bucket.points.push(jnum(py));
    bucket.points.push(jnum(world_z_to_meters(position[2])));
    bucket.ids.push(Value::String(props::lossy(instance_name)));
    bucket.counts.push(num_items);
    // Raw world-space X/Y, same split as collectResourceNodes.
    bucket.world_positions.push(jnum(position[0]));
    bucket.world_positions.push(jnum(position[1]));
}

pub fn collect_dropped_items(scan: &SaveScan) -> Value {
    let data = scan.data();
    let mut buckets: IndexMap<String, DropBucket> = IndexMap::new();

    for (actor_slot, obj_slot) in scan.actors_of_type(&[ITEM_PICKUP_TYPE_PATH]) {
        let Some(obj_slot) = obj_slot else { continue };
        let object = scan.object(obj_slot);
        // mPickupItems is a StructProperty: Python's pickupItems[0] is the
        // [innerProps, innerPropTypes] pair's props list. (Were it any other
        // shape, Python's getPropertyValue over it would return None for both
        // "Item" and "NumItems" and the actor would be skipped -- same as
        // this None arm.)
        let Some(first) = props::struct_props(&object.properties, data, b"mPickupItems") else {
            continue;
        };
        // `item[0] if isinstance(item, (list, tuple)) else item`.
        let item_path = props::item_path(first, data, b"Item");
        let num_items = match find_prop(first, data, b"NumItems") {
            Some(PropertyValue::Int(n)) => Some(*n as i64),
            Some(PropertyValue::Int64(n)) => Some(*n),
            _ => None,
        };
        // `if not itemPath or not numItems`: NumItems 0 = an already-picked-up
        // leftover actor.
        let (Some(item_path), Some(num_items)) = (item_path, num_items) else { continue };
        if item_path.is_empty() || num_items == 0 {
            continue;
        }
        let actor = scan.actor(actor_slot);
        append_drop(
            &mut buckets,
            &props::lossy(props::short_name(item_path)),
            f3(actor.position),
            actor.instance_name.bytes(data),
            Value::from(num_items),
        );
    }

    for (short_name, quantity, position, instance_name) in uncollected_catalog_drops(scan) {
        append_drop(&mut buckets, short_name, position, instance_name.as_bytes(), Value::from(quantity));
    }

    // sorted(key=(-len(ids), label)) -- stable; UTF-8 byte order matches
    // Python's code-point string comparison.
    let mut rows: Vec<DropBucket> = buckets.into_values().collect();
    rows.sort_by(|a, b| b.ids.len().cmp(&a.ids.len()).then_with(|| a.label.cmp(&b.label)));
    Value::Array(
        rows.into_iter()
            .map(|b| {
                json!({
                    "itemPath": b.item_path,
                    "label": b.label,
                    "icon": b.icon,
                    "points": b.points,
                    "ids": b.ids,
                    "counts": b.counts,
                    "worldPositions": b.world_positions,
                })
            })
            .collect(),
    )
}
