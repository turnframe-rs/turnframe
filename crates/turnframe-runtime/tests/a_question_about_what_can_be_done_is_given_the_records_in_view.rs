//! A question about what can be done that names no record is answered with what the records in
//! view hold beside what is on offer: «what is the new flight, and can I take it?» asks both.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::QuestionTopic;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, ScriptedReply, UnderstandingBuilder};
use turnframe_test::workflows::trip::{SAMPLE_NAME, complete_case};

const QUESTION: &str = "what can I do about the new flight?";

#[tokio::test]
async fn a_question_about_what_can_be_done_is_given_the_records_in_view() {
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .reply_to(
            ModelPurpose::Answer,
            ScriptedReply::answer("You can rebook it."),
        )
        .acknowledging("You can rebook it.")
        .build_shared();
    let understanding = UnderstandingBuilder::of(QUESTION)
        .ask(QUESTION)
        .about(QuestionTopic::Capabilities)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), QUESTION))
        .await
        .unwrap();

    let brief = provider
        .calls_for(ModelPurpose::Answer)
        .first()
        .expect("the question is answered")
        .user_text();
    assert!(
        brief.contains(SAMPLE_NAME),
        "what the record holds: {brief}"
    );
}
