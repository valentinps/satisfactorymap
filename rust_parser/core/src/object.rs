//! Object (actor/component entity) parsing, including the per-class trailing
//! `actorSpecificInfo` dispatch and the satisfactory-calculator quirk
//! handling. Mirrors Object.parse in patches/sav_parse.py.

use crate::error::{perr, PResult};
use crate::properties::{parse_object_reference, parse_properties};
use crate::reader::Cursor;
use crate::store::*;
use crate::version_data::parse_save_object_version_data;

/// Class tables passed in from Python (sav_data.data stays the source of truth).
#[derive(Clone)]
pub struct ClassTables {
    pub conveyor_belts: Vec<String>,
}

const GAME_MODE_STATE: [&str; 2] = [
    "/Game/FactoryGame/-Shared/Blueprint/BP_GameMode.BP_GameMode_C",
    "/Game/FactoryGame/-Shared/Blueprint/BP_GameState.BP_GameState_C",
];
const PLAYER_STATE: &str = "/Game/FactoryGame/Character/Player/BP_PlayerState.BP_PlayerState_C";
const DRONE_TRANSPORT: &str =
    "/Game/FactoryGame/Buildable/Factory/DroneStation/BP_DroneTransport.BP_DroneTransport_C";
const CIRCUIT_SUBSYSTEM: &str = "/Game/FactoryGame/-Shared/Blueprint/BP_CircuitSubsystem.BP_CircuitSubsystem_C";
pub(crate) const POWER_LINES: [&str; 2] = [
    "/Game/FactoryGame/Buildable/Factory/PowerLine/Build_PowerLine.Build_PowerLine_C",
    "/Game/FactoryGame/Events/Christmas/Buildings/PowerLineLights/Build_XmassLightsLine.Build_XmassLightsLine_C",
];
const TRAINS: [&str; 2] = [
    "/Game/FactoryGame/Buildable/Vehicle/Train/Locomotive/BP_Locomotive.BP_Locomotive_C",
    "/Game/FactoryGame/Buildable/Vehicle/Train/Wagon/BP_FreightWagon.BP_FreightWagon_C",
];
const VEHICLES: [&str; 6] = [
    "/Game/FactoryGame/Buildable/Vehicle/Cyberwagon/Testa_BP_WB.Testa_BP_WB_C",
    "/Game/FactoryGame/Buildable/Vehicle/Explorer/BP_Explorer.BP_Explorer_C",
    "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_Golfcart.BP_Golfcart_C",
    "/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C",
    "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_Truck.BP_Truck_C",
    "/Game/FactoryGame/Buildable/Vehicle/Truck/BP_FluidTruck.BP_FluidTruck_C",
];
pub(crate) const LIGHTWEIGHT_SUBSYSTEM: &str = "/Script/FactoryGame.FGLightweightBuildableSubsystem";
pub(crate) const CONVEYOR_CHAINS: [&str; 5] = [
    "/Script/FactoryGame.FGConveyorChainActor",
    "/Script/FactoryGame.FGConveyorChainActor_RepSizeNoCull",
    "/Script/FactoryGame.FGConveyorChainActor_RepSizeMedium",
    "/Script/FactoryGame.FGConveyorChainActor_RepSizeLarge",
    "/Script/FactoryGame.FGConveyorChainActor_RepSizeHuge",
];
const PICKUP_SPAWNABLE: &str = "/Script/FactoryGame.FGItemPickup_Spawnable";
const MODDED_RAW: [&str; 5] = [
    "/AB_CableMod/Cables_Heavy/Build_AB-PLHeavy-Cu.Build_AB-PLHeavy-Cu_C",
    "/FlexSplines/Conveyor/Build_Belt2.Build_Belt2_C",
    "/FlexSplines/PowerLine/Build_FlexPowerline.Build_FlexPowerline_C",
    "/Game/FactoryGame/Buildable/Vehicle/Golfcart/BP_GolfcartGold.BP_GolfcartGold_C",
    "/CharacterReplacer/Logic/SCS_CR_PlayerHook.SCS_CR_PlayerHook_C",
];
const COMPONENT_TRAILING: [&str; 18] = [
    "/Script/FactoryGame.FGDroneMovementComponent",
    "/Script/FactoryGame.FGFactoryConnectionComponent",
    "/Script/FactoryGame.FGFactoryLegsComponent",
    "/Script/FactoryGame.FGHealthComponent",
    "/Script/FactoryGame.FGInventoryComponent",
    "/Script/FactoryGame.FGInventoryComponentEquipment",
    "/Script/FactoryGame.FGInventoryComponentTrash",
    "/Script/FactoryGame.FGPipeConnectionComponent",
    "/Script/FactoryGame.FGPipeConnectionComponentHyper",
    "/Script/FactoryGame.FGPipeConnectionFactory",
    "/Script/FactoryGame.FGPowerConnectionComponent",
    "/Script/FactoryGame.FGPowerInfoComponent",
    "/Script/FactoryGame.FGRailroadTrackConnectionComponent",
    "/Script/FactoryGame.FGShoppingListComponent",
    "/Script/FactoryGame.FGTrainPlatformConnection",
    "/Script/FactoryGame.FGVehicleAutopilotComponent",
    "/Script/FicsitFarming.FFDoggoHealthInfoComponent",
    "/EditSwatchNames/DataHolder.DataHolder_C",
];
const CALC_ACTOR_WHITELIST: [&str; 19] = [
    "/Script/FactoryGame.FGBlueprintProxy",
    "/Script/FactoryGame.FGCentralStorageSubsystem",
    "/Script/FactoryGame.FGDockingStationInfo",
    "/Script/FactoryGame.FGDrivingTargetList",
    "/Script/FactoryGame.FGDroneStationInfo",
    "/Script/FactoryGame.FGFoliageRemovalSubsystem",
    "/Script/FactoryGame.FGGameRulesSubsystem",
    "/Script/FactoryGame.FGMapManager",
    "/Script/FactoryGame.FGPipeNetwork",
    "/Script/FactoryGame.FGPipeSubsystem",
    "/Script/FactoryGame.FGRailroadTimeTable",
    "/Script/FactoryGame.FGRecipeManager",
    "/Script/FactoryGame.FGResourceSinkSubsystem",
    "/Script/FactoryGame.FGSavedWheeledVehiclePath",
    "/Script/FactoryGame.FGScannableSubsystem",
    "/Script/FactoryGame.FGStatisticsSubsystem",
    "/Script/FactoryGame.FGTrainStationIdentifier",
    "/Script/FactoryGame.FGWheeledVehicleInfo",
    "/Script/FactoryGame.FGWorldSettings",
];
const CALC_COMPONENT_WHITELIST: [&str; 7] = [
    "/Script/FactoryGame.FGBlueprintShortcut",
    "/Script/FactoryGame.FGEmoteShortcut",
    "/Script/FactoryGame.FGFactoryCustomizationShortcut",
    "/Script/FactoryGame.FGHighlightedMarker_MapMarker",
    "/Script/FactoryGame.FGPlayerHotbar",
    "/Script/FactoryGame.FGPowerCircuit",
    "/Script/FactoryGame.FGRecipeShortcut",
];

