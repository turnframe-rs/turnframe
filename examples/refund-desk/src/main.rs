//! The refund desk: nine attacks on a refund, and the runtime's answer to each.
//!
//! `cargo run -p refund-desk` prints every run, station by station. With
//! `--record <path>` it writes the recording the website plays instead.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout)]

use refund_desk::record::{Frame, Run};
use refund_desk::runs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let recording = runs::all().await?;
    let mut args = std::env::args().skip(1);
    if let Some(flag) = args.next() {
        anyhow::ensure!(
            flag == "--record",
            "unknown argument {flag:?}; the one known is --record <path>"
        );
        let path = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("--record takes the path to write"))?;
        std::fs::write(&path, runs::json(&recording)?)
            .map_err(|error| anyhow::anyhow!("could not write {path}: {error}"))?;
        println!("recorded {} runs to {path}", recording.runs.len());
        return Ok(());
    }
    for run in &recording.runs {
        print_run(run);
    }
    Ok(())
}

fn print_run(run: &Run) {
    println!(
        "\n-- {} {}",
        run.label,
        "-".repeat(70_usize.saturating_sub(run.label.len()))
    );
    println!("  attack   {}", run.attack);
    for frame in &run.frames {
        print_frame(frame);
    }
    let stopped = run.stopped_at.map_or_else(
        || "nowhere".to_owned(),
        |station| format!("{station:?}").to_lowercase(),
    );
    println!("  stopped  {stopped}");
    println!("  verdict  {}", run.verdict.text);
}

fn print_frame(frame: &Frame) {
    let station = format!("{:?}", frame.station).to_lowercase();
    let state = frame.state.unwrap_or("");
    let scripted = if frame.scripted { " (scripted)" } else { "" };
    println!("  {station:<9}{state:<10}{}{scripted}", frame.text);
    if let Some(card) = &frame.card {
        println!("  {:<19}[{}] {}", "", card.title, card.body);
        for entry in &card.entries {
            println!(
                "  {:<19}{}: {} -> {}",
                "", entry.label, entry.before, entry.after
            );
        }
    }
}
