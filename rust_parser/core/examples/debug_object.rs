//! Debug helper: lean-parse a save, then re-parse one object by slot and
//! dump its header, span and bytes so a mis-parse can be diagnosed.
//!
//!     cargo run --release --example debug_object -- save.sav 3339 87620

use sav_core::level::parse_full_save_lean;
use sav_core::object::ClassTables;
use sav_core::store::Header;

fn hexdump(data: &[u8], start: usize, len: usize, mark: Option<usize>) {
    let end = (start + len).min(data.len());
    let mut off = start;
    while off < end {
        let row_end = (off + 16).min(end);
        let mut hex = String::new();
        let mut asc = String::new();
        for i in off..row_end {
            let sep = if Some(i) == mark { '>' } else { ' ' };
            hex.push(sep);
            hex.push_str(&format!("{:02x}", data[i]));
            let b = data[i];
            asc.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        println!("{:>12}  {:<49} {}", off, hex, asc);
        off = row_end;
    }
}

fn find_belts(store: &sav_core::store::SaveStore) {
    let mut shown = 0;
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, h) in level.headers.iter().enumerate() {
            let Header::Actor(a) = h else { continue };
            let tp = store.s(a.type_path);
            if !tp.contains("Build_ConveyorBelt") && !tp.contains("Build_ConveyorLift") {
                continue;
            }
            match store.parse_object_at(li, oi) {
                Ok(o) => {
                    if let sav_core::store::ActorSpecific::ConveyorBelt { items, .. } = &o.actor_specific {
                        if items.is_empty() {
                            continue;
                        }
                        let (off, len) = level.object_spans[oi];
                        println!(
                            "belt ({li},{oi}) {} items={} span off={} len={}",
                            tp,
                            items.len(),
                            off,
                            len
                        );
                        let tail = 220.min(len as usize);
                        hexdump(&store.data, off + len as usize - tail, tail, None);
                        shown += 1;
                        if shown >= 2 {
                            return;
                        }
                    }
                }
                Err(e) => {
                    println!("belt ({li},{oi}) {} PARSE FAILED: {}", tp, e);
                    shown += 1;
                    if shown >= 2 {
                        return;
                    }
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, sav, li, oi] = &args[..] else {
        eprintln!("usage: debug_object <save.sav> <level_index|find-belt> <object_index>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(sav).expect("read save");
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);

    println!(
        "header: save_version={} build_version={}",
        store.info.save_version, store.info.build_version
    );
    println!("levels: {}", store.levels.len());
    if li == "find-belt" {
        find_belts(&store);
        return;
    }
    let li: usize = li.parse().unwrap();
    let oi: usize = oi.parse().unwrap();
    let level = &store.levels[li];
    println!(
        "level {} name={:?} objects={} ue5_version={} save_version={}",
        li,
        level.level_name.map(|n| store.s(n)),
        level.headers.len(),
        level.object_ue5_version,
        level.level_save_version
    );
    match &level.headers[oi] {
        Header::Actor(a) => {
            println!(
                "ACTOR type_path={} root={} instance={}",
                store.s(a.type_path),
                store.s(a.root_object),
                store.s(a.instance_name)
            );
        }
        Header::Component(c) => {
            println!(
                "COMPONENT class={} root={} instance={} parent={}",
                store.s(c.class_name),
                store.s(c.root_object),
                store.s(c.instance_name),
                store.s(c.parent_actor_name)
            );
        }
    }
    let (off, len) = level.object_spans[oi];
    println!("object span: off={} len={}", off, len);

    match store.parse_object_at(li, oi) {
        Ok(o) => {
            println!("parse OK: {} properties", o.properties.props.len());
            for p in &o.properties.props {
                println!("  prop {}", store.s(p.name));
            }
        }
        Err(e) => {
            println!("parse FAILED: {}", e);
            let msg = format!("{}", e);
            // Pull the failure offset out of the message if present.
            let mark = msg
                .rsplit("offset ")
                .next()
                .and_then(|s| s.trim_end_matches('.').parse::<usize>().ok());
            println!("--- first 512 bytes of object span ---");
            hexdump(&store.data, off, 512, None);
            if let Some(m) = mark {
                let ctx_start = m.saturating_sub(256);
                println!("--- around failure offset {} (span-relative {}) ---", m, m as i64 - off as i64);
                hexdump(&store.data, ctx_start, 512, Some(m));
            }
        }
    }
}
