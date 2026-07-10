//! Embeds the item-icon manifest: the Python _itemIconFilename checked
//! os.path.exists per call, which a wasm build can't do -- so the set of
//! extracted icon files is snapshotted at compile time instead (icons only
//! change when game_data/copy_icons.py reruns, which implies a rebuild
//! anyway).

use std::io::Write;
use std::path::Path;

fn main() {
    let icons_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../map/static/map/icons/items");
    println!("cargo:rerun-if-changed={}", icons_dir.display());

    let mut stems: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&icons_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".png") {
                stems.push(stem.to_string());
            }
        }
    } else {
        println!(
            "cargo:warning=item icons dir missing ({}); all droppedItems icons will be null",
            icons_dir.display()
        );
    }
    stems.sort();

    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("item_icon_stems.rs");
    let mut f = std::fs::File::create(out).unwrap();
    writeln!(f, "/// Sorted .png stems under map/static/map/icons/items/.").unwrap();
    writeln!(f, "pub static ITEM_ICON_STEMS: &[&str] = &[").unwrap();
    for stem in &stems {
        writeln!(f, "    {:?},", stem).unwrap();
    }
    writeln!(f, "];").unwrap();
}
