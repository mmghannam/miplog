//! Demo: read a Mittelmann concatenated SCIP log, split it, parse one
//! instance's output, print the unified summary.
//!
//! Run with:  SOLVERLOG_CONCAT_SCIP=/path/to/modified.scip.12threads.7200s.out.gz cargo run --example scip_demo

use orlog::{autodetect, input};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("SOLVERLOG_CONCAT_SCIP")?;
    let text = input::read_file(&path)?;
    let entries = input::split_concatenated(&text);
    println!("found {} instance logs in {path}\n", entries.len());

    // Pick one with a decent amount of B&B progress.
    let target = entries
        .iter()
        .find(|e| e.instance.contains("30n20b8"))
        .unwrap_or(&entries[0]);
    println!("showing: {}\n", target.instance);
    let log = autodetect(&target.text)?;
    println!("{log}");
    Ok(())
}
