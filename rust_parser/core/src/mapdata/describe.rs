//! sav_map_data.describeInstance (lines 3043-3363) -- the per-click tooltip
//! endpoint. Exact behavioral port: key insertion order, None-vs-absent
//! semantics, truthiness gates, py_round call sites and the rawProperties
//! toString formatting (display.rs) all mirror the Python reference.

use super::collectors::trains_progression::recipe_label;
use super::consts::*;
use super::display::py_str;
use super::index::{MapIndex, PIPE_CONNECTOR_SUFFIXES};
use super::jsonval::{jnum, py_round};
use super::names::readable_label;
use super::props;
use super::queries;
use crate::extract::{find_prop, stack_item};
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Python truthiness of a converted property value (`if getPropertyValue(...)`)
/// -- convert.rs defines the Python shape each variant becomes.
fn truthy(v: &PropertyValue, data: &[u8]) -> bool {
    match v {
        PropertyValue::Bool(b) => *b != 0, // converts to a plain int
        PropertyValue::Int8(_) => true,    // one-byte bytes value: always truthy
        PropertyValue::Int(x) => *x != 0,
        PropertyValue::UInt32(x) => *x != 0,
        PropertyValue::Int64(x) => *x != 0,
        PropertyValue::Float(x) => *x != 0.0,
        PropertyValue::Double(x) => *x != 0.0,
        PropertyValue::Str(s) => !s.bytes(data).is_empty(),
        // Fixed-shape non-empty lists / object instances.
        PropertyValue::Byte { .. }
        | PropertyValue::Enum { .. }
        | PropertyValue::Text(_)
        | PropertyValue::Set { .. }
        | PropertyValue::Object(_)
        | PropertyValue::SoftObject(..) => true,
        PropertyValue::Array(av) => match av {
            ArrayValue::I32(v) => !v.is_empty(),
            ArrayValue::I64(v) => !v.is_empty(),
            ArrayValue::U8(v) => !v.is_empty(),
            ArrayValue::F32(v) => !v.is_empty(),
            ArrayValue::F64(v) => !v.is_empty(),
            ArrayValue::Str(v) => !v.is_empty(),
            ArrayValue::SoftObj(v) => !v.is_empty(),
            ArrayValue::Refs(v) => !v.is_empty(),
            ArrayValue::Text(v) => !v.is_empty(),
            ArrayValue::LinearColor(v) => !v.is_empty(),
            ArrayValue::Vector(v) => !v.is_empty(),
            ArrayValue::Guid(v) => !v.is_empty(),
            ArrayValue::Opaque { .. } => true, // at least the blob element
            ArrayValue::Structs(v) => !v.is_empty(),
        },
        PropertyValue::Struct(sv) => match sv {
            StructValue::FluidBox(f) => *f != 0.0, // bare Python float
            StructValue::DateTime(x) => *x != 0,   // bare Python int
            StructValue::Raw(d) => !d.bytes(data).is_empty(),
            _ => true, // fixed-shape non-empty lists
        },
        PropertyValue::Map(entries) => !entries.is_empty(),
    }
}

/// sav_map_data._inventoryContents: merged same-item rows, insertion-ordered
/// by first appearance. Solids keep Python-int counts; fluids become
/// round(count / 1000, 1) floats with a "unit" key.
fn inventory_contents(data: &[u8], component: Option<&Object>) -> Vec<Value> {
    let Some(component) = component else {
        return Vec::new();
    };
    let Some(stacks) = props::array_structs(&component.properties, data, b"mInventoryStacks")
    else {
        return Vec::new();
    };
    let mut count_by_item: IndexMap<String, i64> = IndexMap::new();
    let mut fluid_labels: HashSet<String> = HashSet::new();
    for stack in stacks {
        if let Some((item_path, num_items)) = stack_item(stack, data) {
            let path = props::lossy(item_path);
            let label = readable_label(&path);
            *count_by_item.entry(label.clone()).or_insert(0) += num_items;
            if queries::is_fluid_item_path(&path) {
                fluid_labels.insert(label);
            }
        }
    }
    count_by_item
        .into_iter()
        .map(|(label, count)| {
            if fluid_labels.contains(&label) {
                json!({"item": label, "count": jnum(py_round(count as f64 / 1000.0, 1)), "unit": "m³"})
            } else {
                json!({"item": label, "count": count})
            }
        })
        .collect()
}

