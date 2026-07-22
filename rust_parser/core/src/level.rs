//! parseLevel + the top-level readFullSaveFile flow (SaveStore assembly).

use crate::decompress::decompress_save_file;
use crate::error::{perr, PResult};
use crate::object::{parse_object, skip_object, ClassTables};
use crate::properties::parse_object_reference;
use crate::reader::Cursor;
use crate::save_header::parse_save_file_info;
use crate::store::*;
use crate::version_data::parse_save_object_version_data;

/// Progress callback: (phase, current, total). Phase 0 = decompression
/// (units: compressed file bytes), phase 1 = parsing (units: level bytes).
pub type ProgressFn<'a> = &'a mut dyn FnMut(u8, u64, u64);

/// One object-header record ([u32 headerType][actor or component fields]).
/// Also used standalone by the cross-save clipboard, whose blobs are exactly
/// these records (StrRefs come out relative to the cursor's buffer).
pub(crate) fn parse_one_header(c: &mut Cursor) -> PResult<Header> {
    let header_type = c.u32()?;
    match header_type {
        1 => {
            let type_path = c.string()?;
            let root_object = c.string()?;
            let instance_name = c.string()?;
            let flags = c.u32()?;
            let need_transform = c.bool_u32("needTransform")?;
            if c.pos + 40 > c.data.len() {
                return Err(perr!(
                    "Offset {} too large for ActorHeader transform in {}-byte data.",
                    c.pos,
                    c.data.len()
                ));
            }
            let transform_off = c.pos;
            let f = |i: usize| -> f32 {
                f32::from_le_bytes(c.data[c.pos + i * 4..c.pos + i * 4 + 4].try_into().unwrap())
            };
            let rotation = [f(0), f(1), f(2), f(3)];
            let position = [f(4), f(5), f(6)];
            let scale = [f(7), f(8), f(9)];
            c.pos += 40;
            let was_placed_in_level = c.bool_u32("wasPlacedInLevel")?;
            Ok(Header::Actor(ActorHeader {
                type_path,
                root_object,
                instance_name,
                flags,
                need_transform,
                rotation,
                position,
                scale,
                was_placed_in_level,
                transform_off,
            }))
        }
        0 => {
            let class_name = c.string()?;
            let root_object = c.string()?;
            let instance_name = c.string()?;
            let flags = c.u32()?;
            let parent_actor_name = c.string()?;
            Ok(Header::Component(ComponentHeader {
                class_name,
                root_object,
                instance_name,
                flags,
                parent_actor_name,
            }))
        }
        other => Err(perr!("Invalid headerType {}", other)),
    }
}

