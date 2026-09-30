//! Two parts read as creating the same record, the second placed on the first: when the first
//! reading is dropped, the second is the creation itself. It creates its own record, and never
//! waits on itself.
#![allow(clippy::panic)]

mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ActTarget};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const REGISTER: &str = "traveler.create_draft";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

#[tokio::test]
async fn a_second_reading_of_a_creation_is_the_creation() {
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
    let input = UnderstandingInput::new("register a traveler, a new one please", "en-GB", today())
        .with_workflow(travelers);
    // [1]register [2]a [3]traveler, [4]a [5]new [6]one [7]please
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two readings of one request.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "traveler"},
                {"kind": "request", "words": {"from": 4, "to": 7}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER]}))
        .answer("u2/route", json!({"operations": [REGISTER]}))
        .answer("u2/locate", json!({"record": "s1", "named": null}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Not asked.", "arguments": {}, "overall": "not_requested"}),
        )
        .answer(
            "u2/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let [act] = acts.as_slice() else {
        panic!("one creation: {acts:?}");
    };
    assert_eq!(
        act.target,
        ActTarget::New {
            workflow: "traveler".into()
        },
        "{acts:?}"
    );
    assert!(act.depends_on.is_empty(), "{acts:?}");
    assert_eq!(act.status, ActStatus::Ready, "{acts:?}");
}
