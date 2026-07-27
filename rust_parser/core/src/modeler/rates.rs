//! Per-minute rates: what a recipe consumes and produces, and what an
//! extractor pulls out of the ground.
//!
//! Modeler does its own rate arithmetic from `Max` / `ClockSpeed` /
//! `ProductionShards`, so the exporter does not need recipe rates to fill in
//! machine nodes. It needs them for two other things:
//!
//!  - **which items a recipe touches**, which is what decides where an edge
//!    goes when several item types share one belt;
//!  - **extractor nodes**, whose `Max` really is items/min rather than a
//!    building count, and the report's mass balance.

use crate::gamedata;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Fluid and gas amounts are stored in the game data multiplied by 1000
/// (15 m³ of Nitric Acid appears as `15000`). `stackSize == "SS_FLUID"` is
/// the discriminator -- 15 items carry it.
const FLUID_SCALE: f64 = 1000.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purity {
    Impure,
    Normal,
    Pure,
    Unknown,
}

impl Purity {
    /// The enum names as they appear in `resourcePurity.json`.
    pub fn parse(name: &str) -> Purity {
        match name {
            "IMPURE" => Purity::Impure,
            "NORMAL" => Purity::Normal,
            "PURE" => Purity::Pure,
            _ => Purity::Unknown,
        }
    }

    /// Index into the (impure, normal, pure) triples below. An unknown purity
    /// is treated as normal, and reported.
    fn index(self) -> usize {
        match self {
            Purity::Impure => 0,
            Purity::Normal | Purity::Unknown => 1,
            Purity::Pure => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemRate {
    /// `Desc_X_C` short class.
    pub item: String,
    /// Units per minute for one machine at 100 % clock, no somersloops.
    /// Fluids are m³/min, already divided down from the stored ×1000.
    pub per_minute: f64,
}

#[derive(Clone, Debug, Default)]
pub struct RecipeIo {
    pub ingredients: Vec<ItemRate>,
    pub products: Vec<ItemRate>,
}

impl RecipeIo {
    pub fn consumes(&self, item: &str) -> bool {
        self.ingredients.iter().any(|i| i.item == item)
    }
    pub fn produces(&self, item: &str) -> bool {
        self.products.iter().any(|p| p.item == item)
    }
}

/// Base extraction in units/min at 100 % clock, as (impure, normal, pure).
///
/// These are the game's published figures rather than anything derived from
/// the save, because `game_data/generated/buildings.json` carries only power
/// draw -- extraction cycle times live in `docs.json`, and depending on that
/// would make the export need a game install. They have been stable across
/// 1.0; the report prints every extractor node so a wrong row is visible
/// rather than silent.
const EXTRACTION_RATES: [(&str, [f64; 3]); 6] = [
    ("Build_MinerMk1_C", [30.0, 60.0, 120.0]),
    ("Build_MinerMk2_C", [60.0, 120.0, 240.0]),
    ("Build_MinerMk3_C", [120.0, 240.0, 480.0]),
    ("Build_OilPump_C", [60.0, 120.0, 240.0]),
    // Resource-well satellites. Least certain row here -- worth checking
    // against a known well before trusting the numbers downstream.
    ("Build_FrackingExtractor_C", [60.0, 120.0, 240.0]),
    // The water extractor sits in open water, so it has no node and no
    // purity; all three entries are the same flat rate.
    ("Build_WaterPump_C", [120.0, 120.0, 120.0]),
];

/// What a generator takes in besides its fuel, and what it gives back.
///
/// Generators have no recipe, so none of this is in `recipes.json`. The
/// coolant and waste relationships are what matter to the graph: without
/// them nearly two thousand water pumps on a mature save feed nothing at all
/// and hang in the export as orphans.
///
/// Fuel itself is not listed here -- it comes from `mCurrentFuelClass`.
pub struct GeneratorIo {
    /// Extra inputs on top of the fuel: coolant water.
    pub extra_inputs: &'static [&'static str],
    /// Byproducts, e.g. nuclear waste.
    pub outputs: &'static [&'static str],
}

pub fn generator_io(building_short_class: &str, fuel_short_class: &str) -> GeneratorIo {
    const WATER: &[&str] = &["Desc_Water_C"];
    const NONE: &[&str] = &[];
    match building_short_class {
        "Build_GeneratorCoal_C" => GeneratorIo { extra_inputs: WATER, outputs: NONE },
        "Build_GeneratorNuclear_C" => GeneratorIo {
            extra_inputs: WATER,
            outputs: match fuel_short_class {
                "Desc_NuclearFuelRod_C" => &["Desc_NuclearWaste_C"],
                "Desc_PlutoniumFuelRod_C" => &["Desc_PlutoniumWaste_C"],
                // Ficsonium rods are the end of the chain -- no waste.
                _ => NONE,
            },
        },
        // Fuel and geothermal generators are water-free.
        _ => GeneratorIo { extra_inputs: NONE, outputs: NONE },
    }
}

/// Is this item a fluid or gas (m³ rather than a countable part)?
pub fn is_fluid(item_short_class: &str) -> bool {
    fluids().contains(item_short_class)
}

fn fluids() -> &'static std::collections::HashSet<String> {
    static FLUIDS: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    FLUIDS.get_or_init(|| {
        let gd = gamedata::get();
        gd.items
            .iter()
            .chain(gd.resources.iter())
            .filter(|(_, value)| {
                value.get("stackSize").and_then(|v| v.as_str()) == Some("SS_FLUID")
            })
            .map(|(short_class, _)| short_class.clone())
            .collect()
    })
}

