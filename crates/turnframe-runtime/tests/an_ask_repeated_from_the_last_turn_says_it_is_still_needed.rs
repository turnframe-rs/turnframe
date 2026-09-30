//! When the reply would ask what the last reply asked, of the same record, and it is still
//! open, it says so: the writer is told the question is asked again, and why.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, narration, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::Understanding;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProviderBuilder, ScriptedReply, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

const AGAIN: &str = "the ask is the one the last reply asked";

fn turns() -> (TurnId, TurnId) {
    (
        TurnId::from(uuid::Uuid::from_u128(1)),
        TurnId::from(uuid::Uuid::from_u128(2)),
    )
}

/// Two date changes on a trip still owing its name: both replies ask for the name.
fn two_dates() -> [(&'static str, Understanding); 2] {
    let (one, two) = turns();
    let date = "fly on 5 October";
    let later = "make it the 6th";
    let understood_date = UnderstandingBuilder::of(date)
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(one, "trip", "trip-1"),
            serde_json::json!({ "value": "2026-10-05" }),
            date,
        )
        .build()
        .unwrap();
    let understood_later = UnderstandingBuilder::of(later)
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(two, "trip", "trip-1"),
            serde_json::json!({ "value": "2026-10-06" }),
            later,
        )
        .build()
        .unwrap();
    [(date, understood_date), (later, understood_later)]
}

async fn run(
    provider: ScriptedProviderBuilder,
) -> (Arc<turnframe_test::providers::ScriptedProvider>, String) {
    let provider = provider.build_shared();
    let [(date, understood_date), (later, understood_later)] = two_dates();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understood_date)
        .understands(understood_later)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    let (one, two) = turns();
    harness.handle(harness.turn(one, date)).await.unwrap();
    let turn = harness.handle(harness.turn(two, later)).await.unwrap();
    (provider, narration(&turn))
}

#[tokio::test]
async fn an_ask_repeated_from_the_last_turn_says_it_is_still_needed() {
    let (provider, _) = run(narrating().acknowledging("Noted.")).await;

    let briefs: Vec<String> = provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .map(|call| call.user_text())
        .collect();
    assert_eq!(briefs.len(), 2);
    assert!(
        !briefs[0].contains(AGAIN),
        "the first ask is new: {}",
        briefs[0]
    );
    assert!(
        briefs[1].contains(AGAIN),
        "the second repeats it: {}",
        briefs[1]
    );
}

#[tokio::test]
async fn code_asking_again_says_it_is_still_needed() {
    let refused = |provider: ScriptedProviderBuilder| {
        provider
            .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Noted."))
            .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
    };
    let (_, said) = run(refused(refused(narrating()))).await;
    assert!(said.contains("I still need this to go on."), "{said}");
}
