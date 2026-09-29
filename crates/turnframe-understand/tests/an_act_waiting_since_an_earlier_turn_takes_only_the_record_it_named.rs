//! An act of an earlier turn still waiting for a record the user named is completed by a
//! record registered later under that name, and never by one registered under another.
mod support;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{
    ActTarget, ArgumentValue, RecordValue, UnderstoodAct, UnderstoodArgument,
};
use turnframe_understand::{PendingAct, UnderstandingInput, WorkflowBrief};

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

async fn registering(message: &str, name: &str) -> Vec<UnderstoodAct> {
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
                .target(TargetPolicy::NewCaseOnly)
                .mutating()
                .arguments::<Register>()
                .argument("full_name", |a| a.label("name").names_the_record()),
        );
    let named = UnderstoodArgument {
        value: ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Luca Ferri".to_owned(),
        }),
        excerpt: None,
    };
    let input = UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers)
        .with_waiting(PendingAct {
            operation: SET_TRAVELER.into(),
            record: Some("tok-trip-1".into()),
            given: BTreeMap::from([("traveler".to_owned(), named)]),
            missing: vec!["traveler".to_owned()],
        });
    let words = message.split_whitespace().count();
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers a traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": words}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": {
                "kind": "words", "text": name, "message": "current", "from": 2, "to": words
            }}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {"full_name": "stated"}, "overall": "confirmed"}),
        );
    understand(script, &input).await.understanding.acts
}

fn sets_the_traveler(acts: &[UnderstoodAct]) -> bool {
    acts.iter().any(|act| {
        act.operation()
            .is_some_and(|op| op.as_str() == SET_TRAVELER)
            && act.target
                == ActTarget::Record {
                    token: "tok-trip-1".into(),
                }
    })
}

#[tokio::test]
async fn an_act_waiting_since_an_earlier_turn_takes_the_record_registered_under_its_name() {
    let acts = registering("register luca  FERRI", "luca  FERRI").await;
    assert!(sets_the_traveler(&acts), "{acts:?}");
}

#[tokio::test]
async fn an_act_waiting_since_an_earlier_turn_never_takes_a_record_named_otherwise() {
    let acts = registering("register Bianchi", "Bianchi").await;
    assert!(!sets_the_traveler(&acts), "{acts:?}");
}
