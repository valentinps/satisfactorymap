//! Python-facing handle classes. Each holds Arc<SaveStore> + indices, so the
//! parsed save stays alive as long as any handle (e.g. the server's cached
//! saveIndex) does. All read-only.

pub mod convert;
pub mod extract;

use crate::store::*;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyString};
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// ObjectReference
// ---------------------------------------------------------------------------

#[pyclass(name = "ObjectReference", module = "sav_parse_rs")]
pub struct ObjectReferencePy {
    pub level_name: String,
    pub path_name: String,
}

#[pymethods]
impl ObjectReferencePy {
    #[new]
    #[pyo3(signature = (level_name = String::new(), path_name = String::new()))]
    fn new(level_name: String, path_name: String) -> Self {
        ObjectReferencePy { level_name, path_name }
    }

    #[getter(levelName)]
    fn level_name(&self) -> &str {
        &self.level_name
    }

    #[getter(pathName)]
    fn path_name(&self) -> &str {
        &self.path_name
    }

    fn __str__(&self) -> String {
        if self.level_name.is_empty() && self.path_name.is_empty() {
            "<ObjectReference/>".to_string()
        } else {
            format!(
                "<ObjectReference: levelName={}, pathName={}>",
                self.level_name, self.path_name
            )
        }
    }

    fn __repr__(&self) -> String {
        self.__str__()
    }
}

// ---------------------------------------------------------------------------
// SaveFileInfo
// ---------------------------------------------------------------------------

#[pyclass(name = "SaveFileInfo", module = "sav_parse_rs")]
pub struct SaveFileInfoPy {
    pub store: Arc<SaveStore>,
}

