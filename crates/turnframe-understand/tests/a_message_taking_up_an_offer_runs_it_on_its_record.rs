//! A message that takes up an offer the last reply made runs that offer: its operation, on its
//! record, with the values it already knew. Nothing is routed or located again for it.
mod support;

use serde_json::json;
use support::{REBOOK, SET_NAME, confirmed, one_request, script, today, trip, trips, understand};
use turnframe_core::understanding::{ActAction, ActTarget, ArgumentValue, UnderstoodArgument};
use turnframe_understand::{OfferBrief, PendingAct, UnderstandingInput};

fn offer(
    words: &str,
    operation: &str,
    record: &str,
    given: &[(&str, serde_json::Value)],
) -> OfferBrief {
    OfferBrief::new(
        words,
        PendingAct {
            operation: operation.into(),
            record: Some(record.into()),
            given: given
                .iter()
                .map(|(name, value)| {
                    (
                        (*name).to_owned(),
                        UnderstoodArgument {
                            value: ArgumentValue::Json(value.clone()),
                            excerpt: None,
                        },
                    )
                })
                .collect(),
            missing: Vec::new(),
        },
    )
}

#[tokio::test]
async fn a_message_taking_up_an_offer_runs_it_on_its_record() {
    // [1]the [2]second [3]one
    let input = UnderstandingInput::new("the second one", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Ferri")]))
        .with_offer(offer(
            "Rebook the quoted flight.",
            REBOOK,
            "tok-trip-2",
            &[],
        ))
        .with_offer(offer(
            "Call it Porto.",
            SET_NAME,
            "tok-trip-1",
            &[("value", json!("Porto"))],
        ));
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/take_up", json!({"offer": "o2"}))
        .answer("u1/verify", confirmed(json!({})));
    let run = understand(script, &input).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act: {:?}", run.understanding);
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
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(json!("Porto"))
    );
    assert!(!run.was_called("u1/route"), "{:?}", run.called());
    assert!(!run.was_called("u1/locate"), "{:?}", run.called());
}
