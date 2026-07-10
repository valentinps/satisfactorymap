//! collectTrains + collectProgression ports (sav_map_data.py lines ~715-780,
//! 864-896, 1583-1940): train consist reconstruction from coupling refs, and
//! the schematics/game-phase progression panels.

use crate::extract::find_prop;
use crate::gamedata;
use crate::mapdata::consts::*;
use crate::mapdata::geometry::{project_xy, rendered_yaw, short_class_name, world_z_to_meters};
use crate::mapdata::jsonval::{jnum, py_round};
use crate::mapdata::names::readable_label;
use crate::mapdata::props;
use crate::mapdata::scan::SaveScan;
use crate::store::*;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Trains (sav_map_data lines ~715-780, 864-896)
// ---------------------------------------------------------------------------

/// sav_map_data._textPropertyValue: display string for HistoryType NONE
/// (list [flags, 255, invariant, s] -> s) or BASE ([flags, 0, ns, key,
/// value] -> value); anything else -> None.
pub(crate) fn text_property_value(value: Option<&PropertyValue>, data: &[u8]) -> Option<String> {
    match value {
        // StrRef::to_string is wide-aware (player-typed names like "Skövde"
        // serialize as UTF-16) -- exactly the str Python sees.
        Some(PropertyValue::Text(TextValue::NoneHistory { s, .. })) => Some(s.to_string(data)),
        Some(PropertyValue::Text(TextValue::Base { value, .. })) => Some(value.to_string(data)),
        _ => None,
    }
}

pub(crate) struct Car<'a> {
    pub(crate) id: &'a [u8],
    pub(crate) type_path: &'a [u8],
    pub(crate) position: [f64; 3],
    pub(crate) rotation: [f64; 4],
}

pub(crate) struct Consist<'a> {
    pub(crate) id: &'a [u8],
    pub(crate) label: Option<String>,
    pub(crate) cars: Vec<Car<'a>>,
}

fn is_railcar(type_path: &[u8]) -> bool {
    type_path == LOCOMOTIVE_TYPE_PATH.as_bytes()
        || type_path == FREIGHT_WAGON_TYPE_PATH.as_bytes()
}

/// _trainConsistsFromMaps' carEntry: header lookup + railcar type check.
fn car_entry<'a>(scan: &SaveScan<'a>, name: &'a [u8]) -> Option<Car<'a>> {
    let data = scan.data();
    let Header::Actor(actor) = scan.header_by_name(name)? else {
        return None; // component header: typePath is None in Python
    };
    let type_path = actor.type_path.bytes(data);
    if !is_railcar(type_path) {
        return None;
    }
    Some(Car {
        id: name,
        type_path,
        position: [actor.position[0] as f64, actor.position[1] as f64, actor.position[2] as f64],
        rotation: [
            actor.rotation[0] as f64,
            actor.rotation[1] as f64,
            actor.rotation[2] as f64,
            actor.rotation[3] as f64,
        ],
    })
}