fn parse_headers_and_level(
    c: &mut Cursor,
    header_save_version: u32,
    persistent_level_ue5_version: i32,
    persistent_level_flag: bool,
    tables: &ClassTables,
    calculator_extras: &mut Vec<String>,
    progress: &mut Option<ProgressFn>,
    progress_base: u64,
    progress_total: u64,
    // Lean mode: record object spans by skipping over each record instead of
    // parsing it; Level.objects comes out None (headers/spans identical).
    lean: bool,
) -> PResult<Level> {
    let level_start = c.pos;
    let level_name = if persistent_level_flag { None } else { Some(c.string()?) };

    let header_size_field_off = c.pos;
    let object_header_and_collectable1_size = c.u64()? as usize;
    let header_start = c.pos;
    let actor_and_component_count = c.u32()?;

    let capped = c.capped_capacity(actor_and_component_count as usize, 12);
    let mut headers: Vec<Header> = Vec::with_capacity(capped);
    let mut header_spans: Vec<(usize, u32)> = Vec::with_capacity(capped);
    let mut last_report = c.pos;
    for _ in 0..actor_and_component_count {
        let header_record_start = c.pos;
        let h = parse_one_header(c)?;
        headers.push(h);
        header_spans.push((header_record_start, (c.pos - header_record_start) as u32));
        if let Some(cb) = progress.as_deref_mut() {
            if c.pos - last_report > 1 << 20 {
                cb(1, progress_base + (c.pos - level_start) as u64, progress_total);
                last_report = c.pos;
            }
        }
    }

    let headers_insert_off = c.pos;

    let mut level_persistent_flag = None;
    if persistent_level_flag {
        let flag = c.bool_u32("Level Persistent Flag")?;
        level_persistent_flag = Some(flag);
        if flag {
            c.confirm_string_msg("Persistent_Level", "Level Persistent String")?;
        }
    }

    // Collectables #1
    let mut collectables1: Option<Vec<ObjectRef>> = None;
    if object_header_and_collectable1_size != c.pos - header_start {
        let mut v = Vec::new();
        let n = c.u32()?;
        for _ in 0..n {
            v.push(parse_object_reference(c)?);
        }
        collectables1 = Some(v);
    }
    if object_header_and_collectable1_size != c.pos - header_start {
        return Err(perr!(
            "Level actor/object size mismatch: expect={} != actual={}",
            object_header_and_collectable1_size,
            c.pos - header_start
        ));
    }

    // Objects blob (separate cursor; the main cursor jumps over it)
    let objects_size_field_off = c.pos;
    let all_objects_size = c.u64()? as usize;
    let object_start = c.pos;
    let mut oc = Cursor::new(c.data, object_start);
    c.pos += all_objects_size;

    let spans = LevelSpans {
        header_size_field_off,
        headers_insert_off,
        objects_size_field_off,
        object_count_field_off: object_start,
        bodies_insert_off: object_start + all_objects_size,
    };

    let level_save_version = c.u32()?;

    let mut collectables2 = Vec::new();
    let mut save_object_version_data = None;
    let object_ue5_version: i32;
    if !persistent_level_flag {
        let mut v: i32 = -1;
        let n = c.u32()?;
        for _ in 0..n {
            collectables2.push(parse_object_reference(c)?);
        }
        if header_save_version >= 53 {
            let has = c.bool_u32("hasSaveObjectVersionData")?;
            if has {
                let vd = parse_save_object_version_data(c)?;
                v = vd.file_version_ue5 as i32;
                save_object_version_data = Some(vd);
            }
        }
        object_ue5_version = v;
    } else {
        object_ue5_version = persistent_level_ue5_version;
    }

    let object_count = oc.u32()?;
    if object_count != actor_and_component_count {
        return Err(perr!(
            "Object count mismatch: objectCount={} != actorAndComponentCount={}",
            object_count,
            actor_and_component_count
        ));
    }
    let capped = c.capped_capacity(actor_and_component_count as usize, 8);
    let mut objects = Vec::with_capacity(if lean { 0 } else { capped });
    let mut object_spans: Vec<(usize, u32)> = Vec::with_capacity(capped);
    let mut last_report = oc.pos;
    for idx in 0..actor_and_component_count as usize {
        let object_record_start = oc.pos;
        if lean {
            skip_object(&mut oc)?;
        } else {
            let obj = parse_object(
                &mut oc,
                header_save_version,
                object_ue5_version,
                &headers[idx],
                tables,
                calculator_extras,
            )?;
            objects.push(obj);
        }
        object_spans.push((object_record_start, (oc.pos - object_record_start) as u32));
        if let Some(cb) = progress.as_deref_mut() {
            if oc.pos - last_report > 1 << 20 {
                cb(
                    1,
                    progress_base + (oc.pos - object_start + header_start - level_start) as u64
                        + object_header_and_collectable1_size as u64,
                    progress_total,
                );
                last_report = oc.pos;
            }
        }
    }
    if oc.pos - object_start != all_objects_size {
        return Err(perr!(
            "Object size mismatch: expect={} != actual={}",
            all_objects_size,
            oc.pos - object_start
        ));
    }

    Ok(Level {
        level_name,
        headers,
        level_persistent_flag,
        collectables1,
        objects: if lean { None } else { Some(objects) },
        object_ue5_version,
        level_save_version,
        collectables2,
        save_object_version_data,
        header_spans,
        object_spans,
        spans,
    })
}

/// Full readFullSaveFile flow, EAGERLY building the per-object model. `file_data`
/// is the raw .sav contents. Production (browser + desktop) loads via
/// `parse_full_save_lean` and never builds the model; this eager variant is the
/// differential test oracle (`lazy_objects.rs` checks the lean span-reparse
/// against it) and the examples. Not a production path.
pub fn parse_full_save(
    file_data: &[u8],
    tables: &ClassTables,
    progress: Option<ProgressFn>,
) -> PResult<SaveStore> {
    parse_full_save_impl(file_data, tables, progress, false)
}

