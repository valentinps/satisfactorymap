//! pathNameToReadableName / readableLabel port (patches/sav_parse.py +
//! sav_map_data.py). Display names prefer game_data/generated/
//! {items,buildings}.json over the hand-curated corrections table; the
//! de-camelCase fallback replicates the Python implementation exactly,
//! including its reuse of the stale dot-position index in `find("_", pos)`.

use crate::gamedata;
use indexmap::IndexMap;
use std::sync::OnceLock;

/// EXTRACTED_DISPLAY_NAMES: className -> displayName from items.json then
/// buildings.json (later files overwrite on collision).
fn extracted_display_names() -> &'static IndexMap<String, String> {
    static NAMES: OnceLock<IndexMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let data = gamedata::get();
        let mut names: IndexMap<String, String> = IndexMap::new();
        for table in [&data.items, &data.buildings] {
            for (class_name, entry) in table {
                if let Some(display) = entry.get("displayName").and_then(|v| v.as_str()) {
                    names.insert(class_name.clone(), display.to_string());
                }
            }
        }
        names
    })
}

/// Python str.find(needle, start) with Python's negative-start semantics
/// (start < 0 counts from the end, clamped to 0). Returns -1 when absent.
fn py_find(s: &str, needle: char, start: isize) -> isize {
    let len = s.len() as isize;
    let begin = if start < 0 { (len + start).max(0) } else { start };
    if begin >= len {
        return -1;
    }
    // Byte indexing is safe here: inputs are ASCII path names.
    match s[begin as usize..].find(needle) {
        Some(i) => begin + i as isize,
        None => -1,
    }
}

/// patches/sav_parse.py pathNameToReadableName -- exact port, quirks and all.
pub fn path_name_to_readable_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let original_name = name;
    let mut pos: isize = match name.rfind('.') {
        Some(i) => i as isize,
        None => -1,
    };
    let mut name: String =
        if pos != -1 { name[(pos + 1) as usize..].to_string() } else { name.to_string() };
    if let Some(display) = extracted_display_names().get(name.as_str()) {
        return display.clone();
    }
    if let Some(display) = gamedata::get().readable_name_corrections.get(name.as_str()) {
        return display.clone();
    }
    // Deliberate Python quirk: `pos` is still the dot index measured on the
    // ORIGINAL string (or -1), reused as the search start on the shortened
    // one -- so this strip almost never fires.
    pos = py_find(&name, '_', pos);
    if pos != -1 {
        name = name[(pos + 1) as usize..].to_string();
    }
    if name.ends_with("_C") {
        name.truncate(name.len() - 2);
    }
    name = name.replace('_', ", ");
    // Insert a space before every ASCII uppercase letter (the Python loop of
    // 26 replace() calls is equivalent to one left-to-right pass).
    let mut spaced = String::with_capacity(name.len() * 2);
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(ch);
    }
    name = spaced;
    // Python "  " -> " ": one left-to-right non-overlapping pass.
    name = name.replace("  ", " ");
    if name.starts_with(' ') {
        name.remove(0);
    }
    format!("{} ({})", name, original_name)
}

/// sav_map_data.readableLabel: trims pathNameToReadableName's
/// " (original/path)" suffix for display.
pub fn readable_label(path_name: &str) -> String {
    if path_name.is_empty() {
        return String::new();
    }
    let label = path_name_to_readable_name(path_name);
    if let Some(paren_index) = label.find(" (") {
        if label.ends_with(')') {
            return label[..paren_index].to_string();
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_names_match_python_reference() {
        // Fixtures generated with the Python implementation on 2026-07-09.
        // (A trailing-underscore, dot-free name crashes the Python reference
        // with IndexError, so that path is unreachable on real data; the
        // Rust port simply doesn't crash.)
        for (input, name, label) in [
            (
                "/Game/FactoryGame/Resource/Parts/IronPlate/Desc_IronPlate.Desc_IronPlate_C",
                "Iron Plate",
                "Iron Plate",
            ),
            (
                "/Game/FactoryGame/Buildable/Factory/SmelterMk1/Build_SmelterMk1.Build_SmelterMk1_C",
                "Smelter",
                "Smelter",
            ),
            (
                "/Game/FactoryGame/Recipes/Smelter/Recipe_IngotIron.Recipe_IngotIron_C",
                "Recipe, Ingot Iron (/Game/FactoryGame/Recipes/Smelter/Recipe_IngotIron.Recipe_IngotIron_C)",
                "Recipe, Ingot Iron",
            ),
            (
                "/Game/FactoryGame/Buildable/Vehicle/Tractor/BP_Tractor.BP_Tractor_C",
                "Tractor",
                "Tractor",
            ),
            ("Desc_NoDotName_C", "Desc, No Dot Name (Desc_NoDotName_C)", "Desc, No Dot Name"),
            (
                "SomethingWithNoUnderscore",
                "Something With No Underscore (SomethingWithNoUnderscore)",
                "Something With No Underscore",
            ),
        ] {
            assert_eq!(path_name_to_readable_name(input), name, "name for {}", input);
            assert_eq!(readable_label(input), label, "label for {}", input);
        }
        assert_eq!(path_name_to_readable_name(""), "");
    }
}
