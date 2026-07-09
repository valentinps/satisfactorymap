//! PropertyValue → Python conversion. Leaves are always real Python objects
//! (list/str/int/float/bytes/bool/ObjectReference) shaped exactly like the
//! reference parser's output, including nested [prop, propTypes] pairs.

use crate::reader::{DataRef, StrRef};
use crate::store::*;
use crate::version_data::VersionData;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyString, PyTuple};

use super::ObjectReferencePy;

pub fn str_ref<'py>(py: Python<'py>, data: &[u8], r: StrRef) -> Bound<'py, PyString> {
    if r.wide {
        PyString::new(py, &r.to_string(data))
    } else {
        // Content already validated as UTF-8 at parse time.
        PyString::new(py, unsafe { std::str::from_utf8_unchecked(r.bytes(data)) })
    }
}

pub fn data_ref<'py>(py: Python<'py>, data: &[u8], r: DataRef) -> Bound<'py, PyBytes> {
    PyBytes::new(py, r.bytes(data))
}

pub fn object_ref(py: Python<'_>, data: &[u8], r: &ObjectRef) -> PyResult<PyObject> {
    let o = ObjectReferencePy {
        level_name: r.level_name.to_string(data),
        path_name: r.path_name.to_string(data),
    };
    Ok(Py::new(py, o)?.into_any())
}

fn pylist<'py>(py: Python<'py>, items: Vec<PyObject>) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, items)
}