/// `ref.pathName if ref is not None and getattr(ref, "pathName", None) else
/// None` -- an ObjectProperty ref with a non-empty pathName.
fn ref_path<'a>(r: Option<&'a ObjectRef>, data: &'a [u8]) -> Option<&'a [u8]> {
    let p = r?.path_name.bytes(data);
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// sav_map_data._trainConsistsFromMaps.
pub(crate) fn train_consists<'a>(scan: &SaveScan<'a>) -> Vec<Consist<'a>> {
    let data = scan.data();
    let mut trains: Vec<Consist<'a>> = Vec::new();
    let mut claimed: HashSet<&[u8]> = HashSet::new();
    let mut railcar_ids: Vec<&[u8]> = Vec::new();
    for (&instance_name, &slot) in scan.by_instance_name.iter() {
        // Component headers have typePath None in Python: not a railcar,
        // not TRAIN_TYPE_PATH -> `continue`.
        let Header::Actor(actor) = scan.header(slot) else { continue };
        let type_path = actor.type_path.bytes(data);
        if is_railcar(type_path) {
            railcar_ids.push(instance_name);
        } else if type_path != TRAIN_TYPE_PATH.as_bytes() {
            continue;
        } else {
            // objectsByInstanceName.get(instanceName): headers/objects are
            // index-aligned and both maps are last-value-wins, so the slot's
            // own object is exactly that lookup.
            let train_object = scan.object(slot);
            let first = props::object_ref(&train_object.properties, data, b"FirstVehicle")
                .or_else(|| props::object_ref(&train_object.properties, data, b"mFirstVehicle"));
            let last = props::object_ref(&train_object.properties, data, b"LastVehicle")
                .or_else(|| props::object_ref(&train_object.properties, data, b"mLastVehicle"));
            let mut ordered: Vec<&[u8]> = Vec::new();
            let mut seen: HashSet<&[u8]> = HashSet::new();
            let mut current: Option<&[u8]> = ref_path(first, data);
            // A consist is at most a few dozen cars -- the cap only guards
            // against a malformed coupling cycle.
            while let Some(cur) = current {
                if seen.contains(cur) || ordered.len() >= 100 {
                    break;
                }
                seen.insert(cur);
                ordered.push(cur);
                current = None;
                if let Some(car_object) = scan.object_by_name(cur) {
                    // Python: isinstance(list) and len == 3 -- only the
                    // Locomotive/FreightWagon actorSpecificInfo shape.
                    if let ActorSpecific::Train { previous, next } = &car_object.actor_specific {
                        for coupled in [previous, next] {
                            let coupled_name = coupled.path_name.bytes(data);
                            if !coupled_name.is_empty() && !seen.contains(coupled_name) {
                                current = Some(coupled_name);
                                break;
                            }
                        }
                    }
                }
            }
            // A one-car train has FirstVehicle == LastVehicle; anything the
            // walk missed at least gets its endpoint.
            if let Some(last_name) = ref_path(last, data) {
                if !seen.contains(last_name) {
                    ordered.push(last_name);
                }
            }
            let mut cars: Vec<Car<'a>> = Vec::new();
            for car_name in ordered {
                if let Some(entry) = car_entry(scan, car_name) {
                    if !claimed.contains(car_name) {
                        claimed.insert(car_name);
                        cars.push(entry);
                    }
                }
            }
            if !cars.is_empty() {
                let label =
                    text_property_value(find_prop(&train_object.properties, data, b"mTrainName"), data);
                trains.push(Consist { id: instance_name, label, cars });
            }
        }
    }
    for car_name in railcar_ids {
        if !claimed.contains(car_name) {
            if let Some(entry) = car_entry(scan, car_name) {
                trains.push(Consist { id: car_name, label: None, cars: vec![entry] });
            }
        }
    }
    trains
}

pub fn collect_trains(scan: &SaveScan) -> Value {
    // (label is None, label or "", entry) -- Python's sort key tuple.
    let mut consists: Vec<(bool, String, Value)> = Vec::new();
    for train in train_consists(scan) {
        let mut car_points: Vec<Value> = Vec::new();
        let mut car_ids: Vec<Value> = Vec::new();
        let mut car_kinds: Vec<Value> = Vec::new();
        for car in &train.cars {
            let [px, py] = project_xy(car.position[0], car.position[1]);
            car_points.push(jnum(px));
            car_points.push(jnum(py));
            car_points.push(jnum(rendered_yaw(car.rotation)));
            car_points.push(jnum(world_z_to_meters(car.position[2])));
            car_ids.push(Value::String(props::lossy(car.id)));
            car_kinds.push(Value::String(readable_label(&props::lossy(car.type_path))));
        }
        let lead_position = train.cars[0].position;
        let [pin_x, pin_y] = project_xy(lead_position[0], lead_position[1]);
        let label_value = match &train.label {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        };
        consists.push((
            train.label.is_none(),
            train.label.clone().unwrap_or_default(),
            json!({
                "id": props::lossy(train.id), "label": label_value,
                "pin": [jnum(pin_x), jnum(pin_y), jnum(world_z_to_meters(lead_position[2]))],
                "cars": {"points": car_points, "ids": car_ids, "kinds": car_kinds},
            }),
        ));
    }
    // consists.sort(key=(label is None, label or "")) -- stable, None last.
    consists.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    json!({
        "consists": consists.into_iter().map(|(_, _, v)| v).collect::<Vec<Value>>(),
        // Locomotive and Freight Car boxes are the same size -- one shared
        // footprint for the frontend's single train-cars bucket.
        "carFootprintPixels": super::simple::vehicle_footprint_pixels(LOCOMOTIVE_TYPE_PATH),
    })
}