/// `merged["count"] = round(merged["count"] + entry["count"], 1)`: Python
/// round() of an int sum is the int itself; any float operand makes the sum
/// (and the rounded result) a float.
fn merged_count(a: &Value, b: &Value) -> Value {
    match (a.as_i64(), b.as_i64()) {
        (Some(x), Some(y)) => Value::from(x + y),
        _ => jnum(py_round(a.as_f64().unwrap_or(0.0) + b.as_f64().unwrap_or(0.0), 1)),
    }
}

/// describeInstance's countedItemList: readable-label counts in
/// first-appearance order (no empty-path filtering -- Python has none).
fn counted_item_list<'a>(item_paths: impl Iterator<Item = &'a [u8]>) -> Vec<Value> {
    let mut count_by_item: IndexMap<String, i64> = IndexMap::new();
    for item_path in item_paths {
        let label = readable_label(&props::lossy(item_path));
        *count_by_item.entry(label).or_insert(0) += 1;
    }
    count_by_item
        .into_iter()
        .map(|(label, count)| json!({"item": label, "count": count}))
        .collect()
}

fn raw_properties(pl: &PropList, data: &[u8]) -> Value {
    Value::Array(
        pl.props
            .iter()
            .map(|p| json!({"name": p.name.to_string(data), "value": py_str(&p.value, data)}))
            .collect(),
    )
}

fn position_value(p: &[f64; 3]) -> Value {
    json!([jnum(p[0]), jnum(p[1]), jnum(p[2])])
}

