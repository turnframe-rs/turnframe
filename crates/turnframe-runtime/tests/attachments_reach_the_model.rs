//! The turn's files, on their way to a model.
//!
//! The stage that answers a question is shown the files the turn carried, as
//! parts of the message the user sent them with, so a question about a document
//! is answered from the document. Where the bytes live is the application's
//! business, behind an [`AttachmentSource`]. What the library owes in return is
//! that a file it could not show is said out loud, as a notice and as a fact:
//! an answer about the wrong document is worse than an answer about none.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use support::{Harness, token_for};
use turnframe_core::ids::{AttachmentId, TurnId};
use turnframe_core::response::{NarratableFact, ResponseBlock};
use turnframe_core::turn::{
    AttachmentContent, AttachmentError, AttachmentRef, AttachmentSource, TurnInput,
};
use turnframe_core::understanding::Understanding;
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::{ContentPart, ModelRequest};
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

const SET: &str = "Set the name to Lisbon";
const ASK: &str = "what does this show?";
const TEXT: &str = "Set the name to Lisbon, and what does this show?";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// A file the turn carries.
fn file(id: &str, media_type: &str) -> AttachmentRef {
    AttachmentRef {
        attachment_id: AttachmentId::from(id),
        media_type: media_type.to_owned(),
        filename: Some(format!("{id}.bin")),
        size_bytes: None,
        digest: None,
    }
}

/// A source that hands over fixed bytes.
#[derive(Debug)]
struct Bytes {
    size: usize,
    media_type: String,
}

impl Bytes {
    fn new(size: usize, media_type: &str) -> Self {
        Self {
            size,
            media_type: media_type.to_owned(),
        }
    }
}

#[async_trait]
impl AttachmentSource for Bytes {
    async fn fetch(
        &self,
        _turn_id: &TurnId,
        _attachment: &AttachmentRef,
    ) -> Result<AttachmentContent, AttachmentError> {
        Ok(AttachmentContent {
            media_type: self.media_type.clone(),
            bytes: vec![7u8; self.size],
        })
    }
}

/// A source that has nothing, which is ordinary rather than exceptional.
#[derive(Debug)]
struct Gone;

#[async_trait]
impl AttachmentSource for Gone {
    async fn fetch(
        &self,
        _turn_id: &TurnId,
        attachment: &AttachmentRef,
    ) -> Result<AttachmentContent, AttachmentError> {
        Err(AttachmentError::Gone {
            attachment_id: attachment.attachment_id.clone(),
        })
    }
}

/// The parts of the user message of one recorded call.
fn user_parts(request: &ModelRequest) -> Vec<&ContentPart> {
    request
        .messages
        .iter()
        .filter(|message| message.role == turnframe_provider::request::Role::User)
        .flat_map(|message| message.content.iter())
        .collect()
}

/// How many image parts a call carried.
fn images(request: &ModelRequest) -> usize {
    user_parts(request)
        .iter()
        .filter(|part| matches!(part, ContentPart::Image { .. }))
        .count()
}

/// How many image parts the stage answering the question was shown.
fn images_answered_from(provider: &ScriptedProvider) -> usize {
    assert_eq!(provider.violations(), Vec::new(), "every call was scripted");
    let calls = provider.calls_for(ModelPurpose::Answer);
    assert_eq!(
        calls.len(),
        1,
        "the question was put to the answering stage"
    );
    images(&calls[0].request)
}

/// Sets the name of trip 1 and asks about the file.
fn setting_and_asking(turn: TurnId) -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            SET,
        )
        .ask(ASK)
        .build()
        .unwrap()
}

/// A provider for both composition stages, in the order the composer calls them.
fn composing(capabilities: Option<ProviderCapabilities>) -> Arc<ScriptedProvider> {
    let mut builder = ScriptedProvider::builder("scripted", "model-1");
    if let Some(capabilities) = capabilities {
        builder = builder.capabilities(capabilities);
    }
    builder
        .answering("It is a quotation for four hundred euro.")
        .acknowledging("Right.")
        .build_shared()
}

/// Declared, because the router refuses a request nothing can serve and the
/// runtime asks it first: a deployment whose models have no vision is not sent
/// the photograph.
fn seeing() -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_preserves_call_ids(true)
        .with_vision(true)
        .with_documents(true)
}

/// A turn carrying `files`, with `source` behind it.
async fn run(
    files: Vec<AttachmentRef>,
    source: Option<Arc<dyn AttachmentSource>>,
    config: turnframe_runtime::config::AttachmentConfig,
) -> (
    Harness,
    Arc<ScriptedProvider>,
    turnframe_core::response::AssistantTurn,
) {
    let id = turn_one();
    let provider = composing(Some(seeing()));
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(setting_and_asking(id))
        .config(
            turnframe_runtime::config::OrchestratorConfig::conservative().with_attachments(config),
        )
        .provider(Arc::clone(&provider));
    if let Some(source) = source {
        builder = builder.attachments(source);
    }
    let harness = builder.build().await;
    let input = TurnInput {
        attachments: files,
        ..harness.turn(id, TEXT)
    };
    let answered = harness.handle(input).await.unwrap();
    (harness, provider, answered)
}

/// With no source registered, nothing is fetched and nothing is claimed: saying
/// "I could not look at your file" on every such turn would be noise about a
/// feature the deployment did not take up.
#[tokio::test]
async fn without_a_source_the_turn_is_what_it_always_was() {
    let (_, provider, answered) = run(
        vec![file("a1", "image/png")],
        None,
        turnframe_runtime::config::AttachmentConfig::conservative(),
    )
    .await;
    assert_eq!(images_answered_from(&provider), 0);
    assert!(
        !answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(notice)
                if notice.code == turnframe_runtime::reduce::notice::ATTACHMENT_NOT_SHOWN)),
        "nothing was lost, so nothing is announced"
    );
}

