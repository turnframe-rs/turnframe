//! The library's own prompts state rules for every domain, so none of them names what a sample
//! domain holds: its workflows, the labels of its fields, the values it enumerates or the
//! records it seeds. What a domain needs a model to know, the domain configures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use turnframe_core::case::CaseRef;
use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::ids::CaseRevision;
use turnframe_core::locale::Locale;
use turnframe_test::workflows::{claim, traveler, trip};

/// Words a sample uses that are also the library's own: every workflow has fields and values.
const SHARED: &[&str] = &[
    "claim",
    "receipt",
    "date",
    "value",
    "name",
    "total",
    "reference",
    "description",
    "field",
    "reason",
];

/// The travel desk's vocabulary, which no core prompt may use.
const TRAVEL: &[&str] = &[
    "trip",
    "flight",
    "leg",
    "fare",
    "airline",
    "traveler",
    "booking",
    "rebook",
    "rebooking",
    "viaggio",
    "volo",
    "tratta",
    "tariffa",
    "compagnia aerea",
    "viaggiatore",
    "prenotazione",
    "ricevuta",
];

fn names<W: WorkflowDefinition>(workflow: &W, states: &[W::State], into: &mut Vec<String>) {
    let english = Locale::new("en");
    for state in states {
        let view = workflow.project(CaseRef::new("sample", "s-1", CaseRevision(1)), Some(state));
        for spec in workflow.operations(&view) {
            let key = spec.key.as_str().to_owned();
            into.extend(key.split('.').next().map(str::to_owned));
            for argument in &spec.arguments {
                into.extend(argument.labels.iter().map(|label| label.text.clone()));
            }
        }
        for enumeration in workflow.enumerations(&view) {
            for value in &enumeration.values {
                into.push(value.label.resolve(&english).to_owned());
            }
        }
    }
}

fn prompts(dir: &Path, into: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            prompts(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            let text = std::fs::read_to_string(&path).unwrap();
            into.push((path, text));
        }
    }
}

fn names_it(text: &str, name: &str) -> bool {
    let text = text.to_lowercase();
    let name = name.to_lowercase();
    text.match_indices(&name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

#[test]
fn no_core_prompt_names_a_sample_domain() {
    let mut vocabulary = Vec::new();
    names(
        &trip::TripWorkflow::default(),
        &[
            trip::incomplete_case(),
            trip::unassigned_case(),
            trip::complete_case(),
            trip::awaiting_rebooking_confirmation(),
        ],
        &mut vocabulary,
    );
    names(
        &traveler::TravelerWorkflow::default(),
        &[
            traveler::incomplete_draft(),
            traveler::awaiting_activation(),
            traveler::active_traveler(),
        ],
        &mut vocabulary,
    );
    names(
        &claim::ClaimWorkflow::default(),
        &[
            claim::awaiting_extraction(),
            claim::under_review(claim::complete_proposal()),
        ],
        &mut vocabulary,
    );
    vocabulary.extend(
        [
            trip::SAMPLE_NAME,
            traveler::SAMPLE_NAME,
            traveler::SAMPLE_EMAIL,
            traveler::SAMPLE_LOYALTY_NUMBER,
            claim::EXTRACTED_MERCHANT,
        ]
        .map(str::to_owned),
    );
    // The desk's own words, in both languages, beside what its configuration labels.
    vocabulary.extend(TRAVEL.iter().map(|word| (*word).to_owned()));
    vocabulary.retain(|name| !SHARED.contains(&name.to_lowercase().as_str()));
    vocabulary.sort();
    vocabulary.dedup();
    assert!(
        vocabulary.len() > 10,
        "too few names to mean anything: {vocabulary:?}"
    );

    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for owner in ["turnframe-understand", "turnframe-runtime"] {
        prompts(&crates.join(owner).join("prompts"), &mut files);
    }
    assert!(!files.is_empty());

    let named: Vec<String> = files
        .iter()
        .flat_map(|(path, text)| {
            vocabulary
                .iter()
                .filter(|name| names_it(text, name))
                .map(move |name| format!("{} names «{name}»", path.display()))
        })
        .collect();
    assert!(named.is_empty(), "{named:#?}");
}
