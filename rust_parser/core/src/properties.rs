//! parseProperties port. Structure and quirk-for-quirk behavior mirror
//! patches/sav_parse.py (the live reference): the same branch order, the same
//! confirm checks, the same retainedPropertyType (meta) contents, and the
//! same function-local variables that persist across loop iterations
//! (enumName / arrayType / setType / structPropertyType / structureSubType /
//! structSize / keyType / valueType).

use crate::error::{perr, PResult};
use crate::reader::{Cursor, StrRef};
use crate::store::*;

pub fn parse_object_reference(c: &mut Cursor) -> PResult<ObjectRef> {
    let level_name = c.string()?;
    let path_name = c.string()?;
    Ok(ObjectRef { level_name, path_name })
}

pub fn parse_text_property(c: &mut Cursor) -> PResult<TextValue> {
    let flags = c.u32()?;
    let history_type = c.u8()?;
    match history_type {
        255 => {
            let invariant = c.u32()?;
            let s = if invariant != 0 { c.string()? } else { crate::reader::EMPTY_STR };
            Ok(TextValue::NoneHistory { flags, invariant, s })
        }
        0 => {
            let namespace = c.string()?;
            let key = c.string()?;
            let value = c.string()?;
            Ok(TextValue::Base { flags, namespace, key, value })
        }
        3 => {
            c.confirm_u32(8)?;
            c.confirm_u8(0)?;
            c.confirm_u32(1)?;
            c.confirm_u8(0)?;
            let uuid = c.string()?;
            let format = c.string()?;
            let arg_count = c.u32()?;
            let mut args = Vec::with_capacity(arg_count as usize);
            for _ in 0..arg_count {
                let arg_name = c.string()?;
                c.confirm_u8(4)?;
                let arg_flags = c.u32()?;
                c.confirm_u8(255)?; // historyType
                c.confirm_u32(1)?; // isTextCultureInvariant
                let arg_value = c.string()?;
                args.push((arg_name, arg_value, arg_flags));
            }
            Ok(TextValue::ArgumentFormat { flags, uuid, format, args })
        }
        11 => {
            let table_id = c.string()?;
            let text_key = c.string()?;
            Ok(TextValue::StringTable { flags, table_id, text_key })
        }
        other => Err(perr!("Unexpected TextProperty historyType {}", other)),
    }
}

/// parsePackageName: returns the (possibly empty) list of package name strings.
fn parse_package_name(c: &mut Cursor) -> PResult<Vec<StrRef>> {
    let mut names = Vec::new();
    let flag1 = c.bool_u32("packageNameFlag1")?;
    if flag1 {
        names.push(c.string()?);
        let flag23 = c.bool_u32("packageNameFlag2")?;
        if flag23 {
            names.push(c.string()?);
            names.push(c.string()?);
        }
    }
    Ok(names)
}

fn meta_pkg(names: Vec<StrRef>) -> Meta {
    Meta::List(names.into_iter().map(Meta::Str).collect())
}

fn check_size(property_size: u32, property_start: usize, pos: usize, ptype: &str) -> PResult<()> {
    if property_size as usize != pos - property_start {
        return Err(perr!(
            "Unexpected propery size. diff={} type={} start={}",
            (pos - property_start) as i64 - property_size as i64,
            ptype,
            property_start
        ));
    }
    Ok(())
}

