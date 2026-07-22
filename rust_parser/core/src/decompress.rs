//! Chunked zlib body decompression. The chunk table is scanned first, then
//! all chunks inflate into one preallocated buffer -- in parallel with the
//! `parallel` feature (rayon), sequentially in the same chunk order without
//! it (wasm builds). Output bytes are identical either way.

use crate::error::{perr, PResult};
use crate::reader::Cursor;
use flate2::bufread::ZlibDecoder;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::io::Read;
#[cfg(feature = "parallel")]
use std::sync::atomic::{AtomicU64, Ordering};

struct Chunk {
    file_off: usize,
    comp_len: usize,
    uncomp_len: usize,
}

/// Mirrors decompressSaveFile(offset, data) including its confirm checks.
/// `progress(bytes_of_file_consumed, total_file_bytes)` fires as chunks finish.
pub fn decompress_save_file(
    data: &[u8],
    start: usize,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> PResult<Vec<u8>> {
    let mut c = Cursor::new(data, start);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut total_uncomp: usize = 0;
    while c.pos < data.len() {
        c.confirm_u32(0x9e2a83c1)?; // unrealEnginePackageSignature
        c.confirm_u32(0x22222222)?;
        c.confirm_u8(0)?;
        let _maximum_chunk_size = c.u32()?;
        c.confirm_u32(0x03000000)?;
        let comp1 = c.u64()?;
        let uncomp1 = c.u64()?;
        let comp2 = c.u64()?;
        let uncomp2 = c.u64()?;
        if comp1 != comp2 {
            return Err(perr!("Compressed size mismatch {} != {}", comp1, comp2));
        }
        if uncomp1 != uncomp2 {
            return Err(perr!("Uncompressed size mismatch {} != {}", uncomp1, uncomp2));
        }
        // Compare in u64: on wasm32 `comp1 as usize` truncates, and pos + len
        // can wrap, so a hostile 64-bit length could pass a usize check.
        let remaining = (data.len() - c.pos) as u64;
        if comp1 > remaining {
            return Err(perr!(
                "Chunk compressed length exceeds end of file by {}",
                comp1 - remaining
            ));
        }
        let comp_len = comp1 as usize;
        // zlib's maximum expansion is ~1032:1, so an uncompressed claim beyond
        // that is corrupt. Without this bound the corrupt value flows into
        // `vec![0u8; total_uncomp]` below -- a tiny damaged file claiming
        // terabytes aborts the process on allocation instead of erroring.
        if uncomp1 > comp1.saturating_mul(1032).max(64) || uncomp1 > usize::MAX as u64 {
            return Err(perr!(
                "Chunk uncompressed size {} implausible for {} compressed bytes",
                uncomp1,
                comp1
            ));
        }
        chunks.push(Chunk { file_off: c.pos, comp_len, uncomp_len: uncomp1 as usize });
        total_uncomp = total_uncomp
            .checked_add(uncomp1 as usize)
            .ok_or_else(|| perr!("Total uncompressed size overflow"))?;
        c.pos += comp_len;
    }

    // StrRef/DataRef (and the store's span/offset fields) index this buffer
    // with `usize` offsets. On 64-bit native builds (the desktop app) that's
    // 64-bit, so there is no size cap. On wasm32 `usize` is `u32`, so the body
    // must stay within a 32-bit address space -- and wasm can't hold >4GB
    // anyway. Only that build enforces the cap.
    #[cfg(target_pointer_width = "32")]
    if total_uncomp as u64 + 8 > u32::MAX as u64 {
        return Err(perr!(
            "Decompressed save is {} bytes; saves over 4GB are not supported in the browser (wasm32); use the desktop app.",
            total_uncomp
        ));
    }

    let mut out: Vec<u8> = vec![0u8; total_uncomp];

    // Carve the output into per-chunk slices for parallel inflation.
    let mut slices: Vec<(&Chunk, &mut [u8])> = Vec::with_capacity(chunks.len());
    let mut rest: &mut [u8] = &mut out;
    for ch in &chunks {
        let (head, tail) = rest.split_at_mut(ch.uncomp_len);
        slices.push((ch, head));
        rest = tail;
    }

    let total_file = data.len() as u64;

    #[cfg(feature = "parallel")]
    {
        let done_bytes = AtomicU64::new(0);
        let results: Vec<PResult<u64>> = slices
            .into_par_iter()
            .map(|(ch, dst)| {
                inflate_chunk(data, ch, dst)?;
                Ok(done_bytes.fetch_add(ch.comp_len as u64, Ordering::Relaxed)
                    + ch.comp_len as u64)
            })
            .collect();
        for r in &results {
            if let Err(e) = r {
                return Err(e.clone());
            }
        }
    }

    #[cfg(not(feature = "parallel"))]
    {
        // Sequential inflate can drive the &mut progress callback per chunk
        // (the parallel path can't share it across threads).
        let mut done_bytes: u64 = 0;
        for (ch, dst) in slices {
            inflate_chunk(data, ch, dst)?;
            done_bytes += ch.comp_len as u64;
            if let Some(cb) = progress.as_deref_mut() {
                cb(done_bytes, total_file);
            }
        }
    }

    if let Some(cb) = progress.as_deref_mut() {
        cb(total_file, total_file);
    }
    Ok(out)
}

fn inflate_chunk(data: &[u8], ch: &Chunk, dst: &mut [u8]) -> PResult<()> {
    let src = &data[ch.file_off..ch.file_off + ch.comp_len];
    let mut dec = ZlibDecoder::new(src);
    let mut written = 0usize;
    while written < dst.len() {
        match dec.read(&mut dst[written..]) {
            Ok(0) => break,
            Ok(n) => written += n,
            Err(e) => return Err(perr!("Decompression failed: {}", e)),
        }
    }
    // Confirm the stream is fully drained and exactly the right size.
    let mut extra = [0u8; 1];
    let trailing = dec.read(&mut extra).map_err(|e| perr!("Decompression failed: {}", e))?;
    if written != dst.len() || trailing != 0 {
        return Err(perr!(
            "Decompression didn't return the expected amount return={} != expected={}",
            written + trailing,
            dst.len()
        ));
    }
    Ok(())
}