pub fn describe_instance(store: &SaveStore, index: &MapIndex, instance_name: &str) -> Value {
    let data: &[u8] = &store.data;

    // Lightweight buildables resolve from their own synthetic index.
    if let Some((type_path, position)) = index.lightweight_entry(instance_name) {
        return json!({
            "instanceName": instance_name,
            "typePath": type_path,
            "label": readable_label(type_path),
            "position": position_value(&position),
        });
    }

    let Some(header) = index.header_by_name(store, instance_name.as_bytes()) else {
        return json!({"error": "Instance not found in the currently loaded save (it may have been removed, mined out, or collected)."});
    };

    // `getattr(header, "typePath", None) or getattr(header, "className", None)`:
    // an actor's empty typePath falls through to None (components have no
    // typePath attribute, so their className is taken even when empty).
    let type_path: Option<String> = match header {
        Header::Actor(a) => {
            let tp = a.type_path.to_string(data);
            if tp.is_empty() {
                None
            } else {
                Some(tp)
            }
        }
        Header::Component(c) => Some(c.class_name.to_string(data)),
    };
    let header_position: Value = match header {
        Header::Actor(a) => position_value(&[
            a.position[0] as f64,
            a.position[1] as f64,
            a.position[2] as f64,
        ]),
        Header::Component(_) => Value::Null, // getattr(header, "position", None)
    };
    // `readableLabel(typePath) if typePath else instanceName` -- "" is falsy.
    let label = match &type_path {
        Some(tp) if !tp.is_empty() => readable_label(tp),
        _ => instance_name.to_string(),
    };

    let mut result = Map::new();
    result.insert("instanceName".into(), Value::String(instance_name.to_string()));
    result.insert(
        "typePath".into(),
        match &type_path {
            Some(tp) => Value::String(tp.clone()),
            None => Value::Null,
        },
    );
    result.insert("label".into(), Value::String(label));
    result.insert("position".into(), header_position);

    // A whole train consist: composition + every member car's cargo summed.
    if let Some(&train_idx) = index.train_info_by_instance_name.get(instance_name) {
        let train = &index.train_consists[train_idx];
        // `trainInfo["label"] or "Train"` -- empty/None falls back.
        let train_label = match &train.label {
            Some(l) if !l.is_empty() => l.clone(),
            _ => "Train".to_string(),
        };
        result.insert("label".into(), Value::String(train_label));
        result.insert("position".into(), position_value(&train.cars[0].position));
        result.insert(
            "trainCars".into(),
            Value::Array(
                train
                    .cars
                    .iter()
                    .map(|car| {
                        json!({"kind": readable_label(&car.type_path), "instanceName": car.id})
                    })
                    .collect(),
            ),
        );
        let mut total_by_item: IndexMap<String, Value> = IndexMap::new();
        for car in &train.cars {
            let Some(car_object) = index.parse_object_by_name(store, car.id.as_bytes()) else {
                continue;
            };
            let mut car_inventory = inventory_contents(
                data,
                queries::resolve_component_object(
                    store,
                    index,
                    &car_object.properties,
                    b"mStorageInventory",
                )
                .as_ref(),
            );
            if car_inventory.is_empty() {
                car_inventory = inventory_contents(
                    data,
                    queries::resolve_component_object(
                        store,
                        index,
                        &car_object.properties,
                        b"mInventory",
                    )
                    .as_ref(),
                );
            }
            for entry in car_inventory {
                let item = entry["item"].as_str().unwrap_or_default().to_string();
                match total_by_item.entry(item) {
                    indexmap::map::Entry::Vacant(slot) => {
                        slot.insert(entry); // dict(entry) -- key order kept
                    }
                    indexmap::map::Entry::Occupied(mut slot) => {
                        let merged = slot.get_mut();
                        let count = merged_count(&merged["count"], &entry["count"]);
                        merged["count"] = count;
                    }
                }
            }
        }
        if !total_by_item.is_empty() {
            // sorted(values, key=count, reverse=True) -- stable.
            let mut rows: Vec<Value> = total_by_item.into_values().collect();
            rows.sort_by(|a, b| {
                let ca = a["count"].as_f64().unwrap_or(0.0);
                let cb = b["count"].as_f64().unwrap_or(0.0);
                cb.partial_cmp(&ca).expect("count NaN")
            });
            result.insert("cargoInventory".into(), Value::Array(rows));
        }
        return Value::Object(result);
    }

    // `if stationName:` -- truthy (non-empty).
    if let Some(station_name) = index.station_name_by_station_instance.get(instance_name) {
        if !station_name.is_empty() {
            result.insert("stationName".into(), Value::String(station_name.clone()));
        }
    }

    let Some(object) = index.parse_object_by_name(store, instance_name.as_bytes()) else {
        return Value::Object(result);
    };
    let properties = &object.properties;

    // The tamed Lizard Doggo's pet name: mDisplayName is a TextProperty,
    // exposed as a list whose LAST element is taken (`displayName[-1]`).
    if type_path.as_deref() == Some(LIZARD_DOGGO_TYPE_PATH) {
        if let Some(PropertyValue::Text(t)) = find_prop(properties, data, b"mDisplayName") {
            let pet_name: Value = match t {
                TextValue::NoneHistory { s, .. } => Value::String(s.to_string(data)),
                TextValue::Base { value, .. } => Value::String(value.to_string(data)),
                TextValue::ArgumentFormat { args, .. } => Value::Array(
                    args.iter()
                        .map(|(name, value, aflags)| {
                            json!([name.to_string(data), value.to_string(data), aflags])
                        })
                        .collect(),
                ),
                TextValue::StringTable { text_key, .. } => Value::String(text_key.to_string(data)),
            };
            let pet_truthy = match &pet_name {
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                _ => false,
            };
            if pet_truthy {
                result.insert("petName".into(), pet_name);
            }
        }
    }

    // Players: label + inventory + rawProperties only, returned early.
    if type_path.as_deref() == Some(PLAYER_TYPE_PATH) {
        if let Some(player_name) = props::string(properties, data, b"mCachedPlayerName") {
            let player_name = player_name.to_string(data);
            if !player_name.is_empty() {
                result.insert("label".into(), Value::String(player_name));
            }
        }
        let inventory = inventory_contents(
            data,
            queries::resolve_component_object(store, index, properties, b"mInventory").as_ref(),
        );
        if !inventory.is_empty() {
            result.insert("playerInventory".into(), Value::Array(inventory));
        }
        result.insert("rawProperties".into(), raw_properties(properties, data));
        return Value::Object(result);
    }

    let recipe = find_prop(properties, data, b"mCurrentRecipe");
    let recipe_path_name: Option<String> = match recipe {
        Some(PropertyValue::Object(r)) if !r.path_name.bytes(data).is_empty() => {
            Some(r.path_name.to_string(data))
        }
        _ => None,
    };
    if let Some(recipe_path_name) = &recipe_path_name {
        result.insert("recipe".into(), Value::String(recipe_label(recipe_path_name)));
    }

    let can_overclock = recipe.is_some()
        || find_prop(properties, data, b"mExtractableResource").is_some()
        || type_path.as_deref().is_some_and(|tp| tp.contains("Generator"));
    let mut clock_speed_fraction = 1.0f64;
    if can_overclock {
        clock_speed_fraction =
            props::float(properties, data, b"mCurrentPotential").unwrap_or(1.0);
        result.insert(
            "clockSpeedPercent".into(),
            jnum(py_round(clock_speed_fraction * 100.0, 1)),
        );

        // mIsProducing/mIsProductionPaused are only serialized when True.
        let running_status = if find_prop(properties, data, b"mIsProductionPaused")
            .is_some_and(|v| truthy(v, data))
        {
            "Paused"
        } else if find_prop(properties, data, b"mIsProducing").is_some_and(|v| truthy(v, data)) {
            "Running"
        } else {
            "Idle"
        };
        result.insert("runningStatus".into(), Value::String(running_status.to_string()));
    }

    if let Some(progress) = props::float(properties, data, b"mCurrentManufacturingProgress") {
        result.insert("productionProgressPercent".into(), jnum(py_round(progress * 100.0, 1)));
    }

    if let Some(rated) =
        queries::rated_power_for_type_path(type_path.as_deref(), recipe_path_name.as_deref())
    {
        match queries::scale_rated_power_for_clock_speed(rated, clock_speed_fraction) {
            queries::ScaledPower::Range(lo, hi, mid) => {
                result.insert("basePowerConsumptionRangeMW".into(), json!([jnum(lo), jnum(hi)]));
                result.insert("basePowerConsumptionMeanMW".into(), jnum(mid));
            }
            queries::ScaledPower::Scalar(scaled) => {
                result.insert("basePowerConsumptionMW".into(), jnum(scaled));
            }
        }
    }

    // Power Storage charge: a plain float directly on the actor.
    if let Some(power_store) = props::float(properties, data, b"mPowerStore") {
        result.insert("powerStoredMWh".into(), jnum(py_round(power_store, 1)));
    }

    // Generators report production via their FGPowerInfoComponent.
    if type_path.as_deref().is_some_and(|tp| tp.contains("Generator")) {
        if let Some(power_component) =
            queries::resolve_component_object(store, index, properties, b"mPowerInfo")
        {
            let production =
                props::float(&power_component.properties, data, b"mDynamicProductionCapacity")
                    .or_else(|| {
                        props::float(&power_component.properties, data, b"mBaseProduction")
                    });
            if let Some(production) = production {
                result.insert("powerProductionMW".into(), jnum(py_round(production, 1)));
            }
        }
    }

    // Pipes/pumps: current fluid amount + network fluid type/total.
    let mut connector_key: Vec<u8> = Vec::new();
    if let Some(fluid_content) = props::fluid_box(properties, data, b"mFluidBox") {
        result.insert("fluidContent".into(), jnum(py_round(fluid_content, 1)));
        for connector_suffix in PIPE_CONNECTOR_SUFFIXES {
            connector_key.clear();
            connector_key.extend_from_slice(instance_name.as_bytes());
            connector_key.extend_from_slice(connector_suffix.as_bytes());
            let Some(connector_object) = index.parse_object_by_name(store, &connector_key) else {
                continue;
            };
            let network_id = props::int(&connector_object.properties, data, b"mPipeNetworkID");
            let fluid_label = network_id.and_then(|id| index.pipe_network_id_to_fluid.get(&id));
            if let Some(fluid_label) = fluid_label {
                result.insert("fluidType".into(), Value::String(fluid_label.clone()));
                let network_total =
                    network_id.and_then(|id| index.pipe_network_id_to_total_fluid.get(&id));
                if let Some(&network_total) = network_total {
                    result.insert("networkFluidContent".into(), jnum(py_round(network_total, 1)));
                }
                break;
            }
        }
    }

    // Mixed-mark pipe network detection (rated members only).
    if queries::pipe_flow_limit_per_minute(type_path.as_deref()).is_some() {
        for connector_suffix in PIPE_CONNECTOR_SUFFIXES {
            connector_key.clear();
            connector_key.extend_from_slice(instance_name.as_bytes());
            connector_key.extend_from_slice(connector_suffix.as_bytes());
            let Some(connector_object) = index.parse_object_by_name(store, &connector_key) else {
                continue;
            };
            let network_id = props::int(&connector_object.properties, data, b"mPipeNetworkID");
            let member_names =
                network_id.and_then(|id| index.pipe_network_id_to_members.get(&id));
            // `if memberNames:` -- the break sits inside this truthy gate.
            if let Some(member_names) = member_names {
                if !member_names.is_empty() {
                    if let Some(network_bottleneck) = queries::pipe_network_bottleneck(
                        store,
                        index,
                        member_names,
                        type_path.as_deref(),
                    ) {
                        result.insert("lineBottleneck".into(), network_bottleneck);
                    }
                    break;
                }
            }
        }
    }

    // Inventories, per component-name convention.
    let mut input_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mInputInventory").as_ref(),
    );
    input_inventory.extend(inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mFuelInventory").as_ref(),
    ));
    if !input_inventory.is_empty() {
        result.insert("inputInventory".into(), Value::Array(input_inventory));
    }
    let output_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mOutputInventory").as_ref(),
    );
    if !output_inventory.is_empty() {
        result.insert("outputInventory".into(), Value::Array(output_inventory));
    }
    let mut storage_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mStorageInventory").as_ref(),
    );
    if storage_inventory.is_empty() {
        // Wheeled vehicles' cargo trunk: name-linked child component.
        let mut key = instance_name.as_bytes().to_vec();
        key.extend_from_slice(b".StorageInventory");
        storage_inventory =
            inventory_contents(data, index.parse_object_by_name(store, &key).as_ref());
    }
    if !storage_inventory.is_empty() {
        result.insert("storageInventory".into(), Value::Array(storage_inventory));
    }

    // Wheeled vehicles' fuel slot, gated on the absence of mFuelInventory.
    if find_prop(properties, data, b"mFuelInventory").is_none() {
        let mut key = instance_name.as_bytes().to_vec();
        key.extend_from_slice(b".FuelInventory");
        let fuel_inventory =
            inventory_contents(data, index.parse_object_by_name(store, &key).as_ref());
        if !fuel_inventory.is_empty() {
            result.insert("fuelInventory".into(), Value::Array(fuel_inventory));
        }
    }

    let buffer_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mBufferInventory").as_ref(),
    );
    if !buffer_inventory.is_empty() {
        result.insert("bufferInventory".into(), Value::Array(buffer_inventory));
    }

    // AWESOME Sink / Shop one-off names (overwrite storageInventory in place).
    let coupon_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mCouponInventory").as_ref(),
    );
    if !coupon_inventory.is_empty() {
        result.insert("storageInventory".into(), Value::Array(coupon_inventory));
    }
    let shop_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mShopInventory").as_ref(),
    );
    if !shop_inventory.is_empty() {
        result.insert("storageInventory".into(), Value::Array(shop_inventory));
    }

    // Train Docking Stations use a plain "mInventory" for cargo.
    let cargo_inventory = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mInventory").as_ref(),
    );
    if !cargo_inventory.is_empty() {
        result.insert("cargoInventory".into(), Value::Array(cargo_inventory));
    }

    // Freight platform load/unload direction.
    if let Some(orientation_reversed) = find_prop(properties, data, b"mIsOrientationReversed") {
        let load_mode = if truthy(orientation_reversed, data) {
            "Unloading from train"
        } else {
            "Loading onto train"
        };
        result.insert("loadMode".into(), Value::String(load_mode.to_string()));
    }

    // The Power Shard slot.
    let power_shard_slots = inventory_contents(
        data,
        queries::resolve_component_object(store, index, properties, b"mInventoryPotential").as_ref(),
    );
    if !power_shard_slots.is_empty() {
        result.insert("powerShardSlots".into(), Value::Array(power_shard_slots));
    }

    // Belts/lifts: in-transit items on the shared FGConveyorChainActor.
    if let Some(chain_actor) =
        queries::resolve_component_object(store, index, properties, b"mConveyorChainActor")
    {
        if let ActorSpecific::ConveyorChain { belts, items, .. } = &chain_actor.actor_specific {
            let segment_items = counted_item_list(
                queries::conveyor_chain_segment_item_paths(
                    &chain_actor.actor_specific,
                    data,
                    instance_name.as_bytes(),
                )
                .into_iter(),
            );
            if !segment_items.is_empty() {
                result.insert("itemsOnBelt".into(), Value::Array(segment_items));
            }
            if belts.len() > 1 {
                let line_items =
                    counted_item_list(items.iter().map(|(path, _)| path.bytes(data)));
                if !line_items.is_empty() {
                    result.insert("itemsOnLine".into(), Value::Array(line_items));
                    result.insert("lineSegmentCount".into(), Value::from(belts.len() as i64));
                }
                if let Some(line_bottleneck) = queries::conveyor_chain_bottleneck(
                    store,
                    index,
                    &chain_actor.actor_specific,
                    type_path.as_deref(),
                ) {
                    result.insert("lineBottleneck".into(), line_bottleneck);
                }
            }
        }
    }

    result.insert("rawProperties".into(), raw_properties(properties, data));
    Value::Object(result)
}
