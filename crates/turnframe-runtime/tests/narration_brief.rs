//! What the narration stage is told, and what it can therefore say.
//!
//! Narration may state only what the server has already established, and it is
//! bounded by that rule, not starved by it. The acknowledgement is shown each
//! receipt's own words and the one obligation code chose to ask for; an answer is
//! shown what its record holds and still needs.
//!
//! The obligations are the ones left AFTER the turn. A collection flow records a
//! value and asks for the next one; narrating from the projection that framed
//! the turn would ask again for the value just given.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::NarratableFact;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// What the first call of `purpose` was written from, as the model read it.
fn written_from(provider: &Arc<ScriptedProvider>, purpose: ModelPurpose) -> String {
    provider
        .calls_for(purpose)
        .into_iter()
        .next()
        .expect("the stage ran")
        .user_text()
}

/// A turn that sets the name on a draft with several fields still open.
async fn a_recording_turn() -> Arc<ScriptedProvider> {
    let turn = turn_one();
    let text = "Set the name to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();
    provider
}

#[tokio::test]
async fn the_narrator_is_shown_what_the_receipt_says() {
    let provider = a_recording_turn().await;
    let shown = written_from(&provider, ModelPurpose::Acknowledge);
    // A status code alone cannot be described; the receipt's own words can.
    assert!(
        shown.contains("Trip named: The trip is now called \\\"Lisbon\\\"."),
        "a narrator asked to describe a receipt has to be able to read it: {shown}"
    );
}

#[tokio::test]
async fn the_narrator_is_told_what_the_case_still_needs() {
    let provider = a_recording_turn().await;
    let shown = written_from(&provider, ModelPurpose::Acknowledge);
    assert!(
        shown.contains("\"ask\": {\n    \"record\": \"Trip 1\""),
        "an incomplete draft has obligations, and asking for one is the whole \
         behaviour of a collection flow: {shown}"
    );
}

#[tokio::test]
async fn the_obligations_are_the_ones_left_after_the_turn() {
    // The projection that framed the turn still lists the name as
    // outstanding; narrating from it would ask for the name just set.
    let provider = a_recording_turn().await;
    let shown = written_from(&provider, ModelPurpose::Acknowledge);
    assert!(
        !shown.contains("What is the trip's name?"),
        "the value the user just supplied is not something to ask for again: {shown}"
    );
    assert!(
        shown.contains("\"ask\""),
        "and the fields still missing are still asked for: {shown}"
    );
}

#[test]
fn the_fact_vocabulary_carries_the_case_it_speaks_about() {
    // A fact naming no case cannot be checked against a projection, so the
    // variant carries the reference it was read at rather than a bare value.
    let fact = NarratableFact::ObligationOpen {
        case_ref: turnframe_core::case::CaseRef::new(
            "trip",
            "trip-1",
            turnframe_core::ids::CaseRevision(4),
        ),
        obligation: serde_json::json!({"kind": "needs_traveler"}),
        relevance: turnframe_core::response::FactRelevance::ThisTurn,
        sentence: None,
    };
    let rendered = serde_json::to_value(&fact).expect("the fact serializes");
    assert_eq!(rendered["kind"], "obligation_open");
    assert_eq!(rendered["case_ref"]["expected_revision"], 4);
}

/// A question the user asked beside an act is answered, whatever it names: a
/// field still open, a field already set, or nothing at all.
mod a_question_beside_an_act {
    use super::*;
    use turnframe_core::plan::AnswerBasis;
    use turnframe_test::providers::ScriptedReply;
    use turnframe_test::workflows::trip::complete_case;

    /// The words that carry the act.
    const SETTING: &str = "Set the name to Lisbon";

    /// The words the question is written in.
    const ASKING: &str = "and what about the rest";

    /// The whole message: the instruction, then the question.
    const MESSAGE: &str = "Set the name to Lisbon, and what about the rest";

    /// Runs a turn that sets the name and asks about `references`, and
    /// reports how many answer blocks came back.
    async fn answers_for(
        references: &[&str],
        state: turnframe_test::workflows::trip::TripState,
    ) -> usize {
        let turn = turn_one();
        let understanding = UnderstandingBuilder::of(MESSAGE)
            .apply(
                operations::SET_NAME,
                token_for(turn, "trip", "trip-1"),
                serde_json::json!({"value": "Lisbon"}),
                SETTING,
            )
            .ask_about(AnswerBasis::CurrentCommittedState, None, references, ASKING)
            .build()
            .unwrap();
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .reply_to(
                ModelPurpose::Acknowledge,
                ScriptedReply::Text(String::from("Noted.")),
            )
            .reply_to(
                ModelPurpose::Answer,
                ScriptedReply::Text(String::from("Answered.")),
            )
            .build_shared();
        let harness = Harness::builder()
            .trip("trip-1", "Trip 1", 3, state)
            .understands(understanding)
            .provider(Arc::clone(&provider))
            .build()
            .await;
        let answered = harness.handle(harness.turn(turn, MESSAGE)).await.unwrap();
        answered
            .blocks
            .iter()
            .filter(|block| matches!(block, turnframe_core::response::ResponseBlock::Answer(_)))
            .count()
    }

    #[tokio::test]
    async fn a_question_about_something_else_is_kept() {
        let answers = answers_for(&["travel_date", "total_amount"], incomplete_case()).await;
        assert_eq!(
            answers, 1,
            "a question naming something outstanding and something else is still a question"
        );
    }

    #[tokio::test]
    async fn a_question_about_a_settled_field_is_kept() {
        let answers = answers_for(&["travel_date"], complete_case()).await;
        assert_eq!(answers, 1, "asking about a field that is set is asking");
    }

    #[tokio::test]
    async fn a_question_with_no_references_is_kept() {
        let answers = answers_for(&[], incomplete_case()).await;
        assert_eq!(answers, 1);
    }
}

/// The transcript depth is the deployment's, with no shipped number.
#[test]
fn how_much_conversation_a_turn_loads_is_configurable_and_unset() {
    let config = turnframe_runtime::config::UnderstandingConfig::conservative();
    assert_eq!(
        config.transcript_turns, None,
        "nothing is bounded until a deployment bounds it"
    );
    assert_eq!(
        config.with_transcript_turns(Some(40)).transcript_turns,
        Some(40),
        "and a deployment that wants forty turns can have forty"
    );
}

/// What a case HOLDS reaches the stage that answers, not only what it needs.
///
/// A workflow declares which of its values may be stated, and each becomes a
/// [`NarratableFact::StateValue`]: open obligations alone cannot answer «what did
/// you set as the name?».
#[tokio::test]
async fn the_answering_stage_is_told_what_the_case_holds() {
    let turn = turn_one();
    let text = "Set the name to Lisbon. What is the name now?";
    let trip = token_for(turn, "trip", "trip-1");
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            trip.clone(),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon.",
        )
        .ask_about(
            turnframe_core::plan::AnswerBasis::CurrentCommittedState,
            Some(trip),
            &["name"],
            "What is the name now?",
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Lisbon.")
        .acknowledging("Noted. Lisbon.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let shown = written_from(&provider, ModelPurpose::Answer);
    assert!(
        shown.contains("\"kind\": \"state_value\"") && shown.contains("\"Lisbon\""),
        "the value the turn recorded is among the answer's facts: {shown}"
    );
    assert!(
        shown.contains("\"kind\": \"obligation_open\""),
        "and what it holds does not replace what it needs: {shown}"
    );
    assert!(
        shown.contains("\"case_id\": \"trip-1\""),
        "every fact names the case it speaks about: {shown}"
    );
}
