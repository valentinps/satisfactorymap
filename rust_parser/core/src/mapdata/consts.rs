//! Type-path constants -- port of sav_map_data.py's module-level tables
//! (lines ~33-72, 588-660). The sav_data-derived lists (CONVEYOR_BELTS,
//! MINERS, ...) live in gamedata::get().type_paths; everything here is
//! hardcoded in the Python module itself.

use std::collections::HashSet;
use std::sync::OnceLock;

pub const PIPELINE_SEGMENTS: [&str; 4] = [
    "/Game/FactoryGame/Buildable/Factory/Pipeline/Build_Pipeline.Build_Pipeline_C",
    "/Game/FactoryGame/Buildable/Factory/Pipeline/Build_Pipeline_NoIndicator.Build_Pipeline_NoIndicator_C",
    "/Game/FactoryGame/Buildable/Factory/PipelineMk2/Build_PipelineMK2.Build_PipelineMK2_C",
    "/Game/FactoryGame/Buildable/Factory/PipelineMk2/Build_PipelineMK2_NoIndicator.Build_PipelineMK2_NoIndicator_C",
];

pub const RAILROAD_SEGMENTS: [&str; 2] = [
    "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrack.Build_RailroadTrack_C",
    "/Game/FactoryGame/Buildable/Factory/Train/Track/Build_RailroadTrackIntegrated.Build_RailroadTrackIntegrated_C",
];

pub const HYPERTUBE_SEGMENTS: [&str; 2] = [
    "/Game/FactoryGame/Buildable/Factory/PipeHyper/Build_PipeHyper.Build_PipeHyper_C",
    "/Game/FactoryGame/Buildable/Factory/PipeHyperStart/Build_PipeHyperStart.Build_PipeHyperStart_C",
];

pub const VEHICLE_PATH_SEGMENTS: [&str; 5] = [
    "/Game/FactoryGame/Buildable/Vehicle/Explorer/Build_VehiclePath_Explorer.Build_VehiclePath_Explorer_C",
    "/Game/FactoryGame/Buildable/Vehicle/Golfcart/Build_VehiclePath_FactoryCart.Build_VehiclePath_FactoryCart_C",
    "/Game/FactoryGame/Buildable/Vehicle/Tractor/Build_VehiclePath_Tractor.Build_VehiclePath_Tractor_C",
    "/Game/FactoryGame/Buildable/Vehicle/Truck/Build_VehiclePath_Truck.Build_VehiclePath_Truck_C",
    "/Game/FactoryGame/Buildable/Vehicle/VehiclePath/Build_VehiclePath_Universal.Build_VehiclePath_Universal_C",
];

pub const OTHER_CATEGORY: &str = "Unknown";

pub const TOP_CATEGORY_ORDER_GUESS: [&str; 6] =
    ["Sub_Organisation", "Sub_Walls", "Sub_Production", "Sub_Power", "Sub_Transport", "Sub_Special"];

pub const EXCLUDED_BUILDING_TYPE_PATHS: [&str; 2] = [
    "/Game/FactoryGame/Buildable/Factory/ProjectAssembly/BP_ProjectAssembly.BP_ProjectAssembly_C",
    "/Game/FactoryGame/Buildable/Vehicle/Train/-Shared/BP_Train.BP_Train_C",
];

/// (typePath, iconFilename) in Python dict order.
pub const VEHICLE_ICONS_BY_TYPE_PATH: [(&str, &str); 10] = [
    ("/Game/FactoryGame/Buildable/Vehicle/Explorer/BP_Explorer.BP_Explorer_C", "Explorer.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_Golfcart.BP_Golfcart_C", "FactoryCart.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_GolfcartGold.BP_GolfcartGold_C", "FactoryCart.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C", "Tractor.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Truck/BP_Truck.BP_Truck_C", "Truck.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Truck/BP_FluidTruck.BP_FluidTruck_C", "Truck.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Cyberwagon/Testa_BP_WB.Testa_BP_WB_C", "CyberWagon.png"),
    ("/Game/FactoryGame/Buildable/Factory/DroneStation/BP_DroneTransport.BP_DroneTransport_C", "Drone.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C", "Train.png"),
    ("/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C", "Train.png"),
];

