//! A question the frame reads as asking whether one particular thing can be done («can I
//! set A?») asks for it: it is routed as a request, and stays a question only when no
//! operation does what it asks.
mod support;

use serde_json::json;
use support::{SET_NAME, routed, script, today, trip, trips, understand};
use turnframe_core::understanding::{ActStatus, UnitKind};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::UnderstandingInput;

fn asked(route: serde_json::Value) -> ScriptedTasks {
    // [1]can [2]I [3]name [4]the [5]trip?
    script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question.", "units": [{
                "kind": "question", "words": {"from": 1, "to": 5}, "workflow": "trip",
                "basis": "current_committed_state", "continues_previous": false
            }]}),
        )
        .answer("u1/frame", json!({"topic": "ability", "record": "r1"}))
        .answer("u1/route", route)
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
}

fn input() -> UnderstandingInput {
    UnderstandingInput::new("can I name the trip?", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]))
}

#[tokio::test]
async fn a_question_whether_one_thing_can_be_done_asks_for_it() {
    let run = understand(asked(routed(SET_NAME)), &input()).await;

    let understanding = &run.understanding;
    assert!(
        understanding.questions.is_empty(),
        "{:?} {understanding:?}",
        run.called()
    );
    assert_eq!(understanding.units[0].kind, UnitKind::Request);
    let [act] = understanding.acts.as_slice() else {
        panic!("{:?} {understanding:?}", run.called());
    };
    assert_eq!(act.operation().map(|op| op.as_str()), Some(SET_NAME));
    assert!(
        matches!(act.status, ActStatus::NeedsValue { .. }),
        "{act:?}"
    );
}

#[tokio::test]
async fn a_question_whether_something_no_operation_does_can_be_done_stays_a_question() {
    let run = understand(asked(json!({"operations": ["none"]})), &input()).await;

    let understanding = &run.understanding;
    assert_eq!(understanding.questions.len(), 1, "{understanding:?}");
    assert_eq!(understanding.units[0].kind, UnitKind::Question);
    assert!(understanding.acts.is_empty(), "{understanding:?}");
}