pub fn parse_properties(
    c: &mut Cursor,
    current_entity_save_version: u32,
    object_ue5_version: i32,
) -> PResult<PropList> {
    let mut props: Vec<Property> = Vec::new();

    // Python function locals that persist across loop iterations.
    let mut enum_name: Option<StrRef> = None; // None also encodes Python's None
    let mut enum_name_is_none_value = false; // true when enumName was explicitly set to None
    let mut array_type: Option<StrRef> = None;
    let mut set_type: Option<StrRef> = None;
    let mut struct_property_type: Option<StrRef> = None;
    let mut structure_sub_type: Option<StrRef> = None;
    let mut struct_size: Option<u32> = None;
    let mut key_type: Option<StrRef> = None;
    let mut value_type: Option<StrRef> = None;

    loop {
        let property_name = c.string()?;
        if property_name.eq_ascii(c.data, "None") {
            break;
        }
        let property_type = c.string()?;
        let ptype_string = property_type.to_string(c.data);
        let ptype = ptype_string.as_str();
        let mut meta: Vec<Meta> = vec![Meta::Str(property_name), Meta::Str(property_type)];

        let property_header_flag = object_ue5_version >= 1012;
        if property_header_flag {
            let type_a = c.u32()?;
            meta.push(Meta::U32(type_a));
            if type_a == 0 {
                enum_name = None;
                enum_name_is_none_value = true;
            } else if type_a == 1 {
                match ptype {
                    "ArrayProperty" => {
                        array_type = Some(c.string()?);
                        let type_b = c.u32()?;
                        meta.push(Meta::Str(array_type.unwrap()));
                        meta.push(Meta::U32(type_b));
                        if type_b == 1 {
                            structure_sub_type = Some(c.string()?);
                            let pkg = parse_package_name(c)?;
                            meta.push(Meta::Str(structure_sub_type.unwrap()));
                            meta.push(meta_pkg(pkg));
                        } else if type_b == 2 {
                            enum_name = Some(c.string()?);
                            enum_name_is_none_value = false;
                            let pkg = parse_package_name(c)?;
                            meta.push(Meta::Str(enum_name.unwrap()));
                            meta.push(meta_pkg(pkg));
                            c.confirm_string("ByteProperty")?;
                            c.confirm_u32(0)?;
                        }
                    }
                    "ByteProperty" => {
                        enum_name = Some(c.string()?);
                        enum_name_is_none_value = false;
                        let pkg = parse_package_name(c)?;
                        meta.push(Meta::Str(enum_name.unwrap()));
                        meta.push(meta_pkg(pkg));
                    }
                    "SetProperty" => {
                        set_type = Some(c.string()?);
                        let pkg = parse_package_name(c)?;
                        meta.push(Meta::Str(set_type.unwrap()));
                        meta.push(meta_pkg(pkg));
                    }
                    "StructProperty" => {
                        struct_property_type = Some(c.string()?);
                        let pkg = parse_package_name(c)?;
                        meta.push(Meta::Str(struct_property_type.unwrap()));
                        meta.push(meta_pkg(pkg));
                    }
                    _ => {}
                }
            } else if type_a == 2 {
                match ptype {
                    "EnumProperty" => {
                        enum_name = Some(c.string()?);
                        enum_name_is_none_value = false;
                        let pkg = parse_package_name(c)?;
                        meta.push(Meta::Str(enum_name.unwrap()));
                        meta.push(meta_pkg(pkg));
                        c.confirm_string("ByteProperty")?;
                        c.confirm_u32(0)?;
                    }
                    "MapProperty" => {
                        key_type = Some(c.string()?);
                        meta.push(Meta::Str(key_type.unwrap()));
                        let has_key_type_name = c.bool_u32("MapProperty.hasKeyTypeName")?;
                        if has_key_type_name {
                            c.confirm_string("IntVector")?;
                            let pkg = parse_package_name(c)?;
                            meta.push(meta_pkg(pkg));
                        } else {
                            meta.push(Meta::Null);
                        }
                        value_type = Some(c.string()?);
                        meta.push(Meta::Str(value_type.unwrap()));
                        let has_value_type_name = c.bool_u32("MapProperty.hasValueTypeName")?;
                        if has_value_type_name {
                            let value_name = c.string()?;
                            let pkg = parse_package_name(c)?;
                            meta.push(Meta::Str(value_name));
                            meta.push(meta_pkg(pkg));
                        } else {
                            meta.push(Meta::Null);
                        }
                    }
                    _ => {}
                }
            }
        }

        let property_size = c.u32()?;
        if !property_header_flag {
            let property_index = c.u32()?;
            meta.push(Meta::U32(property_index));
        }

        let value: PropertyValue = match ptype {
            "BoolProperty" => {
                let v = c.u8()?;
                if !property_header_flag {
                    c.confirm_u8(0)?; // don't read GUID
                }
                let start = c.pos;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Bool(v)
            }
            "ByteProperty" => {
                if !property_header_flag {
                    enum_name = Some(c.string()?);
                    enum_name_is_none_value = false;
                }
                c.confirm_u8(0)?; // no GUID
                let start = c.pos;
                let en = if enum_name_is_none_value { None } else { enum_name };
                let is_none_enum = match en {
                    None => true,
                    Some(e) => e.eq_ascii(c.data, "None"),
                };
                let v = if is_none_enum {
                    ByteVal::U8(c.u8()?)
                } else {
                    ByteVal::Str(c.string()?)
                };
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Byte { enum_name: en, value: v }
            }
            "Int8Property" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.u8()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Int8(v)
            }
            "IntProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.i32()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Int(v)
            }
            "UInt32Property" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.u32()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::UInt32(v)
            }
            "Int64Property" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.i64()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Int64(v)
            }
            "FloatProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.f32()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Float(v)
            }
            "DoubleProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.f64()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Double(v)
            }
            "EnumProperty" => {
                if !property_header_flag {
                    enum_name = Some(c.string()?);
                    enum_name_is_none_value = false;
                }
                c.confirm_u8(0)?; // no GUID
                let start = c.pos;
                let v = c.string()?;
                check_size(property_size, start, c.pos, ptype)?;
                let en = if enum_name_is_none_value { None } else { enum_name };
                PropertyValue::Enum { enum_name: en, value: v }
            }
            "StrProperty" | "NameProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = c.string()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Str(v)
            }
            "TextProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = parse_text_property(c)?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Text(v)
            }
            "SetProperty" => {
                if !property_header_flag {
                    set_type = Some(c.string()?);
                }
                let zero_or_eight = c.u8()?;
                meta.push(Meta::U8(zero_or_eight));
                let start = c.pos;
                c.confirm_u32(0)?; // no ModeType
                let type_count = c.u32()?;
                let st = set_type.ok_or_else(|| perr!("SetProperty without setType"))?;
                let values = if st.eq_ascii(c.data, "UInt32Property") {
                    let mut v = Vec::with_capacity(type_count as usize);
                    for _ in 0..type_count {
                        v.push(c.u32()?);
                    }
                    SetValues::U32(v)
                } else if st.eq_ascii(c.data, "StructProperty") {
                    let mut v = Vec::with_capacity(type_count as usize);
                    for _ in 0..type_count {
                        let a = c.u64()?;
                        let b = c.u64()?;
                        v.push([a, b]);
                    }
                    SetValues::Guid(v)
                } else if st.eq_ascii(c.data, "ObjectProperty") {
                    let mut v = Vec::with_capacity(type_count as usize);
                    for _ in 0..type_count {
                        v.push(parse_object_reference(c)?);
                    }
                    SetValues::Refs(v)
                } else {
                    return Err(perr!("Unhandled SetProperty type {}", st.to_string(c.data)));
                };
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Set { set_type: st, values }
            }
            "ObjectProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let v = parse_object_reference(c)?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Object(v)
            }
            "SoftObjectProperty" => {
                c.confirm_u8(0)?;
                let start = c.pos;
                let r = parse_object_reference(c)?;
                let v = c.u32()?;
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::SoftObject(r, v)
            }
            "ArrayProperty" => {
                if !property_header_flag {
                    array_type = Some(c.string()?);
                    meta.push(Meta::Str(array_type.unwrap()));
                }
                let zero_or_eight = c.u8()?;
                meta.push(Meta::U8(zero_or_eight));
                let start = c.pos;
                let array_count = c.u32()?;
                let at = array_type.ok_or_else(|| perr!("ArrayProperty without arrayType"))?;
                let at_s = at.to_string(c.data);
                let av: ArrayValue = match at_s.as_str() {
                    "IntProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.i32()?);
                        }
                        ArrayValue::I32(v)
                    }
                    "Int64Property" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.i64()?);
                        }
                        ArrayValue::I64(v)
                    }
                    "ByteProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.u8()?);
                        }
                        ArrayValue::U8(v)
                    }
                    "FloatProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.f32()?);
                        }
                        ArrayValue::F32(v)
                    }
                    "DoubleProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.f64()?);
                        }
                        ArrayValue::F64(v)
                    }
                    "StrProperty" | "EnumProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(c.string()?);
                        }
                        ArrayValue::Str(v)
                    }
                    "SoftObjectProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            let r = parse_object_reference(c)?;
                            let x = c.u32()?;
                            v.push((r, x));
                        }
                        ArrayValue::SoftObj(v)
                    }
                    "InterfaceProperty" | "ObjectProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(parse_object_reference(c)?);
                        }
                        ArrayValue::Refs(v)
                    }
                    "TextProperty" => {
                        let mut v = Vec::with_capacity(array_count as usize);
                        for _ in 0..array_count {
                            v.push(parse_text_property(c)?);
                        }
                        ArrayValue::Text(v)
                    }
                    "StructProperty" => {
                        if !property_header_flag {
                            let name = c.string()?;
                            if name.bytes(c.data) != property_name.bytes(c.data) || name.wide != property_name.wide {
                                return Err(perr!(
                                    "Unexpected StructProperty name '{}' != propertyName '{}'",
                                    name.to_string(c.data),
                                    property_name.to_string(c.data)
                                ));
                            }
                            c.confirm_string("StructProperty")?;
                            struct_size = Some(c.u32()?);
                            c.confirm_u32(0)?;
                            structure_sub_type = Some(c.string()?);
                            meta.push(Meta::Str(structure_sub_type.unwrap()));
                            let uuid = c.data_ref(17)?;
                            if uuid.bytes(c.data).iter().any(|&b| b != 0) {
                                meta.push(Meta::Bytes(uuid));
                            } else {
                                meta.push(Meta::Null);
                            }
                        }
                        let struct_start = c.pos;
                        let sst = structure_sub_type
                            .ok_or_else(|| perr!("ArrayProperty StructProperty without structureSubType"))?;
                        let sst_s = sst.to_string(c.data);
                        let inner: ArrayValue = match sst_s.as_str() {
                            "LinearColor" => {
                                let mut v = Vec::with_capacity(array_count as usize);
                                for _ in 0..array_count {
                                    v.push([c.f32()?, c.f32()?, c.f32()?, c.f32()?]);
                                }
                                ArrayValue::LinearColor(v)
                            }
                            "Vector" => {
                                let mut v = Vec::with_capacity(array_count as usize);
                                for _ in 0..array_count {
                                    v.push([c.f64()?, c.f64()?, c.f64()?]);
                                }
                                ArrayValue::Vector(v)
                            }
                            "Guid" => {
                                let mut v = Vec::with_capacity(array_count as usize);
                                for _ in 0..array_count {
                                    v.push([c.u64()?, c.u64()?]);
                                }
                                ArrayValue::Guid(v)
                            }
                            "ConnectionData" | "BuildingConnection" | "STRUCT_ProgElevator_Floor"
                            | "Struct_InputConfiguration" => {
                                let ss = struct_size.ok_or_else(|| {
                                    perr!("Opaque modded struct array without structSize (property header format)")
                                })?;
                                let blob = c.data_ref(ss as usize)?;
                                ArrayValue::Opaque { blob, array_count }
                            }
                            "BlueprintCategoryRecord"
                            | "BlueprintSubCategoryRecord"
                            | "CachedPlayerInfo"
                            | "CachedPlayerPlatformInfo"
                            | "DockingStationVehicleTracking"
                            | "DroneTripInformation"
                            | "ElevatorFloorStopInfo"
                            | "FactoryCustomizationColorSlot"
                            | "FeetOffset"
                            | "FGCachedConnectedWire"
                            | "FGDroneFuelRuntimeData"
                            | "GCheckmarkUnlockData"
                            | "GlobalColorPreset"
                            | "GlobalPrefabIconElementSaveData"
                            | "HardDriveData"
                            | "HighlightedMarkerPair"
                            | "Hotbar"
                            | "InventoryStack"
                            | "ItemAmount"
                            | "LocalUserNetIdBundle"
                            | "MapMarker"
                            | "MessageData"
                            | "MiniGameResult"
                            | "PhaseCost"
                            | "PrefabIconElementSaveData"
                            | "PrefabTextElementSaveData"
                            | "ProjectAssemblyLaunchSequenceValue"
                            | "ResearchData"
                            | "ResearchTime"
                            | "ResourceSinkHistory"
                            | "ScannableObjectData"
                            | "ScannableResourcePair"
                            | "SchematicCost"
                            | "ShoppingListBlueprintEntry"
                            | "ShoppingListClassEntry"
                            | "ShoppingListRecipeEntry"
                            | "SpawnData"
                            | "SplinePointData"
                            | "SplitterSortRule"
                            | "SubCategoryMaterialDefault"
                            | "TimeTableStop"
                            | "VehiclePathBlock"
                            | "VehiclePathBlockReference"
                            | "VehiclePathSegmentValidationData"
                            | "WireInstance"
                            | "DTConfigStruct"
                            | "ManagedSignConnectionSettings"
                            | "ResourceNodeData"
                            | "SignComponentData"
                            | "SignComponentVariableData"
                            | "SignComponentVariableMetaData"
                            | "SwatchGroupData"
                            | "USSSwatchSaveInfo" => {
                                let mut v = Vec::with_capacity(array_count as usize);
                                for _ in 0..array_count {
                                    v.push(parse_properties(c, current_entity_save_version, object_ue5_version)?);
                                }
                                ArrayValue::Structs(v)
                            }
                            other => {
                                // Unknown (modded) struct element type: keep
                                // the raw bytes so the save still loads. The
                                // blob length falls out of the property size
                                // (== structSize when the old format carries
                                // one), so this works in both header formats.
                                let remaining =
                                    (property_size as usize).checked_sub(c.pos - start).ok_or_else(
                                        || {
                                            perr!(
                                                "Opaque struct array '{}' overruns its property size",
                                                other
                                            )
                                        },
                                    )?;
                                let blob = c.data_ref(remaining)?;
                                ArrayValue::Opaque { blob, array_count }
                            }
                        };
                        if !property_header_flag {
                            let ss = struct_size.unwrap();
                            if ss as usize != c.pos - struct_start {
                                return Err(perr!(
                                    "Unexpected StructProperty size. diff={} type={}",
                                    (c.pos - struct_start) as i64 - ss as i64,
                                    ptype
                                ));
                            }
                        }
                        inner
                    }
                    other => return Err(perr!("Unsupported ArrayProperty type '{}'", other)),
                };
                if property_size as usize != c.pos - start {
                    return Err(perr!(
                        "Unexpected propery size. diff={} propertyType={} arrayType={} arrayCount={} start={} propertyName={}",
                        (c.pos - start) as i64 - property_size as i64,
                        ptype,
                        at_s,
                        array_count,
                        start,
                        property_name.to_string(c.data)
                    ));
                }
                PropertyValue::Array(av)
            }
            "StructProperty" => {
                if !property_header_flag {
                    struct_property_type = Some(c.string()?);
                    meta.push(Meta::Str(struct_property_type.unwrap()));
                    let u1 = c.u64()?;
                    let u2 = c.u64()?;
                    if u1 != 0 || u2 != 0 {
                        meta.push(Meta::List(vec![Meta::U64(u1), Meta::U64(u2)]));
                    } else {
                        meta.push(Meta::Null);
                    }
                }
                let struct_index1 = c.u8()?;
                meta.push(Meta::U8(struct_index1));
                if struct_index1 == 9 {
                    let struct_index2 = c.u32()?;
                    meta.push(Meta::U32(struct_index2));
                }
                let start = c.pos;
                let spt = struct_property_type
                    .ok_or_else(|| perr!("StructProperty without structPropertyType"))?;
                let spt_s = spt.to_string(c.data);
                let sv: StructValue = match spt_s.as_str() {
                    "InventoryItem" => {
                        c.confirm_u32(0)?;
                        let item_name = c.string()?;
                        let has_props =
                            c.bool_u32("StructProperty.InventoryItem.itemHasPropertiesFlag")?;
                        let item_properties = if has_props {
                            c.confirm_u32(0)?;
                            let type_path = c.string()?;
                            let item_property_size = c.u32()?;
                            let item_start = c.pos;
                            let inner =
                                parse_properties(c, current_entity_save_version, object_ue5_version)?;
                            if item_property_size as usize != c.pos - item_start {
                                return Err(perr!(
                                    "Unexpected InventoryItem size. diff={}",
                                    (c.pos - item_start) as i64 - item_property_size as i64
                                ));
                            }
                            InvItemProps::Props { type_path, props: inner }
                        } else if start as i64 + property_size as i64 - c.pos as i64 == 4 {
                            c.confirm_u32(0)?;
                            InvItemProps::Two
                        } else {
                            InvItemProps::One
                        };
                        StructValue::InventoryItem { item_name, item_properties }
                    }
                    "LinearColor" => StructValue::LinearColor([c.f32()?, c.f32()?, c.f32()?, c.f32()?]),
                    "Vector2D" => StructValue::Vector2D([c.f64()?, c.f64()?]),
                    "Vector" => StructValue::Vector([c.f64()?, c.f64()?, c.f64()?]),
                    "Quat" => StructValue::Quat([c.f64()?, c.f64()?, c.f64()?, c.f64()?]),
                    "Box" => {
                        let vals = [c.f64()?, c.f64()?, c.f64()?, c.f64()?, c.f64()?, c.f64()?];
                        let flag = c.bool_u8("StructProperty.Box.flag")?;
                        StructValue::Box { vals, flag }
                    }
                    "FluidBox" => StructValue::FluidBox(c.f32()?),
                    "RailroadTrackPosition" => {
                        let r = parse_object_reference(c)?;
                        let rtp_offset = c.f32()?;
                        let forward = c.f32()?;
                        StructValue::RailroadTrackPosition(r, rtp_offset, forward)
                    }
                    "DateTime" => StructValue::DateTime(c.i64()?),
                    "ClientIdentityInfo" => {
                        let uuid = c.string()?;
                        let identity_count = c.u32()?;
                        let mut identities = Vec::with_capacity(identity_count as usize);
                        for _ in 0..identity_count {
                            let client_type = c.u8()?;
                            let client_size = c.u32()?;
                            let data = c.data_ref(client_size as usize)?;
                            identities.push((client_type, data));
                        }
                        StructValue::ClientIdentityInfo { uuid, identities }
                    }
                    "PlayerInfoHandle" | "UniqueNetIdRepl" | "Guid" | "Rotator"
                    | "SignComponentEditorMetadata" => {
                        StructValue::Raw(c.data_ref(property_size as usize)?)
                    }
                    "BlueprintRecord"
                    | "BoomBoxPlayerState"
                    | "DroneDockingStateInfo"
                    | "DroneTripInformation"
                    | "FGPlayerPortalData"
                    | "FGPortalCachedFactoryTickData"
                    | "FactoryCustomizationColorSlot"
                    | "FactoryCustomizationData"
                    | "InventoryStack"
                    | "InventoryToRespawnWith"
                    | "LightSourceControlData"
                    | "MapMarker"
                    | "PersistentGlobalIconId"
                    | "PlayerCustomizationData"
                    | "PlayerRules"
                    | "ResearchData"
                    | "ShoppingListSettings"
                    | "SwitchData"
                    | "TimerHandle"
                    | "TopLevelAssetPath"
                    | "TrainDockingRuleSet"
                    | "TrainSimulationData"
                    | "Transform"
                    | "Vector_NetQuantize"
                    | "VehiclePathValidationInfo"
                    | "BuildingConnections"
                    | "DTActiveConfig"
                    | "LBBalancerData"
                    | "ManagedSignData"
                    | "Struct_PC_PartInfo" => StructValue::Props(parse_properties(
                        c,
                        current_entity_save_version,
                        object_ue5_version,
                    )?),
                    // Unknown (modded) struct type: keep the raw bytes so the
                    // save still loads; property_size spans the whole value.
                    _ => StructValue::Raw(c.data_ref(property_size as usize)?),
                };
                if property_size as usize != c.pos - start {
                    return Err(perr!(
                        "Unexpected propery size. diff={} type={} structPropertyType={} start={}",
                        (c.pos - start) as i64 - property_size as i64,
                        ptype,
                        spt_s,
                        start
                    ));
                }
                PropertyValue::Struct(sv)
            }
            "MapProperty" => {
                if !property_header_flag {
                    key_type = Some(c.string()?);
                    value_type = Some(c.string()?);
                    meta.push(Meta::Str(key_type.unwrap()));
                    meta.push(Meta::Str(value_type.unwrap()));
                }
                let zero_or_eight = c.u8()?;
                meta.push(Meta::U8(zero_or_eight));
                let start = c.pos;
                c.confirm_u32(0)?; // no ModeType
                let n = c.u32()?;
                let kt = key_type.ok_or_else(|| perr!("MapProperty without keyType"))?;
                let vt = value_type.ok_or_else(|| perr!("MapProperty without valueType"))?;
                let kt_s = kt.to_string(c.data);
                let vt_s = vt.to_string(c.data);
                let mut entries = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let k = match kt_s.as_str() {
                        "StructProperty" => MapKey::IntVector([c.i32()?, c.i32()?, c.i32()?]),
                        "ObjectProperty" => MapKey::Ref(parse_object_reference(c)?),
                        "IntProperty" => MapKey::I32(c.i32()?),
                        "NameProperty" | "EnumProperty" | "StrProperty" => MapKey::Str(c.string()?),
                        other => return Err(perr!("Unsupported map keyType {}", other)),
                    };
                    let v = match vt_s.as_str() {
                        "StructProperty" => MapVal::Props(parse_properties(
                            c,
                            current_entity_save_version,
                            object_ue5_version,
                        )?),
                        "IntProperty" => MapVal::I32(c.i32()?),
                        "Int64Property" => MapVal::I64(c.i64()?),
                        "ByteProperty" => MapVal::U8(c.u8()?),
                        "DoubleProperty" => MapVal::F64(c.f64()?),
                        "ObjectProperty" => MapVal::Ref(parse_object_reference(c)?),
                        // Same wire format for all three: one length-prefixed
                        // string (seen in modded saves' config maps).
                        "StrProperty" | "NameProperty" | "EnumProperty" => {
                            MapVal::Str(c.string()?)
                        }
                        other => return Err(perr!("Unsupported map valueType {}", other)),
                    };
                    entries.push((k, v));
                }
                if vt_s == "StructProperty" {
                    meta.push(Meta::MapStructPropTypes);
                }
                check_size(property_size, start, c.pos, ptype)?;
                PropertyValue::Map(entries)
            }
            other => {
                return Err(perr!(
                    "Unsupported propertyType '{}' for property '{}' at offset {} of size {} bytes",
                    other,
                    property_name.to_string(c.data),
                    c.pos,
                    property_size
                ))
            }
        };

        props.push(Property { name: property_name, value, meta });
    }

    Ok(PropList { props })
}
