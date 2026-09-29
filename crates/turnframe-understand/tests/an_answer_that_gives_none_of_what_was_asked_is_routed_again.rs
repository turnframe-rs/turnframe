//! An answer read as the act that asked, giving none of what it asked, is routed once more,
//! told so: it may take up what the assistant offered instead. A second route that finds
//! nothing else leaves the first reading, which asks again.
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
    ActId, ActStatus, ArgumentValue, RecordValue, UnderstoodArgument, UnitId,
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

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

/// Asked who the trip is for after «Omar Haddad» found nothing.
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
        .on_new_case(REGISTER)
        .operation(
            OperationSpec::new(REGISTER)
                .summary("Register a new traveler.")
                .target(TargetPolicy::AllowsNewCase)
                .mutating()
                .arguments::<Register>()
                .argument("full_name", |a| a.label("name").names_the_record()),
        );
    let given = BTreeMap::from([(
        "traveler".to_owned(),
        UnderstoodArgument {
            value: ArgumentValue::Record(RecordValue::Named {
                workflow: "traveler".into(),
                named: "Omar Haddad".to_owned(),
            }),
            excerpt: None,
        },
    )]);
    UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers)
        .with_expectation(Expectation::Values(PendingAct {
            operation: SET_TRAVELER.into(),
            record: Some("tok-trip-1".into()),
            given,
            missing: vec!["traveler".to_owned()],
        }))
}

/// «new traveler», read first as the answer that sets the traveler, and given none.
fn first_reading() -> ScriptedTasks {
    script()
        .answer(
            "turn/segment",
            json!({"analysis": "Answers the question.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 2}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_TRAVELER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "No traveler given.", "arguments": {}, "overall": "confirmed"}),
        )
}

#[tokio::test]
async fn an_answer_that_gives_none_of_what_was_asked_is_routed_again() {
    let script = first_reading()
        .answer("u1/route.again", json!({"operations": [REGISTER]}))
        .answer(
            "u1/extract.after_reroute",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify.after_reroute",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        );
    let run = understand(script, &input("new traveler")).await;

    let rerouted = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/route.again"))
        .expect("routed again");
    let said = format!("{:?}", rerouted.messages.last().unwrap());
    assert!(said.contains("give no traveler"), "{said}");

    let acts = &run.understanding.acts;
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
async fn a_second_route_finding_nothing_else_leaves_the_question_asked() {
    let script = first_reading().answer("u1/route.again", json!({"operations": ["none"]}));
    let run = understand(script, &input("not sure")).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(
        acts[0].operation().map(|op| op.as_str()),
        Some(SET_TRAVELER)
    );
    // Still only the name that found nothing: the runtime's lookup asks for it again.
    assert_eq!(
        acts[0].arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Omar Haddad".to_owned(),
        })
    );
    assert!(run.understanding.not_understood.is_empty());
}
