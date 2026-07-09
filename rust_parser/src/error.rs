use pyo3::create_exception;
use pyo3::exceptions::PyException;

// Mirrors sav_parse.ParseError. The Python-visible exception type is created
// here; internal parsing uses PError and converts at the boundary.
create_exception!(sav_parse_rs, ParseError, PyException);

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
