//! Core Satisfactory .sav parsing: chunked zlib decompression and
//! property/object/level parsing into a `SaveStore` of typed values over the
//! retained decompressed buffer. No binding dependencies; the `py/` (PyO3)
//! and `wasm/` (wasm-bindgen) crates wrap this.

pub mod decompress;
pub mod error;
pub mod extract;
pub mod gamedata;
pub mod level;
pub mod mapdata;
pub mod object;
pub mod properties;
pub mod reader;
pub mod save_header;
pub mod store;
pub mod version_data;
