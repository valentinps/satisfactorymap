//! Byte-level readers mirroring sav_parse.py's parse* primitives, including
//! their exact ParseError messages where feasible (the differential harness
//! compares successful parses, but keeping messages close aids debugging).

use crate::error::{perr, PResult};

/// Range into the retained decompressed buffer holding a string's content
/// bytes (null terminator excluded). `wide` marks UTF-16LE content.
///
/// `off` is `usize` so the buffer can exceed 4GB on 64-bit native builds
/// (the desktop app). On wasm32 `usize` is `u32`, so the browser build keeps
/// its 4GB address space and footprint unchanged. `len` stays `u32`: a single
/// string is never that large.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrRef {
    pub off: usize,
    pub len: u32,
    pub wide: bool,
}

pub const EMPTY_STR: StrRef = StrRef { off: 0, len: 0, wide: false };

impl StrRef {
    pub fn bytes<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.off..self.off + self.len as usize]
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Compare against an ASCII literal (wide strings never match).
    pub fn eq_ascii(&self, data: &[u8], s: &str) -> bool {
        !self.wide && self.bytes(data) == s.as_bytes()
    }
    pub fn to_string(&self, data: &[u8]) -> String {
        if self.len == 0 {
            return String::new();
        }
        if self.wide {
            let b = self.bytes(data);
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            // Validated at parse time; lossy here is unreachable.
            String::from_utf16(&units).unwrap_or_default()
        } else {
            // Validated at parse time.
            String::from_utf8_lossy(self.bytes(data)).into_owned()
        }
    }
}

/// Range into the retained buffer holding raw bytes (parseData results).
#[derive(Debug, Clone, Copy)]
pub struct DataRef {
    /// `usize` for >4GB buffers on native; `u32` on wasm32. See `StrRef`.
    pub off: usize,
    pub len: u32,
}

impl DataRef {
    pub fn bytes<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.off..self.off + self.len as usize]
    }
}

pub struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

