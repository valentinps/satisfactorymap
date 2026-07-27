//! Save class names -> Modeler node/item names.
//!
//! Modeler is closed-source and ships no name table, so this is derived from
//! `game_data` and pinned by a test that walks every node name and input key
//! in the `.sfmd` sample files. A wrong name makes Modeler reject the file
//! outright, so that test is the hard gate on this module.
//!
//! The rule is simple: a node's name is the recipe's `displayName` with the
//! `"Alternate: "` prefix stripped. Everything that does not follow it is
//! enumerated in the override tables below -- checked exhaustively against
//! the samples, not guessed.

use crate::gamedata;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Recipes whose Modeler name differs from `displayName` minus `"Alternate: "`.
/// Modeler uses the singular; the game pluralises screws.
const RECIPE_OVERRIDES: [(&str, &str); 3] = [
    ("Recipe_Screw_C", "Screw"),               // game: "Screws"
    ("Recipe_Alternate_Screw_C", "Cast Screw"), // game: "Alternate: Cast Screws"
    ("Recipe_Alternate_Screw_2_C", "Steel Screw"), // game: "Alternate: Steel Screws"
];

/// Items whose Modeler name differs from `displayName`.
const ITEM_OVERRIDES: [(&str, &str); 1] = [("Desc_IronScrew_C", "Screw")]; // game: "Screws"

/// Recipes that exist in `recipes.json` but are not automatable, so they must
/// never shadow a real node name. `Recipe_AlienPowerBuilding_C`'s displayName
/// is "Alien Power Augmenter" -- the same string as the *power building*
/// node -- so without this filter a build-gun recipe would win the lookup.
const NON_AUTOMATED_PRODUCERS: [&str; 7] = [
    "BP_BuildGun_C",
    "FGBuildGun",
    "BP_WorkBenchComponent_C",
    "BP_WorkshopComponent_C",
    "FGBuildableAutomatedWorkBench",
    "Build_AutomatedWorkBench_C",
    "Desc_AutomatedWorkBench_C",
];

/// Node names Modeler provides that have no game recipe behind them.
pub mod virtual_nodes {
    pub const AWESOME_SINK: &str = "AWESOME Sink";
    pub const STORAGE_CONTAINER: &str = "Storage Container";
    pub const DIMENSIONAL_DEPOT: &str = "Dimensional Depot";
    pub const OUTPOST: &str = "Outpost";
    pub const PRIORITY_SPLITTER: &str = "Priority Splitter";
    pub const PRIORITY_MERGER: &str = "Priority Merger";
    pub const PRIORITY_SPLURGER: &str = "Priority Splurger";
    pub const ALIEN_POWER_AUGMENTER: &str = "Alien Power Augmenter";
}

/// Storage Container `Capacity` values. Omitting the field means
/// "Partially Full", so that variant carries no string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capacity {
    PartiallyFull,
    Full,
    Empty,
    InputEqualsOutput,
}

impl Capacity {
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Capacity::PartiallyFull => None,
            Capacity::Full => Some("Full"),
            Capacity::Empty => Some("Empty"),
            Capacity::InputEqualsOutput => Some("Input = Output"),
        }
    }
}

