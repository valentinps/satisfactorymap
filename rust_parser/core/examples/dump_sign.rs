//! Inspect widget signs in a save: property list of the sign actor and each
//! of its components, then simulate a cross-save paste and byte-diff the
//! pasted body against the original (only renamed refs should differ).

use sav_core::editor::clipboard::{extract_clipboard, inflate_payload};
use sav_core::editor::ops::parse_ops_json;
use sav_core::editor::session;
use sav_core::level::parse_full_save;
use sav_core::mapdata::scan::SaveScan;
use sav_core::object::ClassTables;
use sav_core::store::{Header, SaveStore};

fn load(path: &str) -> SaveStore {
    let bytes = std::fs::read(path).expect("read save");
    parse_full_save(&bytes, &ClassTables::embedded(), None).unwrap()
}

fn prop_names(store: &SaveStore, li: usize, oi: usize) -> Vec<String> {
    match store.parse_object_at(li, oi) {
        Ok(o) => o
            .properties
            .props
            .iter()
            .map(|p| p.name.to_string(&store.data))
            .collect(),
        Err(e) => vec![format!("<parse error: {}>", e.msg)],
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = load(&args[0]);
    let data: &[u8] = &source.data;

    let mut sign: Option<(usize, usize, String)> = None;
    let mut n_signs = 0;
    for (li, level) in source.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let Header::Actor(a) = header else { continue };
            let tp = a.type_path.to_string(data);
            if tp.contains("WidgetSign") || tp.contains("Billboard") {
                n_signs += 1;
                if sign.is_none() {
                    sign = Some((li, oi, a.instance_name.to_string(data)));
                    println!("sign: {} ({})", a.instance_name.to_string(data), tp);
                }
            }
        }
    }
    println!("total sign-like actors: {n_signs}");
    let Some((li, oi, name)) = sign else { return };

    println!("actor props: {:?}", prop_names(&source, li, oi));
    let object = source.parse_object_at(li, oi).unwrap();
    let scan = SaveScan::new(&source);
    if let Some((parent, comps)) = &object.actor_reference_associations {
        println!("parent: {:?}", parent.path_name.to_string(data));
        for comp in comps {
            let cname = comp.path_name.to_string(data);
            match scan.by_instance_name.get(comp.path_name.bytes(data)) {
                Some(&(cli, coi)) => {
                    println!("component {}: props {:?}", cname, prop_names(&source, cli, coi))
                }
                None => println!("component {}: NOT IN SAVE", cname),
            }
        }
    } else {
        println!("no components");
    }

    // Cross-save paste into the second save; diff the pasted sign's body.
    let target = load(&args[1]);
    let blob_json = extract_clipboard(&source, &[name.clone()], &[]).unwrap();
    let blob: serde_json::Value = serde_json::from_str(&blob_json).unwrap();
    let payload =
        inflate_payload(blob["z"].as_str().unwrap(), blob["zLen"].as_u64().unwrap()).unwrap();
    println!("blob actors (sign + components): {}", payload.actors.len());

    let op = serde_json::json!([{
        "op": "pasteExternal",
        "saveVersion": blob["saveVersion"],
        "objectVersion": blob["objectVersion"],
        "lightweightVersion": blob["lightweightVersion"],
        "z": blob["z"],
        "zLen": blob["zLen"],
        "anchor": blob["anchor"],
        "delta": [1000.0, 0.0, 0.0],
        "seed": 7u64,
    }]);
    let op = parse_ops_json(&op.to_string()).unwrap().remove(0);
    let target2 = session::step(&target, &op, &ClassTables::embedded()).unwrap();

    // Find the pasted sign (new name of the same type) and byte-diff.
    let scan_before = SaveScan::new(&target);
    let (s_off, s_len) = source.levels[li].object_spans[oi];
    let original = &source.data[s_off..s_off + s_len as usize];
    for (pli, level) in target2.levels.iter().enumerate() {
        for (poi, header) in level.headers.iter().enumerate() {
            let Header::Actor(a) = header else { continue };
            let tp = a.type_path.to_string(&target2.data);
            if !(tp.contains("WidgetSign") || tp.contains("Billboard")) {
                continue;
            }
            if scan_before.by_instance_name.contains_key(a.instance_name.bytes(&target2.data)) {
                continue;
            }
            let (p_off, p_len) = target2.levels[pli].object_spans[poi];
            let pasted = &target2.data[p_off..p_off + p_len as usize];
            println!(
                "pasted sign {} body: {} bytes (original {})",
                a.instance_name.to_string(&target2.data),
                pasted.len(),
                original.len()
            );
            if pasted.len() == original.len() {
                let diffs: Vec<usize> =
                    (0..pasted.len()).filter(|&i| pasted[i] != original[i]).collect();
                println!("differing byte positions: {} total", diffs.len());
                let mut last_shown = 0usize;
                for &d in &diffs {
                    if d < last_shown {
                        continue; // inside the previously printed window
                    }
                    let lo = d.saturating_sub(30);
                    let hi = (d + 30).min(pasted.len());
                    let show = |b: &[u8]| -> String {
                        b.iter()
                            .map(|&c| if (32..127).contains(&c) { c as char } else { '.' })
                            .collect()
                    };
                    println!("  @{d}:");
                    println!("    orig   {}", show(&original[lo..hi]));
                    println!("    pasted {}", show(&pasted[lo..hi]));
                    last_shown = hi;
                }
            }
        }
    }
}
