//! A traveler the user names while none is registered is read as a name to look up, not
//! as nothing given: the task is told none is listed, and copies the name before it
//! chooses a handle.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ArgumentValue, RecordValue};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::OutputSpec;
use turnframe_understand::{Expectation, PendingAct, UnderstandingInput};

const SET_TRAVELER: &str = "trip.set_traveler";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

#[tokio::test]
async fn a_record_named_when_none_is_listed_is_looked_up_by_name() {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
    let input = UnderstandingInput::new("it is for Omar Haddad", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_expectation(Expectation::Values(PendingAct {
            operation: SET_TRAVELER.into(),
            record: Some("tok-trip-1".into()),
            given: BTreeMap::new(),
            missing: vec!["traveler".to_owned()],
        }));
    // [1]it [2]is [3]for [4]Omar [5]Haddad
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Names the traveler.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 5}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_TRAVELER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {
                "kind": "record", "name": "Omar Haddad", "message": "current",
                "from": 4, "to": 5, "record": "by_name"
            }}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Omar Haddad".to_owned(),
        })
    );
    let calls = run.provider.calls();
    let extract = calls
        .iter()
        .find(|call| call.purpose == ModelPurpose::Extract)
        .expect("an extract call");
    assert!(
        format!("{:?}", extract.messages)
            .contains("No record is listed for traveler: one the user names is by_name")
    );
    let OutputSpec::Json { schema, .. } = &extract.output else {
        panic!("a schema")
    };
    let record = &schema["properties"]["arguments"]["properties"]["traveler"]["anyOf"][1];
    let order: Vec<&str> = record["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        order[..2],
        ["kind", "name"],
        "the name right after the kind"
    );
}