macro_rules! prim_reader {
    ($name:ident, $ty:ty, $len:expr, $pyname:expr) => {
        #[inline]
        pub fn $name(&mut self) -> PResult<$ty> {
            let end = self.pos + $len;
            if end > self.data.len() {
                return Err(perr!(
                    "Offset {} too large for {} in {}-byte data.",
                    self.pos,
                    $pyname,
                    self.data.len()
                ));
            }
            let v = <$ty>::from_le_bytes(self.data[self.pos..end].try_into().unwrap());
            self.pos = end;
            Ok(v)
        }
    };
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Cursor { data, pos }
    }

    prim_reader!(u8, u8, 1, "uint8");
    prim_reader!(u16, u16, 2, "uint16");
    prim_reader!(i32, i32, 4, "int32");
    prim_reader!(u32, u32, 4, "uint32");
    prim_reader!(i64, i64, 8, "int64");
    prim_reader!(u64, u64, 8, "uint64");
    prim_reader!(f32, f32, 4, "float");
    prim_reader!(f64, f64, 8, "double");

    /// parseBool(..., parseUint32, ctx): strict 0/1.
    pub fn bool_u32(&mut self, ctx: &str) -> PResult<bool> {
        let start = self.pos;
        let v = self.u32()?;
        if v != 0 && v != 1 {
            return Err(perr!(
                "Oops: Inaccurate assumption of {} value.  Actual={} at offset {}",
                ctx,
                v,
                start
            ));
        }
        Ok(v != 0)
    }

    /// parseBool(..., parseUint8, ctx): strict 0/1.
    pub fn bool_u8(&mut self, ctx: &str) -> PResult<bool> {
        let start = self.pos;
        let v = self.u8()?;
        if v != 0 && v != 1 {
            return Err(perr!(
                "Oops: Inaccurate assumption of {} value.  Actual={} at offset {}",
                ctx,
                v,
                start
            ));
        }
        Ok(v != 0)
    }

    /// parseString: i32 length prefix; positive = UTF-8 with 1-byte null,
    /// negative = UTF-16LE with 2-byte null. Encoding validated eagerly so a
    /// corrupt file fails at the same point as the Python parser.
    pub fn string(&mut self) -> PResult<StrRef> {
        let len_off = self.pos;
        let strlen = self.i32()?;
        if strlen == 0 {
            return Ok(EMPTY_STR);
        }
        if strlen > 0 {
            let n = strlen as usize;
            if self.data.len() < self.pos + n {
                return Err(perr!("String length too large, size {} at offset {}.", strlen, len_off));
            }
            let content = &self.data[self.pos..self.pos + n - 1];
            if std::str::from_utf8(content).is_err() {
                return Err(perr!(
                    "String decode failure at offset {} of length {}",
                    self.pos,
                    strlen
                ));
            }
            let r = StrRef { off: self.pos, len: (n - 1) as u32, wide: false };
            self.pos += n;
            Ok(r)
        } else {
            let n = (-(strlen as i64)) as usize; // number of UTF-16 code units incl. null
            let byte_len = n * 2;
            if self.data.len() < self.pos + byte_len {
                return Err(perr!("String length too large, size {} at offset {}.", strlen, len_off));
            }
            let content = &self.data[self.pos..self.pos + byte_len - 2];
            let units: Vec<u16> = content
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            if String::from_utf16(&units).is_err() {
                return Err(perr!(
                    "String decode failure at offset {} of length {}",
                    self.pos,
                    byte_len
                ));
            }
            let r = StrRef { off: self.pos, len: (byte_len - 2) as u32, wide: true };
            self.pos += byte_len;
            Ok(r)
        }
    }

    pub fn data_ref(&mut self, len: usize) -> PResult<DataRef> {
        if self.pos + len > self.data.len() {
            return Err(perr!(
                "Offset {} too large for data of length {} in {}-byte data.",
                self.pos,
                len,
                self.data.len()
            ));
        }
        let r = DataRef { off: self.pos, len: len as u32 };
        self.pos += len;
        Ok(r)
    }

    pub fn confirm_u8(&mut self, expected: u8) -> PResult<()> {
        let start = self.pos;
        let v = self.u8()?;
        if v != expected {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}.",
                v, start, expected
            ));
        }
        Ok(())
    }

    pub fn confirm_u32(&mut self, expected: u32) -> PResult<()> {
        let start = self.pos;
        let v = self.u32()?;
        if v != expected {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}.",
                v, start, expected
            ));
        }
        Ok(())
    }

    pub fn confirm_u32_msg(&mut self, expected: u32, msg: &str) -> PResult<()> {
        let start = self.pos;
        let v = self.u32()?;
        if v != expected {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}: {}",
                v, start, expected, msg
            ));
        }
        Ok(())
    }

    pub fn confirm_f64(&mut self, expected: f64) -> PResult<()> {
        let start = self.pos;
        let v = self.f64()?;
        if v != expected {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}.",
                v, start, expected
            ));
        }
        Ok(())
    }

    pub fn confirm_string(&mut self, expected: &str) -> PResult<()> {
        let start = self.pos;
        let s = self.string()?;
        if !s.eq_ascii(self.data, expected) {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}.",
                s.to_string(self.data),
                start,
                expected
            ));
        }
        Ok(())
    }

    pub fn confirm_string_msg(&mut self, expected: &str, msg: &str) -> PResult<()> {
        let start = self.pos;
        let s = self.string()?;
        if !s.eq_ascii(self.data, expected) {
            return Err(perr!(
                "Value {} at offset {} does not match the expected value {}: {}",
                s.to_string(self.data),
                start,
                expected,
                msg
            ));
        }
        Ok(())
    }

    /// Owned-string variant used for the (non-retained) compressed file header.
    pub fn string_owned(&mut self) -> PResult<String> {
        let r = self.string()?;
        Ok(r.to_string(self.data))
    }
}
