//! Measure the CBOR-serialized MapIndex size for a save (lean-handoff
//! transfer sizing).
//!
//!     cargo run --release --example index_size -- save.sav

use sav_core::level::parse_full_save;
use sav_core::mapdata;
use sav_core::object::ClassTables;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, sav] = &args[..] else {
        eprintln!("usage: index_size <save.sav>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(sav).expect("read save");
    let store = parse_full_save(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);
    let (_payload, index) = mapdata::build_all_json(&store, None).expect("build");
    let t = std::time::Instant::now();
    let cbor = index.to_cbor().expect("serialize");
    eprintln!(
        "index cbor: {:.1} MB in {:.2?} ({} instance names)",
        cbor.len() as f64 / 1e6,
        t.elapsed(),
        index.by_instance_name.len(),
    );
    let t = std::time::Instant::now();
    let back = mapdata::index::MapIndex::from_cbor(&cbor, &store).expect("deserialize");
    eprintln!("deserialize: {:.2?} ({} names)", t.elapsed(), back.by_instance_name.len());
}
