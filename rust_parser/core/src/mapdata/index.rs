//! MapIndex -- port of sav_map_data._buildSaveIndex (lines 2143-2276) plus
//! _collectStaticItemLocations (2059-2114). One O(n) pass over the parsed
//! save producing an OWNED index (String/Vec<u8> keys, (level, slot) indices
//! into the SaveStore) so the query functions (queries.rs) and, later,
//! describeInstance can answer per-click/per-search requests without
//! rescanning the save. Python dict insertion-order semantics (last value
//! wins, first position kept) are preserved everywhere via IndexMap.

use super::collectors::{buildings, trains_progression};
use super::consts::*;
use super::geometry::{project_xy, world_z_to_meters};
use super::jsonval::jnum;
use super::names::readable_label;
use super::props;
use super::scan::{SaveScan, Slot};
use crate::extract::find_prop;
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Value};

/// sav_map_data.PIPE_CONNECTOR_SUFFIXES: every naming convention seen so far
/// for the FGPipeConnectionComponent sub-objects that carry "mPipeNetworkID".
/// Tried in order until one resolves.
pub const PIPE_CONNECTOR_SUFFIXES: [&str; 7] = [
    ".PipelineConnection0",
    ".PipelineConnection1",
    ".Connection0",
    ".Connection1",
    ".Connection2",
    ".Connection3",
    ".FGPipeConnectionFactory",
];

/// sav_map_data.COLLECTABLE_ITEM_SHORT_NAMES, in Python dict order, zipped
/// with collectableLabels from _collectStaticItemLocations.
const COLLECTABLE_ITEMS: [(&str, &str, &str); 5] = [
    ("slugsBlue", "Desc_Crystal_C", "Blue Power Slug"),
    ("slugsYellow", "Desc_Crystal_mk2_C", "Yellow Power Slug"),
    ("slugsPurple", "Desc_Crystal_mk3_C", "Purple Power Slug"),
    ("somersloops", "Desc_WAT1_C", "Somersloop"),
    ("mercerSpheres", "Desc_WAT2_C", "Mercer Sphere"),
];

const HARD_DRIVE_ITEM_SHORT_NAME: &str = "Desc_HardDrive_C";

const TRAIN_STATION_IDENTIFIER_TYPE_PATH: &str = "/Script/FactoryGame.FGTrainStationIdentifier";
const PIPE_NETWORK_TYPE_PATH: &str = "/Script/FactoryGame.FGPipeNetwork";

/// One railcar of an owned train consist (the "cars" entries of
/// _trainConsistsFromMaps' dicts).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TrainCar {
    pub id: String,
    pub type_path: String,
    pub position: [f64; 3],
    pub rotation: [f64; 4],
}

/// One consist of _trainConsistsFromMaps' output, owned.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TrainInfo {
    pub id: String,
    pub label: Option<String>,
    pub cars: Vec<TrainCar>,
}

/// One type path's instances, by reference: actor entries as Slots into the
/// store (their names decode from headers on demand), lightweights as a bare
/// count (their synthetic "LightweightBuildable:<tp>:<idx>" ids are fully
/// derivable). The old shape materialized every instance name as an owned
/// String -- tens of MB duplicated beside the store, inside a worker already
/// near the 4GB wasm ceiling, all of it CBOR-shipped in the lean handoff.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct TypePathBucket {
    pub actor_slots: Vec<Slot>,
    pub lightweight_count: usize,
}

/// One row of staticItemLocations (typePath is always None there). position/
/// worldPosition are kept as ready-made JSON arrays: findItemLocations passes
/// them through verbatim (Python `dict(staticEntry, count=...)`).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct StaticItemLocation {
    pub instance_name: String,
    pub label: String,
    pub count: i64,
    pub position: Value,
    pub world_position: Value,
}

