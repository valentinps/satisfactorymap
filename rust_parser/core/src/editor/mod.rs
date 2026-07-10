//! Save editing: byte-level transforms over the retained decompressed body
//! plus re-export to a downloadable .sav. The strict parser re-validates the
//! body after every edit, so corruption surfaces immediately instead of in
//! the game.

pub mod export;

pub use export::{effective_body, export_sav};
