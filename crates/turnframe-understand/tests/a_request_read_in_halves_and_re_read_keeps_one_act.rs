//! A request whose parts are read as two halves of it, one of them also re-read as another
//! request, comes out as the one act the user asked for: «add a checked bag, it costs 40 euros,
//! the airline pays» adds one bag at 40 euros paid by the airline, and nothing vanishes.
#![allow(clippy::panic)]

mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, trip, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::ActStatus;
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Extra {
    description: String,
    price: String,
    payer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Payer {
    extra: u32,
    payer: String,
}

#[tokio::test]
async fn a_request_read_in_halves_and_re_read_keeps_one_act() {
    let spec = |key: &str| {
        OperationSpec::new(key)
            .summary("A change to the trip.")
            .target(TargetPolicy::RequiresExistingCase)
            .mutating()
    };
    let workflow = WorkflowBrief::new("trip")
        .operation(spec("trip.add_extra").arguments::<Extra>())
        .operation(spec("trip.assign_payer").arguments::<Payer>())
        .record(trip(1, "Bianchi").offering(["trip.add_extra", "trip.assign_payer"]));
    let message = "add a checked bag, it costs 40 euros, the airline pays";
    let input = UnderstandingInput::new(message, "en-GB", today()).with_workflow(workflow);
    // [1]add [2]a [3]checked [4]bag, [5]it [6]costs [7]40 [8]euros, [9]the [10]airline [11]pays
    let text = |text: &str, from: usize, to: usize| json!({"kind": "words", "text": text, "message": "current", "from": from, "to": to});
    let bag = text("checked bag", 3, 4);
    let price = text("40 euros", 7, 8);
    let payer = text("airline", 10, 10);
    let none = json!({"kind": "not_given"});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request in two parts.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 11}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": ["trip.add_extra"]}))
        .answer(
            "u2/route",
            json!({"operations": ["trip.add_extra", "trip.assign_payer"]}),
        )
        .answer(
            "u1/extract",
            json!({"arguments": {"description": bag, "price": price, "payer": payer}}),
        )
        .answer(
            "u1/extract.after_elsewhere",
            json!({"arguments": {"description": bag, "price": none, "payer": none}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"description": bag, "price": price, "payer": payer}}),
        )
        .answer(
            "u2/extract.after_elsewhere",
            json!({"arguments": {"description": none, "price": price, "payer": payer}}),
        )
        .answer(
            "u2.a2/extract",
            json!({"arguments": {"extra": {"kind": "not_given"}, "payer": payer}}),
        )
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer(
            "u2/verify",
            confirmed(json!({"price": "stated", "payer": "stated"})),
        )
        .answer("u2.a2/verify", confirmed(json!({"payer": "stated"})));
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let [adding] = acts.as_slice() else {
        panic!("one act: {acts:?}");
    };
    assert_eq!(
        adding.operation().map(|op| op.as_str()),
        Some("trip.add_extra")
    );
    assert_eq!(adding.status, ActStatus::Ready, "{acts:?}");
    for name in ["description", "price", "payer"] {
        assert!(adding.arguments.contains_key(name), "{name}: {acts:?}");
    }
}
