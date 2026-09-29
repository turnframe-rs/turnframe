//! The README's metric catalogue is generated from the code, not written by
//! hand. This test regenerates it and fails when the two disagree, so the
//! published documentation cannot drift away from what the crate emits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::observe::Signal;
use turnframe_telemetry::metrics::catalogue_markdown;

/// Line that opens the generated block in `README.md`.
const BEGIN: &str = "<!-- BEGIN GENERATED METRIC CATALOGUE -->";
/// Line that closes it.
const END: &str = "<!-- END GENERATED METRIC CATALOGUE -->";

const README: &str = include_str!("../README.md");

/// The text between the two markers, without the markers themselves.
fn embedded_table() -> String {
    let start = README
        .find(BEGIN)
        .unwrap_or_else(|| panic!("README.md has no {BEGIN} marker"));
    let end = README
        .find(END)
        .unwrap_or_else(|| panic!("README.md has no {END} marker"));
    assert!(start < end, "the catalogue markers are in the wrong order");
    README[start + BEGIN.len()..end].trim().to_owned()
}

#[test]
fn the_readme_catalogue_matches_the_code() {
    let generated = catalogue_markdown();
    let embedded = embedded_table();
    assert_eq!(
        embedded,
        generated.trim(),
        "README.md is out of date. Replace the block between the markers with:\n\n{generated}"
    );
}

#[test]
fn the_readme_catalogue_lists_every_metric() {
    let embedded = embedded_table();
    for signal in Signal::ALL {
        assert!(
            embedded.contains(signal.name()),
            "README.md does not document {}",
            signal.name()
        );
    }
    // One header row, one separator row and one row per metric.
    assert_eq!(embedded.lines().count(), Signal::ALL.len() + 2);
}
