//! A request routed to create a new record, whose words name a record of that workflow already
//! in view by its whole label, is routed once more, told that record exists: «open Trip 1 and
//! name it Lisbon» names Trip 1, and a second trip is not started for it.
mod support;

use serde_json::json;
use support::{OPEN, SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActAction;

#[tokio::test]
async fn a_new_record_naming_one_in_view_is_routed_again() {
    // [1]open [2]Trip [3]1 [4]and [5]name [6]it [7]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", json!({"operations": [OPEN, SET_NAME]}))
        .answer("u1/route.again", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(7, 7)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("open Trip 1 and name it Lisbon")).await;

    let actions: Vec<&ActAction> = run
        .understanding
        .acts
        .iter()
        .map(|act| &act.action)
        .collect();
    assert_eq!(
        actions,
        [&ActAction::Apply {
            operation: SET_NAME.into()
        }],
        "{:?}",
        run.understanding
    );
    let again = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/route.again"))
        .expect("routed again");
    assert!(format!("{:?}", again.messages).contains("Trip 1"));
}

#[tokio::test]
async fn a_new_record_naming_none_in_view_is_not_routed_again() {
    // [1]open [2]a [3]trip [4]for [5]Porto
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(OPEN));
    let run = understand(script, &turn("open a trip for Porto")).await;

    assert!(!run.was_called("u1/route.again"), "{:?}", run.called());
}
