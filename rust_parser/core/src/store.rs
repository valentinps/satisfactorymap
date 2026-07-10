//! SaveStore: the whole parsed save, kept compact on the Rust side.
//! Python-facing handles hold Arc<SaveStore> + indices (see py/).

use crate::reader::{DataRef, StrRef};
use crate::save_header::SaveFileInfo;
use crate::version_data::VersionData;

#[derive(Debug, Clone)]
pub struct ObjectRef {
    pub level_name: StrRef,
    pub path_name: StrRef,
}

#[derive(Debug)]
pub enum Header {
    Actor(ActorHeader),
    Component(ComponentHeader),
}

#[derive(Debug)]
pub struct ActorHeader {
    pub type_path: StrRef,
    pub root_object: StrRef,
    pub instance_name: StrRef,
    pub flags: u32,
    pub need_transform: bool,
    pub rotation: [f32; 4],
    pub position: [f32; 3],
    pub scale: [f32; 3],
    pub was_placed_in_level: bool,
    /// Offset in `SaveStore.data` of the 40-byte rotation/position/scale block.
    pub transform_off: u32,
}

#[derive(Debug)]
pub struct ComponentHeader {
    pub class_name: StrRef,
    pub root_object: StrRef,
    pub instance_name: StrRef,
    pub flags: u32,
    pub parent_actor_name: StrRef,
}