#[pymethods]
impl SaveFileInfoPy {
    #[getter(saveHeaderType)]
    fn save_header_type(&self) -> u32 {
        self.store.info.save_header_type
    }
    #[getter(saveVersion)]
    fn save_version(&self) -> u32 {
        self.store.info.save_version
    }
    #[getter(buildVersion)]
    fn build_version(&self) -> u32 {
        self.store.info.build_version
    }
    #[getter(saveName)]
    fn save_name(&self) -> &str {
        &self.store.info.save_name
    }
    #[getter(mapName)]
    fn map_name(&self) -> &str {
        &self.store.info.map_name
    }
    #[getter(mapOptions)]
    fn map_options(&self) -> &str {
        &self.store.info.map_options
    }
    #[getter(sessionName)]
    fn session_name(&self) -> &str {
        &self.store.info.session_name
    }
    #[getter(playDurationInSeconds)]
    fn play_duration(&self) -> u32 {
        self.store.info.play_duration_in_seconds
    }
    #[getter(saveDateTimeInTicks)]
    fn save_ticks(&self) -> u64 {
        self.store.info.save_date_time_in_ticks
    }
    #[getter(saveDatetime)]
    fn save_datetime(&self, py: Python<'_>) -> PyResult<PyObject> {
        // Same expression as the reference, evaluated with Python integer
        // semantics: ticks / TICKS_IN_SECOND is a correctly-rounded int/int
        // true division (ticks exceeds 2^53, so f64 widening drifts by µs).
        let ticks = self.store.info.save_date_time_in_ticks.into_pyobject(py)?;
        let ts = ticks
            .call_method1("__truediv__", (crate::save_header::TICKS_IN_SECOND,))?
            .call_method1("__sub__", (crate::save_header::EPOCH_1_TO_1970,))?;
        let datetime = py.import("datetime")?.getattr("datetime")?;
        Ok(datetime.call_method1("fromtimestamp", (ts,))?.unbind())
    }
    #[getter(sessionVisibility)]
    fn session_visibility<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyBytes> {
        pyo3::types::PyBytes::new(py, &[self.store.info.session_visibility])
    }
    #[getter(editorObjectVersion)]
    fn editor_object_version(&self) -> u32 {
        self.store.info.editor_object_version
    }
    #[getter(modMetadata)]
    fn mod_metadata(&self) -> &str {
        &self.store.info.mod_metadata
    }
    #[getter(isModdedSave)]
    fn is_modded_save(&self) -> bool {
        self.store.info.is_modded_save
    }
    #[getter(saveIdentifier)]
    fn save_identifier(&self) -> &str {
        &self.store.info.save_identifier
    }
    #[getter(saveDataHash)]
    fn save_data_hash(&self) -> Vec<u64> {
        self.store.info.save_data_hash.to_vec()
    }
    #[getter(isCreativeModeEnabled)]
    fn is_creative_mode_enabled(&self) -> bool {
        self.store.info.is_creative_mode_enabled
    }
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

macro_rules! actor_field {
    ($self:ident) => {
        match &$self.store.levels[$self.li as usize].headers[$self.hi as usize] {
            Header::Actor(a) => a,
            _ => unreachable!("ActorHeaderPy over non-actor header"),
        }
    };
}

#[pyclass(name = "ActorHeader", module = "sav_parse_rs")]
pub struct ActorHeaderPy {
    pub store: Arc<SaveStore>,
    pub li: u32,
    pub hi: u32,
    // Hot attributes are re-read across many buildMapPayload/buildSaveIndex
    // passes; memoize the converted Python objects per handle. Consumers are
    // read-only (verified), so sharing the same list object is safe.
    pub type_path_cache: OnceLock<Py<PyString>>,
    pub instance_name_cache: OnceLock<Py<PyString>>,
    pub rotation_cache: OnceLock<Py<PyList>>,
    pub position_cache: OnceLock<Py<PyList>>,
}

fn cached_str(
    py: Python<'_>,
    cache: &OnceLock<Py<PyString>>,
    data: &[u8],
    r: crate::reader::StrRef,
) -> Py<PyString> {
    if let Some(v) = cache.get() {
        return v.clone_ref(py);
    }
    let s = convert::str_ref(py, data, r).unbind();
    let _ = cache.set(s.clone_ref(py));
    s
}

fn cached_f32_list(
    py: Python<'_>,
    cache: &OnceLock<Py<PyList>>,
    vals: &[f32],
) -> PyResult<Py<PyList>> {
    if let Some(v) = cache.get() {
        return Ok(v.clone_ref(py));
    }
    let items: Vec<f64> = vals.iter().map(|&x| x as f64).collect();
    let l = PyList::new(py, items)?.unbind();
    let _ = cache.set(l.clone_ref(py));
    Ok(l)
}

#[pymethods]
impl ActorHeaderPy {
    #[getter(typePath)]
    fn type_path(&self, py: Python<'_>) -> Py<PyString> {
        cached_str(py, &self.type_path_cache, &self.store.data, actor_field!(self).type_path)
    }
    #[getter(rootObject)]
    fn root_object<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        convert::str_ref(py, &self.store.data, actor_field!(self).root_object)
    }
    #[getter(instanceName)]
    fn instance_name(&self, py: Python<'_>) -> Py<PyString> {
        cached_str(py, &self.instance_name_cache, &self.store.data, actor_field!(self).instance_name)
    }
    #[getter(flags)]
    fn flags(&self) -> u32 {
        actor_field!(self).flags
    }
    #[getter(needTransform)]
    fn need_transform(&self) -> bool {
        actor_field!(self).need_transform
    }
    #[getter(rotation)]
    fn rotation(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        cached_f32_list(py, &self.rotation_cache, &actor_field!(self).rotation)
    }
    #[getter(position)]
    fn position(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        cached_f32_list(py, &self.position_cache, &actor_field!(self).position)
    }
    #[getter(scale)]
    fn scale(&self) -> Vec<f64> {
        actor_field!(self).scale.iter().map(|&x| x as f64).collect()
    }
    #[getter(wasPlacedInLevel)]
    fn was_placed_in_level(&self) -> bool {
        actor_field!(self).was_placed_in_level
    }
}

macro_rules! component_field {
    ($self:ident) => {
        match &$self.store.levels[$self.li as usize].headers[$self.hi as usize] {
            Header::Component(c) => c,
            _ => unreachable!("ComponentHeaderPy over non-component header"),
        }
    };
}