/// A file the application no longer holds is said out loud, on both channels.
#[tokio::test]
async fn a_file_that_cannot_be_fetched_is_reported_and_the_turn_stands() {
    let (_, provider, answered) = run(
        vec![file("a1", "image/png")],
        Some(Arc::new(Gone) as Arc<dyn AttachmentSource>),
        turnframe_runtime::config::AttachmentConfig::conservative(),
    )
    .await;
    assert_eq!(images_answered_from(&provider), 0);
    assert!(
        answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(notice)
                if notice.code == turnframe_runtime::reduce::notice::ATTACHMENT_NOT_SHOWN)),
        "the user is told deterministically, whether or not a model ran"
    );
    assert!(
        !answered.blocks.is_empty(),
        "and the turn still answered: a file it could not open is not a failure"
    );
}

/// A budget takes the files in the order the user attached them, and says which
/// ones it left.
#[tokio::test]
async fn a_budget_keeps_the_first_files_and_names_the_rest() {
    let config =
        turnframe_runtime::config::AttachmentConfig::conservative().with_max_files(Some(1));
    let (_, provider, answered) = run(
        vec![file("a1", "image/png"), file("a2", "image/png")],
        Some(Arc::new(Bytes::new(8, "image/png")) as Arc<dyn AttachmentSource>),
        config,
    )
    .await;
    assert_eq!(
        images_answered_from(&provider),
        1,
        "the first file fits and the second does not"
    );
    let named: Vec<&AttachmentId> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Transition(block) => Some(&block.facts_used),
            _ => None,
        })
        .flatten()
        .filter_map(|fact| match fact {
            NarratableFact::AttachmentNotShown { attachment_id, .. } => Some(attachment_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        vec![&AttachmentId::from("a2")],
        "and the writing stage is told which one, so its prose cannot describe it"
    );
}

/// A file too large for the whole budget does not take the smaller ones with it.
#[tokio::test]
async fn one_oversized_file_does_not_cost_the_others() {
    let config =
        turnframe_runtime::config::AttachmentConfig::conservative().with_max_total_bytes(Some(10));
    // Both files are the same size here, so what is under test is the order and
    // not the sizes: the first fits the budget, the second does not, and a third
    // that would fit is still taken.
    let (_, provider, _) = run(
        vec![
            file("a1", "image/png"),
            file("a2", "image/png"),
            file("a3", "image/png"),
        ],
        Some(Arc::new(Bytes::new(6, "image/png")) as Arc<dyn AttachmentSource>),
        config,
    )
    .await;
    assert_eq!(
        images_answered_from(&provider),
        1,
        "six bytes fit, twelve do not, and the budget is spent in the user's order"
    );
}

/// A file no model this deployment runs can accept costs a file, not the turn.
///
/// A part carrying an image makes the request require vision, and a router with
/// nothing to serve it fails the call; so the runtime asks first, and a
/// photograph sent to a deployment without vision is a notice, not a failure.
#[tokio::test]
async fn a_file_no_model_accepts_does_not_cost_the_turn() {
    let id = turn_one();
    // The default scripted profile: structured output and tools, and no vision.
    let provider = composing(None);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(setting_and_asking(id))
        .provider(Arc::clone(&provider))
        .attachments(Arc::new(Bytes::new(8, "image/png")) as Arc<dyn AttachmentSource>)
        .build()
        .await;
    let answered = harness
        .handle(TurnInput {
            attachments: vec![file("a1", "image/png")],
            ..harness.turn(id, TEXT)
        })
        .await
        .expect("a file nobody can read is not a reason to lose the turn");

    assert_eq!(
        images_answered_from(&provider),
        0,
        "the request nothing could serve was never sent"
    );
    assert!(
        answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(notice)
                if notice.code == turnframe_runtime::reduce::notice::ATTACHMENT_NOT_SHOWN)),
        "and the user is told, rather than left with prose about a file nobody saw"
    );
    assert!(
        !harness.events("trip", "trip-1").await.is_empty(),
        "while what the message actually asked for still happened"
    );
}

/// The stage that answers is given the file, and the one that thanks and asks is
/// not: it answers nothing about the file, and is given less to invent from.
#[tokio::test]
async fn the_stage_that_answers_is_given_the_file() {
    let id = turn_one();
    let question = "What does this document say?";
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .capabilities(seeing())
        .answering(
            serde_json::json!({"answers": [{
                "question_id": "questions[0]",
                "text": "It is a quotation for four hundred euro.",
                "answered": true
            }]})
            .to_string(),
        )
        .acknowledging("What should the name be?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(
            UnderstandingBuilder::of(question)
                .ask(question)
                .build()
                .unwrap(),
        )
        .provider(Arc::clone(&provider))
        .attachments(Arc::new(Bytes::new(16, "application/pdf")) as Arc<dyn AttachmentSource>)
        .build()
        .await;
    harness
        .handle(TurnInput {
            attachments: vec![file("a1", "application/pdf")],
            ..harness.turn(id, question)
        })
        .await
        .unwrap();

    assert_eq!(provider.violations(), Vec::new(), "every call was scripted");
    let answering = provider.calls_for(ModelPurpose::Answer);
    assert_eq!(answering.len(), 1, "the question was answered");
    assert!(
        user_parts(&answering[0].request)
            .iter()
            .any(|part| matches!(part, ContentPart::Document { .. })),
        "and the stage answering it was looking at the document"
    );
    for call in provider.calls_for(ModelPurpose::Acknowledge) {
        assert!(
            user_parts(&call.request)
                .iter()
                .all(|part| matches!(part, ContentPart::Text { .. })),
            "while the stage that thanks and asks has nothing to invent from"
        );
    }
}
