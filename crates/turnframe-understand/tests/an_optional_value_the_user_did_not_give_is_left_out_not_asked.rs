//! An optional value the check finds the user did not give is left out, and the act goes on
//! without it: nobody asked for it, so it is never asked back. A required one is asked.
#![allow(clippy::panic)]

mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::ActStatus;
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const NOTE: &str = "trip.add_note";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AddNote {
    text: String,
    by: Option<String>,
}

#[tokio::test]
async fn an_optional_value_the_user_did_not_give_is_left_out_not_asked() {
    let workflow = WorkflowBrief::new("trip")
        .operation(
            OperationSpec::new(NOTE)
                .summary("Add a note to the trip, and who wrote it when said.")
                .target(TargetPolicy::RequiresExistingCase)
                .mutating()
                .arguments::<AddNote>()
                .argument("text", |a| a.label("note").required())
                .argument("by", |a| a.label("written by")),
        )
        .record(trip(1, "Bianchi").offering([NOTE]));
    let input =
        UnderstandingInput::new("note seats together", "en-GB", today()).with_workflow(workflow);
    // [1]note [2]seats [3]together
    let read = json!({"arguments": {"text": words(2, 3), "by": words(1, 1)}});
    let found = json!({"reason": "Nobody said who wrote it.",
        "arguments": {"text": "stated", "by": "not_stated"}, "overall": "confirmed"});
    let script = script()
        .answer("turn/segment", support::one_request(1, 3))
        .answer("u1/route", json!({"operations": [NOTE]}))
        .answer("u1/extract", read.clone())
        .answer("u1/verify", found.clone())
        .answer("u1/extract.after_verify", read)
        .answer("u1/verify.after_repair", found);
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    let [act] = understanding.acts.as_slice() else {
        panic!("one act: {understanding:?}");
    };
    assert_eq!(act.status, ActStatus::Ready, "{understanding:?}");
    assert!(!act.arguments.contains_key("by"), "{understanding:?}");
}