/// parse_full_save, but the object model is skipped (every Level.objects is
/// None; headers/spans/data identical). The payload/index builder re-parses
/// objects on demand from their spans, so this is the standing load path --
/// peak memory is ~body + headers, never body + full model.
pub fn parse_full_save_lean(
    file_data: &[u8],
    tables: &ClassTables,
    progress: Option<ProgressFn>,
) -> PResult<SaveStore> {
    parse_full_save_impl(file_data, tables, progress, true)
}

fn parse_full_save_impl(
    file_data: &[u8],
    tables: &ClassTables,
    mut progress: Option<ProgressFn>,
    lean: bool,
) -> PResult<SaveStore> {
    let (info, body_offset) = parse_save_file_info(file_data)?;

    if let Some(cb) = progress.as_deref_mut() {
        cb(0, 0, file_data.len() as u64);
    }
    let decompressed = {
        let mut cb_adapter = progress.as_deref_mut().map(|cb| {
            move |cur: u64, total: u64| cb(0, cur, total)
        });
        let mut dyn_cb: Option<&mut dyn FnMut(u64, u64)> = match cb_adapter.as_mut() {
            Some(f) => Some(f),
            None => None,
        };
        decompress_save_file(file_data, body_offset, dyn_cb.take())?
    };

    let file_header = file_data[..body_offset].to_vec();
    if lean {
        parse_body_bytes_lean(decompressed, file_header, info, tables, progress)
    } else {
        parse_body_bytes(decompressed, file_header, info, tables, progress)
    }
}

/// Parse an already-decompressed body (bytes start at the leading u64
/// uncompressedSize; anything past uncompressedSize+8 is truncated away).
/// `file_header` is the raw uncompressed .sav prefix, retained for export.
/// This is also the editor's validation gate: every edited body goes through
/// here before it replaces the live store.
pub fn parse_body_bytes(
    decompressed: Vec<u8>,
    file_header: Vec<u8>,
    info: crate::save_header::SaveFileInfo,
    tables: &ClassTables,
    progress: Option<ProgressFn>,
) -> PResult<SaveStore> {
    parse_body_bytes_impl(decompressed, file_header, info, tables, progress, false)
}

/// parse_body_bytes, but objects are skipped instead of parsed: every
/// Level.objects is None while headers, byte spans and `data` come out
/// identical to a full parse (gated by tests). This is the lean-session load
/// path -- peak memory is ~body + headers instead of body + full model, and
/// it runs much faster. Queries, the payload/index builder and edits all
/// re-parse single objects on demand via the spans.
pub fn parse_body_bytes_lean(
    decompressed: Vec<u8>,
    file_header: Vec<u8>,
    info: crate::save_header::SaveFileInfo,
    tables: &ClassTables,
    progress: Option<ProgressFn>,
) -> PResult<SaveStore> {
    parse_body_bytes_impl(decompressed, file_header, info, tables, progress, true)
}

fn parse_body_bytes_impl(
    decompressed: Vec<u8>,
    file_header: Vec<u8>,
    info: crate::save_header::SaveFileInfo,
    tables: &ClassTables,
    mut progress: Option<ProgressFn>,
    lean: bool,
) -> PResult<SaveStore> {
    // SaveFileHeader: uncompressedSize (+8), truncate.
    let mut hc = Cursor::new(&decompressed, 0);
    let uncompressed_size = hc.u64()? as usize + 8;
    if uncompressed_size > decompressed.len() {
        return Err(perr!(
            "Reported uncompressed size {} is larger than the actual uncompressed size {}.",
            uncompressed_size,
            decompressed.len()
        ));
    }
    let mut data = decompressed;
    data.truncate(uncompressed_size);

    let mut calculator_extras: Vec<String> = Vec::new();

    // First pass over the (possibly later padded) buffer.
    let parse_result = parse_body(
        &data,
        &info,
        tables,
        &mut calculator_extras,
        &mut progress,
        lean,
    );
    let (persistent_level_version_data, partitions, levels, a_level_name, drop_pod_refs, extra_refs, padded) =
        match parse_result {
            Ok(r) => r,
            Err(e) => return Err(e),
        };
    if padded {
        data.extend_from_slice(&[0, 0, 0, 0]);
    }

    Ok(SaveStore {
        data,
        info,
        persistent_level_version_data,
        partitions,
        levels,
        a_level_name,
        drop_pod_refs,
        extra_refs,
        calculator_extras,
        file_header,
        padded,
        tables: tables.clone(),
    })
}

