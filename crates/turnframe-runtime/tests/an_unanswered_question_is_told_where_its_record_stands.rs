//! A question no fact answers is not left at «I cannot tell»: the reply gives where the
//! record it is about stands, what it holds and what it still needs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narration};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{
    ScriptedProvider, ScriptedProviderBuilder, ScriptedReply, UnderstandingBuilder,
};
use turnframe_test::workflows::trip::{SAMPLE_NAME, complete_case};

const QUESTION: &str = "is everything in order with it?";

async fn run(provider: ScriptedProviderBuilder) -> (Arc<ScriptedProvider>, String) {
    let provider = provider.build_shared();
    let understanding = UnderstandingBuilder::of(QUESTION)
        .ask(QUESTION)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    let turn = harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), QUESTION))
        .await
        .unwrap();
    (provider, narration(&turn))
}

fn unanswered() -> ScriptedProviderBuilder {
    ScriptedProvider::builder("scripted", "model-1").reply_to(
        ModelPurpose::Answer,
        ScriptedReply::cannot_answer("Nothing I know says."),
    )
}

#[tokio::test]
async fn the_writer_is_given_where_the_record_stands() {
    let (provider, _) = run(unanswered().acknowledging("It is on track.")).await;
    let brief = provider
        .calls_for(ModelPurpose::Acknowledge)
        .first()
        .expect("a reply is written")
        .user_text();
    assert!(brief.contains("\"standing\""), "{brief}");
    assert!(
        brief.contains(SAMPLE_NAME),
        "what the record holds: {brief}"
    );
}

#[tokio::test]
async fn code_says_where_the_record_stands() {
    let refused = |provider: ScriptedProviderBuilder| {
        provider
            .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Fine."))
            .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
    };
    let (_, said) = run(refused(refused(unanswered()))).await;
    assert!(said.contains(SAMPLE_NAME), "what the record holds: {said}");
}
