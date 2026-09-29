//! A value chosen from a closed set is the member chosen, whatever words it was read from:
//! «too many words» is a doubt about copied text, and on a choice it holds nothing up.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{one_request, routed, script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::UnderstandingInput;

const ASSIGN: &str = "trip.assign_payer";

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum Payer {
    Traveler,
    Airline,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AssignPayer {
    payer: Payer,
}

#[tokio::test]
async fn a_chosen_value_cannot_take_too_many_words() {
    let assign = OperationSpec::new(ASSIGN)
        .summary("Say who pays for an extra.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<AssignPayer>()
        .argument("payer", |a| a.label("who pays").required());
    let input = UnderstandingInput::new("airline then", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]).operation(assign));
    // [1]airline [2]then
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(ASSIGN))
        .answer(
            "u1/extract",
            json!({"arguments": {"payer": {
                "kind": "value", "message": "current", "from": 1, "to": 2, "value": "airline"
            }}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "It adds «then».", "arguments": {"payer": "too_much"},
                   "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert_eq!(
        act.arguments["payer"].value,
        ArgumentValue::Json("airline".into())
    );
    assert!(!run.was_called("u1/extract.after_verify"));
}
