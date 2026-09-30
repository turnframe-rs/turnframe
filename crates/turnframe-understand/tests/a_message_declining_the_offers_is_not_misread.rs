//! A message saying no to what the last reply offered («no, that's it») asks for nothing and
//! is small talk: no act, nothing reported as not understood, and nothing routed.
mod support;

use serde_json::json;
use support::{REBOOK, one_request, script, turn, understand};
use turnframe_core::understanding::UnitKind;
use turnframe_understand::{OfferBrief, PendingAct};

#[tokio::test]
async fn a_message_declining_the_offers_is_not_misread() {
    // [1]no, [2]that's [3]it
    let input = turn("no, that's it").with_offer(OfferBrief::new(
        "Rebook the quoted flight.",
        PendingAct {
            operation: REBOOK.into(),
            record: Some("tok-trip-1".into()),
            given: Default::default(),
            missing: Vec::new(),
        },
    ));
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/take_up", json!({"offer": "declines"}));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    assert!(!run.was_called("u1/route"), "{:?}", run.called());
    assert_eq!(
        understanding.units[0].kind,
        UnitKind::Chitchat,
        "it asks for nothing"
    );
}

#[tokio::test]
async fn a_cancel_of_nothing_earlier_may_decline_the_offers() {
    // [1]nope, [2]that's [3]all
    let input = turn("nope, that's all").with_offer(OfferBrief::new(
        "Rebook the quoted flight.",
        PendingAct {
            operation: REBOOK.into(),
            record: Some("tok-trip-1".into()),
            given: Default::default(),
            missing: Vec::new(),
        },
    ));
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Withdraws.", "units": [{"kind": "cancel",
                "words": {"from": 1, "to": 3}, "workflow": "trip", "cancels": null}]}),
        )
        .answer("u1/take_up", json!({"offer": "declines"}));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
}