/// Generators: the Modeler node name depends on the *fuel*, not the building.
/// `Build_GeneratorFuel_C` alone fans out to five nodes. Keyed by
/// `(buildingShortClass, fuelItemShortClass)`.
///
/// `Build_GeneratorBiomass_Automated_C` is deliberately absent -- Modeler has
/// no biomass node, so those generators are dropped and reported.
const GENERATOR_NODES: [(&str, &str, &str); 12] = [
    ("Build_GeneratorCoal_C", "Desc_Coal_C", "Coal Generator"),
    ("Build_GeneratorCoal_C", "Desc_CompactedCoal_C", "Compacted Coal Generator"),
    ("Build_GeneratorCoal_C", "Desc_PetroleumCoke_C", "Petroleum Coke Generator"),
    ("Build_GeneratorFuel_C", "Desc_LiquidFuel_C", "Fuel Generator"),
    ("Build_GeneratorFuel_C", "Desc_LiquidTurboFuel_C", "Turbofuel Generator"),
    ("Build_GeneratorFuel_C", "Desc_RocketFuel_C", "Rocket Fuel Generator"),
    ("Build_GeneratorFuel_C", "Desc_IonizedFuel_C", "Ionized Fuel Generator"),
    ("Build_GeneratorFuel_C", "Desc_LiquidBiofuel_C", "Liquid Biofuel Generator"),
    ("Build_GeneratorNuclear_C", "Desc_NuclearFuelRod_C", "Uranium Nuclear Power Plant"),
    ("Build_GeneratorNuclear_C", "Desc_PlutoniumFuelRod_C", "Plutonium Nuclear Power Plant"),
    ("Build_GeneratorNuclear_C", "Desc_FicsoniumFuelRod_C", "Ficsonium Nuclear Power Plant"),
    // No fuel: the geyser supplies it. Matched on the building alone.
    ("Build_GeneratorGeoThermal_C", "", "Geothermal Generator"),
];

pub struct NameTable {
    /// `Recipe_X_C` -> Modeler node name (automatable recipes only).
    recipe_nodes: HashMap<String, String>,
    /// `Desc_X_C` -> Modeler item name.
    item_names: HashMap<String, String>,
    /// Every name Modeler is known to accept, for the validity gate.
    known: HashMap<String, ()>,
}

impl NameTable {
    /// Modeler node name for a machine's `mCurrentRecipe`, or `None` when the
    /// recipe is not automatable (and so has no node).
    pub fn recipe_node(&self, recipe_short_class: &str) -> Option<&str> {
        self.recipe_nodes.get(recipe_short_class).map(String::as_str)
    }

    /// Modeler item name for an item descriptor, used as an `Inputs` key.
    pub fn item(&self, item_short_class: &str) -> Option<&str> {
        self.item_names.get(item_short_class).map(String::as_str)
    }

    /// Raw-resource node name. Identical to `item()` -- Modeler names an
    /// extraction node after the resource it yields -- but kept separate
    /// because the two are semantically different nodes (`Max` is items/min
    /// here, a building count everywhere else).
    pub fn resource_node(&self, item_short_class: &str) -> Option<&str> {
        self.item(item_short_class)
    }

    pub fn generator_node(
        &self,
        building_short_class: &str,
        fuel_short_class: &str,
    ) -> Option<&'static str> {
        GENERATOR_NODES
            .iter()
            .find(|(building, fuel, _)| {
                *building == building_short_class && (fuel.is_empty() || *fuel == fuel_short_class)
            })
            .map(|(_, _, node)| *node)
    }

    /// Whether Modeler will accept this node name. The emitter asserts on
    /// this so a mapping bug fails our tests instead of Modeler's parser.
    pub fn is_known_node(&self, name: &str) -> bool {
        self.known.contains_key(name)
    }
}

pub fn table() -> &'static NameTable {
    static TABLE: OnceLock<NameTable> = OnceLock::new();
    TABLE.get_or_init(build)
}

