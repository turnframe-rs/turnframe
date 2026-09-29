//! Asked which values a record's field may take, the reply gives them and asks for the one
//! the record still needs, so a bare choice in the next message answers it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::Expectation;
use turnframe_core::understanding::QuestionTopic;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::unassigned_case;

#[tokio::test]
async fn a_question_about_the_values_a_record_takes_asks_for_one() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let asked = "who can pay for the extra?";
    let understood = UnderstandingBuilder::of(asked)
        .ask_about(
            AnswerBasis::GeneralDomainKnowledge,
            Some(token_for(turn, "trip", "trip-1")),
            &["extras.payer"],
            asked,
        )
        .about(QuestionTopic::AcceptedValues)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("The traveler, the company or the airline can pay for it. Who pays?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, unassigned_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let reply = harness.handle(harness.turn(turn, asked)).await.unwrap();

    assert!(
        reply
            .expectations
            .iter()
            .any(|expectation| match expectation {
                Expectation::AwaitingObligation { case_ref, .. }
                | Expectation::AwaitingOperation { case_ref, .. } => {
                    case_ref.case_id.as_str() == "trip-1"
                }
                _ => false,
            }),
        "the reply asks what the record still needs: {:?}",
        reply.expectations
    );
}
