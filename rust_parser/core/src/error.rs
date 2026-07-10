// Internal parse error; binding layers (wasm) convert at the boundary.
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

pub type PResult<T> = Result<T, PError>;

macro_rules! perr {
    ($($arg:tt)*) => {
        crate::error::PError::new(format!($($arg)*))
    };
}
pub(crate) use perr;
