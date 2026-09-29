//! The verifier is shown the record an act is aimed at as extraction saw it: the fields
//! that tell it from the others, so «the Haddad trip» can be judged against the trip
//! for Haddad, and what it holds, so «the same as the other line» can be judged against
//! that line.
mod support;

use serde_json::json;
use support::{
    SET_NAME, confirmed, one_request, routed, script, today, trip, trips, understand, words,
};
use turnframe_understand::UnderstandingInput;

#[tokio::test]
async fn the_verifier_sees_what_tells_the_record_apart() {
    let input =
        UnderstandingInput::new("on the Haddad trip set the name to Porto", "en-GB", today())
            .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")]));
    // [1]on [2]the [3]Haddad [4]trip [5]set [6]the [7]name [8]to [9]Porto
    let script = script()
        .answer("turn/segment", one_request(1, 9))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/locate",
            json!({"record": "r2", "named": {"from": 3, "to": 4}}),
        )
        .answer("u1/extract", json!({"arguments": {"value": words(9, 9)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    let shown = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("\"u1/verify\""))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(shown.contains("Record: Trip 2"), "{shown}");
    assert!(shown.contains("traveler: Haddad"), "{shown}");
    assert!(shown.contains("name: none"), "{shown}");
}
