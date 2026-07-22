//! Adversarial-input tests: a corrupt or hostile save must produce Err --
//! never a panic, and never an allocator abort. This regression-locks the
//! hardening pass (Cursor::capped_capacity clamps, checked string/length
//! arithmetic, the decompress 1032:1 expansion bound): before it, a single
//! corrupted count field could request 100+ GB from the allocator and kill
//! the whole process, and a wasm32 build could trap on wrapped arithmetic.
//!
//! Approach: real save bytes (the public test-saves-v1 corpus) mutated in
//! targeted and pseudo-random ways. An abort/panic fails the suite by
//! killing the test process, so simply completing IS the assertion; the
//! Err/Ok distinction is not asserted per mutation (some flips land in
//! don't-care bytes and legitimately still parse).

use sav_core::level::parse_full_save;
use sav_core::object::ClassTables;
use sav_core::reader::Cursor;
use std::path::PathBuf;

fn save_bytes(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads").join(name);
    std::fs::read(path).expect("test save present -- run tools/fetch_test_saves.py")
}

fn parse(bytes: &[u8]) -> Result<(), String> {
    parse_full_save(bytes, &ClassTables::embedded(), None).map(|_| ()).map_err(|e| e.to_string())
}

/// Tiny deterministic PRNG (xorshift64*) -- test must not depend on rand.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

#[test]
fn truncated_saves_error_not_panic() {
    let bytes = save_bytes("All_080726-163150.sav");
    // Truncation at many lengths: inside the header, inside the chunk table,
    // mid-chunk, and just short of the end.
    let mut lengths: Vec<usize> = (0..64).collect();
    lengths.extend([100, 500, 1000, 4096, 40_000, bytes.len() / 2, bytes.len() - 1]);
    for len in lengths {
        let truncated = &bytes[..len.min(bytes.len())];
        assert!(parse(truncated).is_err(), "truncation to {len} bytes parsed successfully");
    }
}

#[test]
fn hostile_length_fields_error_not_abort() {
    let bytes = save_bytes("All_080726-163150.sav");
    // Overwrite 4-byte windows across the first 4KB (the uncompressed save
    // header + first chunk header live here) with hostile values: huge
    // counts/lengths, negative lengths, i32::MIN (whose *2 wraps on wasm32).
    for offset in (0..4096).step_by(16) {
        for hostile in [u32::MAX, 0x8000_0000, 0x7FFF_FFFF, 0xFFFF_FF00] {
            let mut corrupt = bytes.clone();
            corrupt[offset..offset + 4].copy_from_slice(&hostile.to_le_bytes());
            let _ = parse(&corrupt); // must return, not abort/panic
        }
    }
}

#[test]
fn random_single_byte_flips_never_panic() {
    let bytes = save_bytes("All_080726-163150.sav");
    let mut rng = Rng(0x5EED_CAFE_F00D_0001);
    for _ in 0..150 {
        let offset = (rng.next() as usize) % bytes.len();
        let flip = (rng.next() as u8) | 1; // never 0 -- always a real change
        let mut corrupt = bytes.clone();
        corrupt[offset] ^= flip;
        let _ = parse(&corrupt); // Err or Ok, never a panic/abort
    }
}

#[test]
fn hostile_string_lengths_error() {
    // parseString directly: i32::MIN's UTF-16 byte length (2^31 * 2) wraps to
    // 0 on wasm32 without checked arithmetic; huge positive lengths must
    // bounds-fail, not slice-panic.
    for prefix in [i32::MIN, i32::MIN + 1, i32::MAX, 0x7FFF_FFF0] {
        let mut data = prefix.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 64]);
        let mut cursor = Cursor::new(&data, 0);
        assert!(cursor.string().is_err(), "length prefix {prefix} did not error");
    }
}

#[test]
fn tiny_file_claiming_terabytes_errors_fast() {
    // A synthetic chunk header claiming a huge uncompressed size: without the
    // zlib 1032:1 expansion bound this aborted in vec![0u8; total] before any
    // data was even inflated. Reuse a real save's first bytes so the
    // uncompressed header parses, then splice a hostile chunk header.
    let bytes = save_bytes("All_080726-163150.sav");
    for hostile_uncomp in [1u64 << 40, u64::MAX / 2] {
        let mut corrupt = bytes.clone();
        // The first chunk header follows the uncompressed save header; find it
        // by its package signature and overwrite the two uncompressed-size
        // fields (offsets +17 and +33 within the 49-byte chunk header).
        let sig = 0x9e2a83c1u32.to_le_bytes();
        let start = corrupt.windows(4).position(|w| w == sig).expect("chunk signature");
        corrupt[start + 25..start + 33].copy_from_slice(&hostile_uncomp.to_le_bytes());
        corrupt[start + 41..start + 49].copy_from_slice(&hostile_uncomp.to_le_bytes());
        assert!(parse(&corrupt).is_err(), "1TB uncompressed claim parsed successfully");
    }
}