#[pyclass(name = "ComponentHeader", module = "sav_parse_rs")]
pub struct ComponentHeaderPy {
    pub store: Arc<SaveStore>,
    pub li: u32,
    pub hi: u32,
    pub class_name_cache: OnceLock<Py<PyString>>,
    pub instance_name_cache: OnceLock<Py<PyString>>,
}

#[pymethods]
impl ComponentHeaderPy {
    #[getter(className)]
    fn class_name(&self, py: Python<'_>) -> Py<PyString> {
        cached_str(py, &self.class_name_cache, &self.store.data, component_field!(self).class_name)
    }
    #[getter(rootObject)]
    fn root_object<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        convert::str_ref(py, &self.store.data, component_field!(self).root_object)
    }
    #[getter(instanceName)]
    fn instance_name(&self, py: Python<'_>) -> Py<PyString> {
        cached_str(py, &self.instance_name_cache, &self.store.data, component_field!(self).instance_name)
    }
    #[getter(flags)]
    fn flags(&self) -> u32 {
        component_field!(self).flags
    }
    #[getter(parentActorName)]
    fn parent_actor_name<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        convert::str_ref(py, &self.store.data, component_field!(self).parent_actor_name)
    }
}

// ---------------------------------------------------------------------------
// PropertyList
// ---------------------------------------------------------------------------

#[pyclass(name = "PropertyList", module = "sav_parse_rs")]
pub struct PropertyListPy {
    pub store: Arc<SaveStore>,
    pub li: u32,
    pub oi: u32,
    /// Memoized converted values, one slot per property.
    pub converted: Mutex<Vec<Option<PyObject>>>,
}

impl PropertyListPy {
    fn props(&self) -> &PropList {
        &self.store.levels[self.li as usize].objects[self.oi as usize].properties
    }

    fn value_at(&self, py: Python<'_>, idx: usize) -> PyResult<PyObject> {
        {
            let cache = self.converted.lock().unwrap();
            if let Some(v) = &cache[idx] {
                return Ok(v.clone_ref(py));
            }
        }
        let v = convert::property_value(py, &self.store.data, &self.props().props[idx].value)?;
        let mut cache = self.converted.lock().unwrap();
        if cache[idx].is_none() {
            cache[idx] = Some(v.clone_ref(py));
        }
        Ok(v)
    }
}

#[pymethods]
impl PropertyListPy {
    fn __len__(&self) -> usize {
        self.props().props.len()
    }

