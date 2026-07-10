//! Scratch driver: replay an op sequence against a save and print failures.

use sav_core::editor::ops::EditOp;
use sav_core::editor::{effective_body, session};
use sav_core::level::parse_full_save;
use sav_core::object::ClassTables;
use sav_core::store::Header;

fn main() {
    let path = std::env::args().nth(1).expect("save path");
    let bytes = std::fs::read(path).unwrap();
    let tables = ClassTables::embedded();
    let store = parse_full_save(&bytes, &tables, None).unwrap();

    let find = |prefix: &str| -> String {
        for level in &store.levels {
            for header in &level.headers {
                if let Header::Actor(a) = header {
                    if a.type_path.to_string(&store.data).starts_with(prefix) {
                        return a.instance_name.to_string(&store.data);
                    }
                }
            }
        }
        panic!("no {prefix}");
    };
    let smelter = find("/Game/FactoryGame/Buildable/Factory/SmelterMk1/");
    let constructor = find("/Game/FactoryGame/Buildable/Factory/ConstructorMk1/");
    println!("smelter={smelter} constructor={constructor}");

    let ops = vec![
        EditOp::MoveActors { names: vec![smelter.clone()], delta: [7000.0, 3600.0, 0.0], rotate_yaw_deg: 0.0, pivot: None },
        EditOp::DuplicateActors { names: vec![constructor], delta: [13000.0, 0.0, 0.0], rotate_yaw_deg: 0.0, pivot: None, seed: 12345 },
        EditOp::DeleteActors { names: vec![smelter] },
    ];
    let pristine = effective_body(&store).to_vec();
    let mut current = parse_full_save(&bytes, &tables, None).unwrap();
    for (i, op) in ops.iter().enumerate() {
        match session::step(&current, op, &tables) {
            Ok(s) => {
                println!("op {i} OK ({} bytes)", s.data.len());
                current = s;
            }
            Err(e) => {
                println!("op {i} FAILED: {}", e.msg);
                std::process::exit(1);
            }
        }
    }
    let _ = pristine;
    println!("ALL OK");
}
