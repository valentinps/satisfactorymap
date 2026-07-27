//! Export a save as a Satisfactory Modeler `.sfmd` planning graph.
//!
//!     cargo run --release --example modeler_export -- save.sav out.sfmd
//!         [--no-hubs]     one edge per producer/consumer pair instead of
//!                         routing dense blocks through a shared bus node
//!         [--no-stubs]    leave vehicle-fed factories unsupplied rather
//!                         than standing a storage node in for the station
//!         [--quiet]       suppress the report

use sav_core::level::parse_full_save_lean;
use sav_core::modeler::{self, aggregate::Options};
use sav_core::object::ClassTables;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut options = Options::default();
    let mut quiet = false;
    args.retain(|arg| match arg.as_str() {
        "--no-hubs" => {
            options.bus_hubs = false;
            false
        }
        "--no-stubs" => {
            options.station_stubs = false;
            false
        }
        "--no-overflow-sinks" => {
            options.overflow_sinks = false;
            false
        }
        "--merge-by-item" => {
            options.merge_by_item = true;
            false
        }
        "--merge-resources" => {
            options.merge_resource_nodes = true;
            false
        }
        _ if arg.starts_with("--solver=") => {
            options.solver = arg["--solver=".len()..].to_string();
            false
        }
        "--quiet" => {
            quiet = true;
            false
        }
        _ => true,
    });
    let [save, out] = &args[..] else {
        eprintln!(
            "usage: modeler_export <save.sav> <out.sfmd> \
             [--no-hubs] [--no-stubs] [--merge-resources] [--merge-by-item] [--no-overflow-sinks] [--solver=Full|Basic|Manual|None] [--quiet]"
        );
        std::process::exit(2);
    };

    let started = std::time::Instant::now();
    let bytes = std::fs::read(save).expect("read save");
    let store = parse_full_save_lean(&bytes, &ClassTables::embedded(), None).expect("parse");
    drop(bytes);
    let parsed = started.elapsed();

    let (sfmd, report) = modeler::export(&store, &options);
    std::fs::write(out, &sfmd).expect("write .sfmd");

    let nodes = sfmd.matches("\"Name\":").count();
    println!(
        "{out}: {nodes} nodes, {} KB  (parse {:?}, total {:?})",
        sfmd.len() / 1024,
        parsed,
        started.elapsed(),
    );
    if quiet {
        return;
    }

    if report.hub_nodes > 0 {
        println!(
            "  {} bus hub nodes replaced {} direct edges",
            report.hub_nodes, report.edges_saved_by_hubs,
        );
    }
    section("recipes with no Modeler node", report.unmapped_recipes.iter().cloned());
    section(
        "buildings dropped",
        report.dropped.iter().map(|(class, why)| format!("{class}: {why}")),
    );
    section(
        "ingredients nothing supplies (Modeler treats these as external)",
        report.unsupplied_inputs.iter().map(|(node, item)| format!("{node} needs {item}")),
    );
    if report.overflow_sinks > 0 {
        println!("  {} surplus outputs were given a sink so they do not deadlock", report.overflow_sinks);
    }
    section(
        "products nothing consumes",
        report.unconsumed_outputs.iter().map(|(node, item)| format!("{node} makes {item}")),
    );
    section(
        "vehicle stations stubbed",
        report.station_stubs.iter().map(|(class, item)| format!("{class} -> {item}")),
    );
    section(
        "extraction rates assumed",
        report.extractor_rates.iter().map(|(node, how)| format!("{node}: {how}")),
    );
    if report.stale_somersloop_boosts > 0 {
        println!(
            "  NOTE: {} machines record a somersloop boost with no somersloop installed; \
             the slot count won",
            report.stale_somersloop_boosts,
        );
    }
}

fn section(title: &str, rows: impl Iterator<Item = String>) {
    let rows: Vec<String> = rows.collect();
    if rows.is_empty() {
        return;
    }
    println!("  {title} ({}):", rows.len());
    for row in rows.iter().take(12) {
        println!("    {row}");
    }
    if rows.len() > 12 {
        println!("    ... and {} more", rows.len() - 12);
    }
}
