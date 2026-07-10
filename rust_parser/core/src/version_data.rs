//! SaveObjectVersionData blocks (save version >= 53).
//! Python shape: [version, [ue4, ue5], licensee, [maj, min, patch, changelist, branch], [[uuidA, uuidB, ver], ...]]

use crate::error::PResult;
use crate::reader::{Cursor, StrRef};

#[derive(Debug, Clone)]
pub struct VersionData {
    pub version: u32,
    pub file_version_ue4: u32,
    pub file_version_ue5: u32,
    pub licensee_version: u32,
    pub engine_major: u16,
    pub engine_minor: u16,
    pub engine_patch: u16,
    pub engine_changelist: u32,
    pub engine_branch: StrRef,
    pub custom_versions: Vec<[u64; 2]>,
    pub custom_version_numbers: Vec<u32>,
}

pub fn parse_save_object_version_data(c: &mut Cursor) -> PResult<VersionData> {
    let version = c.u32()?;
    let file_version_ue4 = c.u32()?;
    let file_version_ue5 = c.u32()?;
    let licensee_version = c.u32()?;
    let engine_major = c.u16()?;
    let engine_minor = c.u16()?;
    let engine_patch = c.u16()?;
    let engine_changelist = c.u32()?;
    let engine_branch = c.string()?;
    let count = c.u32()?;
    let mut custom_versions = Vec::with_capacity(count as usize);
    let mut custom_version_numbers = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let a = c.u64()?;
        let b = c.u64()?;
        let v = c.u32()?;
        custom_versions.push([a, b]);
        custom_version_numbers.push(v);
    }
    Ok(VersionData {
        version,
        file_version_ue4,
        file_version_ue5,
        licensee_version,
        engine_major,
        engine_minor,
        engine_patch,
        engine_changelist,
        engine_branch,
        custom_versions,
        custom_version_numbers,
    })
}
