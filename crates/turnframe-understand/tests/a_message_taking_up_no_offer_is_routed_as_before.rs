//! A message that takes up none of the offers the last reply made is routed as it always is:
//! reading it against the offers first changes nothing when it asks for something else.
mod support;

use serde_json::json;
use support::{REBOOK, SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ArgumentValue};
use turnframe_understand::{OfferBrief, PendingAct};

#[tokio::test]
async fn a_message_taking_up_no_offer_is_routed_as_before() {
    // [1]name [2]it [3]Lisbon
    let input = turn("name it Lisbon").with_offer(OfferBrief::new(
        "Rebook the quoted flight.",
        PendingAct {
            operation: REBOOK.into(),
            record: Some("tok-trip-1".into()),
            given: Default::default(),
            missing: Vec::new(),
        },
    ));
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/take_up", json!({"offer": "none"}))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.action,
        ActAction::Apply {
            operation: SET_NAME.into()
        }
    );
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(json!("Lisbon"))
    );
    assert!(run.was_called("u1/take_up"), "{:?}", run.called());
}