/// (typePath, (lengthMeters, widthMeters)) in Python dict order.
pub const VEHICLE_FOOTPRINTS_METERS_BY_TYPE_PATH: [(&str, (f64, f64)); 10] = [
    ("/Game/FactoryGame/Buildable/Vehicle/Explorer/BP_Explorer.BP_Explorer_C", (7.0, 4.5)),
    ("/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_Golfcart.BP_Golfcart_C", (3.2, 2.2)),
    ("/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_GolfcartGold.BP_GolfcartGold_C", (3.2, 2.2)),
    ("/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C", (8.5, 5.5)),
    ("/Game/FactoryGame/Buildable/Vehicle/Truck/BP_Truck.BP_Truck_C", (10.5, 5.5)),
    ("/Game/FactoryGame/Buildable/Vehicle/Truck/BP_FluidTruck.BP_FluidTruck_C", (10.5, 5.5)),
    ("/Game/FactoryGame/Buildable/Vehicle/Cyberwagon/Testa_BP_WB.Testa_BP_WB_C", (6.5, 3.5)),
    ("/Game/FactoryGame/Buildable/Factory/DroneStation/BP_DroneTransport.BP_DroneTransport_C", (9.0, 9.0)),
    ("/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C", (16.0, 5.4)),
    ("/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C", (16.0, 5.4)),
];

pub const TRAIN_TYPE_PATH: &str =
    "/Game/FactoryGame/Buildable/Vehicle/Train/-Shared/BP_Train.BP_Train_C";
pub const LOCOMOTIVE_TYPE_PATH: &str =
    "/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C";
pub const FREIGHT_WAGON_TYPE_PATH: &str =
    "/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C";

pub const HUB_TYPE_PATH: &str =
    "/Game/FactoryGame/Buildable/Factory/TradingPost/Build_TradingPost.Build_TradingPost_C";
pub const LIGHTWEIGHT_BUILDABLE_SUBSYSTEM_TYPE_PATH: &str =
    "/Script/FactoryGame.FGLightweightBuildableSubsystem";
pub const PLAYER_TYPE_PATH: &str =
    "/Game/FactoryGame/Character/Player/Char_Player.Char_Player_C";
pub const LIZARD_DOGGO_TYPE_PATH: &str =
    "/Game/FactoryGame/Character/Creature/Wildlife/SpaceRabbit/Char_SpaceRabbit.Char_SpaceRabbit_C";
pub const SPACE_ELEVATOR_TYPE_PATH: &str =
    "/Game/FactoryGame/Buildable/Factory/SpaceElevator/Build_SpaceElevator.Build_SpaceElevator_C";
pub const GAME_STATE_TYPE_PATH_SUBSTRING: &str = "BP_GameState_C";
/// Items dropped loose on the ground -- each is its own actor of this one
/// engine class (sav_map_data.ITEM_PICKUP_TYPE_PATH).
pub const ITEM_PICKUP_TYPE_PATH: &str = "/Script/FactoryGame.FGItemPickup_Spawnable";

pub fn railcar_type_paths() -> [&'static str; 2] {
    [LOCOMOTIVE_TYPE_PATH, FREIGHT_WAGON_TYPE_PATH]
}

/// CONVEYOR_BELT_ONLY_TYPE_PATHS: belts minus lifts, in sav_data order.
pub fn conveyor_belt_only_type_paths() -> &'static Vec<String> {
    static PATHS: OnceLock<Vec<String>> = OnceLock::new();
    PATHS.get_or_init(|| {
        crate::gamedata::get()
            .type_paths
            .conveyor_belts
            .iter()
            .filter(|p| !p.contains("ConveyorLift"))
            .cloned()
            .collect()
    })
}

/// LINE_RENDERED_TYPE_PATHS: membership set (Python set -- order never used).
pub fn line_rendered_type_paths() -> &'static HashSet<&'static str> {
    static PATHS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let mut set: HashSet<&'static str> = HashSet::new();
        set.extend(conveyor_belt_only_type_paths().iter().map(|s| s.as_str()));
        set.extend(PIPELINE_SEGMENTS);
        set.extend(RAILROAD_SEGMENTS);
        set.extend(HYPERTUBE_SEGMENTS);
        set.extend(VEHICLE_PATH_SEGMENTS);
        set
    })
}

pub fn vehicle_icon(type_path: &str) -> Option<&'static str> {
    VEHICLE_ICONS_BY_TYPE_PATH.iter().find(|(p, _)| *p == type_path).map(|(_, icon)| *icon)
}

pub fn vehicle_footprint_meters(type_path: &str) -> Option<(f64, f64)> {
    VEHICLE_FOOTPRINTS_METERS_BY_TYPE_PATH
        .iter()
        .find(|(p, _)| *p == type_path)
        .map(|(_, fp)| *fp)
}
