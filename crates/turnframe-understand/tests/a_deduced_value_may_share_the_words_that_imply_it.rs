//! A value that may be deduced points at the words that imply it, which may be the words of
//! the value they state: «a seat upgrade» is one line of one. Two stated values still
//! may not share their words.
#![allow(clippy::unwrap_used, clippy::panic)]
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use support::{confirmed, one_request, routed, script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::UnderstandingInput;

const CHARGE: &str = "trip.charge";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Charge {
    description: String,
    quantity: u32,
}

#[tokio::test]
async fn a_deduced_value_may_share_the_words_that_imply_it() {
    let charge = OperationSpec::new(CHARGE)
        .summary("Charge something.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<Charge>()
        .argument("description", |a| a.required())
        .argument("quantity", |a| a.required().inferred());
    let mut workflow = trips(vec![]).operation(charge);
    workflow = workflow.record(trip(1, "Bianchi").offering([CHARGE]));
    let input =
        UnderstandingInput::new("charge a seat upgrade", "en-GB", today()).with_workflow(workflow);
    // [1]charge [2]a [3]seat [4]upgrade
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(CHARGE))
        .answer(
            "u1/extract",
            json!({"arguments": {
                "description": {"kind": "words", "message": "current", "from": 2, "to": 4,
                                "text": "seat upgrade"},
                "quantity": {"kind": "value", "message": "current", "from": 2, "to": 4, "value": 1}
            }}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "quantity": "stated"})),
        );
    let run = understand(script, &input).await;

    let act = run
        .understanding
        .acts
        .first()
        .unwrap_or_else(|| panic!("{:?}", run.understanding));
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["description"].value,
        ArgumentValue::Json(Value::from("seat upgrade"))
    );
    assert_eq!(
        act.arguments["quantity"].value,
        ArgumentValue::Json(json!(1))
    );
}