// ---------------------------------------------------------------------------
// Progression (sav_map_data lines ~1583-1940)
// ---------------------------------------------------------------------------

/// In-game MAM tab names for schematics.json's researchTree tokens.
const RESEARCH_TREE_LABELS: [(&str, &str); 9] = [
    ("AlienOrganisms", "Alien Organisms"),
    ("AlienTech", "Alien Technology"),
    ("Caterium", "Caterium"),
    ("Mycelia", "Mycelia"),
    ("Nutrients", "Nutrients"),
    ("PowerSlugs", "Power Slugs"),
    ("Quartz", "Quartz"),
    ("Sulfur", "Sulfur"),
    ("XMas", "FICSMAS"),
];

fn research_tree_label(name: &str) -> &str {
    RESEARCH_TREE_LABELS
        .iter()
        .find(|(token, _)| *token == name)
        .map(|(_, label)| *label)
        .unwrap_or(name)
}

const HIDDEN_RESEARCH_TREES: [&str; 1] = ["HardDrive"];

const SCHEMATIC_MANAGER_TYPE_PATH_SUBSTRING: &str = "BP_SchematicManager_C";
const RESEARCH_MANAGER_TYPE_PATH_SUBSTRING: &str = "BP_ResearchManager_C";
const GAME_PHASE_MANAGER_TYPE_PATH_SUBSTRING: &str = "BP_GamePhaseManager_C";

/// _GAME_PHASES = generated gamePhases.json or the hand-written fallback
/// (the generated file is compiled in, so the fallback only fires if it was
/// generated empty).
fn game_phases() -> &'static Map<String, Value> {
    let generated = &gamedata::get().game_phases;
    if !generated.is_empty() {
        return generated;
    }
    static FALLBACK: OnceLock<Map<String, Value>> = OnceLock::new();
    FALLBACK.get_or_init(|| {
        let Value::Object(map) = json!({
            "GP_Project_Assembly_Phase_1": {"phaseNumber": 1, "displayName": "Distribution Platform", "cost": [
                {"item": "Desc_SpaceElevatorPart_1_C", "amount": 50}]},
            "GP_Project_Assembly_Phase_2": {"phaseNumber": 2, "displayName": "Construction Dock", "cost": [
                {"item": "Desc_SpaceElevatorPart_1_C", "amount": 1000},
                {"item": "Desc_SpaceElevatorPart_2_C", "amount": 1000},
                {"item": "Desc_SpaceElevatorPart_3_C", "amount": 100}]},
            "GP_Project_Assembly_Phase_3": {"phaseNumber": 3, "displayName": "Main Body", "cost": [
                {"item": "Desc_SpaceElevatorPart_2_C", "amount": 2500},
                {"item": "Desc_SpaceElevatorPart_4_C", "amount": 500},
                {"item": "Desc_SpaceElevatorPart_5_C", "amount": 100}]},
            "GP_Project_Assembly_Phase_4": {"phaseNumber": 4, "displayName": "Propulsion", "cost": [
                {"item": "Desc_SpaceElevatorPart_7_C", "amount": 500},
                {"item": "Desc_SpaceElevatorPart_6_C", "amount": 500},
                {"item": "Desc_SpaceElevatorPart_8_C", "amount": 250},
                {"item": "Desc_SpaceElevatorPart_9_C", "amount": 100}]},
            "GP_Project_Assembly_Phase_5": {"phaseNumber": 5, "displayName": "Assembly", "cost": [
                {"item": "Desc_SpaceElevatorPart_9_C", "amount": 1000},
                {"item": "Desc_SpaceElevatorPart_10_C", "amount": 1000},
                {"item": "Desc_SpaceElevatorPart_12_C", "amount": 256},
                {"item": "Desc_SpaceElevatorPart_11_C", "amount": 200}]},
        }) else {
            unreachable!()
        };
        map
    })
}