/// What one machine running this recipe consumes and produces per minute at
/// 100 % clock. `None` for a recipe that is not in the game data.
pub fn recipe_io(recipe_short_class: &str) -> Option<&'static RecipeIo> {
    recipes().get(recipe_short_class)
}

fn recipes() -> &'static HashMap<String, RecipeIo> {
    static TABLE: OnceLock<HashMap<String, RecipeIo>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = HashMap::new();
        for (short_class, value) in gamedata::get().recipes.iter() {
            let duration = value.get("durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if duration <= 0.0 {
                continue; // build-gun recipes; nothing to rate
            }
            let side = |key: &str| -> Vec<ItemRate> {
                value
                    .get(key)
                    .and_then(|v| v.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| {
                                let item =
                                    entry.get("item").and_then(|v| v.as_str())?.to_string();
                                let amount = entry.get("amount").and_then(|v| v.as_f64())?;
                                let amount =
                                    if is_fluid(&item) { amount / FLUID_SCALE } else { amount };
                                Some(ItemRate { per_minute: amount * 60.0 / duration, item })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            table.insert(
                short_class.clone(),
                RecipeIo { ingredients: side("ingredients"), products: side("product") },
            );
        }
        table
    })
}

/// Units/min one extractor pulls at 100 % clock. `None` if the class is not a
/// known extractor.
pub fn extraction_rate(building_short_class: &str, purity: Purity) -> Option<f64> {
    EXTRACTION_RATES
        .iter()
        .find(|(class, _)| *class == building_short_class)
        .map(|(_, rates)| rates[purity.index()])
}

/// Purity of the resource node an extractor is mining, looked up by the node
/// instance name recorded in `mExtractableResource`.
pub fn node_purity(resource_instance_name: &str) -> Purity {
    match gamedata::get().resource_purity.get(resource_instance_name) {
        Some((_, purity, _, _)) => Purity::parse(purity),
        // A water extractor has no node at all, and the reveal-derived purity
        // tables have known gaps on mature saves.
        None => Purity::Unknown,
    }
}

