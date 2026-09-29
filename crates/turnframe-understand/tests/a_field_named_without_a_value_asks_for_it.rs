//! «I want to set name» names the name and gives none: the act asks, never writes. It is
//! verified first, and asked for its value because the verdict found it asked for.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand};
use turnframe_core::understanding::{ActAction, ActStatus, ActTarget};

#[tokio::test]
async fn a_field_named_without_a_value_asks_for_it() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asks to set the name.", "arguments": {}, "overall": "confirmed"}),
        );
    let run = understand(script, &turn("I want to set name")).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act expected: {:?}", run.understanding);
    };
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
    assert!(act.arguments.is_empty());
    assert_eq!(
        act.status,
        ActStatus::NeedsValue {
            arguments: vec!["value".to_owned()],
            reason: None
        }
    );
    assert!(
        run.was_called("u1/verify"),
        "an act that asks is verified: {:?}",
        run.called()
    );
    assert!(
        !run.was_called("u1/locate"),
        "one record in view needs no locating"
    );
}
