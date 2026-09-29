//! The conversation as a turn loads it: earlier messages, and what cannot be started.

use serde::{Deserialize, Serialize};
use turnframe_core::ids::WorkflowKey;

/// Who wrote a message of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    /// The user.
    User,
    /// The assistant.
    Assistant,
}

/// One earlier message of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentMessage {
    /// Who wrote it.
    pub role: TranscriptRole,
    /// What it said.
    pub text: String,
}

impl RecentMessage {
    /// A message the user wrote.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::User,
            text: text.into(),
        }
    }

    /// A message the assistant wrote.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::Assistant,
            text: text.into(),
        }
    }
}

/// A workflow this turn cannot start, and the reason it declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableWorkflow {
    /// The workflow.
    pub workflow: WorkflowKey,
    /// Why, resolved to the turn's locale, in the workflow's own words.
    pub reason: String,
}