type BodyResult = (
    Option<crate::version_data::VersionData>,
    Vec<Partition>,
    Vec<Level>,
    crate::reader::StrRef,
    Vec<ObjectRef>,
    Vec<ObjectRef>,
    bool, // "Missing final array count" padding applied
);

fn parse_body(
    data: &[u8],
    info: &crate::save_header::SaveFileInfo,
    tables: &ClassTables,
    calculator_extras: &mut Vec<String>,
    progress: &mut Option<ProgressFn>,
    lean: bool,
) -> PResult<BodyResult> {
    // The quirk path appends 4 zero bytes to the buffer mid-parse. To keep
    // the buffer immutable during parsing, run against a padded copy only
    // when the quirk triggers (rare, and only for tiny remaining reads).
    let mut c = Cursor::new(data, 8);

    let mut persistent_level_version_data = None;
    let mut persistent_level_ue5_version: i32 = -1;
    if info.save_version >= 53 {
        let vd = parse_save_object_version_data(&mut c)?;
        persistent_level_ue5_version = vd.file_version_ue5 as i32;
        persistent_level_version_data = Some(vd);
    }

    // Partitions
    let mut partitions = Vec::new();
    let partition_count = c.u32()?;
    for _ in 0..partition_count {
        let name = c.string()?;
        let i = c.u32()?;
        let grid_hex = c.u32()?;
        let n = c.u32()?;
        let mut levels = Vec::with_capacity(c.capped_capacity(n as usize, 4));
        for _ in 0..n {
            let level_name = c.string()?;
            let lhex = c.u32()?;
            levels.push((level_name, lhex));
        }
        partitions.push(Partition { name, i, grid_hex, levels });
    }

    // Levels
    let mut levels = Vec::new();
    let level_count = c.u32()?;
    let levels_start = c.pos;
    let progress_total = (data.len() - levels_start) as u64;

    for _ in 0..level_count {
        let base = (c.pos - levels_start) as u64;
        let level = parse_headers_and_level(
            &mut c,
            info.save_version,
            -1,
            false,
            tables,
            calculator_extras,
            progress,
            base,
            progress_total,
            lean,
        )?;
        levels.push(level);
    }
    let base = (c.pos - levels_start) as u64;
    let level = parse_headers_and_level(
        &mut c,
        info.save_version,
        persistent_level_ue5_version,
        true,
        tables,
        calculator_extras,
        progress,
        base,
        progress_total,
        lean,
    )?;
    levels.push(level);

    // "Missing final array count" quirk: 4 zero bytes get appended.
    let mut padded = false;
    let padded_data: Vec<u8>;
    if c.pos == data.len() {
        calculator_extras.push("Missing final array count".to_string());
        padded = true;
        let mut v = Vec::with_capacity(data.len() + 4);
        v.extend_from_slice(data);
        v.extend_from_slice(&[0, 0, 0, 0]);
        padded_data = v;
        c = Cursor::new(&padded_data, c.pos);
    } else {
        padded_data = Vec::new();
        let _ = &padded_data;
    }

    let a_level_name = c.string()?;
    let mut drop_pod_refs = Vec::new();
    let mut extra_refs = Vec::new();
    if a_level_name.eq_ascii(c.data, "Persistent_Level") {
        let n = c.u32()?;
        for _ in 0..n {
            drop_pod_refs.push(parse_object_reference(&mut c)?);
        }
        if c.pos == c.data.len() {
            calculator_extras.push("Premature file end".to_string());
        } else {
            let n = c.u32()?;
            for _ in 0..n {
                extra_refs.push(parse_object_reference(&mut c)?);
            }
        }
    }
    if c.pos != c.data.len() {
        return Err(perr!(
            "Parsed data {} does not match decompressed data {}.",
            c.pos,
            c.data.len()
        ));
    }
    if let Some(cb) = progress.as_deref_mut() {
        cb(1, progress_total, progress_total);
    }

    Ok((
        persistent_level_version_data,
        partitions,
        levels,
        a_level_name,
        drop_pod_refs,
        extra_refs,
        padded,
    ))
}
