//! Typed accessors over PropList mirroring how the Python collectors consume
//! getPropertyValue() results (py/convert.rs defines the Python shapes; each
//! helper here documents the shape it stands in for).

use crate::extract::find_prop;
use crate::reader::StrRef;
use crate::store::*;

/// getPropertyValue -> [innerProps, innerPropTypes] for a StructProperty of
/// nested props; Python code then indexes [0] -- this returns that PropList.
pub fn struct_props<'a>(pl: &'a PropList, data: &[u8], name: &[u8]) -> Option<&'a PropList> {
    match find_prop(pl, data, name)? {
        PropertyValue::Struct(StructValue::Props(inner)) => Some(inner),
        _ => None,
    }
}

/// getPropertyValue -> ObjectReference (ObjectProperty).
pub fn object_ref<'a>(pl: &'a PropList, data: &[u8], name: &[u8]) -> Option<&'a ObjectRef> {
    match find_prop(pl, data, name)? {
        PropertyValue::Object(r) => Some(r),
        _ => None,
    }
}

/// getPropertyValue -> int (IntProperty).
pub fn int(pl: &PropList, data: &[u8], name: &[u8]) -> Option<i32> {
    match find_prop(pl, data, name)? {
        PropertyValue::Int(n) => Some(*n),
        _ => None,
    }
}

/// getPropertyValue -> int (Int64Property).
pub fn int64(pl: &PropList, data: &[u8], name: &[u8]) -> Option<i64> {
    match find_prop(pl, data, name)? {
        PropertyValue::Int64(n) => Some(*n),
        _ => None,
    }
}

/// getPropertyValue -> float (FloatProperty; f32 -> f64 exactly like the
/// Python conversion).
pub fn float(pl: &PropList, data: &[u8], name: &[u8]) -> Option<f64> {
    match find_prop(pl, data, name)? {
        PropertyValue::Float(f) => Some(*f as f64),
        PropertyValue::Double(f) => Some(*f),
        _ => None,
    }
}

/// getPropertyValue -> float (a FluidBox struct converts to a bare Python
/// float; f32 -> f64 like FloatProperty).
pub fn fluid_box(pl: &PropList, data: &[u8], name: &[u8]) -> Option<f64> {
    match find_prop(pl, data, name)? {
        PropertyValue::Struct(StructValue::FluidBox(f)) => Some(*f as f64),
        _ => None,
    }
}

/// getPropertyValue -> bool (BoolProperty converts to Python bool).
pub fn boolean(pl: &PropList, data: &[u8], name: &[u8]) -> Option<bool> {
    match find_prop(pl, data, name)? {
        PropertyValue::Bool(b) => Some(*b != 0),
        _ => None,
    }
}

/// getPropertyValue -> str (StrProperty / NameProperty).
pub fn string<'a>(pl: &'a PropList, data: &[u8], name: &[u8]) -> Option<StrRef> {
    match find_prop(pl, data, name)? {
        PropertyValue::Str(s) => Some(*s),
        _ => None,
    }
}

/// getPropertyValue -> list of [innerProps, innerPropTypes] (ArrayProperty of
/// StructProperty); Python then indexes each entry's [0].
pub fn array_structs<'a>(pl: &'a PropList, data: &[u8], name: &[u8]) -> Option<&'a Vec<PropList>> {
    match find_prop(pl, data, name)? {
        PropertyValue::Array(ArrayValue::Structs(v)) => Some(v),
        _ => None,
    }
}

/// The InventoryItem "Item" idiom: `item[0] if isinstance(item, (list,
/// tuple)) else item` -- the item's full path either way (empty when unset).
pub fn item_path<'a>(pl: &'a PropList, data: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    match find_prop(pl, data, name)? {
        PropertyValue::Struct(StructValue::InventoryItem { item_name, .. }) => {
            Some(item_name.bytes(data))
        }
        PropertyValue::Str(s) => Some(s.bytes(data)),
        _ => None,
    }
}

/// Python's `path.rsplit(".", 1)[-1]`.
pub fn short_name(path: &[u8]) -> &[u8] {
    crate::extract::short_name(path)
}

pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