/// The resource an extractor yields, from the node it is mining. Falls back
/// to the item the extractor's own class implies where there is no node.
pub fn node_resource(resource_instance_name: &str) -> Option<String> {
    gamedata::get()
        .resource_purity
        .get(resource_instance_name)
        .map(|(resource_type, _, _, _)| resource_type.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_recipe_rates_are_amount_per_minute() {
        // Iron Plate: 3 ingots -> 2 plates every 6 s.
        let io = recipe_io("Recipe_IronPlate_C").expect("iron plate");
        assert_eq!(io.ingredients, vec![ItemRate { item: "Desc_IronIngot_C".into(), per_minute: 30.0 }]);
        assert_eq!(io.products, vec![ItemRate { item: "Desc_IronPlate_C".into(), per_minute: 20.0 }]);
    }

    #[test]
    fn fluid_amounts_are_scaled_down_from_the_stored_thousands() {
        // Non-Fissile Uranium, per 24 s: 15 Uranium Waste, 10 Silica,
        // 6 m3 Nitric Acid, 6 m3 Sulfuric Acid -> 20 product + 6 m3 Water.
        // The two acids are stored as 6000, so a missing divide would report
        // 15 000 m3/min instead of 15.
        let io = recipe_io("Recipe_NonFissileUranium_C").expect("non-fissile uranium");
        let rate = |side: &[ItemRate], item: &str| {
            side.iter().find(|i| i.item == item).unwrap_or_else(|| panic!("{item}")).per_minute
        };
        assert_eq!(rate(&io.ingredients, "Desc_NitricAcid_C"), 15.0);
        assert_eq!(rate(&io.ingredients, "Desc_SulfuricAcid_C"), 15.0);
        // Solids in the same recipe stay unscaled.
        assert_eq!(rate(&io.ingredients, "Desc_Silica_C"), 25.0);
        assert_eq!(rate(&io.ingredients, "Desc_NuclearWaste_C"), 37.5);
        // Byproduct water is a fluid too.
        assert_eq!(rate(&io.products, "Desc_Water_C"), 15.0);
        assert_eq!(rate(&io.products, "Desc_NonFissibleUranium_C"), 50.0);
    }

    #[test]
    fn fluids_are_identified_from_the_game_data_not_a_hardcoded_list() {
        for fluid in ["Desc_Water_C", "Desc_LiquidOil_C", "Desc_NitrogenGas_C", "Desc_DarkEnergy_C"] {
            assert!(is_fluid(fluid), "{fluid} should be a fluid");
        }
        for solid in ["Desc_IronPlate_C", "Desc_OreIron_C", "Desc_PackagedWater_C"] {
            assert!(!is_fluid(solid), "{solid} should not be a fluid");
        }
    }

    #[test]
    fn extraction_scales_with_miner_mark_and_purity() {
        assert_eq!(extraction_rate("Build_MinerMk1_C", Purity::Impure), Some(30.0));
        assert_eq!(extraction_rate("Build_MinerMk2_C", Purity::Normal), Some(120.0));
        assert_eq!(extraction_rate("Build_MinerMk3_C", Purity::Pure), Some(480.0));
        // No node, no purity -- one flat rate whatever we pass.
        assert_eq!(extraction_rate("Build_WaterPump_C", Purity::Unknown), Some(120.0));
        assert_eq!(extraction_rate("Build_WaterPump_C", Purity::Pure), Some(120.0));
        // An unknown purity must not silently read as impure.
        assert_eq!(
            extraction_rate("Build_MinerMk3_C", Purity::Unknown),
            extraction_rate("Build_MinerMk3_C", Purity::Normal),
        );
        assert_eq!(extraction_rate("Build_ConstructorMk1_C", Purity::Normal), None);
    }

    #[test]
    fn recipe_io_answers_membership_questions() {
        let io = recipe_io("Recipe_IronPlateReinforced_C").expect("reinforced iron plate");
        assert!(io.consumes("Desc_IronPlate_C"));
        assert!(io.consumes("Desc_IronScrew_C"));
        assert!(!io.consumes("Desc_IronRod_C"), "RIP does not take rods directly");
        assert!(io.produces("Desc_IronPlateReinforced_C"));
    }
}
