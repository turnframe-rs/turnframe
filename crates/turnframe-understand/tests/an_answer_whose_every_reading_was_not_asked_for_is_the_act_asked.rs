//! An answer to what the last reply asked, read only as another act the user did not ask for,
//! is read again as the act that asked: its words still have to give the value, as any
//! answer's do.
mod support;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use support::{REBOOK, SET_NAME, confirmed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ActStatus, ArgumentValue};
use turnframe_understand::{Expectation, PendingAct, Speaker};

fn pending() -> PendingAct {
    PendingAct {
        operation: SET_NAME.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::new(),
        missing: vec!["value".to_owned()],
    }
}

/// A value given, read first as a rebooking nobody asked for, then as `reread` gives it.
fn refused_then(reread: serde_json::Value) -> turnframe_tasks::testing::ScriptedTasks {
    let not_asked = json!({"reason": "Nothing about a rebooking was said.", "arguments": {},
                           "overall": "not_requested"});
    script()
        .answer(
            "turn/segment",
            json!({"analysis": "Gives the name.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 5}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REBOOK]}))
        .answer("u1/verify", not_asked.clone())
        .answer("u1/verify.after_repair", not_asked)
        .answer("u1/extract.after_reroute", reread)
}

#[tokio::test]
async fn an_answer_whose_every_reading_was_not_asked_for_is_the_act_asked() {
    // [1]il [2]nome [3]è [4]offsite [5]marzo
    let script = refused_then(json!({"arguments": {"value": words(4, 5)}})).answer(
        "u1/verify.after_reroute",
        confirmed(json!({"value": "stated"})),
    );
    let input = turn("il nome è offsite marzo").with_expectation(Expectation::Values(pending()));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    let [act] = understanding.acts.as_slice() else {
        panic!("one act: {understanding:?}");
    };
    assert_eq!(
        act.action,
        ActAction::Apply {
            operation: SET_NAME.into()
        }
    );
    assert_eq!(act.status, ActStatus::Ready, "{understanding:?}");
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("offsite marzo"))
    );
}

#[tokio::test]
async fn not_when_the_value_comes_from_other_words_than_its_own() {
    // [1]il [2]nome [3]è [4]offsite [5]marzo
    let script = refused_then(json!({"arguments": {"value": {
        "kind": "words", "message": "m1", "from": 3, "to": 3, "text": "Porto"
    }}}))
    .answer(
        "u1/verify.after_reroute",
        confirmed(json!({"value": "stated"})),
    );
    let input = turn("il nome è offsite marzo")
        .with_earlier(Speaker::User, "call it Porto")
        .with_expectation(Expectation::Values(pending()));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(understanding.not_understood.len(), 1, "{understanding:?}");
}
