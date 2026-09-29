//! The documentation makes one claim it is not allowed to stop making.
//!
//! A judge score is not a substitute for a deterministic assertion, and the crate says
//! so where a reader cannot miss it: in its documentation and its README, beside the
//! reason it cannot happen here. Prose has no compiler, so this file is the compiler.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// The crate documentation, read from source so the assertion is about what a
/// reader of `docs.rs` will actually see.
const LIB: &str = include_str!("../src/lib.rs");

/// The README, which is also compiled as a doc-test by the crate itself.
const README: &str = include_str!("../README.md");

/// The sentence, verbatim.
const THE_SENTENCE: &str = "A judge score is not a substitute for a deterministic assertion";

#[test]
fn the_crate_documentation_says_a_judge_score_is_not_a_substitute() {
    let heading = format!("//! # {THE_SENTENCE}");
    assert!(
        LIB.contains(&heading),
        "the sentence must be a heading in the crate documentation, not a remark buried in a \
         paragraph: `{heading}`"
    );
    let position = LIB.find(&heading).unwrap_or(usize::MAX);
    let features = LIB.find("//! # What is here").unwrap_or(usize::MAX);
    assert!(
        position < features,
        "it must come before the feature tour, because it is more important than the features"
    );
}

#[test]
fn the_readme_says_a_judge_score_is_not_a_substitute() {
    let heading = format!("## {THE_SENTENCE}");
    assert!(
        README.contains(&heading),
        "the sentence must be a section heading in the README: `{heading}`"
    );
}

/// Strips the decoration a reader does not see — comment markers, emphasis,
/// code fences and line breaks — so an assertion about the prose is not an
/// assertion about where the lines happened to wrap.
fn prose(text: &str) -> String {
    text.replace("//!", " ")
        .replace(['*', '`', '[', ']'], "")
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

#[test]
fn both_explain_why_the_type_level_separation_makes_it_impossible() {
    // Stating the rule without the mechanism invites a reader to assume it is a
    // convention they may weigh against convenience. It is not: `JudgeInput` is
    // two strings and `JudgeCriterion` is a closed set, and both documents have
    // to say so beside the rule.
    for (name, text) in [("lib.rs", LIB), ("README.md", README)] {
        let prose = prose(text);
        let start = prose
            .find(THE_SENTENCE)
            .unwrap_or_else(|| panic!("{name} states the rule"));
        let section = &prose[start..];
        for evidence in [
            "JudgeInput",
            "two strings",
            "JudgeCriterion",
            "exactly four variants",
        ] {
            assert!(
                section.contains(evidence),
                "{name} must explain the mechanism beside the rule, and does not mention \
                 `{evidence}`"
            );
        }
    }
}
