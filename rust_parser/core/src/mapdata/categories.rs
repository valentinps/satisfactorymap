//! Build-menu category tree -- port of sav_map_data._loadBuildMenuCategories
//! (lines ~115-185) over the embedded buildingCategories.json /
//! categoryLabels.json / categoryOverrides.json.

use super::consts::{OTHER_CATEGORY, TOP_CATEGORY_ORDER_GUESS};
use super::geometry::short_class_name;
use crate::gamedata;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::OnceLock;

pub struct Categories {
    /// short ClassName -> (category display label, subcategory display label).
    pub classname_to_catsub: IndexMap<String, (String, String)>,
    /// The ordered [{"category", "subcategories"}] tree the frontend renders.
    pub menu_order: Value,
}

fn str_map<'v>(value: Option<&'v Value>) -> IndexMap<&'v str, &'v str> {
    match value {
        Some(Value::Object(map)) => {
            map.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s))).collect()
        }
        _ => IndexMap::new(),
    }
}

pub fn get() -> &'static Categories {
    static CATS: OnceLock<Categories> = OnceLock::new();
    CATS.get_or_init(|| {
        let data = gamedata::get();
        let building_categories = &data.building_categories;
        let labels = &data.category_labels;
        let overrides = &data.category_overrides;

        let top_labels_base = str_map(labels.get("topCategories"));
        let sub_labels = str_map(labels.get("subCategories"));
        let subcategory_overrides = str_map(overrides.get("subcategoryOverrides"));
        // topLabels.update(overrides["topCategoryLabels"]): override wins.
        let mut top_labels = top_labels_base;
        for (k, v) in str_map(overrides.get("topCategoryLabels")) {
            top_labels.insert(k, v);
        }

        let mut classname_to_catsub: IndexMap<String, (String, String)> = IndexMap::new();
        // subInternal -> (topInternal, subLabel, best/lowest menuPriority).
        let mut subcategory_info: IndexMap<&str, (&str, &str, f64)> = IndexMap::new();
        for (class_name, entry) in building_categories {
            let top_internal_raw = entry.get("topCategory").and_then(Value::as_str).unwrap_or("");
            let sub_internal = entry.get("subCategory").and_then(Value::as_str).unwrap_or("");
            let top_internal =
                subcategory_overrides.get(sub_internal).copied().unwrap_or(top_internal_raw);
            let top_label = top_labels.get(top_internal).copied().unwrap_or(top_internal);
            let sub_label = sub_labels.get(sub_internal).copied().unwrap_or(sub_internal);
            classname_to_catsub
                .insert(class_name.clone(), (top_label.to_string(), sub_label.to_string()));
            let priority = entry.get("menuPriority").and_then(Value::as_f64).unwrap_or(0.0);
            match subcategory_info.get(sub_internal) {
                Some(&(_, _, existing)) if priority >= existing => {}
                _ => {
                    subcategory_info.insert(sub_internal, (top_internal, sub_label, priority));
                }
            }
        }

        let mut subs_by_top: IndexMap<&str, Vec<(f64, &str)>> = IndexMap::new();
        for &(top_internal, sub_label, priority) in subcategory_info.values() {
            subs_by_top.entry(top_internal).or_default().push((priority, sub_label));
        }
        // topOrder: the guess list, then every remaining top id sorted.
        let mut top_order: Vec<&str> = TOP_CATEGORY_ORDER_GUESS.to_vec();
        let mut rest: Vec<&str> = subs_by_top
            .keys()
            .copied()
            .filter(|k| !TOP_CATEGORY_ORDER_GUESS.contains(k))
            .collect();
        rest.sort_unstable();
        rest.dedup();
        top_order.extend(rest);

        let mut menu_order: Vec<Value> = Vec::new();
        for top_internal in top_order {
            let Some(subs) = subs_by_top.get_mut(top_internal) else { continue };
            if subs.is_empty() {
                continue;
            }
            subs.sort_by(|a, b| {
                a.0.partial_cmp(&b.0).unwrap().then_with(|| a.1.cmp(b.1))
            });
            menu_order.push(serde_json::json!({
                "category": top_labels.get(top_internal).copied().unwrap_or(top_internal),
                "subcategories": subs.iter().map(|(_, label)| *label).collect::<Vec<_>>(),
            }));
        }

        Categories { classname_to_catsub, menu_order: Value::Array(menu_order) }
    })
}

/// sav_map_data.categorizeTypePath.
pub fn categorize_type_path(type_path: &str) -> &'static str {
    match get().classname_to_catsub.get(short_class_name(type_path)) {
        Some((category, _)) => category,
        None => OTHER_CATEGORY,
    }
}

/// sav_map_data.categorizeSubcategory (None -> empty option).
pub fn categorize_subcategory(type_path: &str) -> Option<&'static str> {
    get().classname_to_catsub.get(short_class_name(type_path)).map(|(_, sub)| sub.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_load() {
        let cats = get();
        assert!(!cats.classname_to_catsub.is_empty());
        let menu = cats.menu_order.as_array().unwrap();
        assert!(!menu.is_empty());
        // Every menu entry has the two keys in order.
        let first = menu[0].as_object().unwrap();
        let keys: Vec<&str> = first.keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["category", "subcategories"]);
    }
}