/// The owned save index. Everything describeInstance/the query functions read
/// from Python's saveIndex dict lives here; headers/objects lookups resolve
/// through `by_instance_name` Slots into the SaveStore.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MapIndex {
    /// saveIndex["headers"]/["objects"]: instanceName -> (level, slot), with
    /// Python dict semantics (last value wins, first position kept). The
    /// store's headers/objects are index-aligned, so one map serves both.
    /// Skipped in the handoff serialization: header-derived, so from_cbor
    /// rebuilds it from the (lean) store instead of shipping ~100MB of it.
    #[serde(skip)]
    pub by_instance_name: IndexMap<Vec<u8>, Slot>,
    /// Every consist _trainConsistsFromMaps yields (collectTrainInfo needs
    /// the full list, orphan single-car entries included).
    pub train_consists: Vec<TrainInfo>,
    /// saveIndex["trainInfoByInstanceName"]: only ids whose header typePath
    /// is BP_Train_C -- values are indices into `train_consists`.
    pub train_info_by_instance_name: IndexMap<String, usize>,
    pub station_name_by_station_instance: IndexMap<String, String>,
    pub pipe_network_id_to_fluid: IndexMap<i32, String>,
    pub pipe_network_id_to_total_fluid: IndexMap<i32, f64>,
    pub pipe_network_id_to_members: IndexMap<i32, Vec<String>>,
    /// Positions per lightweight type path; index in the Vec == the <idx> of
    /// the synthetic id. Replaces the old per-instance map whose keys and
    /// per-entry type_path clones cost ~200 bytes x 100k+ instances.
    pub lightweight_positions_by_type_path: IndexMap<String, Vec<[f64; 3]>>,
    pub instance_slots_by_type_path: IndexMap<String, TypePathBucket>,
    /// itemShortName -> [(instanceName, count)], insertion-ordered (the exact
    /// extract::item_location_index output).
    pub item_location_index: IndexMap<Vec<u8>, Vec<(Vec<u8>, i64)>>,
    pub dimensional_depot_by_item: IndexMap<String, i64>,
    pub static_item_locations: IndexMap<String, Vec<StaticItemLocation>>,
}

