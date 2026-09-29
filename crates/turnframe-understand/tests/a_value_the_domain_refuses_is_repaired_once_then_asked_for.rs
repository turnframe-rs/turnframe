//! The domain's refusal of one argument gets one repair with its explanation, then asks.
//! Both the repair and the reason asked with name the value that was refused.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand_checked, words};
use turnframe_core::error::DomainRejection;
use turnframe_core::locale::LocalizedText;
use turnframe_core::understanding::{ActStatus, UnderstoodAct};
use turnframe_understand::ActChecker;

struct NoSingleWordSubjects;

impl ActChecker for NoSingleWordSubjects {
    fn check(&self, act: &UnderstoodAct) -> Result<(), DomainRejection> {
        match act.arguments.get("value") {
            Some(value) if !format!("{:?}", value.value).contains(' ') => Err(
                DomainRejection::new("subject_too_short", "trip.name.too_short")
                    .on_argument("/value")
                    .with_explanation(LocalizedText::new("A name needs at least two words.")),
            ),
            _ => Ok(()),
        }
    }
}

#[tokio::test]
async fn a_value_the_domain_refuses_is_repaired_once_then_asked_for() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "u1/extract.after_check",
            json!({"arguments": {"value": words(5, 5)}}),
        );
    let run = understand_checked(
        script,
        &turn("set the name to Lisbon"),
        &NoSingleWordSubjects,
    )
    .await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.status,
        ActStatus::NeedsValue {
            arguments: vec!["value".to_owned()],
            reason: Some("«Lisbon»: A name needs at least two words.".to_owned()),
        }
    );
    assert!(act.arguments.is_empty());
    assert!(run.was_called("u1/extract.after_check"));
    let repair = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/extract.after_check"))
        .unwrap();
    let said = format!("{:?}", repair.messages.last().unwrap());
    assert!(said.contains("«Lisbon» was refused"), "{said}");
}
