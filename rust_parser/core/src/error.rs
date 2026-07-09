// Mirrors sav_parse.ParseError. The Python-visible exception type is created
// here (the orphan rule keeps the PError -> PyErr conversion in this crate);
// internal parsing uses PError and converts at the boundary. Both PyO3 items
// are gated behind the `pyo3-errors` feature so the core stays
// binding-agnostic for wasm builds.
#[cfg(feature = "pyo3-errors")]
pyo3::create_exception!(sav_parse_rs, ParseError, pyo3::exceptions::PyException);

#[derive(Debug, Clone)]
pub struct PError {
    pub msg: String,
}

impl PError {
    pub fn new(msg: impl Into<String>) -> Self {
        PError { msg: msg.into() }
    }
}

impl std::fmt::Display for PError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

#[cfg(feature = "pyo3-errors")]
impl From<PError> for pyo3::PyErr {
    fn from(e: PError) -> pyo3::PyErr {
        ParseError::new_err(e.msg)
    }
}

pub type PResult<T> = Result<T, PError>;

macro_rules! perr {
    ($($arg:tt)*) => {
        crate::error::PError::new(format!($($arg)*))
    };
}
pub(crate) use perr;
