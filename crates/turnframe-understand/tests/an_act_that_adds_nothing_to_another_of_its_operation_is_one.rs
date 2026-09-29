//! Two acts of one operation on one record, where one gives nothing the other does not, are
//! one act read twice: the one with more stands, and nothing runs empty.
#![allow(clippy::unwrap_used, clippy::panic)]
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use support::{confirmed, routed, script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::ArgumentValue;
use turnframe_understand::UnderstandingInput;

const CHANGE: &str = "trip.change_extra";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Change {
    extra: u32,
    quantity: Option<u32>,
}

#[tokio::test]
async fn an_act_that_adds_nothing_to_another_of_its_operation_is_one() {
    let change = OperationSpec::new(CHANGE)
        .summary("Change an extra.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<Change>()
        .argument("extra", |a| a.required().inferred());
    let workflow = trips(vec![])
        .operation(change)
        .record(trip(1, "Bianchi").offering([CHANGE]));
    let input = UnderstandingInput::new("actually, the bags are 13", "en-GB", today())
        .with_workflow(workflow);
    // [1]actually, [2]the [3]bags [4]are [5]13
    let extra = |from: usize, to: usize| json!({"kind": "value", "message": "current", "from": from, "to": to, "value": 1});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A correction.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 1}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 2, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(CHANGE))
        .answer("u2/route", routed(CHANGE))
        .answer(
            "u1/extract",
            json!({"arguments": {"extra": extra(1, 1), "quantity": {"kind": "not_given"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"extra": extra(3, 3), "quantity":
                {"kind": "value", "message": "current", "from": 5, "to": 5, "value": 13}}}),
        )
        .answer("u1/verify", confirmed(json!({"extra": "stated"})))
        .answer(
            "u2/verify",
            confirmed(json!({"extra": "stated", "quantity": "stated"})),
        );
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(
        acts[0].arguments["quantity"].value,
        ArgumentValue::Json(Value::from(13))
    );
}
