//! Every task's prompt on the sample domain is pinned by a snapshot and held to a budget.
//!
//! The budget counts the instructions, the rendered context and the answer schema, at
//! four bytes a token. A prompt that grows past it is a prompt that stopped being small.
mod support;

use std::collections::BTreeMap;

use serde_json::Value;
use support::{SET_NAME, today, trip, trips};
use turnframe_core::understanding::{
    ArgumentValue, Excerpt, MessageRef, UnderstoodArgument, UnitKind,
};
use turnframe_provider::request::{ContentPart, Message};
use turnframe_tasks::ModelTask;
use turnframe_understand::tasks::coverage::{Coverage, Found};
use turnframe_understand::tasks::cross_check::{CrossCheck, CrossCheckInput, ShownAct};
use turnframe_understand::tasks::extract::{Extract, ExtractInput, RecordContext};
use turnframe_understand::tasks::locate::{Candidate, Locate, LocateInput};
use turnframe_understand::tasks::question_frame::{QuestionFrame, QuestionInput};
use turnframe_understand::tasks::respects::{Respects, RespectsInput};
use turnframe_understand::tasks::route::{Route, RouteInput};
use turnframe_understand::tasks::segment::Segment;
use turnframe_understand::tasks::verify::{Verify, VerifyInput};
use turnframe_understand::{OpenCard, Span, Speaker, UnderstandingInput};

fn turn() -> UnderstandingInput {
    UnderstandingInput::new("I want to set name", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")]).subject("total"))
        .with_earlier(Speaker::User, "open the Bianchi trip")
        .with_earlier(
            Speaker::Assistant,
            "Trip 1 for Bianchi is open. What should it say?",
        )
        .with_card(
            OpenCard::new("trip", "Rebook leg 1 of Trip 1 on AZ612?")
                .about("tok-trip-1")
                .option("confirm", "Rebook")
                .option("decline", "Not now"),
        )
}

fn text(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

fn pinned<T: ModelTask>(name: &str, task: &T, input: &T::Input, budget_tokens: usize) {
    let context = text(&task.render(input));
    let schema = task.schema(input).to_string();
    let tokens = (task.instructions().len() + context.len() + schema.len()) / 4;
    insta::assert_snapshot!(name, format!("{context}\n\n=== schema ===\n{schema}"));
    assert!(
        tokens <= budget_tokens,
        "{name}: about {tokens} tokens, over its budget of {budget_tokens}"
    );
}

#[test]
fn every_task_prompt_stays_within_its_size_budget() {
    let turn = turn();
    let workflow = &turn.workflows[0];
    let spec = workflow.spec(&SET_NAME.into()).unwrap();
    let unit = Span::new(3, 4);

    pinned("segment", &Segment::new(&turn), &(), 1600);
    pinned(
        "coverage",
        &Coverage::new(&turn),
        &Found {
            units: vec![(UnitKind::Request, unit)],
        },
        500,
    );
    pinned(
        "route",
        &Route::new(&turn),
        &RouteInput {
            label: "Request",
            words: unit,
            workflows: vec![workflow],
            note: None,
            others: Vec::new(),
        },
        700,
    );
    pinned(
        "locate",
        &Locate::new(&turn),
        &LocateInput {
            label: "Request",
            words: unit,
            spec,
            workflow: &workflow.key,
            candidates: workflow.records.iter().map(Candidate::Record).collect(),
            allow_new: false,
            allow_not_listed: true,
            note: None,
        },
        600,
    );
    pinned(
        "extract",
        &Extract::new(&turn),
        &ExtractInput {
            label: "Request",
            words: unit,
            spec,
            workflow,
            record: RecordContext::Existing(&workflow.records[0]),
            arguments: spec.arguments.iter().collect(),
            record_choices: BTreeMap::new(),
            continues: None,
            others: Vec::new(),
            kin: Vec::new(),
            also: Vec::new(),
            transcript: 4,
            note: None,
            occurrence: None,
            corrected: std::collections::BTreeMap::new(),
        },
        900,
    );
    let range = turn.message.range(Span::new(3, 4)).unwrap();
    let arguments = BTreeMap::from([(
        "value".to_owned(),
        UnderstoodArgument {
            value: ArgumentValue::Json(Value::from("set name")),
            excerpt: Some(Excerpt {
                message: MessageRef::Current,
                words: range,
            }),
        },
    )]);
    pinned(
        "verify",
        &Verify::new(&turn),
        &VerifyInput {
            label: "Request",
            words: unit,
            meaning: format!("{}: {}", spec.key, spec.summary),
            record: "Trip 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::from([("value".to_owned(), "name".to_owned())]),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: None,
            note: None,
            continues: None,
        },
        800,
    );
    pinned(
        "question_frame",
        &QuestionFrame::new(&turn),
        &QuestionInput {
            words: unit,
            records: workflow.records.iter().collect(),
            subjects: vec!["total"],
        },
        500,
    );
    pinned(
        "cross_check",
        &CrossCheck::new(&turn),
        &CrossCheckInput {
            acts: vec![ShownAct {
                id: "u1.a1".to_owned(),
                line: "u1.a1 trip.set_name on Trip 1: value «Lisbon» (words 4 to 5)".to_owned(),
                arguments: vec!["value".to_owned()],
            }],
            questions: Vec::new(),
            constraints: Vec::new(),
            unread: Vec::new(),
            held: vec![unit],
        },
        700,
    );
    pinned(
        "respects",
        &Respects::new(&turn),
        &RespectsInput {
            constraint: unit,
            act: "u1.a1 trip.set_name on Trip 1: value «Lisbon» (words 4 to 5)".to_owned(),
        },
        400,
    );
}