fn build() -> NameTable {
    let gd = gamedata::get();

    let mut item_names: HashMap<String, String> = HashMap::new();
    for source in [&gd.items, &gd.resources] {
        for (short_class, value) in source.iter() {
            if let Some(display) = value.get("displayName").and_then(|v| v.as_str()) {
                item_names.insert(short_class.clone(), display.to_string());
            }
        }
    }
    for (short_class, name) in ITEM_OVERRIDES {
        item_names.insert(short_class.to_string(), name.to_string());
    }

    let mut recipe_nodes: HashMap<String, String> = HashMap::new();
    for (short_class, value) in gd.recipes.iter() {
        let produced_in: Vec<&str> = value
            .get("producedIn")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        // No producer at all, or only hand-craft producers: not a node.
        if produced_in.is_empty()
            || produced_in.iter().all(|p| NON_AUTOMATED_PRODUCERS.contains(p))
        {
            continue;
        }
        let Some(display) = value.get("displayName").and_then(|v| v.as_str()) else { continue };
        let name = display.strip_prefix("Alternate: ").unwrap_or(display);
        recipe_nodes.insert(short_class.clone(), name.to_string());
    }
    for (short_class, name) in RECIPE_OVERRIDES {
        recipe_nodes.insert(short_class.to_string(), name.to_string());
    }

    let mut known: HashMap<String, ()> = HashMap::new();
    for name in recipe_nodes.values().chain(item_names.values()) {
        known.insert(name.clone(), ());
    }
    for (_, _, node) in GENERATOR_NODES {
        known.insert(node.to_string(), ());
    }
    for name in [
        virtual_nodes::AWESOME_SINK,
        virtual_nodes::STORAGE_CONTAINER,
        virtual_nodes::DIMENSIONAL_DEPOT,
        virtual_nodes::OUTPOST,
        virtual_nodes::PRIORITY_SPLITTER,
        virtual_nodes::PRIORITY_MERGER,
        virtual_nodes::PRIORITY_SPLURGER,
        virtual_nodes::ALIEN_POWER_AUGMENTER,
    ] {
        known.insert(name.to_string(), ());
    }

    NameTable { recipe_nodes, item_names, known }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_standard_and_alternate_recipes() {
        let t = table();
        assert_eq!(t.recipe_node("Recipe_IronPlate_C"), Some("Iron Plate"));
        // "Alternate: Pure Caterium Ingot" -> "Pure Caterium Ingot"
        assert_eq!(t.recipe_node("Recipe_Alternate_PureCateriumIngot_C"), Some("Pure Caterium Ingot"));
        // Parenthesised disambiguators are part of the real name, kept as-is.
        assert_eq!(t.recipe_node("Recipe_FicsiteIngot_AL_C"), Some("Ficsite Ingot (Aluminum)"));
    }

    #[test]
    fn screws_are_singular_in_modeler() {
        let t = table();
        assert_eq!(t.recipe_node("Recipe_Screw_C"), Some("Screw"));
        assert_eq!(t.recipe_node("Recipe_Alternate_Screw_C"), Some("Cast Screw"));
        assert_eq!(t.recipe_node("Recipe_Alternate_Screw_2_C"), Some("Steel Screw"));
        assert_eq!(t.item("Desc_IronScrew_C"), Some("Screw"));
    }

    #[test]
    fn build_gun_recipes_are_not_nodes() {
        let t = table();
        // Would otherwise shadow the Alien Power Augmenter *building* node.
        assert_eq!(t.recipe_node("Recipe_AlienPowerBuilding_C"), None);
        assert!(t.is_known_node(virtual_nodes::ALIEN_POWER_AUGMENTER));
    }

    #[test]
    fn generator_nodes_key_on_fuel_not_building() {
        let t = table();
        assert_eq!(
            t.generator_node("Build_GeneratorFuel_C", "Desc_RocketFuel_C"),
            Some("Rocket Fuel Generator")
        );
        assert_eq!(
            t.generator_node("Build_GeneratorFuel_C", "Desc_LiquidTurboFuel_C"),
            Some("Turbofuel Generator")
        );
        assert_eq!(
            t.generator_node("Build_GeneratorNuclear_C", "Desc_PlutoniumFuelRod_C"),
            Some("Plutonium Nuclear Power Plant")
        );
        // Geothermal takes no fuel, so it matches on the building alone.
        assert_eq!(t.generator_node("Build_GeneratorGeoThermal_C", ""), Some("Geothermal Generator"));
        // Modeler has no biomass burner node.
        assert_eq!(t.generator_node("Build_GeneratorBiomass_Automated_C", "Desc_Wood_C"), None);
    }

    #[test]
    fn raw_resources_resolve() {
        let t = table();
        for (short_class, name) in [
            ("Desc_OreIron_C", "Iron Ore"),
            ("Desc_LiquidOil_C", "Crude Oil"),
            ("Desc_Water_C", "Water"),
            ("Desc_OreBauxite_C", "Bauxite"),
            ("Desc_NitrogenGas_C", "Nitrogen Gas"),
        ] {
            assert_eq!(t.resource_node(short_class), Some(name));
        }
    }
}