pub fn parse_object(
    c: &mut Cursor,
    header_save_version: u32,
    mut object_ue5_version: i32,
    header: &Header,
    tables: &ClassTables,
    calculator_extras: &mut Vec<String>,
) -> PResult<Object> {
    let object_game_version = c.u32()?;
    let should_migrate =
        c.bool_u32("Object.shouldMigrateObjectRefsToPersistentFlag")?;
    let object_size = c.u32()? as usize;
    let offset_start_this = c.pos;

    let mut per_object_version_data = None;
    let mut jump_offset = 0usize;
    if object_game_version >= 53 {
        let mut jc = Cursor::new(c.data, c.pos + object_size);
        let should_serialize =
            jc.bool_u32("Object.shouldSerializePerObjectVersionData")?;
        if should_serialize {
            let vd = parse_save_object_version_data(&mut jc)?;
            object_ue5_version = vd.file_version_ue5 as i32;
            per_object_version_data = Some(vd);
        }
        jump_offset = jc.pos;
    }

    let is_actor = matches!(header, Header::Actor(_));
    let mut actor_reference_associations = None;
    if is_actor {
        let parent = parse_object_reference(c)?;
        let n = c.u32()?;
        let mut components = Vec::with_capacity(n as usize);
        for _ in 0..n {
            components.push(parse_object_reference(c)?);
        }
        actor_reference_associations = Some((parent, components));
    }

    if object_ue5_version >= 1011 {
        c.confirm_u8(0)?; // serializationControl
    }

    let properties = parse_properties(c, object_game_version, object_ue5_version)?;
    c.confirm_u32(0)?;

    let mut actor_specific = ActorSpecific::None;
    let trailing_byte_size = (offset_start_this + object_size) as i64 - c.pos as i64;

    match header {
        Header::Actor(ah) => {
            let type_path_bytes = ah.type_path.bytes(c.data);
            let tp = std::str::from_utf8(type_path_bytes).unwrap_or("");
            if tables.conveyor_belts.iter().any(|b| b == tp) {
                let count = c.u32()?;
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let length = c.u32()?;
                    let name = c.string()?;
                    c.confirm_string("")?;
                    c.confirm_string("")?;
                    let position = c.f32()?;
                    items.push((length, name, position));
                }
                actor_specific = ActorSpecific::ConveyorBelt(items);
            } else if GAME_MODE_STATE.contains(&tp) {
                let count = c.u32()?;
                let mut refs = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    refs.push(parse_object_reference(c)?);
                }
                actor_specific = ActorSpecific::RefList(refs);
            } else if tp == PLAYER_STATE {
                let player_state_type = c.u8()?;
                if trailing_byte_size == 1 && player_state_type == 3 {
                    actor_specific = ActorSpecific::PlayerStateType(player_state_type);
                } else if player_state_type == 241 {
                    let client_type = c.u8()?;
                    let client_size = c.u32()?;
                    let data = c.data_ref(client_size as usize)?;
                    actor_specific = ActorSpecific::PlayerStateClient { client_type, data };
                } else {
                    c.pos -= 1;
                    let data = c.data_ref(trailing_byte_size.max(0) as usize)?;
                    actor_specific = ActorSpecific::RawBytes(data);
                }
            } else if tp == DRONE_TRANSPORT {
                let data = c.data_ref(trailing_byte_size.max(0) as usize)?;
                actor_specific = ActorSpecific::RawBytes(data);
            } else if tp == CIRCUIT_SUBSYSTEM {
                let n = c.u32()?;
                let mut circuits = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let id = c.u32()?;
                    let r = parse_object_reference(c)?;
                    circuits.push((id, r));
                }
                actor_specific = ActorSpecific::Circuits(circuits);
            } else if POWER_LINES.contains(&tp) {
                let source = parse_object_reference(c)?;
                let target = parse_object_reference(c)?;
                actor_specific = ActorSpecific::PowerLine(source, target);
            } else if TRAINS.contains(&tp) {
                let num_trains = c.u32()?;
                if num_trains > 0 {
                    return Err(perr!(
                        "numTrains {} for Object trailing data for {} now allows greater parse testing.",
                        num_trains,
                        tp
                    ));
                }
                let previous = parse_object_reference(c)?;
                let next = parse_object_reference(c)?;
                actor_specific = ActorSpecific::Train { previous, next };
            } else if VEHICLES.contains(&tp) {
                let n = c.u32()?;
                let mut vehicles = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let name = c.string()?;
                    let data = c.data_ref(105)?;
                    vehicles.push((name, data));
                }
                actor_specific = ActorSpecific::Vehicles(vehicles);
            } else if tp == LIGHTWEIGHT_SUBSYSTEM {
                let lightweight_version = c.u32()?;
                let count1 = c.u32()?;
                let mut items = Vec::with_capacity(count1 as usize);
                for _ in 0..count1 {
                    c.confirm_u32(0)?;
                    let build_item_path = c.string()?;
                    let count_field_off = c.pos;
                    let count2 = c.u32()?;
                    let mut instances = Vec::with_capacity(count2 as usize);
                    for _ in 0..count2 {
                        let record_off = c.pos;
                        let rotation = [c.f64()?, c.f64()?, c.f64()?, c.f64()?];
                        let position = [c.f64()?, c.f64()?, c.f64()?];
                        for _ in 0..3 {
                            c.confirm_f64(1.0)?; // scale
                        }
                        let swatch = parse_object_reference(c)?;
                        let material = parse_object_reference(c)?;
                        if !material.level_name.is_empty() || !material.path_name.is_empty() {
                            return Err(perr!("ERROR: Unexpected material level path."));
                        }
                        let pattern = parse_object_reference(c)?;
                        let skin = parse_object_reference(c)?;
                        if !skin.level_name.is_empty() || !skin.path_name.is_empty() {
                            return Err(perr!("ERROR: Unexpected skin level path."));
                        }
                        let primary_color = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
                        let secondary_color = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
                        let paint_finish = parse_object_reference(c)?;
                        let pattern_rotation = c.u8()?;
                        let recipe = parse_object_reference(c)?;
                        let blueprint_proxy = parse_object_reference(c)?;

                        let mut data_property = None;
                        let mut service_provider = None;
                        let mut player_info_table_index = None;
                        if lightweight_version >= 2 {
                            let data_flag = c.u32()?;
                            if data_flag != 0 {
                                c.confirm_u32(0)?; // ObjectReference.Level
                                c.confirm_string("/Script/FactoryGame.BuildableBeamLightweightData")?;
                                let size = c.u32()?;
                                let start = c.pos;
                                let inner = parse_properties(
                                    c,
                                    header_save_version,
                                    object_ue5_version,
                                )?;
                                if c.pos != start + size as usize {
                                    return Err(perr!(
                                        "Unexpected LightweightBuildableSubsystem lightweightDataPropertySize: offset={} != {} = start={} + size={}.",
                                        c.pos,
                                        start + size as usize,
                                        start,
                                        size
                                    ));
                                }
                                data_property = Some(inner);
                            }
                            if lightweight_version >= 3 {
                                if header_save_version == 57 {
                                    // In-game merge-conflict bug workaround.
                                    let _ = c.u8()?;
                                    let _ = c.i32()?;
                                }
                                service_provider = Some(c.u8()?);
                                player_info_table_index = Some(if header_save_version >= 57 {
                                    PlayerIdx::I32(c.i32()?)
                                } else {
                                    PlayerIdx::U8(c.u8()?)
                                });
                            }
                        }
                        instances.push(LightweightInstance {
                            rotation,
                            position,
                            swatch,
                            pattern,
                            primary_color,
                            secondary_color,
                            paint_finish,
                            pattern_rotation,
                            recipe,
                            blueprint_proxy,
                            data_property,
                            service_provider,
                            player_info_table_index,
                            record_off,
                            record_len: (c.pos - record_off) as u32,
                        });
                    }
                    items.push(LightweightGroup {
                        type_path: build_item_path,
                        count_field_off,
                        end_off: c.pos,
                        instances,
                    });
                }
                actor_specific = ActorSpecific::Lightweight { version: lightweight_version, items };
            } else if CONVEYOR_CHAINS.contains(&tp) {
                let _starting_belt = parse_object_reference(c)?;
                let _ending_belt = parse_object_reference(c)?;
                let num_belts = c.u32()?;
                if num_belts == 0 {
                    // Python would hit a NameError building actorSpecificInfo.
                    return Err(perr!("Conveyor chain with zero belts."));
                }
                let mut chain_actor = ObjectRef {
                    level_name: crate::reader::EMPTY_STR,
                    path_name: crate::reader::EMPTY_STR,
                };
                let mut belts = Vec::with_capacity(num_belts as usize);
                for idx in 0..num_belts {
                    chain_actor = parse_object_reference(c)?;
                    let belt = parse_object_reference(c)?;
                    let num_elements = c.u32()?;
                    let elements_off = c.pos;
                    let mut elements = Vec::with_capacity(num_elements as usize);
                    for _ in 0..num_elements {
                        let mut nine = [[0f64; 3]; 3];
                        for row in nine.iter_mut() {
                            for v in row.iter_mut() {
                                *v = c.f64()?;
                            }
                        }
                        elements.push(nine);
                    }
                    let a = c.u32()?;
                    let b = c.u32()?;
                    let c3 = c.u32()?;
                    let lead = c.i32()?;
                    let tail = c.i32()?;
                    c.confirm_u32(idx)?;
                    belts.push(ChainBelt { belt, elements_off, elements, a, b, c: c3, lead_item_index: lead, tail_item_index: tail });
                }
                let cu32 = c.u32()?;
                let maximum_items = c.i32()?;
                let chain_lead = c.i32()?;
                let chain_tail = c.i32()?;
                let num_items = c.u32()?;
                let mut items = Vec::with_capacity(num_items as usize);
                for _ in 0..num_items {
                    c.confirm_u32(0)?;
                    let item_path = c.string()?;
                    c.confirm_u32(0)?;
                    let item_instance_id = c.u32()?;
                    items.push((item_path, item_instance_id));
                }
                actor_specific = ActorSpecific::ConveyorChain {
                    chain_actor,
                    belts,
                    items,
                    cu32,
                    maximum_items,
                    chain_lead_item_index: chain_lead,
                    chain_tail_item_index: chain_tail,
                };
            } else if tp == PICKUP_SPAWNABLE {
                if trailing_byte_size == 4 {
                    actor_specific = ActorSpecific::PickupSpawnable(true);
                    c.confirm_u32(0)?;
                } else {
                    actor_specific = ActorSpecific::PickupSpawnable(false);
                }
            } else if MODDED_RAW.contains(&tp) {
                let data = c.data_ref(trailing_byte_size.max(0) as usize)?;
                actor_specific = ActorSpecific::RawBytes(data);
            }
        }
        Header::Component(ch) => {
            let class_bytes = ch.class_name.bytes(c.data);
            let cn = std::str::from_utf8(class_bytes).unwrap_or("");
            if COMPONENT_TRAILING.contains(&cn) {
                let has_trailing = c.pos < offset_start_this + object_size;
                actor_specific = ActorSpecific::ComponentTrailing(has_trailing);
                if has_trailing {
                    c.confirm_u32_msg(0, cn)?;
                }
            }
        }
    }

    // satisfactory-calculator.com re-save quirk handling
    if c.pos < offset_start_this + object_size {
        match header {
            Header::Actor(ah) => {
                let tp = ah.type_path.to_string(c.data);
                calculator_extras.push(tp.clone());
                if CALC_ACTOR_WHITELIST.contains(&tp.as_str()) {
                    c.confirm_u32(0)?;
                }
            }
            Header::Component(ch) => {
                let cn = ch.class_name.to_string(c.data);
                calculator_extras.push(cn.clone());
                if CALC_COMPONENT_WHITELIST.contains(&cn.as_str()) {
                    c.confirm_u32(0)?;
                }
            }
        }
    }

    if c.pos > offset_start_this + object_size {
        return Err(perr!(
            "Unexpected objectSize: expect={} < actual={}.  Offset passed expected position by {} bytes.  Started at {}.",
            object_size,
            c.pos - offset_start_this,
            c.pos - offset_start_this - object_size,
            offset_start_this
        ));
    }
    if c.pos < offset_start_this + object_size {
        return match header {
            Header::Actor(ah) => Err(perr!(
                "Found {} extra trailing bytes for ActorHeader {}.",
                offset_start_this + object_size - c.pos,
                ah.type_path.to_string(c.data)
            )),
            Header::Component(ch) => Err(perr!(
                "Found {} extra trailing bytes for ComponentHeader {}.",
                offset_start_this + object_size - c.pos,
                ch.class_name.to_string(c.data)
            )),
        };
    }

    if object_game_version >= 53 {
        c.pos = jump_offset;
    }

    Ok(Object {
        object_game_version,
        should_migrate_object_refs_to_persistent_flag: should_migrate,
        per_object_version_data,
        actor_reference_associations,
        properties,
        actor_specific,
    })
}

