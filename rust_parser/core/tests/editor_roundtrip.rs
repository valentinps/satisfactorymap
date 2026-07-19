//! Round-trip gate for the save editor's write path: for every .sav in
//! map/uploads, export must decompress back to the exact original body and
//! the exported file must re-parse with an identical map payload.

use sav_core::decompress::decompress_save_file;
use sav_core::editor::{effective_body, export_sav};
use sav_core::level::parse_full_save;
use sav_core::mapdata;
use sav_core::object::ClassTables;
use std::path::PathBuf;

fn upload_saves() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../map/uploads");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipping: {} not present", dir.display());
        return Vec::new();
    };
    let mut saves: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sav")))
        .collect();
    saves.sort();
    saves
}

#[test]
fn export_roundtrip_all_uploads() {
    let tables = ClassTables::embedded();
    for path in upload_saves() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let file_data = std::fs::read(&path).unwrap();
        let store = parse_full_save(&file_data, &tables, None)
            .unwrap_or_else(|e| panic!("{name}: parse failed: {}", e.msg));

        let exported = export_sav(&store.file_header, effective_body(&store));

        // The uncompressed header prefix round-trips verbatim.
        assert_eq!(
            &exported[..store.file_header.len()],
            &file_data[..store.file_header.len()],
            "{name}: exported header differs"
        );

        // Decompressing our own chunk stream returns the exact body bytes.
        let redecompressed =
            decompress_save_file(&exported, store.file_header.len(), None)
                .unwrap_or_else(|e| panic!("{name}: exported file failed to decompress: {}", e.msg));
        // The original body may carry bytes past the leading-u64 size that
        // parse_full_save truncates; the effective body is the truncated view.
        assert!(
            redecompressed.starts_with(effective_body(&store)),
            "{name}: exported body differs from original"
        );
        assert_eq!(
            redecompressed.len(),
            effective_body(&store).len(),
            "{name}: exported body length differs"
        );

        // The exported file parses end-to-end with an identical payload.
        let store2 = parse_full_save(&exported, &tables, None)
            .unwrap_or_else(|e| panic!("{name}: exported file failed to parse: {}", e.msg));
        let (payload1, _) = mapdata::build_all_json(&store, None).unwrap();
        let (payload2, _) = mapdata::build_all_json(&store2, None).unwrap();
        assert_eq!(payload1, payload2, "{name}: payload JSON differs after round-trip");

        println!("{name}: OK ({} -> {} bytes)", file_data.len(), exported.len());
    }
}
