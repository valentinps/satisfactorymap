//! Print per-belt item records (the old pre-chain format) found in saves:
//! empirical reference for what the leading u32 and the position float hold
//! in records the game itself wrote.

use sav_core::level::parse_full_save;
use sav_core::object::ClassTables;
use sav_core::store::ActorSpecific;

fn main() {
    for arg in std::env::args().skip(1) {
        let bytes = std::fs::read(&arg).expect("read save");
        let store = match parse_full_save(&bytes, &ClassTables::embedded(), None) {
            Ok(s) => s,
            Err(e) => {
                println!("{arg}: parse failed: {}", e.msg);
                continue;
            }
        };
        let mut n_belts = 0usize;
        let mut n_with_items = 0usize;
        let mut printed = 0usize;
        for (li, level) in store.levels.iter().enumerate() {
            for (oi, object) in level.parsed_objects().iter().enumerate() {
                if let ActorSpecific::ConveyorBelt { items, .. } = &object.actor_specific {
                    n_belts += 1;
                    if items.is_empty() {
                        continue;
                    }
                    n_with_items += 1;
                    if printed < 6 {
                        printed += 1;
                        let name = store.levels[li].headers[oi]
                            .instance_name()
                            .to_string(&store.data);
                        println!("{arg}: {name} ({} items):", items.len());
                        for (len, path, pos) in items.iter().take(10) {
                            println!("  u32={} pos={} {}", len, pos, path.to_string(&store.data));
                        }
                    }
                }
            }
        }
        println!("{arg}: belts={n_belts} with_items={n_with_items}");
    }
}
