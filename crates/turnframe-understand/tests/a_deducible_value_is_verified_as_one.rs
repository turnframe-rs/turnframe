//! A value the operation lets be deduced («a bag» is one bag) is shown to the verifier as
//! deducible, so words that imply it are not judged as saying nothing.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, one_request, routed, script, today, trip, trips, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_understand::UnderstandingInput;

const ADD_ITEM: &str = "trip.add_item";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AddItem {
    name: String,
    count: u32,
}

#[tokio::test]
async fn a_deducible_value_is_verified_as_one() {
    let add_item = OperationSpec::new(ADD_ITEM)
        .summary("Add an item.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<AddItem>()
        .argument("name", |a| a.label("name").required())
        .argument("count", |a| a.label("count").required().inferred());
    let input = UnderstandingInput::new("add a bag", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]).operation(add_item));
    // [1]add [2]a [3]bag
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(ADD_ITEM))
        .answer(
            "u1/extract",
            json!({"arguments": {
                "name": words(3, 3),
                "count": {"kind": "value", "message": "current", "from": 2, "to": 2, "value": 1}
            }}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"name": "stated", "count": "stated"})),
        );
    let run = understand(script, &input).await;

    let shown = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("\"u1/verify\""))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(shown.contains("count (may be deduced)"), "{shown}");
}

/// «Not stated» is what a deduced value is, not a doubt about it: the deduction stands.
#[tokio::test]
async fn a_deduced_value_is_not_doubted_for_being_unsaid() {
    let add_item = OperationSpec::new(ADD_ITEM)
        .summary("Add an item.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<AddItem>()
        .argument("name", |a| a.label("name").required())
        .argument("count", |a| a.label("count").required().inferred());
    let input = UnderstandingInput::new("add a bag", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]).operation(add_item));
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(ADD_ITEM))
        .answer(
            "u1/extract",
            json!({"arguments": {
                "name": words(3, 3),
                "count": {"kind": "value", "message": "current", "from": 2, "to": 2, "value": 1}
            }}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "No count is said.", "arguments": {"name": "stated", "count": "not_stated"},
                   "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.status,
        turnframe_core::understanding::ActStatus::Ready,
        "{act:?}"
    );
    assert!(!run.was_called("u1/extract.after_verify"));
}
