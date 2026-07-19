//! Re-emit a .sav file: retained uncompressed header + chunked zlib body.
//! Exact inverse of decompress.rs. Output is body-identical to the input
//! save, not file-identical (our zlib streams differ from the game's; only
//! the decompressed content matters to the game).

use crate::store::SaveStore;
use flate2::{read::ZlibEncoder, Compression};
use std::io::Read;

/// Uncompressed bytes per chunk, matching what the game writes.
const CHUNK_SIZE: usize = 131072;
/// The game writes this constant in the chunk header's maximumChunkSize
/// field (it does not reflect the actual chunk size).
const MAX_CHUNK_SIZE_FIELD: u32 = 0x200;

/// The body as it exists in the original file: `SaveStore.data` minus the
/// 4 zero bytes the parser appends for the "Missing final array count" quirk.
pub fn effective_body(store: &SaveStore) -> &[u8] {
    let data = &store.data[..];
    if store.padded { &data[..data.len() - 4] } else { data }
}

/// Serialize a complete .sav: `file_header` verbatim, then the body split
/// into CHUNK_SIZE slices, each zlib-compressed behind the 49-byte chunk
/// header decompress.rs expects.
pub fn export_sav(file_header: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file_header.len() + body.len() / 2);
    out.extend_from_slice(file_header);

    let mut comp_buf: Vec<u8> = Vec::with_capacity(CHUNK_SIZE);
    for chunk in body.chunks(CHUNK_SIZE) {
        comp_buf.clear();
        let mut enc = ZlibEncoder::new(chunk, Compression::default());
        enc.read_to_end(&mut comp_buf).expect("zlib compression cannot fail on in-memory data");

        out.extend_from_slice(&0x9e2a83c1u32.to_le_bytes()); // UE package signature
        out.extend_from_slice(&0x22222222u32.to_le_bytes());
        out.push(0u8);
        out.extend_from_slice(&MAX_CHUNK_SIZE_FIELD.to_le_bytes());
        out.extend_from_slice(&0x03000000u32.to_le_bytes()); // algorithm: zlib
        let comp = comp_buf.len() as u64;
        let uncomp = chunk.len() as u64;
        out.extend_from_slice(&comp.to_le_bytes());
        out.extend_from_slice(&uncomp.to_le_bytes());
        out.extend_from_slice(&comp.to_le_bytes());
        out.extend_from_slice(&uncomp.to_le_bytes());
        out.extend_from_slice(&comp_buf);
    }
    out
}