/// Python truthiness over a JSON value (`if entry.get(...)` idiom).
fn value_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// sav_map_data.recipeLabel.
pub fn recipe_label(recipe_path_name: &str) -> String {
    let entry = gamedata::get().recipes.get(short_class_name(recipe_path_name));
    if let Some(entry) = entry {
        // `if entry and entry.get("displayName")` -- both truthy.
        if value_truthy(Some(entry)) && value_truthy(entry.get("displayName")) {
            if let Some(display) = entry.get("displayName").and_then(Value::as_str) {
                return display.to_string();
            }
        }
    }
    let mut label = readable_label(recipe_path_name);
    for noise_prefix in ["Recipe, ", "Alternate, "] {
        if let Some(rest) = label.strip_prefix(noise_prefix) {
            label = rest.to_string();
        }
    }
    label
}

/// sav_map_data._humanizeShopCategory.
fn humanize_shop_category(short_class_name: &str) -> String {
    // Cases the generic camel-case splitter gets wrong.
    match short_class_name {
        "SC_RSS_Equipment2_C" => return "Equipment".to_string(),
        "SC_RSS_Massage-2ABb_C" => return "Massage-2ABb".to_string(),
        _ => {}
    }
    let mut name = short_class_name;
    if let Some(rest) = name.strip_prefix("SC_RSS_") {
        name = rest;
    }
    if let Some(rest) = name.strip_suffix("_C") {
        name = rest;
    }
    // re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", name): space at each ASCII
    // lower/digit -> upper boundary.
    let mut out = String::with_capacity(name.len() + 8);
    let mut prev: Option<char> = None;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if let Some(p) = prev {
                if p.is_ascii_lowercase() || p.is_ascii_digit() {
                    out.push(' ');
                }
            }
        }
        out.push(ch);
        prev = Some(ch);
    }
    out
}

/// sav_map_data._firstRecipeProductItem.
fn first_recipe_product_item(entry: &Value) -> Value {
    if let Some(Value::Array(recipe_class_names)) = entry.get("unlockRecipes") {
        for recipe_class_name in recipe_class_names {
            let recipe = recipe_class_name
                .as_str()
                .and_then(|name| gamedata::get().recipes.get(name));
            if let Some(recipe) = recipe {
                if !value_truthy(Some(recipe)) {
                    continue; // `if recipe:` -- an empty dict is falsy
                }
                if let Some(Value::Array(products)) = recipe.get("product") {
                    if let Some(product) = products.first() {
                        return product.get("item").cloned().unwrap_or(Value::Null);
                    }
                }
            }
        }
    }
    if let Some(Value::Array(given_items)) = entry.get("giveItems") {
        if let Some(given_item) = given_items.first() {
            return given_item.get("item").cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// sav_map_data._findObjectByTypePathSubstring.
fn find_object_by_type_path_substring<'a>(
    scan: &SaveScan<'a>,
    substring: &str,
) -> Option<&'a Object> {
    let data = scan.data();
    let needle = substring.as_bytes();
    for level in &scan.store.levels {
        for (oi, header) in level.headers.iter().enumerate() {
            // object.instanceName == the index-aligned header's instanceName.
            if find_subslice(header.instance_name().bytes(data), needle) {
                return Some(&level.parsed_objects()[oi]);
            }
        }
    }
    // Fallback: match the ActorHeader's typePath and resolve that header's
    // instanceName (typePath buckets are keyed in first-encounter order).
    for (type_path, seq_headers) in &scan.actor_seqs_by_type_path {
        if find_subslice(type_path, needle) {
            let name = scan.header(seq_headers[0].1).instance_name().bytes(data);
            return scan.object_by_name(name);
        }
    }
    None
}

/// sav_map_data._shortNamesFromObjectReferenceList (the input is always an
/// ArrayProperty of ObjectReferences or missing).
fn short_names_from_object_reference_list<'a>(
    value: Option<&'a PropertyValue>,
    data: &'a [u8],
) -> HashSet<&'a [u8]> {
    let mut short_names: HashSet<&[u8]> = HashSet::new();
    if let Some(PropertyValue::Array(ArrayValue::Refs(references))) = value {
        for reference in references {
            let path_name = reference.path_name.bytes(data);
            if !path_name.is_empty() {
                short_names.insert(props::short_name(path_name));
            }
        }
    }
    short_names
}

