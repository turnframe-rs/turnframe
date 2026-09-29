//! With two trips in view and nothing telling them apart, the act asks which one.
mod support;

use serde_json::json;
use support::{
    SET_NAME, confirmed, one_request, routed, script, today, trip, trips, understand, words,
};
use turnframe_core::understanding::ActTarget;
use turnframe_understand::UnderstandingInput;

#[tokio::test]
async fn several_records_that_fit_leave_the_act_ambiguous() {
    let input = UnderstandingInput::new("set the name to Lisbon", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")]));
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/locate", json!({"record": "ambiguous", "named": null}))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    assert_eq!(
        run.understanding.acts[0].target,
        ActTarget::Ambiguous {
            candidates: vec!["tok-trip-1".into(), "tok-trip-2".into()]
        }
    );
    let locate = run
        .provider
        .calls()
        .into_iter()
        .find(|c| c.metadata.get("task") == Some("u1/locate"))
        .unwrap();
    let shown = format!("{:?}", locate.messages);
    assert!(
        shown.contains("r1: Trip 1 · collecting · traveler: Bianchi"),
        "{shown}"
    );
    assert!(
        !shown.contains("name: none"),
        "locate shows identifying fields only"
    );
}
