//! A consumer sees each decision as a step, in the order the chain made them.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_understand::progress::{Located, Routing, Step};

#[tokio::test]
async fn every_step_is_published_as_it_is_decided() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Lisbon")).await;

    let steps = run.steps.steps();
    let names: Vec<&str> = steps
        .iter()
        .map(|step| match step {
            Step::Reading { .. } => "reading",
            Step::Segmented { .. } => "segmented",
            Step::Routed { .. } => "routed",
            Step::Located { .. } => "located",
            Step::Extracted { .. } => "extracted",
            Step::Verified { .. } => "verified",
            Step::Assembled { .. } => "assembled",
            other => panic!("unexpected step {other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "reading",
            "segmented",
            "routed",
            "located",
            "extracted",
            "verified",
            "assembled"
        ]
    );
    assert!(steps.contains(&Step::Routed {
        unit: turnframe_core::understanding::UnitId(1),
        to: Routing::Operation {
            operation: SET_NAME.into()
        },
    }));
    assert!(steps.iter().any(|step| matches!(
        step,
        Step::Located { record: Located::Record { label, .. }, .. } if label == "Trip 1"
    )));
    for step in &steps {
        assert!(!step.describe().is_empty());
    }
    let extracted = steps
        .iter()
        .find(|s| matches!(s, Step::Extracted { .. }))
        .unwrap();
    assert_eq!(
        extracted.describe(),
        "u1.a1: value = «Lisbon» (from «Lisbon»)."
    );
}