    fn __getitem__(&self, py: Python<'_>, idx: isize) -> PyResult<PyObject> {
        let n = self.props().props.len() as isize;
        let i = if idx < 0 { idx + n } else { idx };
        if i < 0 || i >= n {
            return Err(pyo3::exceptions::PyIndexError::new_err("list index out of range"));
        }
        let i = i as usize;
        let name = convert::str_ref(py, &self.store.data, self.props().props[i].name)
            .into_any()
            .unbind();
        let value = self.value_at(py, i)?;
        Ok(PyList::new(py, vec![name, value])?.into_any().unbind())
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<PyObject> {
        let n = self.props().props.len();
        let mut items: Vec<PyObject> = Vec::with_capacity(n);
        for i in 0..n {
            let name = convert::str_ref(py, &self.store.data, self.props().props[i].name)
                .into_any()
                .unbind();
            let value = self.value_at(py, i)?;
            items.push(PyList::new(py, vec![name, value])?.into_any().unbind());
        }
        let list = PyList::new(py, items)?;
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    /// Rust-side getPropertyValue: converts only the matched value.
    #[pyo3(signature = (needle, case_insensitive = false))]
    pub fn get(&self, py: Python<'_>, needle: &str, case_insensitive: bool) -> PyResult<PyObject> {
        let data = &self.store.data;
        let pl = self.props();
        for (i, p) in pl.props.iter().enumerate() {
            let matched = if p.name.wide {
                let name = p.name.to_string(data);
                if case_insensitive {
                    name.to_lowercase() == needle.to_lowercase()
                } else {
                    name == needle
                }
            } else {
                let name = p.name.bytes(data);
                if case_insensitive {
                    name.eq_ignore_ascii_case(needle.as_bytes())
                } else {
                    name == needle.as_bytes()
                }
            };
            if matched {
                return self.value_at(py, i);
            }
        }
        Ok(py.None())
    }
}

// ---------------------------------------------------------------------------
// Object
// ---------------------------------------------------------------------------

#[pyclass(name = "Object", module = "sav_parse_rs")]
pub struct ObjectPy {
    pub store: Arc<SaveStore>,
    pub li: u32,
    pub oi: u32,
    pub props_cache: OnceLock<Py<PropertyListPy>>,
    pub asi_cache: OnceLock<PyObject>,
    pub instance_name_cache: OnceLock<Py<PyString>>,
}

impl ObjectPy {
    fn obj(&self) -> &Object {
        &self.store.levels[self.li as usize].objects[self.oi as usize]
    }
}

#[pymethods]
impl ObjectPy {
    #[getter(instanceName)]
    fn instance_name(&self, py: Python<'_>) -> Py<PyString> {
        let name = self.store.levels[self.li as usize].headers[self.oi as usize].instance_name();
        cached_str(py, &self.instance_name_cache, &self.store.data, name)
    }
    #[getter(objectGameVersion)]
    fn object_game_version(&self) -> u32 {
        self.obj().object_game_version
    }
    #[getter(shouldMigrateObjectRefsToPersistentFlag)]
    fn should_migrate(&self) -> bool {
        self.obj().should_migrate_object_refs_to_persistent_flag
    }
    #[getter(perObjectVersionData)]
    fn per_object_version_data(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.obj().per_object_version_data {
            None => Ok(py.None()),
            Some(vd) => convert::version_data(py, &self.store.data, vd),
        }
    }
    #[getter(actorReferenceAssociations)]
    fn actor_reference_associations(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.obj().actor_reference_associations {
            None => Ok(py.None()),
            Some((parent, components)) => {
                let refs: Vec<PyObject> = components
                    .iter()
                    .map(|r| convert::object_ref(py, &self.store.data, r))
                    .collect::<PyResult<_>>()?;
                Ok(PyList::new(
                    py,
                    vec![
                        convert::object_ref(py, &self.store.data, parent)?,
                        PyList::new(py, refs)?.into_any().unbind(),
                    ],
                )?
                .into_any()
                .unbind())
            }
        }
    }
    #[getter(properties)]
    fn properties(&self, py: Python<'_>) -> PyResult<Py<PropertyListPy>> {
        if let Some(v) = self.props_cache.get() {
            return Ok(v.clone_ref(py));
        }
        let n = self.obj().properties.props.len();
        let pl = Py::new(
            py,
            PropertyListPy {
                store: self.store.clone(),
                li: self.li,
                oi: self.oi,
                converted: Mutex::new(vec![None; n].into_iter().map(|_: Option<()>| None).collect()),
            },
        )?;
        let _ = self.props_cache.set(pl.clone_ref(py));
        Ok(pl)
    }
    #[getter(propertyTypes)]
    fn property_types(&self, py: Python<'_>) -> PyResult<PyObject> {
        let pl = &self.obj().properties;
        let mut types: Vec<PyObject> = Vec::with_capacity(pl.props.len());
        for p in &pl.props {
            types.push(convert::meta_list(py, &self.store.data, &p.meta, &p.value)?);
        }
        Ok(PyList::new(py, types)?.into_any().unbind())
    }
    #[getter(actorSpecificInfo)]
    fn actor_specific_info(&self, py: Python<'_>) -> PyResult<PyObject> {
        if let Some(v) = self.asi_cache.get() {
            return Ok(v.clone_ref(py));
        }
        let v = convert::actor_specific(py, &self.store.data, &self.obj().actor_specific)?;
        let _ = self.asi_cache.set(v.clone_ref(py));
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Level
// ---------------------------------------------------------------------------

#[pyclass(name = "Level", module = "sav_parse_rs")]
pub struct LevelPy {
    pub store: Arc<SaveStore>,
    pub li: u32,
    pub headers_cache: OnceLock<Py<PyList>>,
    pub objects_cache: OnceLock<Py<PyList>>,
}

impl LevelPy {
    fn level(&self) -> &Level {
        &self.store.levels[self.li as usize]
    }
}

#[pymethods]
impl LevelPy {
    #[getter(levelName)]
    fn level_name(&self, py: Python<'_>) -> PyObject {
        match self.level().level_name {
            None => py.None(),
            Some(s) => convert::str_ref(py, &self.store.data, s).into_any().unbind(),
        }
    }
    #[getter(levelPersistentFlag)]
    fn level_persistent_flag(&self, py: Python<'_>) -> PyObject {
        match self.level().level_persistent_flag {
            None => py.None(),
            Some(b) => pyo3::types::PyBool::new(py, b).to_owned().into_any().unbind(),
        }
    }
    #[getter(levelSaveVersion)]
    fn level_save_version(&self) -> u32 {
        self.level().level_save_version
    }
    #[getter(actorAndComponentObjectHeaders)]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        if let Some(v) = self.headers_cache.get() {
            return Ok(v.clone_ref(py));
        }
        let n = self.level().headers.len();
        let mut items: Vec<PyObject> = Vec::with_capacity(n);
        for hi in 0..n {
            let obj: PyObject = match &self.level().headers[hi] {
                Header::Actor(_) => Py::new(
                    py,
                    ActorHeaderPy {
                        store: self.store.clone(),
                        li: self.li,
                        hi: hi as u32,
                        type_path_cache: OnceLock::new(),
                        instance_name_cache: OnceLock::new(),
                        rotation_cache: OnceLock::new(),
                        position_cache: OnceLock::new(),
                    },
                )?
                .into_any(),
                Header::Component(_) => Py::new(
                    py,
                    ComponentHeaderPy {
                        store: self.store.clone(),
                        li: self.li,
                        hi: hi as u32,
                        class_name_cache: OnceLock::new(),
                        instance_name_cache: OnceLock::new(),
                    },
                )?
                .into_any(),
            };
            items.push(obj);
        }
        let list = PyList::new(py, items)?.unbind();
        let _ = self.headers_cache.set(list.clone_ref(py));
        Ok(list)
    }
    #[getter(objects)]
    fn objects(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        if let Some(v) = self.objects_cache.get() {
            return Ok(v.clone_ref(py));
        }
        let n = self.level().objects.len();
        let mut items: Vec<PyObject> = Vec::with_capacity(n);
        for oi in 0..n {
            items.push(
                Py::new(
                    py,
                    ObjectPy {
                        store: self.store.clone(),
                        li: self.li,
                        oi: oi as u32,
                        props_cache: OnceLock::new(),
                        asi_cache: OnceLock::new(),
                        instance_name_cache: OnceLock::new(),
                    },
                )?
                .into_any(),
            );
        }
        let list = PyList::new(py, items)?.unbind();
        let _ = self.objects_cache.set(list.clone_ref(py));
        Ok(list)
    }
    #[getter(collectables1)]
    fn collectables1(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.level().collectables1 {
            None => Ok(py.None()),
            Some(refs) => {
                let items: Vec<PyObject> = refs
                    .iter()
                    .map(|r| convert::object_ref(py, &self.store.data, r))
                    .collect::<PyResult<_>>()?;
                Ok(PyList::new(py, items)?.into_any().unbind())
            }
        }
    }
    #[getter(collectables2)]
    fn collectables2(&self, py: Python<'_>) -> PyResult<PyObject> {
        let items: Vec<PyObject> = self
            .level()
            .collectables2
            .iter()
            .map(|r| convert::object_ref(py, &self.store.data, r))
            .collect::<PyResult<_>>()?;
        Ok(PyList::new(py, items)?.into_any().unbind())
    }
    #[getter(saveObjectVersionData)]
    fn save_object_version_data(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.level().save_object_version_data {
            None => Ok(py.None()),
            Some(vd) => convert::version_data(py, &self.store.data, vd),
        }
    }
}

// ---------------------------------------------------------------------------
// ParsedSave
// ---------------------------------------------------------------------------

#[pyclass(name = "ParsedSave", module = "sav_parse_rs")]
pub struct ParsedSavePy {
    pub store: Arc<SaveStore>,
    pub levels_cache: OnceLock<Py<PyList>>,
}

#[pymethods]
impl ParsedSavePy {
    #[getter(saveFileInfo)]
    fn save_file_info(&self, py: Python<'_>) -> PyResult<Py<SaveFileInfoPy>> {
        Py::new(py, SaveFileInfoPy { store: self.store.clone() })
    }
    #[getter(levels)]
    fn levels(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        if let Some(v) = self.levels_cache.get() {
            return Ok(v.clone_ref(py));
        }
        let n = self.store.levels.len();
        let mut items: Vec<PyObject> = Vec::with_capacity(n);
        for li in 0..n {
            items.push(
                Py::new(
                    py,
                    LevelPy {
                        store: self.store.clone(),
                        li: li as u32,
                        headers_cache: OnceLock::new(),
                        objects_cache: OnceLock::new(),
                    },
                )?
                .into_any(),
            );
        }
        let list = PyList::new(py, items)?.unbind();
        let _ = self.levels_cache.set(list.clone_ref(py));
        Ok(list)
    }
    #[getter(persistentLevelSaveObjectVersionData)]
    fn persistent_level_version_data(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.store.persistent_level_version_data {
            None => Ok(py.None()),
            Some(vd) => convert::version_data(py, &self.store.data, vd),
        }
    }
    #[getter(partitions)]
    fn partitions(&self, py: Python<'_>) -> PyResult<PyObject> {
        let items: Vec<PyObject> = self
            .store
            .partitions
            .iter()
            .map(|p| {
                let levels: Vec<PyObject> = p
                    .levels
                    .iter()
                    .map(|(name, lhex)| {
                        Ok(PyList::new(
                            py,
                            vec![
                                convert::str_ref(py, &self.store.data, *name).into_any().unbind(),
                                lhex.into_pyobject(py)?.into_any().unbind(),
                            ],
                        )?
                        .into_any()
                        .unbind())
                    })
                    .collect::<PyResult<_>>()?;
                Ok(PyList::new(
                    py,
                    vec![
                        convert::str_ref(py, &self.store.data, p.name).into_any().unbind(),
                        p.i.into_pyobject(py)?.into_any().unbind(),
                        p.grid_hex.into_pyobject(py)?.into_any().unbind(),
                        PyList::new(py, levels)?.into_any().unbind(),
                    ],
                )?
                .into_any()
                .unbind())
            })
            .collect::<PyResult<_>>()?;
        Ok(PyList::new(py, items)?.into_any().unbind())
    }
    #[getter(aLevelName)]
    fn a_level_name<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        convert::str_ref(py, &self.store.data, self.store.a_level_name)
    }
    #[getter(dropPodObjectReferenceList)]
    fn drop_pod_refs(&self, py: Python<'_>) -> PyResult<PyObject> {
        let items: Vec<PyObject> = self
            .store
            .drop_pod_refs
            .iter()
            .map(|r| convert::object_ref(py, &self.store.data, r))
            .collect::<PyResult<_>>()?;
        Ok(PyList::new(py, items)?.into_any().unbind())
    }
    #[getter(extraObjectReferenceList)]
    fn extra_refs(&self, py: Python<'_>) -> PyResult<PyObject> {
        let items: Vec<PyObject> = self
            .store
            .extra_refs
            .iter()
            .map(|r| convert::object_ref(py, &self.store.data, r))
            .collect::<PyResult<_>>()?;
        Ok(PyList::new(py, items)?.into_any().unbind())
    }
    /// satisfactoryCalculatorInteractiveMapExtras equivalent (the shim
    /// republishes this as the module-level global for compatibility).
    #[getter(calculatorExtras)]
    fn calculator_extras(&self) -> Vec<String> {
        self.store.calculator_extras.clone()
    }
}
