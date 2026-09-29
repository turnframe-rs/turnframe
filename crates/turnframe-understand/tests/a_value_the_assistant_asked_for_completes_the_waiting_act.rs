//! «Porto» after «what is the name?» completes the act that was waiting for it.
//! The route confirms the value is for that act; its record is the act's, not looked for.
mod support;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ActStatus, ActTarget, ArgumentValue};
use turnframe_understand::{Expectation, PendingAct};

#[tokio::test]
async fn a_value_the_assistant_asked_for_completes_the_waiting_act() {
    let pending = PendingAct {
        operation: SET_NAME.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::new(),
        missing: vec!["value".to_owned()],
    };
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Gives the name.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 1}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_NAME]}))
        .answer("u1/extract", json!({"arguments": {"value": words(1, 1)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let input = turn("Porto").with_expectation(Expectation::Values(pending));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.action,
        ActAction::Apply {
            operation: SET_NAME.into()
        }
    );
    assert_eq!(
        act.target,
        ActTarget::Record {
            token: "tok-trip-1".into()
        }
    );
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("Porto"))
    );
    assert!(run.was_called("u1/route") && !run.was_called("u1/locate"));
    let segment = &run.provider.calls()[0];
    assert!(
        format!("{:?}", segment.messages).contains("The assistant asked for: trip name of Trip 1")
    );
}