/// sav_map_data._labeledCost.
fn labeled_cost(cost_entries: Option<&Value>) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Some(Value::Array(entries)) = cost_entries {
        for cost in entries {
            let item = cost.get("item").cloned().unwrap_or(Value::Null);
            let label = readable_label(item.as_str().unwrap_or(""));
            let amount = cost.get("amount").cloned().unwrap_or(Value::Null);
            out.push(json!({"item": item, "label": label, "amount": amount}));
        }
    }
    Value::Array(out)
}

/// A grouped schematic entry: the output dict (menuPriority already
/// excluded, matching Python's pop-during-sort) plus the sort/count fields.
struct SchematicEntry {
    value: Value,
    menu_priority: f64,
    label: String,
    done: bool,
}

fn sort_by_priority_then_label(entries: &mut [SchematicEntry]) {
    // list.sort(key=(menuPriority, label)) -- stable.
    entries.sort_by(|a, b| {
        a.menu_priority
            .partial_cmp(&b.menu_priority)
            .expect("menuPriority NaN")
            .then_with(|| a.label.cmp(&b.label))
    });
}

fn done_count(entries: &[SchematicEntry]) -> usize {
    entries.iter().filter(|e| e.done).count()
}

pub fn collect_progression(scan: &SaveScan) -> Value {
    let data = scan.data();

    let schematic_manager =
        find_object_by_type_path_substring(scan, SCHEMATIC_MANAGER_TYPE_PATH_SUBSTRING);
    let purchased: HashSet<&[u8]> = match schematic_manager {
        Some(object) => short_names_from_object_reference_list(
            find_prop(&object.properties, data, b"mPurchasedSchematics"),
            data,
        ),
        None => HashSet::new(),
    };

    let research_manager =
        find_object_by_type_path_substring(scan, RESEARCH_MANAGER_TYPE_PATH_SUBSTRING);
    let unlocked_trees: HashSet<String> = match research_manager {
        Some(object) => short_names_from_object_reference_list(
            find_prop(&object.properties, data, b"mUnlockedResearchTrees"),
            data,
        )
        .into_iter()
        .map(|name| props::lossy(name).replace("BPD_ResearchTree_", "").replace("_C", ""))
        .collect(),
        None => HashSet::new(),
    };

    let mut nodes_by_tree: IndexMap<String, Vec<SchematicEntry>> = IndexMap::new();
    let mut alternate_recipes: Vec<(f64, String, Value)> = Vec::new(); // (techTier, label, entry)
    let mut shop_by_category: IndexMap<String, Vec<SchematicEntry>> = IndexMap::new();
    let mut coupons_spent: i64 = 0;
    let mut milestones_by_tier: IndexMap<i64, Vec<SchematicEntry>> = IndexMap::new();

    for (class_name, entry) in &gamedata::get().schematics {
        let schematic_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        let done = purchased.contains(class_name.as_bytes());
        let display_name = entry.get("displayName").and_then(Value::as_str).unwrap_or("");
        // Legacy research nodes with a literal "Discontinued - " display name.
        if display_name.starts_with("Discontinued") {
            continue;
        }
        let label_value = entry.get("displayName").cloned().unwrap_or(Value::Null);
        let menu_priority = entry.get("menuPriority").and_then(Value::as_f64).unwrap_or(0.0);
        match schematic_type {
            "MAM" => {
                let tree = entry.get("researchTree").and_then(Value::as_str).unwrap_or("");
                if HIDDEN_RESEARCH_TREES.contains(&tree) {
                    continue;
                }
                nodes_by_tree.entry(tree.to_string()).or_default().push(SchematicEntry {
                    value: json!({
                        "className": class_name, "label": label_value, "done": done,
                        "cost": labeled_cost(entry.get("cost")),
                    }),
                    menu_priority,
                    label: display_name.to_string(),
                    done,
                });
            }
            "Alternate" => {
                // Only hard-drive alternates that actually unlock a recipe.
                let product_item = first_recipe_product_item(entry);
                if !value_truthy(entry.get("unlockRecipes")) {
                    continue;
                }
                let tech_tier = entry.get("techTier").cloned().unwrap_or(json!(0));
                let tech_tier_key = tech_tier.as_f64().unwrap_or(0.0);
                alternate_recipes.push((
                    tech_tier_key,
                    display_name.to_string(),
                    json!({
                        "className": class_name, "label": label_value, "done": done,
                        "techTier": tech_tier, "productItem": product_item,
                    }),
                ));
            }
            "ResourceSink" => {
                // Repeatable item bundles are never recorded in
                // mPurchasedSchematics -- only one-time unlocks belong here.
                if value_truthy(entry.get("giveItems")) && !value_truthy(entry.get("unlockRecipes"))
                {
                    continue;
                }
                let mut coupon_cost: i64 = 0;
                if let Some(Value::Array(costs)) = entry.get("cost") {
                    for cost in costs {
                        if cost.get("item").and_then(Value::as_str)
                            == Some("Desc_ResourceSinkCoupon_C")
                        {
                            // int(cost.get("amount", 0)) -- truncation.
                            coupon_cost =
                                cost.get("amount").and_then(Value::as_f64).unwrap_or(0.0).trunc()
                                    as i64;
                        }
                    }
                }
                if done {
                    coupons_spent += coupon_cost;
                }
                // categories = entry.get("shopCategories") or [None]
                let first_category: Option<&str> = match entry.get("shopCategories") {
                    Some(Value::Array(categories)) if !categories.is_empty() => {
                        categories[0].as_str()
                    }
                    _ => None,
                };
                let category_label = match first_category {
                    Some(category) if !category.is_empty() => humanize_shop_category(category),
                    _ => "Other".to_string(),
                };
                shop_by_category.entry(category_label).or_default().push(SchematicEntry {
                    value: json!({
                        "className": class_name, "label": label_value, "done": done,
                        "couponCost": coupon_cost, "productItem": first_recipe_product_item(entry),
                    }),
                    menu_priority,
                    label: display_name.to_string(),
                    done,
                });
            }
            "Milestone" | "Tutorial" => {
                let tier = entry.get("techTier").and_then(Value::as_i64).unwrap_or(0);
                milestones_by_tier.entry(tier).or_default().push(SchematicEntry {
                    value: json!({
                        "className": class_name, "label": label_value, "done": done,
                        "cost": labeled_cost(entry.get("cost")),
                    }),
                    menu_priority,
                    label: display_name.to_string(),
                    done,
                });
            }
            _ => {}
        }
    }

    // -- MAM research, grouped by tree, sorted by tab label -------------------
    let mut tree_keys: Vec<String> = nodes_by_tree.keys().cloned().collect();
    tree_keys.sort_by(|a, b| research_tree_label(a).cmp(research_tree_label(b))); // stable
    let mut mam_trees: Vec<Value> = Vec::new();
    for tree in tree_keys {
        let mut nodes = nodes_by_tree.shift_remove(&tree).unwrap();
        sort_by_priority_then_label(&mut nodes);
        mam_trees.push(json!({
            "tree": tree,
            "label": research_tree_label(&tree),
            "treeUnlocked": unlocked_trees.contains(&tree),
            "doneCount": done_count(&nodes),
            "nodes": nodes.into_iter().map(|n| n.value).collect::<Vec<Value>>(),
        }));
    }

    // -- Alternate recipes, sorted by (techTier, label) -----------------------
    alternate_recipes.sort_by(|a, b| {
        a.0.partial_cmp(&b.0).expect("techTier NaN").then_with(|| a.1.cmp(&b.1))
    });
    let alternate_recipes: Vec<Value> =
        alternate_recipes.into_iter().map(|(_, _, v)| v).collect();

    // -- AWESOME Shop, grouped by tab, sorted by tab label --------------------
    let mut category_keys: Vec<String> = shop_by_category.keys().cloned().collect();
    category_keys.sort();
    let mut shop_categories: Vec<Value> = Vec::new();
    for category_label in category_keys {
        let mut entries = shop_by_category.shift_remove(&category_label).unwrap();
        sort_by_priority_then_label(&mut entries);
        shop_categories.push(json!({
            "label": category_label,
            "doneCount": done_count(&entries),
            "entries": entries.into_iter().map(|e| e.value).collect::<Vec<Value>>(),
        }));
    }

    // -- HUB milestones, grouped by tier --------------------------------------
    let mut tiers: Vec<i64> = milestones_by_tier.keys().copied().collect();
    tiers.sort();
    let mut hub_tiers: Vec<Value> = Vec::new();
    for tier in tiers {
        let mut milestones = milestones_by_tier.shift_remove(&tier).unwrap();
        sort_by_priority_then_label(&mut milestones);
        hub_tiers.push(json!({
            "tier": tier,
            "label": if tier == 0 { "HUB Upgrades".to_string() } else { format!("Tier {}", tier) },
            "doneCount": done_count(&milestones),
            "milestones": milestones.into_iter().map(|m| m.value).collect::<Vec<Value>>(),
        }));
    }

    json!({
        "mamTrees": mam_trees,
        "alternateRecipes": alternate_recipes,
        "shopCategories": shop_categories,
        "couponsSpent": coupons_spent,
        "hubTiers": hub_tiers,
        "spaceElevator": collect_space_elevator_state(scan),
    })
}

