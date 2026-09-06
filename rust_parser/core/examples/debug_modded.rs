//! Survey the non-vanilla (modded) classes in a save and report which ones
//! carry a trailing payload the parser skips as opaque bytes.

use sav_core::level::parse_full_save_lean;
use sav_core::object::ClassTables;
use sav_core::store::{ActorSpecific, Header};
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("save path");
    let bytes = std::fs::read(&path).expect("read save");
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("lean parse");

    // class -> (instances, instances with opaque trailing bytes, total bytes)
    let mut modded: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for (li, level) in store.levels.iter().enumerate() {
        for (oi, header) in level.headers.iter().enumerate() {
            let class = match header {
                Header::Actor(a) => store.s(a.type_path).to_string(),
                Header::Component(c) => store.s(c.class_name).to_string(),
            };
            if class.starts_with("/Script/FactoryGame.") || class.starts_with("/Game/FactoryGame/") {
                continue;
            }
            let entry = modded.entry(class).or_default();
            entry.0 += 1;
            if let Ok(object) = store.parse_object_at(li, oi) {
                if let ActorSpecific::RawBytes(d) = object.actor_specific {
                    entry.1 += 1;
                    entry.2 += d.len as usize;
                }
            }
        }
    }
    println!("{:<58} {:>7} {:>8} {:>12}", "modded class", "count", "opaque", "opaque bytes");
    for (class, (n, opaque, bytes)) in &modded {
        println!("{class:<58} {n:>7} {opaque:>8} {bytes:>12}");
    }
    let total: usize = modded.values().map(|v| v.0).sum();
    println!("\n{} modded classes, {} objects", modded.len(), total);
}