impl MapIndex {
    /// scan.headersByInstanceName.get(name) -- the Header, or None.
    pub fn header_by_name<'a>(&self, store: &'a SaveStore, name: &[u8]) -> Option<&'a Header> {
        self.by_instance_name.get(name).map(|&(li, oi)| &store.levels[li].headers[oi])
    }

    /// scan.objectsByInstanceName.get(name) -- the Object, or None. Owned:
    /// re-parsed on demand from its recorded byte span, since the parsed
    /// model is dropped once the payload/index are built (a single object
    /// re-parses in microseconds). None also if the reparse fails, which
    /// would be a bug -- the same bytes parsed cleanly at load.
    pub fn parse_object_by_name(&self, store: &SaveStore, name: &[u8]) -> Option<Object> {
        let &(li, oi) = self.by_instance_name.get(name)?;
        let object = store.parse_object_at(li, oi);
        debug_assert!(object.is_ok(), "reparse of a known instance failed: {:?}", object.as_ref().err().map(|e| &e.msg));
        object.ok()
    }

    /// Materialized instance names for one type path: actor names decode
    /// from their headers, lightweight ids synthesize from the count --
    /// same content and order the old eagerly-materialized map stored.
    pub fn instance_names_for_type_path(&self, store: &SaveStore, type_path: &str) -> Vec<String> {
        let Some(bucket) = self.instance_slots_by_type_path.get(type_path) else {
            return Vec::new();
        };
        let data: &[u8] = &store.data;
        let mut names: Vec<String> = bucket
            .actor_slots
            .iter()
            .map(|&(li, oi)| store.levels[li].headers[oi].instance_name().to_string(data))
            .collect();
        names.extend(
            (0..bucket.lightweight_count)
                .map(|idx| format!("LightweightBuildable:{}:{}", type_path, idx)),
        );
        names
    }

    /// Resolve a synthetic "LightweightBuildable:<tp>:<idx>" id to its type
    /// path and position (None for ordinary instance names or stale ids).
    pub fn lightweight_entry(&self, instance_name: &str) -> Option<(&str, [f64; 3])> {
        let rest = instance_name.strip_prefix("LightweightBuildable:")?;
        let (type_path, idx) = rest.rsplit_once(':')?;
        let idx: usize = idx.parse().ok()?;
        let (key, positions) = self.lightweight_positions_by_type_path.get_key_value(type_path)?;
        positions.get(idx).map(|&p| (key.as_str(), p))
    }

    /// CBOR-serialize for the lean-worker handoff: the loaded worker ships
    /// its index to a fresh wasm instance so the index never has to be
    /// rebuilt (rebuilding needs the full parsed model). Header-derived
    /// parts are skipped (from_cbor recomputes them). Two passes: a counting
    /// pass sizes the output exactly, so the buffer is one right-sized
    /// allocation instead of doubling -- this runs in a worker whose heap
    /// already sits near the 4GB wasm ceiling. Same-build, same-page-load
    /// transfers only; no versioning concerns.
    pub fn to_cbor(&self) -> Result<Vec<u8>, String> {
        struct Counter(usize);
        impl ciborium_io::Write for &mut Counter {
            type Error = core::convert::Infallible;
            fn write_all(&mut self, data: &[u8]) -> Result<(), Self::Error> {
                self.0 += data.len();
                Ok(())
            }
            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        let mut counter = Counter(0);
        ciborium::into_writer(self, &mut counter).map_err(|e| format!("index size: {e}"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(counter.0).map_err(|e| format!("index buffer: {e}"))?;
        ciborium::into_writer(self, &mut out).map_err(|e| format!("index serialize: {e}"))?;
        Ok(out)
    }

    /// Deserialize a handoff index against the (lean) store it describes,
    /// rebuilding the skipped header-derived maps from the store's headers.
    pub fn from_cbor(bytes: &[u8], store: &SaveStore) -> Result<MapIndex, String> {
        let mut index = Self::from_cbor_partial(bytes)?;
        index.rebuild_header_maps(store);
        Ok(index)
    }

    /// First half of from_cbor: everything except the header-derived maps.
    /// Split out so load_lean can free the CBOR bytes before decompressing
    /// the body (peak-memory ordering).
    pub fn from_cbor_partial(bytes: &[u8]) -> Result<MapIndex, String> {
        ciborium::from_reader(bytes).map_err(|e| format!("index deserialize: {e}"))
    }

    /// Second half of from_cbor: recompute the serde-skipped maps from the
    /// (lean) store's headers.
    pub fn rebuild_header_maps(&mut self, store: &SaveStore) {
        let scan = SaveScan::new(store); // headers only -- works on a lean store
        self.by_instance_name =
            scan.by_instance_name.iter().map(|(k, &v)| (k.to_vec(), v)).collect();
    }

    pub fn build(store: &SaveStore) -> MapIndex {
        Self::build_with_scan(&SaveScan::new(store))
    }

    /// Build from an existing SaveScan (shared with the payload build --
    /// avoids a second full pass over every object).
    pub fn build_with_scan(scan: &SaveScan) -> MapIndex {
        let data = scan.data();

        let by_instance_name: IndexMap<Vec<u8>, Slot> =
            scan.by_instance_name.iter().map(|(k, &v)| (k.to_vec(), v)).collect();

        // -- instanceSlotsByTypePath ------------------------------------------
        // Keys decode via each bucket's first actor's StrRef so wide (UTF-16)
        // type paths come out exactly like Python str (same as buildings.rs).
        // Values are slots/counts; names materialize on demand via
        // instance_names_for_type_path.
        let mut instance_slots_by_type_path: IndexMap<String, TypePathBucket> = IndexMap::new();
        for (_, seq_headers) in &scan.actor_seqs_by_type_path {
            let type_path = scan.actor(seq_headers[0].1).type_path.to_string(data);
            instance_slots_by_type_path.insert(
                type_path,
                TypePathBucket {
                    actor_slots: seq_headers.iter().map(|&(_, slot)| slot).collect(),
                    lightweight_count: 0,
                },
            );
        }
        // Lightweight buildables' synthetic "LightweightBuildable:<tp>:<idx>"
        // ids fold in (setdefault + extend).
        for group in buildings::find_lightweight_buildable_groups(&scan) {
            let type_path = group.type_path.to_string(data);
            instance_slots_by_type_path.entry(type_path).or_default().lightweight_count +=
                group.instances.len();
        }

        // -- stationNameByStationInstance --------------------------------------
        let mut station_name_by_station_instance: IndexMap<String, String> = IndexMap::new();
        for (_, obj_slot) in scan.actors_of_type(&[TRAIN_STATION_IDENTIFIER_TYPE_PATH]) {
            let Some(obj_slot) = obj_slot else { continue };
            let Some(identifier_object) = scan.parse_object(obj_slot) else { continue };
            let station = props::object_ref(&identifier_object.properties, data, b"mStation");
            let station_name = trains_progression::text_property_value(
                find_prop(&identifier_object.properties, data, b"mStationName"),
                data,
            );
            // `if station is not None and hasattr(station, "pathName") and
            // stationName:` -- hasattr always holds for an ObjectReference;
            // the name must be truthy (non-empty). The pathName itself is NOT
            // emptiness-checked (Python doesn't either).
            if let (Some(station), Some(name)) = (station, station_name) {
                if !name.is_empty() {
                    station_name_by_station_instance
                        .insert(station.path_name.to_string(data), name);
                }
            }
        }

        // -- pipe networks -----------------------------------------------------
        let mut pipe_network_id_to_fluid: IndexMap<i32, String> = IndexMap::new();
        let mut pipe_network_id_to_total_fluid: IndexMap<i32, f64> = IndexMap::new();
        let mut pipe_network_id_to_members: IndexMap<i32, Vec<String>> = IndexMap::new();
        for (_, obj_slot) in scan.actors_of_type(&[PIPE_NETWORK_TYPE_PATH]) {
            let Some(obj_slot) = obj_slot else { continue };
            let Some(network_object) = scan.parse_object(obj_slot) else { continue };
            let fluid_label: Option<String> =
                props::object_ref(&network_object.properties, data, b"mFluidDescriptor")
                    .and_then(|r| {
                        if r.path_name.bytes(data).is_empty() {
                            None
                        } else {
                            Some(readable_label(&r.path_name.to_string(data)))
                        }
                    });
            // `getPropertyValue(..., "mFluidIntegrantScriptInterfaces") or []`.
            let members: &[ObjectRef] =
                match find_prop(&network_object.properties, data, b"mFluidIntegrantScriptInterfaces")
                {
                    Some(PropertyValue::Array(ArrayValue::Refs(v))) => v,
                    _ => &[],
                };
            let mut network_id: Option<i32> = None;
            let mut total_fluid: f64 = 0.0;
            let mut member_names: Vec<String> = Vec::new();
            let mut connector_key: Vec<u8> = Vec::new();
            for member_reference in members {
                let path = member_reference.path_name.bytes(data);
                if path.is_empty() {
                    continue;
                }
                member_names.push(member_reference.path_name.to_string(data));
                if let Some(member_object) = scan.parse_object_by_name(path) {
                    // `if memberFluid:` -- truthiness on the float.
                    if let Some(member_fluid) =
                        props::fluid_box(&member_object.properties, data, b"mFluidBox")
                    {
                        if member_fluid != 0.0 {
                            total_fluid += member_fluid;
                        }
                    }
                }
                if network_id.is_some() {
                    continue; // All members share one ID -- resolving it once is enough.
                }
                for connector_suffix in PIPE_CONNECTOR_SUFFIXES {
                    connector_key.clear();
                    connector_key.extend_from_slice(path);
                    connector_key.extend_from_slice(connector_suffix.as_bytes());
                    let Some(connector_object) = scan.parse_object_by_name(&connector_key) else {
                        continue;
                    };
                    network_id = props::int(&connector_object.properties, data, b"mPipeNetworkID");
                    if network_id.is_some() {
                        break;
                    }
                }
            }
            if let Some(id) = network_id {
                pipe_network_id_to_members.insert(id, member_names);
                if let Some(label) = fluid_label {
                    pipe_network_id_to_fluid.insert(id, label);
                    pipe_network_id_to_total_fluid.insert(id, total_fluid);
                }
            }
        }

        // -- lightweightPositionsByTypePath ------------------------------------
        let mut lightweight_positions_by_type_path: IndexMap<String, Vec<[f64; 3]>> =
            IndexMap::new();
        for group in buildings::find_lightweight_buildable_groups(&scan) {
            let type_path = group.type_path.to_string(data);
            lightweight_positions_by_type_path
                .insert(type_path, group.instances.iter().map(|i| i.position).collect());
        }

        // -- train consists ------------------------------------------------------
        let consists = trains_progression::train_consists(&scan);
        let mut train_consists: Vec<TrainInfo> = Vec::with_capacity(consists.len());
        let mut train_info_by_instance_name: IndexMap<String, usize> = IndexMap::new();
        for consist in &consists {
            let owned = TrainInfo {
                id: props::lossy(consist.id),
                label: consist.label.clone(),
                cars: consist
                    .cars
                    .iter()
                    .map(|car| TrainCar {
                        id: props::lossy(car.id),
                        type_path: props::lossy(car.type_path),
                        position: car.position,
                        rotation: car.rotation,
                    })
                    .collect(),
            };
            // Only ids whose header typePath is BP_Train_C (orphan single-car
            // entries are skipped -- see the Python comment).
            if let Some(Header::Actor(actor)) = scan.header_by_name(consist.id) {
                if actor.type_path.bytes(data) == TRAIN_TYPE_PATH.as_bytes() {
                    train_info_by_instance_name.insert(owned.id.clone(), train_consists.len());
                }
            }
            train_consists.push(owned);
        }

        // -- itemLocationIndex ---------------------------------------------------
        let item_location_index: IndexMap<Vec<u8>, Vec<(Vec<u8>, i64)>> =
            crate::extract::item_location_index(scan).into_iter().collect();

        // -- dimensionalDepotByItem ------------------------------------------------
        // {entry["itemPath"]: entry["count"]} over the depot rows -- a dict
        // comprehension, so a duplicate itemPath is last-wins (IndexMap
        // insert overwrites the value, keeps the first position -- same).
        let mut dimensional_depot_by_item: IndexMap<String, i64> = IndexMap::new();
        if let Value::Array(rows) = scan.depot_contents() {
            for row in rows {
                let item_path = row["itemPath"].as_str().unwrap_or_default().to_string();
                let count = row["count"].as_i64().unwrap_or(0);
                dimensional_depot_by_item.insert(item_path, count);
            }
        }

        let static_item_locations = collect_static_item_locations(&scan);

        MapIndex {
            by_instance_name,
            train_consists,
            train_info_by_instance_name,
            station_name_by_station_instance,
            pipe_network_id_to_fluid,
            pipe_network_id_to_total_fluid,
            pipe_network_id_to_members,
            lightweight_positions_by_type_path,
            instance_slots_by_type_path,
            item_location_index,
            dimensional_depot_by_item,
            static_item_locations,
        }
    }
}

/// sav_map_data._collectStaticItemLocations. Reads the collectors' own output
/// dicts exactly like Python reads scan.collectables()/collectHardDrives()
/// (positions pass through as the collectors' JSON values, bit-identical).
fn collect_static_item_locations(scan: &SaveScan) -> IndexMap<String, Vec<StaticItemLocation>> {
    let mut index: IndexMap<String, Vec<StaticItemLocation>> = IndexMap::new();

    fn add_entries(
        index: &mut IndexMap<String, Vec<StaticItemLocation>>,
        item_short_name: &str,
        label: &str,
        bucket: &Value,
        ids_key: &str,
        points_key: &str,
        world_key: &str,
    ) {
        let empty: Vec<Value> = Vec::new();
        let ids = bucket[ids_key].as_array().unwrap_or(&empty);
        let points = bucket[points_key].as_array().unwrap_or(&empty);
        let world_positions = bucket[world_key].as_array().unwrap_or(&empty);
        for i in 0..ids.len() {
            index.entry(item_short_name.to_string()).or_default().push(StaticItemLocation {
                instance_name: ids[i].as_str().unwrap_or_default().to_string(),
                label: label.to_string(),
                count: 1,
                position: Value::Array(vec![
                    points[i * 3].clone(),
                    points[i * 3 + 1].clone(),
                    points[i * 3 + 2].clone(),
                ]),
                world_position: Value::Array(vec![
                    world_positions[i * 2].clone(),
                    world_positions[i * 2 + 1].clone(),
                ]),
            });
        }
    }

    let collectables = scan.collectables();
    for (kind, item_short_name, label) in COLLECTABLE_ITEMS {
        add_entries(
            &mut index,
            item_short_name,
            label,
            &collectables[kind],
            "remainingIds",
            "remaining",
            "remainingWorldPositions",
        );
    }

    let hard_drives = scan.hard_drives();
    add_entries(
        &mut index,
        HARD_DRIVE_ITEM_SHORT_NAME,
        "Hard Drive",
        &hard_drives,
        "hasDriveIds",
        "hasDrive",
        "hasDriveWorldPositions",
    );

    // World-spawned free item stacks in not-yet-generated map areas -- the
    // catalog-only remainder (see _uncollectedCatalogDrops).
    for &(short_name, quantity, position, instance_name) in scan.uncollected_catalog_drops() {
        let [px, py] = project_xy(position[0], position[1]);
        index.entry(short_name.to_string()).or_default().push(StaticItemLocation {
            instance_name: instance_name.to_string(),
            label: "Dropped on the ground".to_string(),
            count: quantity,
            position: Value::Array(vec![
                jnum(px),
                jnum(py),
                jnum(world_z_to_meters(position[2])),
            ]),
            world_position: Value::Array(vec![jnum(position[0]), jnum(position[1])]),
        });
    }

    index
}

// ---------------------------------------------------------------------------
// Gating dump
// ---------------------------------------------------------------------------

impl MapIndex {
    /// The saveIndex dump tools/diff_payload.py compares (order-blind
    /// canonical() shapes): headers/objects as sorted instance-name lists,
    /// int dict keys as decimal strings (Python json.dump does the same),
    /// tuples as arrays.
    pub fn dump(&self, store: &SaveStore) -> Value {
        let data: &[u8] = &store.data;
        // sorted(headersByInstanceName.keys()) -- decode each header's own
        // StrRef (wide-aware), then Rust String order == Python code-point
        // order. headers/objects share one key set (index-aligned).
        let mut names: Vec<String> = self
            .by_instance_name
            .values()
            .map(|&(li, oi)| store.levels[li].headers[oi].instance_name().to_string(data))
            .collect();
        names.sort();
        let names = Value::Array(names.into_iter().map(Value::String).collect());

        let mut train_info = serde_json::Map::new();
        for (id, &idx) in &self.train_info_by_instance_name {
            train_info.insert(id.clone(), train_info_value(&self.train_consists[idx]));
        }

        let mut station_names = serde_json::Map::new();
        for (station, name) in &self.station_name_by_station_instance {
            station_names.insert(station.clone(), Value::String(name.clone()));
        }

        let mut fluid = serde_json::Map::new();
        for (id, label) in &self.pipe_network_id_to_fluid {
            fluid.insert(id.to_string(), Value::String(label.clone()));
        }
        let mut total_fluid = serde_json::Map::new();
        for (id, total) in &self.pipe_network_id_to_total_fluid {
            total_fluid.insert(id.to_string(), jnum(*total));
        }
        let mut members = serde_json::Map::new();
        for (id, names) in &self.pipe_network_id_to_members {
            members.insert(
                id.to_string(),
                Value::Array(names.iter().map(|n| Value::String(n.clone())).collect()),
            );
        }

        let mut lightweight = serde_json::Map::new();
        for (type_path, positions) in &self.lightweight_positions_by_type_path {
            for (idx, p) in positions.iter().enumerate() {
                lightweight.insert(
                    format!("LightweightBuildable:{}:{}", type_path, idx),
                    json!({
                        "typePath": type_path,
                        "position": [jnum(p[0]), jnum(p[1]), jnum(p[2])],
                    }),
                );
            }
        }

        let mut by_type_path = serde_json::Map::new();
        for type_path in self.instance_slots_by_type_path.keys() {
            by_type_path.insert(
                type_path.clone(),
                Value::Array(
                    self.instance_names_for_type_path(store, type_path)
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }

        let mut item_index = serde_json::Map::new();
        for (short, entries) in &self.item_location_index {
            item_index.insert(
                props::lossy(short),
                Value::Array(
                    entries
                        .iter()
                        .map(|(name, count)| {
                            json!([Value::String(props::lossy(name)), Value::from(*count)])
                        })
                        .collect(),
                ),
            );
        }

        let mut depot = serde_json::Map::new();
        for (item_path, count) in &self.dimensional_depot_by_item {
            depot.insert(item_path.clone(), Value::from(*count));
        }

        let mut static_locations = serde_json::Map::new();
        for (short, entries) in &self.static_item_locations {
            static_locations.insert(
                short.clone(),
                Value::Array(entries.iter().map(static_location_value).collect()),
            );
        }

        json!({
            "headers": names.clone(),
            "objects": names,
            "trainInfoByInstanceName": Value::Object(train_info),
            "stationNameByStationInstance": Value::Object(station_names),
            "pipeNetworkIdToFluid": Value::Object(fluid),
            "pipeNetworkIdToTotalFluid": Value::Object(total_fluid),
            "pipeNetworkIdToMembers": Value::Object(members),
            "lightweightInstancesById": Value::Object(lightweight),
            "instanceNamesByTypePath": Value::Object(by_type_path),
            "itemLocationIndex": Value::Object(item_index),
            "dimensionalDepotByItem": Value::Object(depot),
            "staticItemLocations": Value::Object(static_locations),
        })
    }
}

/// The _trainConsistsFromMaps dict shape ({"id", "label", "cars": [...]}).
fn train_info_value(train: &TrainInfo) -> Value {
    let cars: Vec<Value> = train
        .cars
        .iter()
        .map(|car| {
            json!({
                "id": car.id,
                "typePath": car.type_path,
                "position": [jnum(car.position[0]), jnum(car.position[1]), jnum(car.position[2])],
                "rotation": [
                    jnum(car.rotation[0]), jnum(car.rotation[1]),
                    jnum(car.rotation[2]), jnum(car.rotation[3]),
                ],
            })
        })
        .collect();
    let label = match &train.label {
        Some(s) => Value::String(s.clone()),
        None => Value::Null,
    };
    json!({"id": train.id, "label": label, "cars": cars})
}

/// The _collectStaticItemLocations entry dict, with its original key order
/// (findItemLocations reuses this shape verbatim).
pub(crate) fn static_location_value(entry: &StaticItemLocation) -> Value {
    json!({
        "instanceName": entry.instance_name,
        "typePath": Value::Null,
        "label": entry.label,
        "count": entry.count,
        "position": entry.position,
        "worldPosition": entry.world_position,
    })
}