// ---------------------------------------------------------------------------
// Space Elevator (sav_map_data lines ~1864-1940)
// ---------------------------------------------------------------------------

struct PhaseInfo {
    asset_name: String,
    phase_number: Value,
    name: Value,
    cost: Vec<Value>,
}

/// re.search(r"Phase_(\d+)$", assetName): a trailing digit run immediately
/// preceded by "Phase_".
fn phase_number_from_asset_name(asset_name: &str) -> Option<i64> {
    let bytes = asset_name.as_bytes();
    let mut digits_start = bytes.len();
    while digits_start > 0 && bytes[digits_start - 1].is_ascii_digit() {
        digits_start -= 1;
    }
    if digits_start == bytes.len() || !asset_name[..digits_start].ends_with("Phase_") {
        return None;
    }
    asset_name[digits_start..].parse::<i64>().ok()
}

/// sav_map_data._phaseInfo.
fn phase_info(reference: Option<&ObjectRef>, data: &[u8]) -> Option<PhaseInfo> {
    let path_name = ref_path(reference, data)?;
    let asset_name = props::lossy(props::short_name(path_name));
    if let Some(entry) = game_phases().get(&asset_name) {
        return Some(PhaseInfo {
            phase_number: entry.get("phaseNumber").cloned().unwrap_or(Value::Null),
            name: entry.get("displayName").cloned().unwrap_or(Value::Null),
            cost: match entry.get("cost") {
                Some(Value::Array(cost)) => cost.clone(),
                _ => Vec::new(),
            },
            asset_name,
        });
    }
    let phase_number = match phase_number_from_asset_name(&asset_name) {
        Some(number) => json!(number),
        None => Value::Null,
    };
    Some(PhaseInfo { phase_number, name: Value::Null, cost: Vec::new(), asset_name })
}