impl Header {
    pub fn instance_name(&self) -> StrRef {
        match self {
            Header::Actor(a) => a.instance_name,
            Header::Component(c) => c.instance_name,
        }
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// One parsed property list (the unit parseProperties returns). Each entry
/// carries both the value and the retained type metadata so the Python-shape
/// [prop, propTypes] pairs can be reconstructed exactly.
#[derive(Debug, Default)]
pub struct PropList {
    pub props: Vec<Property>,
}

#[derive(Debug)]
pub struct Property {
    pub name: StrRef,
    pub value: PropertyValue,
    pub meta: Vec<Meta>, // retainedPropertyType (starts [name, type, ...])
}

/// Elements of Python's retainedPropertyType lists.
#[derive(Debug)]
pub enum Meta {
    Str(StrRef),
    U8(u8),
    U32(u32),
    U64(u64),
    Null,
    Bytes(DataRef),
    List(Vec<Meta>),
    /// MapProperty with StructProperty values appends `propTypess` (one
    /// propTypes list per entry); reconstructed from the stored PropLists.
    MapStructPropTypes,
}

#[derive(Debug)]
pub enum ByteVal {
    U8(u8),
    Str(StrRef),
}

#[derive(Debug)]
pub enum TextValue {
    /// [flags, 255, isTextCultureInvariant, s]
    NoneHistory { flags: u32, invariant: u32, s: StrRef },
    /// [flags, 0, namespace, key, value]
    Base { flags: u32, namespace: StrRef, key: StrRef, value: StrRef },
    /// [flags, 3, uuid, format, [[argName, argValue, argFlags], ...]]
    ArgumentFormat { flags: u32, uuid: StrRef, format: StrRef, args: Vec<(StrRef, StrRef, u32)> },
    /// [flags, 11, tableId, textKey]
    StringTable { flags: u32, table_id: StrRef, text_key: StrRef },
}

#[derive(Debug)]
pub enum SetValues {
    U32(Vec<u32>),
    Guid(Vec<[u64; 2]>),
    Refs(Vec<ObjectRef>),
}

#[derive(Debug)]
pub enum ArrayValue {
    I32(Vec<i32>),
    I64(Vec<i64>),
    U8(Vec<u8>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Str(Vec<StrRef>),
    SoftObj(Vec<(ObjectRef, u32)>),
    Refs(Vec<ObjectRef>),
    Text(Vec<TextValue>),
    LinearColor(Vec<[f32; 4]>),
    Vector(Vec<[f64; 3]>),
    Guid(Vec<[u64; 2]>),
    /// Opaque modded blob: one bytes value then None-padding to arrayCount.
    Opaque { blob: DataRef, array_count: u32 },
    /// Array of structs: each element converts to [innerProps, innerPropTypes].
    Structs(Vec<PropList>),
}

#[derive(Debug)]
pub enum InvItemProps {
    One,
    Two,
    Props { type_path: StrRef, props: PropList },
}

#[derive(Debug)]
pub enum StructValue {
    InventoryItem { item_name: StrRef, item_properties: InvItemProps },
    LinearColor([f32; 4]),
    Vector2D([f64; 2]),
    Vector([f64; 3]),
    Quat([f64; 4]),
    Box { vals: [f64; 6], flag: bool },
    FluidBox(f32),
    RailroadTrackPosition(ObjectRef, f32, f32),
    DateTime(i64),
    ClientIdentityInfo { uuid: StrRef, identities: Vec<(u8, DataRef)> },
    Raw(DataRef),
    Props(PropList),
}

#[derive(Debug)]
pub enum MapKey {
    IntVector([i32; 3]),
    Ref(ObjectRef),
    I32(i32),
    Str(StrRef),
}

#[derive(Debug)]
pub enum MapVal {
    Props(PropList),
    I32(i32),
    I64(i64),
    U8(u8),
    F64(f64),
    Ref(ObjectRef),
}

#[derive(Debug)]
pub enum PropertyValue {
    Bool(u8),
    Byte { enum_name: Option<StrRef>, value: ByteVal },
    Int8(u8),
    Int(i32),
    UInt32(u32),
    Int64(i64),
    Float(f32),
    Double(f64),
    Enum { enum_name: Option<StrRef>, value: StrRef },
    Str(StrRef),
    Text(TextValue),
    Set { set_type: StrRef, values: SetValues },
    Object(ObjectRef),
    SoftObject(ObjectRef, u32),
    Array(ArrayValue),
    Struct(StructValue),
    Map(Vec<(MapKey, MapVal)>),
}

// ---------------------------------------------------------------------------
// Objects / actor-specific trailing data
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LightweightInstance {
    pub rotation: [f64; 4],
    pub position: [f64; 3],
    pub swatch: ObjectRef,
    pub pattern: ObjectRef,
    pub primary_color: [f32; 4],
    pub secondary_color: [f32; 4],
    pub paint_finish: ObjectRef,
    pub pattern_rotation: u8,
    pub recipe: ObjectRef,
    pub blueprint_proxy: ObjectRef,
    /// Converts to a (prop, propTypes) TUPLE (Python parity).
    pub data_property: Option<PropList>,
    pub service_provider: Option<u8>,
    pub player_info_table_index: Option<PlayerIdx>,
    /// Byte extent of this packed record in `SaveStore.data` (starts at the
    /// rotation doubles, covers everything through the trailer).
    pub record_off: u32,
    pub record_len: u32,
}

#[derive(Debug)]
pub enum PlayerIdx {
    I32(i32), // headerSaveVersion >= 57
    U8(u8),
}

/// One type-path group inside the FGLightweightBuildableSubsystem blob.
#[derive(Debug)]
pub struct LightweightGroup {
    pub type_path: StrRef,
    /// Offset in `SaveStore.data` of this group's u32 instance count.
    pub count_field_off: u32,
    /// Offset just past the group's last instance record (insertion point).
    pub end_off: u32,
    pub instances: Vec<LightweightInstance>,
}

#[derive(Debug)]
pub struct ChainBelt {
    pub belt: ObjectRef,
    /// Offset in `SaveStore.data` of the first element double (elements are
    /// contiguous 72-byte 3×3 f64 records, world-space).
    pub elements_off: u32,
    /// numElements × 3×3 doubles.
    pub elements: Vec<[[f64; 3]; 3]>,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub lead_item_index: i32,
    pub tail_item_index: i32,
}

#[derive(Debug)]
pub enum ActorSpecific {
    None,
    /// Conveyor belt items: [length, name, position]
    ConveyorBelt(Vec<(u32, StrRef, f32)>),
    /// GameMode / GameState: list of ObjectReference
    RefList(Vec<ObjectRef>),
    /// BP_PlayerState_C trailing size 1, type 3: bare int
    PlayerStateType(u8),
    /// BP_PlayerState_C 0xF1: [clientType, clientData]
    PlayerStateClient { client_type: u8, data: DataRef },
    /// DroneTransport / modded actors / modded player state: raw bytes
    RawBytes(DataRef),
    /// CircuitSubsystem: [[circuitId, ref], ...]
    Circuits(Vec<(u32, ObjectRef)>),
    /// PowerLine: [source, target]
    PowerLine(ObjectRef, ObjectRef),
    /// Locomotive/FreightWagon: [[], previous, next]
    Train { previous: ObjectRef, next: ObjectRef },
    /// Wheeled vehicles: [[name, bytes(105)], ...]
    Vehicles(Vec<(StrRef, DataRef)>),
    /// FGLightweightBuildableSubsystem: [version, [path, instances], ...]
    Lightweight { version: u32, items: Vec<LightweightGroup> },
    /// FGConveyorChainActor*: 7-element list
    ConveyorChain {
        chain_actor: ObjectRef,
        belts: Vec<ChainBelt>,
        items: Vec<(StrRef, u32)>,
        cu32: u32,
        maximum_items: i32,
        chain_lead_item_index: i32,
        chain_tail_item_index: i32,
    },
    /// FGItemPickup_Spawnable: bool
    PickupSpawnable(bool),
    /// Inventory/connection components: bool
    ComponentTrailing(bool),
}

#[derive(Debug)]
pub struct Object {
    pub object_game_version: u32,
    pub should_migrate_object_refs_to_persistent_flag: bool,
    pub per_object_version_data: Option<VersionData>,
    /// Actors only: (parentObjectReference, [componentReference, ...])
    pub actor_reference_associations: Option<(ObjectRef, Vec<ObjectRef>)>,
    pub properties: PropList,
    pub actor_specific: ActorSpecific,
}

/// Byte offsets (into `SaveStore.data`) of the count/size fields and splice
/// points the save editor needs when inserting or removing objects.
#[derive(Debug)]
pub struct LevelSpans {
    /// The u64 objectHeaderAndCollectableSize field.
    pub header_size_field_off: u32,
    /// Just past the last object header (before the persistent flag /
    /// collectables#1, which are inside the measured size region).
    pub headers_insert_off: u32,
    /// The u64 allObjectsSize field.
    pub objects_size_field_off: u32,
    /// The u32 objectCount at the start of the object-body blob (the u32
    /// actorAndComponentCount lives at header_size_field_off + 8).
    pub object_count_field_off: u32,
    /// Just past the last object body (object_start + all_objects_size).
    pub bodies_insert_off: u32,
}

#[derive(Debug)]
pub struct Level {
    /// None for the persistent level.
    pub level_name: Option<StrRef>,
    pub headers: Vec<Header>,
    /// Persistent level only.
    pub level_persistent_flag: Option<bool>,
    pub collectables1: Option<Vec<ObjectRef>>,
    /// Index-aligned with `headers`.
    pub objects: Vec<Object>,
    pub level_save_version: u32,
    pub collectables2: Vec<ObjectRef>,
    pub save_object_version_data: Option<VersionData>,
    /// (off, len) of each header record (including its u32 headerType),
    /// index-aligned with `headers`.
    pub header_spans: Vec<(u32, u32)>,
    /// (off, len) of each object body record (from the u32 gameVersion
    /// through the v53+ trailing version block), index-aligned with `objects`.
    pub object_spans: Vec<(u32, u32)>,
    pub spans: LevelSpans,
}

#[derive(Debug)]
pub struct Partition {
    pub name: StrRef,
    pub i: u32,
    pub grid_hex: u32,
    pub levels: Vec<(StrRef, u32)>,
}

pub struct SaveStore {
    /// Retained decompressed body (truncated to uncompressedSize, possibly
    /// padded with 4 zero bytes for the "Missing final array count" quirk).
    /// All StrRef/DataRef values index into this buffer.
    pub data: Vec<u8>,
    pub info: SaveFileInfo,
    pub persistent_level_version_data: Option<VersionData>,
    pub partitions: Vec<Partition>,
    pub levels: Vec<Level>,
    pub a_level_name: StrRef,
    pub drop_pod_refs: Vec<ObjectRef>,
    pub extra_refs: Vec<ObjectRef>,
    /// satisfactoryCalculatorInteractiveMapExtras parity (owned strings:
    /// mixes typePaths from data with literal quirk markers).
    pub calculator_extras: Vec<String>,
    /// Raw uncompressed .sav header (bytes 0..body_offset of the original
    /// file), retained so an edited save can be re-exported.
    pub file_header: Vec<u8>,
    /// True when the "Missing final array count" quirk appended 4 zero bytes
    /// to `data`; export must strip them to match the original body.
    pub padded: bool,
}

impl SaveStore {
    #[inline]
    pub fn s(&self, r: StrRef) -> String {
        r.to_string(&self.data)
    }
}
