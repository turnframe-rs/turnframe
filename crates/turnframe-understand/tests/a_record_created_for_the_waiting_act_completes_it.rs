//! Asked who an trip is for, the user registers a new traveler: the act that was
//! waiting for the traveler is completed with the record this message creates, which
//! takes the name the user gave before when this message gives none.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, RecordValue, UnderstoodArgument, UnitId,
};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::{Expectation, PendingAct, UnderstandingInput, WorkflowBrief};

const SET_TRAVELER: &str = "trip.set_traveler";
const REGISTER: &str = "traveler.create_draft";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

fn input(message: &str) -> UnderstandingInput {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
    let travelers = WorkflowBrief::new("traveler")
        .summary("Register travelers.")
        .on_new_case(REGISTER)
        .operation(
            OperationSpec::new(REGISTER)
                .summary("Register a new traveler.")
                .target(TargetPolicy::NewCaseOnly)
                .mutating(),
        );
    UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers)
        .with_expectation(Expectation::Values(PendingAct {
            operation: SET_TRAVELER.into(),
            record: Some("tok-trip-1".into()),
            given: BTreeMap::new(),
            missing: vec!["traveler".to_owned()],
        }))
}

#[tokio::test]
async fn a_record_created_for_the_waiting_act_completes_it() {
    // [1]register [2]them [3]as [4]a [5]new [6]traveler
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers a traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 6}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER]}))
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        );
    let run = understand(script, &input("register them as a new traveler")).await;

    let acts = &run.understanding.acts;
    let completed = acts
        .iter()
        .find(|act| {
            act.operation()
                .is_some_and(|op| op.as_str() == SET_TRAVELER)
        })
        .unwrap_or_else(|| panic!("the waiting act is completed: {acts:?}"));
    let created = ActId::new(UnitId(1), 1);
    assert_eq!(completed.status, ActStatus::Ready);
    assert_eq!(
        completed.target,
        ActTarget::Record {
            token: "tok-trip-1".into()
        }
    );
    assert_eq!(
        completed.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn { act: created })
    );
    assert_eq!(completed.depends_on, vec![created]);
    assert!(matches!(
        acts.iter()
            .find(|act| act.id == created)
            .map(|act| &act.action),
        Some(ActAction::Apply { .. } | ActAction::Start { .. })
    ));
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

/// Asked who the trip is for after «Omar Haddad» found nothing, the user registers
/// a traveler; the message asks for both operations.
fn registering_the_named_traveler(
    message: &str,
    chosen: serde_json::Value,
    verified: serde_json::Value,
) -> (UnderstandingInput, ScriptedTasks) {
    let mut input = input(message);
    input
        .workflows
        .retain(|workflow| workflow.key.as_str() != "traveler");
    let input = input.with_workflow(
        WorkflowBrief::new("traveler")
            .on_new_case(REGISTER)
            .operation(
                OperationSpec::new(REGISTER)
                    .summary("Register a new traveler.")
                    .target(TargetPolicy::AllowsNewCase)
                    .mutating()
                    .arguments::<Register>()
                    .argument("full_name", |a| a.label("name").names_the_record()),
            ),
    );
    let Some(Expectation::Values(mut pending)) = input.expectation.clone() else {
        panic!("a waiting act")
    };
    pending.given.insert(
        "traveler".to_owned(),
        UnderstoodArgument {
            value: ArgumentValue::Record(RecordValue::Named {
                workflow: "traveler".into(),
                named: "Omar Haddad".to_owned(),
            }),
            excerpt: None,
        },
    );
    let input = input.with_expectation(Expectation::Values(pending));
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers the traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER, SET_TRAVELER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        )
        .answer("u1.a2/extract", chosen)
        .answer("u1.a2/verify", verified);
    (input, script)
}

fn assert_completed_and_named(acts: &[turnframe_core::understanding::UnderstoodAct]) {
    let created = ActId::new(UnitId(1), 1);
    let register = acts.iter().find(|act| act.id == created).unwrap();
    assert_eq!(
        register.arguments["full_name"].value,
        ArgumentValue::Json("Omar Haddad".into()),
        "{acts:?}"
    );
    let set: Vec<_> = acts
        .iter()
        .filter(|act| {
            act.operation()
                .is_some_and(|op| op.as_str() == SET_TRAVELER)
        })
        .collect();
    assert_eq!(set.len(), 1, "{acts:?}");
    assert_eq!(set[0].status, ActStatus::Ready);
    assert_eq!(
        set[0].arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn { act: created })
    );
}

#[tokio::test]
async fn the_record_registered_for_it_takes_the_name_that_found_nothing() {
    // [1]yes, [2]register [3]them
    let (input, script) = registering_the_named_traveler(
        "yes, register them",
        json!({"arguments": {"traveler": {
            "kind": "record", "name": "them", "message": "current",
            "from": 3, "to": 3, "record": "s1"
        }}}),
        json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
    );
    let run = understand(script, &input).await;
    assert_completed_and_named(&run.understanding.acts);
}

#[tokio::test]
async fn an_act_left_waiting_for_the_record_registered_is_completed_by_it() {
    // [1]register [2]that [3]traveler
    let (input, script) = registering_the_named_traveler(
        "register that traveler",
        json!({"arguments": {"traveler": {
            "kind": "record", "name": "that traveler", "message": "current",
            "from": 2, "to": 3, "record": "s1"
        }}}),
        json!({"reason": "No traveler named.", "arguments": {"traveler": "not_stated"}, "overall": "not_requested"}),
    );
    let script = script.answer(
        "u1.a2/extract.after_verify",
        json!({"arguments": {"traveler": {"kind": "not_given"}}}),
    );
    let run = understand(script, &input).await;
    assert_completed_and_named(&run.understanding.acts);
}