pub fn text_value(py: Python<'_>, data: &[u8], t: &TextValue) -> PyResult<PyObject> {
    let l: Vec<PyObject> = match t {
        TextValue::NoneHistory { flags, invariant, s } => vec![
            flags.into_pyobject(py)?.into_any().unbind(),
            255u8.into_pyobject(py)?.into_any().unbind(),
            invariant.into_pyobject(py)?.into_any().unbind(),
            str_ref(py, data, *s).into_any().unbind(),
        ],
        TextValue::Base { flags, namespace, key, value } => vec![
            flags.into_pyobject(py)?.into_any().unbind(),
            0u8.into_pyobject(py)?.into_any().unbind(),
            str_ref(py, data, *namespace).into_any().unbind(),
            str_ref(py, data, *key).into_any().unbind(),
            str_ref(py, data, *value).into_any().unbind(),
        ],
        TextValue::ArgumentFormat { flags, uuid, format, args } => {
            let arg_list: Vec<PyObject> = args
                .iter()
                .map(|(name, value, aflags)| {
                    Ok(pylist(
                        py,
                        vec![
                            str_ref(py, data, *name).into_any().unbind(),
                            str_ref(py, data, *value).into_any().unbind(),
                            aflags.into_pyobject(py)?.into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            vec![
                flags.into_pyobject(py)?.into_any().unbind(),
                3u8.into_pyobject(py)?.into_any().unbind(),
                str_ref(py, data, *uuid).into_any().unbind(),
                str_ref(py, data, *format).into_any().unbind(),
                pylist(py, arg_list)?.into_any().unbind(),
            ]
        }
        TextValue::StringTable { flags, table_id, text_key } => vec![
            flags.into_pyobject(py)?.into_any().unbind(),
            11u8.into_pyobject(py)?.into_any().unbind(),
            str_ref(py, data, *table_id).into_any().unbind(),
            str_ref(py, data, *text_key).into_any().unbind(),
        ],
    };
    Ok(pylist(py, l)?.into_any().unbind())
}

/// PropList → (properties list, propertyTypes list), both plain Python lists.
pub fn prop_list(py: Python<'_>, data: &[u8], pl: &PropList) -> PyResult<(PyObject, PyObject)> {
    let mut props: Vec<PyObject> = Vec::with_capacity(pl.props.len());
    let mut types: Vec<PyObject> = Vec::with_capacity(pl.props.len());
    for p in &pl.props {
        let name = str_ref(py, data, p.name).into_any().unbind();
        let value = property_value(py, data, &p.value)?;
        props.push(pylist(py, vec![name, value])?.into_any().unbind());
        types.push(meta_list(py, data, &p.meta, &p.value)?);
    }
    Ok((
        pylist(py, props)?.into_any().unbind(),
        pylist(py, types)?.into_any().unbind(),
    ))
}

/// Nested [prop, propTypes] pair as a Python list (struct values, array
/// elements) — the shape consumers index with el[0].
pub fn prop_types_pair(py: Python<'_>, data: &[u8], pl: &PropList) -> PyResult<PyObject> {
    let (props, types) = prop_list(py, data, pl)?;
    Ok(pylist(py, vec![props, types])?.into_any().unbind())
}

pub fn meta_list(
    py: Python<'_>,
    data: &[u8],
    meta: &[Meta],
    value: &PropertyValue,
) -> PyResult<PyObject> {
    let items: Vec<PyObject> = meta
        .iter()
        .map(|m| meta_value(py, data, m, value))
        .collect::<PyResult<_>>()?;
    Ok(pylist(py, items)?.into_any().unbind())
}

fn meta_value(py: Python<'_>, data: &[u8], m: &Meta, value: &PropertyValue) -> PyResult<PyObject> {
    Ok(match m {
        Meta::Str(s) => str_ref(py, data, *s).into_any().unbind(),
        Meta::U8(v) => v.into_pyobject(py)?.into_any().unbind(),
        Meta::U32(v) => v.into_pyobject(py)?.into_any().unbind(),
        Meta::U64(v) => v.into_pyobject(py)?.into_any().unbind(),
        Meta::Null => py.None(),
        Meta::Bytes(d) => data_ref(py, data, *d).into_any().unbind(),
        Meta::List(l) => {
            let items: Vec<PyObject> = l
                .iter()
                .map(|m| meta_value(py, data, m, value))
                .collect::<PyResult<_>>()?;
            pylist(py, items)?.into_any().unbind()
        }
        Meta::MapStructPropTypes => {
            // One propTypes list per map entry (struct-valued maps).
            let mut lists: Vec<PyObject> = Vec::new();
            if let PropertyValue::Map(entries) = value {
                for (_, v) in entries {
                    if let MapVal::Props(pl) = v {
                        let mut types: Vec<PyObject> = Vec::with_capacity(pl.props.len());
                        for p in &pl.props {
                            types.push(meta_list(py, data, &p.meta, &p.value)?);
                        }
                        lists.push(pylist(py, types)?.into_any().unbind());
                    }
                }
            }
            pylist(py, lists)?.into_any().unbind()
        }
    })
}

pub fn property_value(py: Python<'_>, data: &[u8], v: &PropertyValue) -> PyResult<PyObject> {
    Ok(match v {
        PropertyValue::Bool(b) => b.into_pyobject(py)?.into_any().unbind(),
        PropertyValue::Int8(b) => PyBytes::new(py, &[*b]).into_any().unbind(),
        PropertyValue::Int(x) => x.into_pyobject(py)?.into_any().unbind(),
        PropertyValue::UInt32(x) => x.into_pyobject(py)?.into_any().unbind(),
        PropertyValue::Int64(x) => x.into_pyobject(py)?.into_any().unbind(),
        PropertyValue::Float(x) => (*x as f64).into_pyobject(py)?.into_any().unbind(),
        PropertyValue::Double(x) => x.into_pyobject(py)?.into_any().unbind(),
        PropertyValue::Byte { enum_name, value } => {
            let en = match enum_name {
                Some(s) => str_ref(py, data, *s).into_any().unbind(),
                None => py.None(),
            };
            let val = match value {
                ByteVal::U8(b) => b.into_pyobject(py)?.into_any().unbind(),
                ByteVal::Str(s) => str_ref(py, data, *s).into_any().unbind(),
            };
            pylist(py, vec![en, val])?.into_any().unbind()
        }
        PropertyValue::Enum { enum_name, value } => {
            let en = match enum_name {
                Some(s) => str_ref(py, data, *s).into_any().unbind(),
                None => py.None(),
            };
            pylist(py, vec![en, str_ref(py, data, *value).into_any().unbind()])?
                .into_any()
                .unbind()
        }
        PropertyValue::Str(s) => str_ref(py, data, *s).into_any().unbind(),
        PropertyValue::Text(t) => text_value(py, data, t)?,
        PropertyValue::Set { set_type, values } => {
            let vals: Vec<PyObject> = match values {
                SetValues::U32(v) => v
                    .iter()
                    .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
                    .collect::<PyResult<_>>()?,
                SetValues::Guid(v) => v
                    .iter()
                    .map(|[a, b]| {
                        Ok(pylist(
                            py,
                            vec![
                                a.into_pyobject(py)?.into_any().unbind(),
                                b.into_pyobject(py)?.into_any().unbind(),
                            ],
                        )?
                        .into_any()
                        .unbind())
                    })
                    .collect::<PyResult<_>>()?,
                SetValues::Refs(v) => v
                    .iter()
                    .map(|r| object_ref(py, data, r))
                    .collect::<PyResult<_>>()?,
            };
            pylist(
                py,
                vec![
                    str_ref(py, data, *set_type).into_any().unbind(),
                    pylist(py, vals)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind()
        }
        PropertyValue::Object(r) => object_ref(py, data, r)?,
        PropertyValue::SoftObject(r, x) => pylist(
            py,
            vec![
                object_ref(py, data, r)?,
                x.into_pyobject(py)?.into_any().unbind(),
            ],
        )?
        .into_any()
        .unbind(),
        PropertyValue::Array(av) => array_value(py, data, av)?,
        PropertyValue::Struct(sv) => struct_value(py, data, sv)?,
        PropertyValue::Map(entries) => {
            let items: Vec<PyObject> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        MapKey::IntVector([a, b, c]) => pylist(
                            py,
                            vec![
                                a.into_pyobject(py)?.into_any().unbind(),
                                b.into_pyobject(py)?.into_any().unbind(),
                                c.into_pyobject(py)?.into_any().unbind(),
                            ],
                        )?
                        .into_any()
                        .unbind(),
                        MapKey::Ref(r) => object_ref(py, data, r)?,
                        MapKey::I32(x) => x.into_pyobject(py)?.into_any().unbind(),
                        MapKey::Str(s) => str_ref(py, data, *s).into_any().unbind(),
                    };
                    let val = match v {
                        MapVal::Props(pl) => prop_list(py, data, pl)?.0,
                        MapVal::I32(x) => x.into_pyobject(py)?.into_any().unbind(),
                        MapVal::I64(x) => x.into_pyobject(py)?.into_any().unbind(),
                        MapVal::U8(x) => x.into_pyobject(py)?.into_any().unbind(),
                        MapVal::F64(x) => x.into_pyobject(py)?.into_any().unbind(),
                        MapVal::Ref(r) => object_ref(py, data, r)?,
                    };
                    Ok(pylist(py, vec![key, val])?.into_any().unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(py, items)?.into_any().unbind()
        }
    })
}

fn f32_list(py: Python<'_>, vals: &[f32]) -> PyResult<PyObject> {
    let items: Vec<PyObject> = vals
        .iter()
        .map(|x| Ok((*x as f64).into_pyobject(py)?.into_any().unbind()))
        .collect::<PyResult<_>>()?;
    Ok(pylist(py, items)?.into_any().unbind())
}

fn f64_list(py: Python<'_>, vals: &[f64]) -> PyResult<PyObject> {
    let items: Vec<PyObject> = vals
        .iter()
        .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
        .collect::<PyResult<_>>()?;
    Ok(pylist(py, items)?.into_any().unbind())
}

fn array_value(py: Python<'_>, data: &[u8], av: &ArrayValue) -> PyResult<PyObject> {
    let items: Vec<PyObject> = match av {
        ArrayValue::I32(v) => v
            .iter()
            .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::I64(v) => v
            .iter()
            .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::U8(v) => v
            .iter()
            .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::F32(v) => v
            .iter()
            .map(|x| Ok((*x as f64).into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::F64(v) => v
            .iter()
            .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::Str(v) => v
            .iter()
            .map(|s| Ok(str_ref(py, data, *s).into_any().unbind()))
            .collect::<PyResult<_>>()?,
        ArrayValue::SoftObj(v) => v
            .iter()
            .map(|(r, x)| {
                Ok(pylist(
                    py,
                    vec![
                        object_ref(py, data, r)?,
                        x.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?
                .into_any()
                .unbind())
            })
            .collect::<PyResult<_>>()?,
        ArrayValue::Refs(v) => v
            .iter()
            .map(|r| object_ref(py, data, r))
            .collect::<PyResult<_>>()?,
        ArrayValue::Text(v) => v
            .iter()
            .map(|t| text_value(py, data, t))
            .collect::<PyResult<_>>()?,
        ArrayValue::LinearColor(v) => v
            .iter()
            .map(|c| f32_list(py, c))
            .collect::<PyResult<_>>()?,
        ArrayValue::Vector(v) => v
            .iter()
            .map(|c| f64_list(py, c))
            .collect::<PyResult<_>>()?,
        ArrayValue::Guid(v) => v
            .iter()
            .map(|[a, b]| {
                Ok(pylist(
                    py,
                    vec![
                        a.into_pyobject(py)?.into_any().unbind(),
                        b.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?
                .into_any()
                .unbind())
            })
            .collect::<PyResult<_>>()?,
        ArrayValue::Opaque { blob, array_count } => {
            let mut items: Vec<PyObject> = vec![data_ref(py, data, *blob).into_any().unbind()];
            while (items.len() as u32) < *array_count {
                items.push(py.None());
            }
            items
        }
        ArrayValue::Structs(v) => v
            .iter()
            .map(|pl| prop_types_pair(py, data, pl))
            .collect::<PyResult<_>>()?,
    };
    Ok(pylist(py, items)?.into_any().unbind())
}

fn struct_value(py: Python<'_>, data: &[u8], sv: &StructValue) -> PyResult<PyObject> {
    Ok(match sv {
        StructValue::InventoryItem { item_name, item_properties } => {
            let props: PyObject = match item_properties {
                InvItemProps::One => 1i64.into_pyobject(py)?.into_any().unbind(),
                InvItemProps::Two => 2i64.into_pyobject(py)?.into_any().unbind(),
                InvItemProps::Props { type_path, props } => {
                    let (p, t) = prop_list(py, data, props)?;
                    pylist(
                        py,
                        vec![str_ref(py, data, *type_path).into_any().unbind(), p, t],
                    )?
                    .into_any()
                    .unbind()
                }
            };
            pylist(
                py,
                vec![str_ref(py, data, *item_name).into_any().unbind(), props],
            )?
            .into_any()
            .unbind()
        }
        StructValue::LinearColor(c) => f32_list(py, c)?,
        StructValue::Vector2D(c) => f64_list(py, c)?,
        StructValue::Vector(c) => f64_list(py, c)?,
        StructValue::Quat(c) => f64_list(py, c)?,
        StructValue::Box { vals, flag } => {
            let mut items: Vec<PyObject> = vals
                .iter()
                .map(|x| Ok(x.into_pyobject(py)?.into_any().unbind()))
                .collect::<PyResult<Vec<_>>>()?;
            items.push(flag.into_pyobject(py)?.to_owned().into_any().unbind());
            pylist(py, items)?.into_any().unbind()
        }
        StructValue::FluidBox(x) => (*x as f64).into_pyobject(py)?.into_any().unbind(),
        StructValue::RailroadTrackPosition(r, o, f) => pylist(
            py,
            vec![
                object_ref(py, data, r)?,
                (*o as f64).into_pyobject(py)?.into_any().unbind(),
                (*f as f64).into_pyobject(py)?.into_any().unbind(),
            ],
        )?
        .into_any()
        .unbind(),
        StructValue::DateTime(x) => x.into_pyobject(py)?.into_any().unbind(),
        StructValue::ClientIdentityInfo { uuid, identities } => {
            let ids: Vec<PyObject> = identities
                .iter()
                .map(|(t, d)| {
                    Ok(pylist(
                        py,
                        vec![
                            t.into_pyobject(py)?.into_any().unbind(),
                            data_ref(py, data, *d).into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(
                py,
                vec![
                    str_ref(py, data, *uuid).into_any().unbind(),
                    pylist(py, ids)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind()
        }
        StructValue::Raw(d) => data_ref(py, data, *d).into_any().unbind(),
        StructValue::Props(pl) => prop_types_pair(py, data, pl)?,
    })
}

pub fn actor_specific(py: Python<'_>, data: &[u8], asi: &ActorSpecific) -> PyResult<PyObject> {
    Ok(match asi {
        ActorSpecific::None => py.None(),
        ActorSpecific::ConveyorBelt(items) => {
            let l: Vec<PyObject> = items
                .iter()
                .map(|(len, name, pos)| {
                    Ok(pylist(
                        py,
                        vec![
                            len.into_pyobject(py)?.into_any().unbind(),
                            str_ref(py, data, *name).into_any().unbind(),
                            (*pos as f64).into_pyobject(py)?.into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(py, l)?.into_any().unbind()
        }
        ActorSpecific::RefList(refs) => {
            let l: Vec<PyObject> = refs
                .iter()
                .map(|r| object_ref(py, data, r))
                .collect::<PyResult<_>>()?;
            pylist(py, l)?.into_any().unbind()
        }
        ActorSpecific::PlayerStateType(t) => t.into_pyobject(py)?.into_any().unbind(),
        ActorSpecific::PlayerStateClient { client_type, data: d } => pylist(
            py,
            vec![
                client_type.into_pyobject(py)?.into_any().unbind(),
                data_ref(py, data, *d).into_any().unbind(),
            ],
        )?
        .into_any()
        .unbind(),
        ActorSpecific::RawBytes(d) => data_ref(py, data, *d).into_any().unbind(),
        ActorSpecific::Circuits(circuits) => {
            let l: Vec<PyObject> = circuits
                .iter()
                .map(|(id, r)| {
                    Ok(pylist(
                        py,
                        vec![
                            id.into_pyobject(py)?.into_any().unbind(),
                            object_ref(py, data, r)?,
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(py, l)?.into_any().unbind()
        }
        ActorSpecific::PowerLine(s, t) => pylist(
            py,
            vec![object_ref(py, data, s)?, object_ref(py, data, t)?],
        )?
        .into_any()
        .unbind(),
        ActorSpecific::Train { previous, next } => pylist(
            py,
            vec![
                pylist(py, vec![])?.into_any().unbind(),
                object_ref(py, data, previous)?,
                object_ref(py, data, next)?,
            ],
        )?
        .into_any()
        .unbind(),
        ActorSpecific::Vehicles(v) => {
            let l: Vec<PyObject> = v
                .iter()
                .map(|(name, d)| {
                    Ok(pylist(
                        py,
                        vec![
                            str_ref(py, data, *name).into_any().unbind(),
                            data_ref(py, data, *d).into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(py, l)?.into_any().unbind()
        }
        ActorSpecific::Lightweight { version, items } => {
            let mut l: Vec<PyObject> = vec![version.into_pyobject(py)?.into_any().unbind()];
            for (path, instances) in items {
                let insts: Vec<PyObject> = instances
                    .iter()
                    .map(|i| lightweight_instance(py, data, i))
                    .collect::<PyResult<_>>()?;
                l.push(
                    pylist(
                        py,
                        vec![
                            str_ref(py, data, *path).into_any().unbind(),
                            pylist(py, insts)?.into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind(),
                );
            }
            pylist(py, l)?.into_any().unbind()
        }
        ActorSpecific::ConveyorChain {
            chain_actor,
            belts,
            items,
            cu32,
            maximum_items,
            chain_lead_item_index,
            chain_tail_item_index,
        } => {
            let belts_l: Vec<PyObject> = belts
                .iter()
                .map(|b| {
                    let elements: Vec<PyObject> = b
                        .elements
                        .iter()
                        .map(|nine| {
                            let rows: Vec<PyObject> = nine
                                .iter()
                                .map(|row| f64_list(py, row))
                                .collect::<PyResult<_>>()?;
                            Ok(pylist(py, rows)?.into_any().unbind())
                        })
                        .collect::<PyResult<_>>()?;
                    Ok(pylist(
                        py,
                        vec![
                            object_ref(py, data, &b.belt)?,
                            pylist(py, elements)?.into_any().unbind(),
                            b.a.into_pyobject(py)?.into_any().unbind(),
                            b.b.into_pyobject(py)?.into_any().unbind(),
                            b.c.into_pyobject(py)?.into_any().unbind(),
                            b.lead_item_index.into_pyobject(py)?.into_any().unbind(),
                            b.tail_item_index.into_pyobject(py)?.into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            let items_l: Vec<PyObject> = items
                .iter()
                .map(|(path, id)| {
                    Ok(pylist(
                        py,
                        vec![
                            str_ref(py, data, *path).into_any().unbind(),
                            id.into_pyobject(py)?.into_any().unbind(),
                        ],
                    )?
                    .into_any()
                    .unbind())
                })
                .collect::<PyResult<_>>()?;
            pylist(
                py,
                vec![
                    object_ref(py, data, chain_actor)?,
                    pylist(py, belts_l)?.into_any().unbind(),
                    pylist(py, items_l)?.into_any().unbind(),
                    cu32.into_pyobject(py)?.into_any().unbind(),
                    maximum_items.into_pyobject(py)?.into_any().unbind(),
                    chain_lead_item_index.into_pyobject(py)?.into_any().unbind(),
                    chain_tail_item_index.into_pyobject(py)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind()
        }
        ActorSpecific::PickupSpawnable(b) => b.into_pyobject(py)?.to_owned().into_any().unbind(),
        ActorSpecific::ComponentTrailing(b) => {
            b.into_pyobject(py)?.to_owned().into_any().unbind()
        }
    })
}

fn lightweight_instance(
    py: Python<'_>,
    data: &[u8],
    i: &LightweightInstance,
) -> PyResult<PyObject> {
    let data_property: PyObject = match &i.data_property {
        None => py.None(),
        Some(pl) => {
            // Python stores this one as a TUPLE (prop, propTypes).
            let (p, t) = prop_list(py, data, pl)?;
            PyTuple::new(py, vec![p, t])?.into_any().unbind()
        }
    };
    let service_provider: PyObject = match i.service_provider {
        None => py.None(),
        Some(v) => v.into_pyobject(py)?.into_any().unbind(),
    };
    let player_idx: PyObject = match &i.player_info_table_index {
        None => py.None(),
        Some(PlayerIdx::I32(v)) => v.into_pyobject(py)?.into_any().unbind(),
        Some(PlayerIdx::U8(v)) => v.into_pyobject(py)?.into_any().unbind(),
    };
    Ok(pylist(
        py,
        vec![
            f64_list(py, &i.rotation)?,
            f64_list(py, &i.position)?,
            object_ref(py, data, &i.swatch)?,
            object_ref(py, data, &i.pattern)?,
            pylist(
                py,
                vec![
                    f32_list(py, &i.primary_color)?,
                    f32_list(py, &i.secondary_color)?,
                ],
            )?
            .into_any()
            .unbind(),
            object_ref(py, data, &i.paint_finish)?,
            i.pattern_rotation.into_pyobject(py)?.into_any().unbind(),
            object_ref(py, data, &i.recipe)?,
            object_ref(py, data, &i.blueprint_proxy)?,
            data_property,
            service_provider,
            player_idx,
        ],
    )?
    .into_any()
    .unbind())
}

/// SaveObjectVersionData → Python list shape.
pub fn version_data(py: Python<'_>, data: &[u8], vd: &VersionData) -> PyResult<PyObject> {
    let customs: Vec<PyObject> = vd
        .custom_versions
        .iter()
        .zip(vd.custom_version_numbers.iter())
        .map(|([a, b], v)| {
            Ok(pylist(
                py,
                vec![
                    a.into_pyobject(py)?.into_any().unbind(),
                    b.into_pyobject(py)?.into_any().unbind(),
                    v.into_pyobject(py)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind())
        })
        .collect::<PyResult<_>>()?;
    Ok(pylist(
        py,
        vec![
            vd.version.into_pyobject(py)?.into_any().unbind(),
            pylist(
                py,
                vec![
                    vd.file_version_ue4.into_pyobject(py)?.into_any().unbind(),
                    vd.file_version_ue5.into_pyobject(py)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind(),
            vd.licensee_version.into_pyobject(py)?.into_any().unbind(),
            pylist(
                py,
                vec![
                    vd.engine_major.into_pyobject(py)?.into_any().unbind(),
                    vd.engine_minor.into_pyobject(py)?.into_any().unbind(),
                    vd.engine_patch.into_pyobject(py)?.into_any().unbind(),
                    vd.engine_changelist.into_pyobject(py)?.into_any().unbind(),
                    str_ref(py, data, vd.engine_branch).into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind(),
            pylist(py, customs)?.into_any().unbind(),
        ],
    )?
    .into_any()
    .unbind())
}