/// The stripped {assetName, phaseNumber, name} dict (Python builds the dict
/// with "cost" and removes it before output, keeping this key order).
fn phase_value(phase: &Option<PhaseInfo>) -> Value {
    match phase {
        Some(p) => json!({
            "assetName": p.asset_name, "phaseNumber": p.phase_number, "name": p.name,
        }),
        None => Value::Null,
    }
}

/// sav_map_data._collectSpaceElevatorState.
fn collect_space_elevator_state(scan: &SaveScan) -> Value {
    let data = scan.data();
    let space_elevator_built = !scan.actor_slots_of_type(&[SPACE_ELEVATOR_TYPE_PATH]).is_empty();

    let Some(phase_manager) =
        find_object_by_type_path_substring(scan, GAME_PHASE_MANAGER_TYPE_PATH_SUBSTRING)
    else {
        return json!({
            "built": space_elevator_built, "gameCompleted": false, "currentPhase": null,
            "targetPhase": null, "costMultiplier": 1.0, "targetCost": [],
        });
    };

    let properties = &phase_manager.properties;
    let current_phase = phase_info(props::object_ref(properties, data, b"mCurrentGamePhase"), data);
    let target_phase = phase_info(props::object_ref(properties, data, b"mTargetGamePhase"), data);
    // bool(getPropertyValue(...)): missing -> False.
    let game_completed = props::boolean(properties, data, b"mIsGameCompleted").unwrap_or(false);

    // {itemShortName: amount already delivered toward the TARGET phase}.
    let mut paid_off_by_item: IndexMap<String, i64> = IndexMap::new();
    if let Some(paid_off_costs) =
        props::array_structs(properties, data, b"mTargetGamePhasePaidOffCosts")
    {
        for paid_off_entry in paid_off_costs {
            let item_class = props::object_ref(paid_off_entry, data, b"ItemClass");
            let amount: Option<i64> = match find_prop(paid_off_entry, data, b"Amount") {
                Some(PropertyValue::Int(n)) => Some(*n as i64),
                Some(PropertyValue::Int64(n)) => Some(*n),
                _ => None,
            };
            if let (Some(path_name), Some(amount)) = (ref_path(item_class, data), amount) {
                // Python dict: last value wins, first position kept.
                paid_off_by_item.insert(props::lossy(props::short_name(path_name)), amount);
            }
        }
    }

    // BP_GameState_C's mSpacePartsCostMultiplier; absent when left at 1.0.
    let mut cost_multiplier: f64 = 1.0;
    for &slot in &scan.game_state_objects {
        // save order; last value wins, same as the old full scan
        if let Some(multiplier) =
            props::float(&scan.object(slot).properties, data, b"mSpacePartsCostMultiplier")
        {
            cost_multiplier = multiplier;
        }
    }

    // One row per required part of the target phase, overlaid with delivered
    // amounts; delivered items the static table doesn't know still get rows.
    let mut target_cost: Vec<Value> = Vec::new();
    let mut known_items: HashSet<String> = HashSet::new();
    if let Some(target) = &target_phase {
        for cost in &target.cost {
            let item_short_name = cost.get("item").and_then(Value::as_str).unwrap_or("");
            known_items.insert(item_short_name.to_string());
            let amount = cost.get("amount").and_then(Value::as_f64).unwrap_or(0.0);
            // round(x) with no ndigits: banker's rounding to an int.
            let required = py_round(amount * cost_multiplier, 0) as i64;
            target_cost.push(json!({
                "item": item_short_name, "label": readable_label(item_short_name),
                "required": required,
                "imported": paid_off_by_item.get(item_short_name).copied().unwrap_or(0),
            }));
        }
    }
    for (item_short_name, amount) in &paid_off_by_item {
        if !known_items.contains(item_short_name) {
            target_cost.push(json!({
                "item": item_short_name, "label": readable_label(item_short_name),
                "required": null, "imported": amount,
            }));
        }
    }

    json!({
        "built": space_elevator_built,
        "gameCompleted": game_completed,
        "currentPhase": phase_value(&current_phase),
        "targetPhase": phase_value(&target_phase),
        "costMultiplier": jnum(cost_multiplier),
        "targetCost": target_cost,
    })
}