/// Advance the cursor over one object record without materializing anything
/// -- consumes exactly the bytes parse_object would (the object_size field
/// covers the body; the v53+ trailing version block sits after it). Used by
/// the lean load path that records spans only; gated by the span-identity
/// test against the eager parse.
pub fn skip_object(c: &mut Cursor) -> PResult<()> {
    let object_game_version = c.u32()?;
    c.bool_u32("Object.shouldMigrateObjectRefsToPersistentFlag")?;
    let object_size = c.u32()? as usize;
    if c.pos + object_size > c.data.len() {
        return Err(perr!(
            "Object size {} at offset {} overruns {}-byte data.",
            object_size,
            c.pos,
            c.data.len()
        ));
    }
    c.pos += object_size;
    if object_game_version >= 53 {
        let should_serialize = c.bool_u32("Object.shouldSerializePerObjectVersionData")?;
        if should_serialize {
            parse_save_object_version_data(c)?;
        }
    }
    Ok(())
}

impl crate::store::SaveStore {
    /// Re-parse one object from its recorded byte span. Yields an Object
    /// identical to the eagerly parsed one (same data buffer, so identical
    /// StrRef/DataRef offsets) -- this is how queries read objects after
    /// `drop_object_model`. Cost is microseconds for typical objects.
    pub fn parse_object_at(&self, li: usize, oi: usize) -> PResult<Object> {
        let level = &self.levels[li];
        let (off, len) = level.object_spans[oi];
        let mut c = Cursor::new(&self.data, off as usize);
        // Quirk markers were already recorded during the initial parse.
        let mut scratch_extras = Vec::new();
        let object = parse_object(
            &mut c,
            self.info.save_version,
            level.object_ue5_version,
            &level.headers[oi],
            &self.tables,
            &mut scratch_extras,
        )?;
        debug_assert_eq!(c.pos, off as usize + len as usize, "span reparse length drift");
        Ok(object)
    }
}
