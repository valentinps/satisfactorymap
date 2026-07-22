//! Headless coverage of the desktop `AppSession` orchestration (the part the
//! webview can't be driven to exercise from CI): load a real save, export it,
//! and confirm the export re-parses to a byte-identical payload. A fresh export
//! of an unedited save is body-identical to the original, so its payload must
//! match load's -- this gates the port of load + export against sav_core.

use sav_tauri_lib::session::AppSession;

fn sample_save() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../map/uploads/All_080726-163150.sav");
    std::fs::read(path).unwrap_or_else(|e| panic!("read sample save {}: {}", path, e))
}

#[test]
fn load_then_export_roundtrips_to_same_payload() {
    let noop = |_phase: u8, _c: u64, _t: u64| {};

    let (session, payload_a) = AppSession::load(sample_save(), &noop).expect("load");
    assert!(!payload_a.is_empty(), "payload should be non-empty");
    // The payload is the map-data JSON the frontend consumes; it must parse.
    let value: serde_json::Value =
        serde_json::from_slice(&payload_a).expect("payload is valid JSON");
    assert!(value.is_object(), "payload is a JSON object");

    let exported = session.export_sav().expect("export");
    assert!(exported.len() > 8, "exported .sav should be non-trivial");

    // Re-loading the freshly exported (unedited) save must reproduce the same
    // payload -- export is body-identical, so the whole pipeline round-trips.
    let (_session2, payload_b) = AppSession::load(exported, &noop).expect("reload export");
    assert_eq!(payload_a, payload_b, "export round-trip changed the payload");
}
