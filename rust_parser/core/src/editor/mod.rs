//! Save editing: byte-level transforms over the retained decompressed body
//! plus re-export to a downloadable .sav. The strict parser re-validates the
//! body after every edit, so corruption surfaces immediately instead of in
//! the game.

pub mod apply;
pub mod export;
pub mod ops;
pub mod rename;
pub mod session;

pub use export::{effective_body, export_sav};
pub use ops::EditOp;
