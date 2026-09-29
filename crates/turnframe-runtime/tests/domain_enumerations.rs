//! "These are the values this field accepts" is a claim, and nothing in a
//! sentence tells a real value from an invented one.
//!
//! So the sentence is not the mechanism. A workflow declares the complete set,
//! the runtime answers a question about it from the declaration with no model
//! call, and the values reach the user as data carrying the workflow's own
//! labels, leaving nowhere for a fourth value to appear. Only a question about
//! which values a field takes is answered this way.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::{AnswerStatus, ResponseBlock};
use turnframe_core::understanding::QuestionTopic;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{operations, unassigned_case};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// Runs a turn asking `quote` about `references` with `basis` and `topic`, and
/// reports the answer block and how many times a model was asked to write prose.
async fn ask(
    basis: AnswerBasis,
    topic: QuestionTopic,
    references: &[&str],
    quote: &str,
) -> (turnframe_core::response::GeneratedAnswer, usize) {
    let turn = turn_one();
    let text = format!("Set the name to Lisbon. {quote}");
    let understood = UnderstandingBuilder::of(&text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .ask_about(basis, None, references, quote)
        .about(topic)
        .build()
        .unwrap();
    // A question about the record's own state goes to a model, before the reply that
    // gives its answer; one about the declared set is answered with no model at all.
    let mut script = ScriptedProvider::builder("scripted", "model-1");
    if basis == AnswerBasis::CurrentCommittedState {
        script = script.answering("The traveler pays for the first extra.");
    }
    let provider = script
        .acknowledging("Noted.")
        // Only reached if the runtime asks a model to describe the set, which
        // is the thing this whole mechanism exists not to do.
        .answering("The traveler, the company and the tour operator.")
        .build_shared();
    let harness = Harness::builder()
        // The extra has no payer yet, so `extras.payer` is an open
        // obligation and the drop rule would otherwise fire on this question.
        .trip("trip-1", "Trip 1", 3, unassigned_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, &text)).await.unwrap();
    let answer = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer.clone()),
            _ => None,
        })
        .expect("the question gets a block");
    let asked = provider.calls_for(ModelPurpose::Answer).len();
    (answer, asked)
}

/// The reported shape: a question about which values a field takes.
#[tokio::test]
async fn the_values_come_from_the_workflow_and_not_from_a_model() {
    let (answer, asked) = ask(
        AnswerBasis::GeneralDomainKnowledge,
        QuestionTopic::AcceptedValues,
        &["extras.payer"],
        "which payers can I use?",
    )
    .await;

    assert_eq!(asked, 0, "no model is asked to describe a set we hold");
    assert_eq!(answer.status, AnswerStatus::Answered);
    let declared = answer
        .enumerations
        .iter()
        .find(|enumeration| enumeration.subject == "extras.payer")
        .expect("the set the question was about");
    let labels: Vec<&str> = declared
        .values
        .iter()
        .map(|value| value.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["The traveler", "The company", "The airline",],
        "every value the workflow accepts, and only those"
    );
    assert!(
        !answer.text.contains("flat-rate"),
        "and nothing a model made up: {}",
        answer.text
    );
}

/// The half that made the earlier fix impossible: this question used to be
/// dropped, because everything it references is an open obligation.
#[tokio::test]
async fn a_question_about_the_field_being_collected_survives() {
    let (answer, _) = ask(
        AnswerBasis::GeneralDomainKnowledge,
        QuestionTopic::AcceptedValues,
        &["extras.payer"],
        "which payers can I use?",
    )
    .await;
    assert!(
        !answer.enumerations.is_empty(),
        "a question the deterministic layer can answer is not the assistant \
         telling itself what to do next"
    );
}

/// And the question that is *not* about the set still reaches a model.
///
/// "Who can pay" and "who pays for this extra" name
/// the same field and are different questions. Understanding tells them apart
/// by their basis, so the declaration answers one and not the other.
#[tokio::test]
async fn a_question_about_this_record_still_goes_to_a_model() {
    let (answer, asked) = ask(
        AnswerBasis::CurrentCommittedState,
        QuestionTopic::RecordState,
        &["extras.payer"],
        "who pays for my first extra?",
    )
    .await;
    assert_eq!(asked, 1, "the record's own value is not in the declaration");
    assert!(answer.enumerations.is_empty());
}

/// A surface that shows only text, a chat channel or a voice, still hears every value:
/// the answer's words name them in the workflow's own labels.
#[tokio::test]
async fn the_answer_names_every_value_in_its_own_words() {
    let (answer, _) = ask(
        AnswerBasis::GeneralDomainKnowledge,
        QuestionTopic::AcceptedValues,
        &["extras.payer"],
        "Which payers are there?",
    )
    .await;
    for label in ["The traveler", "The company", "The airline"] {
        assert!(answer.text.contains(label), "{}", answer.text);
    }
}
