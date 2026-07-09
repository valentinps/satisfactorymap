//! Python bindings for the bulk extractors in sav_core::extract: the scans
//! run PyO3-free under allow_threads; only the result materialization here
//! touches Python.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};
use sav_core::extract;

use super::ParsedSavePy;

/// Exact port of sav_map_data._collectItemLocationIndex. Returns
/// dict[itemShortName, list[(instanceName, count:int)]] in the same insertion
/// order the Python implementation produces.
#[pyfunction]
pub fn collect_item_location_index(
    py: Python<'_>,
    save: PyRef<'_, ParsedSavePy>,
) -> PyResult<PyObject> {
    let store = save.store.clone();
    drop(save);

    let index = py.allow_threads(|| extract::item_location_index(&store));

    let out = PyDict::new(py);
    for (short, entries) in index {
        let list_items: Vec<PyObject> = entries
            .into_iter()
            .map(|(name, count)| {
                let name_str = PyString::new(py, &String::from_utf8_lossy(&name));
                Ok(PyTuple::new(
                    py,
                    vec![
                        name_str.into_any().unbind(),
                        count.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?
                .into_any()
                .unbind())
            })
            .collect::<PyResult<_>>()?;
        out.set_item(
            PyString::new(py, &String::from_utf8_lossy(&short)),
            PyList::new(py, list_items)?,
        )?;
    }
    Ok(out.into_any().unbind())
}

/// Projection constants, passed in from sav_map_data.py so the two sides
/// can't drift. Order: (worldToPixelScale, offsetX, offsetY, oldDescale,
/// cropLo, scaleToHighres, mapSize, pixelsPerWorldUnit).
type ProjParams = (f64, f64, f64, f64, f64, f64, f64, f64);

/// Exact port of sav_map_data._splineSegmentPolyline over one whole-save
/// scan. Returns list[(instanceName, typePath, flatPoints)].
#[pyfunction]
pub fn collect_spline_polylines(
    py: Python<'_>,
    save: PyRef<'_, ParsedSavePy>,
    type_paths: Vec<String>,
    spline_property: String,
    proj: ProjParams,
) -> PyResult<PyObject> {
    let store = save.store.clone();
    drop(save);
    let proj = extract::Proj {
        scale: proj.0,
        off_x: proj.1,
        off_y: proj.2,
        descale: proj.3,
        crop_lo: proj.4,
        to_highres: proj.5,
        map_size: proj.6,
        ppwu: proj.7,
    };

    let results = py
        .allow_threads(|| extract::spline_polylines(&store, &type_paths, &spline_property, &proj));

    let items: Vec<PyObject> = results
        .into_iter()
        .map(|(name, type_path, flat)| {
            Ok(PyTuple::new(
                py,
                vec![
                    PyString::new(py, &name).into_any().unbind(),
                    PyString::new(py, &type_path).into_any().unbind(),
                    PyList::new(py, flat)?.into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind())
        })
        .collect::<PyResult<_>>()?;
    Ok(PyList::new(py, items)?.into_any().unbind())
}
