//! Embedded static game data: the generated tables from game_data/ compiled
//! into the binary via include_str! and parsed once on first use. Single
//! source of truth stays in the repo's game_data/ directory; compiling this
//! crate therefore requires game_data extracted (same prerequisite the app
//! itself has always had).
//!
//! - game_data/sav_data/*.json: world data converted from the parser
//!   submodule's Python literals by game_data/extract_sav_data_tables.py
//!   (committed; key order preserved from the Python dicts).
//! - game_data/generated/*.json + game_data/category*.json: extracted from
//!   the game's Docs.json by game_data/extract_docs_json.py (gitignored,
//!   regenerable; documented in game_data/SCHEMA.md).

use indexmap::IndexMap;
use serde::Deserialize;
use std::sync::OnceLock;

/// {pathName: (descClass, purityName, (x,y,z), coreName|null)}
pub type ResourcePurityMap = IndexMap<String, (String, String, [f64; 3], Option<String>)>;

/// {pathName: (x,y,z)}
pub type SlugMap = IndexMap<String, [f64; 3]>;

/// {pathName: (id, quat, pos, metadata)} -- somersloops / mercer spheres /
/// crash sites all share this shape; metadata is heterogeneous.
pub type CollectibleMap =
    IndexMap<String, (String, [f64; 4], [f64; 3], serde_json::Map<String, serde_json::Value>)>;

#[derive(Deserialize)]
pub struct PowerSlugs {
    pub blue: SlugMap,
    pub yellow: SlugMap,
    pub purple: SlugMap,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypePaths {
    pub conveyor_belts: Vec<String>,
    pub miners: Vec<String>,
    pub mined_resources: Vec<String>,
    pub power_line: Vec<String>,
    pub crash_site: String,
}

pub struct GameData {
    // -- game_data/sav_data/ (converted Python literals) --
    pub resource_purity: ResourcePurityMap,
    pub power_slugs: PowerSlugs,
    pub somersloops: CollectibleMap,
    pub mercer_spheres: CollectibleMap,
    pub crash_sites: CollectibleMap,
    /// {itemFullPath: [(count, (x,y,z), instanceName)]}
    pub free_dropped_items: IndexMap<String, Vec<(i64, [f64; 3], String)>>,
    pub readable_name_corrections: IndexMap<String, String>,
    pub type_paths: TypePaths,

    // -- game_data/generated/ + game_data/ (Docs.json extracts) --
    // Kept as ordered JSON maps: consumers pick the fields they need, and
    // iteration order must match Python's json.load dict order.
    pub building_categories: serde_json::Map<String, serde_json::Value>,
    pub category_labels: serde_json::Map<String, serde_json::Value>,
    pub category_overrides: serde_json::Map<String, serde_json::Value>,
    pub buildings: serde_json::Map<String, serde_json::Value>,
    pub items: serde_json::Map<String, serde_json::Value>,
    pub recipes: serde_json::Map<String, serde_json::Value>,
    pub schematics: serde_json::Map<String, serde_json::Value>,
    pub game_phases: serde_json::Map<String, serde_json::Value>,
}

macro_rules! embed {
    ($path:literal) => {
        include_str!(concat!("../../../../game_data/", $path))
    };
}

fn parse<T: serde::de::DeserializeOwned>(name: &str, s: &str) -> T {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("embedded {} is invalid: {}", name, e))
}

/// The embedded tables, parsed on first use.
pub fn get() -> &'static GameData {
    static DATA: OnceLock<GameData> = OnceLock::new();
    DATA.get_or_init(|| GameData {
        resource_purity: parse("resourcePurity.json", embed!("sav_data/resourcePurity.json")),
        power_slugs: parse("powerSlugs.json", embed!("sav_data/powerSlugs.json")),
        somersloops: parse("somersloops.json", embed!("sav_data/somersloops.json")),
        mercer_spheres: parse("mercerSpheres.json", embed!("sav_data/mercerSpheres.json")),
        crash_sites: parse("crashSites.json", embed!("sav_data/crashSites.json")),
        free_dropped_items: parse(
            "freeDroppedItems.json",
            embed!("sav_data/freeDroppedItems.json"),
        ),
        readable_name_corrections: parse(
            "readableNameCorrections.json",
            embed!("sav_data/readableNameCorrections.json"),
        ),
        type_paths: parse("typePaths.json", embed!("sav_data/typePaths.json")),
        building_categories: parse(
            "buildingCategories.json",
            embed!("generated/buildingCategories.json"),
        ),
        category_labels: parse("categoryLabels.json", embed!("categoryLabels.json")),
        category_overrides: parse("categoryOverrides.json", embed!("categoryOverrides.json")),
        buildings: parse("buildings.json", embed!("generated/buildings.json")),
        items: parse("items.json", embed!("generated/items.json")),
        recipes: parse("recipes.json", embed!("generated/recipes.json")),
        schematics: parse("schematics.json", embed!("generated/schematics.json")),
        game_phases: parse("gamePhases.json", embed!("generated/gamePhases.json")),
    })
}

mod item_icons {
    include!(concat!(env!("OUT_DIR"), "/item_icon_stems.rs"));
}

/// Whether icons/items/<stem>.png was extracted (compile-time snapshot of
/// the icons dir -- see build.rs; replaces Python's per-call
/// os.path.exists).
pub fn has_item_icon(stem: &str) -> bool {
    item_icons::ITEM_ICON_STEMS.binary_search(&stem).is_ok()
}

impl crate::object::ClassTables {
    /// ClassTables from the embedded tables (the Python binding instead
    /// receives them from sav_data at call time; both originate from
    /// sav_data.data.CONVEYOR_BELTS).
    pub fn embedded() -> Self {
        crate::object::ClassTables { conveyor_belts: get().type_paths.conveyor_belts.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tables_parse_and_have_expected_sizes() {
        let d = get();
        assert_eq!(d.readable_name_corrections.len(), 935);
        assert_eq!(d.type_paths.conveyor_belts.len(), 12);
        assert_eq!(d.type_paths.miners.len(), 7);
        assert_eq!(d.type_paths.mined_resources.len(), 4);
        assert_eq!(d.type_paths.power_line.len(), 2);
        assert!(d.type_paths.crash_site.contains("BP_DropPod"));
        assert!(d.resource_purity.len() > 500, "resource purity {}", d.resource_purity.len());
        assert!(d.power_slugs.blue.len() > 100);
        assert!(d.power_slugs.yellow.len() > 100);
        assert!(d.power_slugs.purple.len() > 50);
        assert!(d.somersloops.len() > 80, "somersloops {}", d.somersloops.len());
        assert!(d.mercer_spheres.len() > 200);
        assert!(d.crash_sites.len() > 90);
        assert!(d.free_dropped_items.len() >= 50);
        assert!(d.buildings.len() > 400);
        assert!(d.recipes.len() > 300);
        assert!(d.schematics.len() > 200);
        assert!(!d.game_phases.is_empty());
        // Purity names are enum names.
        let (_, purity, _, _) = d.resource_purity.values().next().unwrap();
        assert!(["UNKNOWN", "IMPURE", "NORMAL", "PURE"].contains(&purity.as_str()));
    }

    #[test]
    fn class_tables_embedded_matches_sav_data() {
        let t = crate::object::ClassTables::embedded();
        assert!(t.conveyor_belts.iter().all(|p| p.contains("Conveyor")));
    }
}
