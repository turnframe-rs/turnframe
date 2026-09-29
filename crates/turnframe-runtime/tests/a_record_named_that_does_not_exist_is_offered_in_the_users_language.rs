//! The offer to register a record the user named is written in the user's language, and
//! calls the record by its workflow's own word for it, never by the workflow's key.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::ResponseBlock;
use turnframe_core::understanding::{ActTarget, RecordValue};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{TripState, operations};

#[tokio::test]
async fn a_record_named_that_does_not_exist_is_offered_in_the_users_language() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "il viaggio è per Nadia Rinaldi";
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_TRAVELER,
            ActTarget::Record {
                token: token_for(turn, "trip", "trip-1"),
            },
            serde_json::json!({}),
            text,
        )
        .with_record(
            "traveler",
            RecordValue::Named {
                workflow: "traveler".into(),
                named: "Nadia Rinaldi".to_owned(),
            },
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Viaggio 1", 3, TripState::default())
        .understands(understanding)
        .without_narration()
        .build()
        .await;
    let mut input = harness.turn(turn, text);
    input.locale = "it-IT".into();

    let answered = harness.handle(input).await.unwrap();

    let notice = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Notice(notice) => Some(notice.text.resolve(&"it-IT".into()).to_owned()),
            _ => None,
        })
        .expect("the offer is on screen");
    assert_eq!(
        notice,
        "Non trovo ancora «Nadia Rinaldi» come viaggiatore: posso registrarlo io, oppure puoi \
         indicare un altro nome."
    );
}
