//! Self-regression gate for perf refactors: dump the payload JSON and the
//! save-index dump for a .sav so two builds can be byte-diffed. (The Python
//! oracle was removed once the port gated green; this pins the Rust output
//! against itself across changes that must not alter behavior.)
//!
//!     cargo run --release --features parallel --example dump_payload -- \
//!         save.sav out_payload.json out_index.json

use sav_core::level::{parse_full_save, parse_full_save_lean};
use sav_core::mapdata::{self, index::MapIndex};
use sav_core::object::ClassTables;
use std::sync::atomic::{AtomicU64, Ordering};

// Allocation counters: how hard does the parse hit the allocator? (The wasm
// build spends its time there; these numbers size the fix.)
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn report(label: &str) {
    eprintln!(
        "{}: {}M allocs, {}M reallocs, {:.1} GB requested",
        label,
        ALLOCS.load(Ordering::Relaxed) / 1_000_000,
        REALLOCS.load(Ordering::Relaxed) / 1_000_000,
        BYTES.load(Ordering::Relaxed) as f64 / 1e9,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, sav, payload_out, index_out] = &args[..] else {
        eprintln!("usage: dump_payload <save.sav> <payload.json> <index.json>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(sav).expect("read save");
    // DUMP_LEAN=1 exercises the standing production path: a lean parse (no
    // resident object model) with the builder re-parsing on demand. Output
    // must be byte-identical to the default full-parse path.
    let lean = std::env::var("DUMP_LEAN").is_ok();
    let t = std::time::Instant::now();
    let store = if lean {
        parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse")
    } else {
        parse_full_save(&bytes, &ClassTables::embedded(), None).expect("parse")
    };
    eprintln!("parse{}: {:.2?}", if lean { " (lean)" } else { "" }, t.elapsed());
    report("after parse");
    drop(bytes);

    let t = std::time::Instant::now();
    let (payload, index) = mapdata::build_all_json(&store, None).expect("build");
    eprintln!("payload+index build: {:.2?} ({} payload bytes)", t.elapsed(), payload.len());
    report("after build");
    std::fs::write(payload_out, payload).expect("write payload");
    std::fs::write(index_out, index.dump(&store).to_string()).expect("write index");
}
