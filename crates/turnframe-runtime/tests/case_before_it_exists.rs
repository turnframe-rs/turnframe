//! The phase a case is in before it exists, and whether anyone hears about it.
//!
//! A start mints an identifier and resolves onto it exactly, so the case is the
//! name of the turn. When the domain compiles no commands for it — the answer
//! a singleton relies on — nothing is written or loaded, and the runtime still
//! projects the unborn case so its briefing reaches the writer: the one phase
//! where the workflow has the most to say.
//!
//! A refused start is not projected. Its case will never exist, and briefing the
//! writer about it would contradict the only answer the turn has.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, UNWRITTEN};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::traveler::TravelerState;
use turnframe_test::workflows::trip::incomplete_case;

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// What the acknowledgement was written from, for a turn that started the workflow.
async fn brief_after_starting(prerequisite_met: bool) -> String {
    let turn = turn_one();
    let text = "Apri un viaggio nuovo";
    let understood = UnderstandingBuilder::of(text)
        .start(UNWRITTEN, text)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Va bene.")
        .build_shared();
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .unwritten()
        .understands(understood)
        .provider(Arc::clone(&provider));
    if prerequisite_met {
        // The workflow under test declares that it may only start while a
        // traveler is being filled in, so this is what decides whether the same
        // act resolves or is refused.
        builder = builder.traveler("trav-1", "Ferri", 1, TravelerState::default());
    }
    let harness = builder.build().await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the writing stage ran")
        .user_text()
}

/// The workflow's own words for the phase before the record exists reach the
/// stage that speaks.
#[tokio::test]
async fn a_case_that_does_not_exist_yet_still_briefs_the_writer() {
    let brief = brief_after_starting(true).await;
    assert!(
        brief.contains("\"starting\": [\n    \"unwritten\""),
        "the turn began a record it wrote nothing for: {brief}"
    );
    assert!(
        brief.contains("ask for the traveler"),
        "the one phase where the workflow has the most to say: {brief}"
    );
}

/// And a start the turn refused is not projected as an empty record: the
/// prerequisite is unmet here, so the same act is refused.
#[tokio::test]
async fn a_start_the_turn_refused_is_not_projected() {
    let brief = brief_after_starting(false).await;
    assert!(
        !brief.contains("ask for the traveler"),
        "the same act, refused: there is no case coming, so there is nothing to \
         brief about one: {brief}"
    );
    assert!(
        brief.contains("\"not_done\": [\n    \""),
        "what the turn has to say is the refusal: {brief}"
    );
}
