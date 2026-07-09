mod decompress;
mod error;
mod level;
mod object;
mod properties;
mod py;
mod reader;
mod save_header;
mod store;
mod version_data;

use object::ClassTables;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// M2 test helper: header fields as a dict.
#[pyfunction]
fn read_save_info(py: Python<'_>, filename: &str) -> PyResult<PyObject> {
    let data = std::fs::read(filename)
        .map_err(|e| error::ParseError::new_err(format!("Cannot read {}: {}", filename, e)))?;
    let (info, _) = save_header::parse_save_file_info(&data)?;
    let d = PyDict::new(py);
    d.set_item("saveHeaderType", info.save_header_type)?;
    d.set_item("saveVersion", info.save_version)?;
    d.set_item("buildVersion", info.build_version)?;
    d.set_item("saveName", &info.save_name)?;
    d.set_item("mapName", &info.map_name)?;
    d.set_item("mapOptions", &info.map_options)?;
    d.set_item("sessionName", &info.session_name)?;
    d.set_item("playDurationInSeconds", info.play_duration_in_seconds)?;
    d.set_item("saveDateTimeInTicks", info.save_date_time_in_ticks)?;
    d.set_item("sessionVisibility", PyBytes::new(py, &[info.session_visibility]))?;
    d.set_item("editorObjectVersion", info.editor_object_version)?;
    d.set_item("modMetadata", &info.mod_metadata)?;
    d.set_item("isModdedSave", info.is_modded_save)?;
    d.set_item("saveIdentifier", &info.save_identifier)?;
    d.set_item("saveDataHash", info.save_data_hash.to_vec())?;
    d.set_item("isCreativeModeEnabled", info.is_creative_mode_enabled)?;
    Ok(d.into_any().unbind())
}

/// M2 test helper: full decompressed body (untruncated) as bytes.
#[pyfunction]
fn decompress_body(py: Python<'_>, filename: &str) -> PyResult<PyObject> {
    let data = std::fs::read(filename)
        .map_err(|e| error::ParseError::new_err(format!("Cannot read {}: {}", filename, e)))?;
    let (_, offset) = save_header::parse_save_file_info(&data)?;
    let out = py.allow_threads(|| decompress::decompress_save_file(&data, offset, None))?;
    Ok(PyBytes::new(py, &out).into_any().unbind())
}

/// Full parse. `progress` (optional) is called as progress(phase, current,
/// total) from the main thread at ~100ms cadence; phase 0 = decompression
/// (file bytes), phase 1 = parsing (level bytes).
#[pyfunction]
#[pyo3(signature = (filename, conveyor_belts, progress = None))]
fn read_full_save_file(
    py: Python<'_>,
    filename: &str,
    conveyor_belts: Vec<String>,
    progress: Option<PyObject>,
) -> PyResult<py::ParsedSavePy> {
    let file_data = std::fs::read(filename)
        .map_err(|e| error::ParseError::new_err(format!("Cannot read {}: {}", filename, e)))?;
    let tables = ClassTables { conveyor_belts };

    let store = match progress {
        None => py.allow_threads(|| level::parse_full_save(&file_data, &tables, None))?,
        Some(cb) => {
            let phase = AtomicU8::new(0);
            let cur = AtomicU64::new(0);
            let total = AtomicU64::new(0);
            let result = py.allow_threads(|| {
                std::thread::scope(|s| {
                    let phase_ref = &phase;
                    let cur_ref = &cur;
                    let total_ref = &total;
                    let handle = s.spawn(move || {
                        let mut pf = |p: u8, c: u64, t: u64| {
                            phase_ref.store(p, Ordering::Relaxed);
                            cur_ref.store(c, Ordering::Relaxed);
                            total_ref.store(t, Ordering::Relaxed);
                        };
                        let pf_dyn: level::ProgressFn = &mut pf;
                        level::parse_full_save(&file_data, &tables, Some(pf_dyn))
                    });
                    let mut last: (u8, u64, u64) = (255, u64::MAX, u64::MAX);
                    loop {
                        if handle.is_finished() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let snap = (
                            phase.load(Ordering::Relaxed),
                            cur.load(Ordering::Relaxed),
                            total.load(Ordering::Relaxed),
                        );
                        if snap != last && snap.2 > 0 {
                            last = snap;
                            // Callback errors are swallowed: progress display
                            // must never abort or deadlock the parse.
                            Python::with_gil(|py| {
                                let _ = cb.call1(py, (snap.0, snap.1, snap.2));
                            });
                        }
                    }
                    handle.join().expect("parse thread panicked")
                })
            })?;
            // Final 100% callback for each known phase total.
            let t = total.load(Ordering::Relaxed);
            if t > 0 {
                let _ = cb.call1(py, (phase.load(Ordering::Relaxed), t, t));
            }
            result
        }
    };

    Ok(py::ParsedSavePy { store: Arc::new(store), levels_cache: OnceLock::new() })
}

/// getPropertyValue with the dispatch in Rust: fast path for Rust-backed
/// PropertyList handles, reference-equivalent loop for plain Python lists
/// (nested, already-converted property lists).
#[pyfunction]
#[pyo3(signature = (properties, needle, case_insensitive = false))]
fn get_property_value(
    py: Python<'_>,
    properties: &Bound<'_, PyAny>,
    needle: &str,
    case_insensitive: bool,
) -> PyResult<PyObject> {
    if let Ok(pl) = properties.downcast::<py::PropertyListPy>() {
        return pl.borrow().get(py, needle, case_insensitive);
    }
    for item in properties.try_iter()? {
        let item = item?;
        let name_obj = item.get_item(0)?;
        let name: &str = name_obj.extract()?;
        let matched = if case_insensitive {
            name.eq_ignore_ascii_case(needle)
        } else {
            name == needle
        };
        if matched {
            return Ok(item.get_item(1)?.unbind());
        }
    }
    Ok(py.None())
}

#[pymodule]
fn sav_parse_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(get_property_value, m)?)?;
    m.add_function(wrap_pyfunction!(read_save_info, m)?)?;
    m.add_function(wrap_pyfunction!(decompress_body, m)?)?;
    m.add_function(wrap_pyfunction!(read_full_save_file, m)?)?;
    m.add_class::<py::ParsedSavePy>()?;
    m.add_class::<py::SaveFileInfoPy>()?;
    m.add_class::<py::LevelPy>()?;
    m.add_class::<py::ActorHeaderPy>()?;
    m.add_class::<py::ComponentHeaderPy>()?;
    m.add_class::<py::ObjectPy>()?;
    m.add_class::<py::PropertyListPy>()?;
    m.add_class::<py::ObjectReferencePy>()?;
    m.add("ParseError", m.py().get_type::<error::ParseError>())?;
    Ok(())
}
