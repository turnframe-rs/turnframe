//! Normalized request purposes (spec §20.2) and what each one demands.
//!
//! A [`ModelPurpose`] names *why* the runtime calls a model. It carries two
//! policies: the [`CapabilityRequirements`] a provider must satisfy (spec §20.4,
//! computed by [`ModelPurpose::requirements`]) and the [`LoggingPolicy`] that
//! says how much of the exchange may be logged.
//!
//! The critical rule is spec §0 rule 9 / §20.4: a task that understands a
//! message, and so can lead to an effect, needs native JSON Schema output, a
//! native function schema used purely as transport, or grammar-constrained
//! decoding. `PromptOnly`, `JsonObject` and `None` are rejected unless the
//! application opts into [`SafetyMode::UnsafeExperimental`]. The default never
//! downgrades.

use serde::{Deserialize, Serialize};

use crate::capabilities::{CapabilityRequirements, StructuredOutputCapability};

/// Why the runtime is calling a model (spec §20.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelPurpose {
    /// Offline evaluation of a corpus (spec §27.6); never on the request path.
    OfflineEvaluate,
    /// Split a message into units: requests, questions, constraints and the rest.
    Segment,
    /// Find a request or question the units do not cover.
    Coverage,
    /// Choose the operation a unit asks for.
    Route,
    /// Choose the record an act aims at.
    Locate,
    /// Fill an act's arguments.
    Extract,
    /// Check an act against the words it came from.
    Verify,
    /// Choose the record and subjects a question is about.
    QuestionFrame,
    /// Check the whole understanding of a message against the message.
    CrossCheck,
    /// Judge whether an act changes what a keep-unchanged constraint keeps.
    Respects,
    /// Ask for read-only context before a unit is understood.
    Investigate,
    /// Write what a turn did and what it needs next.
    Acknowledge,
    /// Answer one question.
    Answer,
    /// Check a written block against its facts.
    Review,
    /// Say, as it happens, what the assistant is doing to understand a message.
    Progress,
}

/// Whether the application accepts structured-output transports that are not
/// safe enough for understanding a message (spec §20.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMode {
    /// The default: only native JSON Schema, native function schema (as
    /// transport) or grammar-constrained output may understand a message.
    #[default]
    Default,
    /// Explicit opt-in that also accepts `JsonObject`, `PromptOnly` and `None`
    /// for understanding. Unsafe: the only remaining guard is
    /// all-or-nothing schema validation after the fact.
    UnsafeExperimental,
}

/// How much of a model exchange may be written to logs and traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogDetail {
    /// Identifiers, sizes, purpose and timing only.
    Metadata,
    /// Content after [`crate::secret::Redactor`] processing.
    Redacted,
    /// Full content. Only acceptable for offline corpora without personal data.
    Full,
}

/// Logging and redaction policy attached to a purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LoggingPolicy {
    /// What may be logged about the prompt (system, messages, tools).
    pub prompt: LogDetail,
    /// What may be logged about the model output.
    pub output: LogDetail,
    /// Whether raw provider bodies may be retained anywhere. Always `false` for
    /// request-path purposes (spec §25.2).
    pub retain_raw_bodies: bool,
}

impl LoggingPolicy {
    /// Metadata only for both directions, no raw bodies.
    pub const METADATA_ONLY: Self = Self {
        prompt: LogDetail::Metadata,
        output: LogDetail::Metadata,
        retain_raw_bodies: false,
    };

    /// Metadata for the prompt, redacted output, no raw bodies.
    pub const REDACTED_OUTPUT: Self = Self {
        prompt: LogDetail::Metadata,
        output: LogDetail::Redacted,
        retain_raw_bodies: false,
    };

    /// Full logging, still without raw provider bodies.
    pub const FULL: Self = Self {
        prompt: LogDetail::Full,
        output: LogDetail::Full,
        retain_raw_bodies: false,
    };

    /// Returns `true` when any content (beyond metadata) may be logged.
    #[must_use]
    pub const fn logs_content(&self) -> bool {
        !matches!(self.prompt, LogDetail::Metadata) || !matches!(self.output, LogDetail::Metadata)
    }
}

/// The structured-output transports safe for understanding a message (spec §20.4).
pub const MUTATION_SAFE_STRUCTURED_OUTPUT: [StructuredOutputCapability; 3] = [
    StructuredOutputCapability::NativeJsonSchema,
    StructuredOutputCapability::NativeFunctionSchema,
    StructuredOutputCapability::GrammarConstrained,
];

/// Transports acceptable when the parsed output can only trigger reads.
pub const READ_ONLY_STRUCTURED_OUTPUT: [StructuredOutputCapability; 4] = [
    StructuredOutputCapability::NativeJsonSchema,
    StructuredOutputCapability::NativeFunctionSchema,
    StructuredOutputCapability::GrammarConstrained,
    StructuredOutputCapability::JsonObject,
];

impl ModelPurpose {
    /// Every purpose, for exhaustive registration and tests.
    pub const ALL: [Self; 15] = [
        Self::OfflineEvaluate,
        Self::Segment,
        Self::Coverage,
        Self::Route,
        Self::Locate,
        Self::Extract,
        Self::Verify,
        Self::QuestionFrame,
        Self::CrossCheck,
        Self::Respects,
        Self::Investigate,
        Self::Acknowledge,
        Self::Answer,
        Self::Review,
        Self::Progress,
    ];

    /// The tasks that understand a message, before any effect.
    #[must_use]
    pub const fn is_understanding(self) -> bool {
        matches!(
            self,
            Self::Segment
                | Self::Coverage
                | Self::Route
                | Self::Locate
                | Self::Extract
                | Self::Verify
                | Self::QuestionFrame
                | Self::CrossCheck
                | Self::Respects
                | Self::Investigate
        )
    }

    /// Stable snake-case label, used in replay records and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfflineEvaluate => "offline_evaluate",
            Self::Segment => "segment",
            Self::Coverage => "coverage",
            Self::Route => "route",
            Self::Locate => "locate",
            Self::Extract => "extract",
            Self::Verify => "verify",
            Self::QuestionFrame => "question_frame",
            Self::CrossCheck => "cross_check",
            Self::Respects => "respects",
            Self::Investigate => "investigate",
            Self::Acknowledge => "acknowledge",
            Self::Answer => "answer",
            Self::Review => "review",
            Self::Progress => "progress",
        }
    }

    /// Returns `true` for the purposes whose output can lead to commands or
    /// reads being executed (spec §20.4 "critical stage").
    #[must_use]
    pub const fn is_critical(self) -> bool {
        self.is_understanding()
    }

    /// Capability requirements under [`SafetyMode::Default`] (spec §20.4).
    #[must_use]
    pub fn requirements(self) -> CapabilityRequirements {
        self.requirements_in(SafetyMode::Default)
    }

    /// Capability requirements under an explicit safety mode.
    ///
    /// * The understanding tasks: one of [`MUTATION_SAFE_STRUCTURED_OUTPUT`],
    ///   except `Investigate`, whose output can only trigger reads.
    /// * `Investigate` and the narration tasks: one of
    ///   [`READ_ONLY_STRUCTURED_OUTPUT`]; their documents only become text.
    /// * `OfflineEvaluate`: none; evaluation deliberately measures every transport.
    /// * [`SafetyMode::UnsafeExperimental`] removes the structured-output
    ///   requirement for every purpose, and nothing else.
    #[must_use]
    pub fn requirements_in(self, mode: SafetyMode) -> CapabilityRequirements {
        let structured_output: Vec<StructuredOutputCapability> = match (self, mode) {
            (_, SafetyMode::UnsafeExperimental) | (Self::OfflineEvaluate, _) => Vec::new(),
            (
                Self::Investigate
                | Self::Acknowledge
                | Self::Answer
                | Self::Review
                | Self::Progress,
                SafetyMode::Default,
            ) => READ_ONLY_STRUCTURED_OUTPUT.to_vec(),
            (_, SafetyMode::Default) => MUTATION_SAFE_STRUCTURED_OUTPUT.to_vec(),
        };
        CapabilityRequirements {
            structured_output,
            needs_tools: false,
            needs_streaming: false,
            min_context_tokens: None,
            needs_vision: false,
            needs_documents: false,
        }
    }

    /// Logging policy of the purpose.
    ///
    /// Request-path purposes never log prompts beyond metadata because prompts
    /// carry user text and case state; outputs are logged redacted where they
    /// are useful for debugging the understanding. `OfflineEvaluate` may log in
    /// full because evaluation corpora are curated. No purpose retains raw
    /// provider bodies (spec §25.2).
    #[must_use]
    pub const fn logging_policy(self) -> LoggingPolicy {
        match self {
            Self::Segment
            | Self::Coverage
            | Self::Route
            | Self::Locate
            | Self::Extract
            | Self::Verify
            | Self::QuestionFrame
            | Self::CrossCheck
            | Self::Respects
            | Self::Investigate => LoggingPolicy::REDACTED_OUTPUT,
            Self::Acknowledge | Self::Answer | Self::Review | Self::Progress => {
                LoggingPolicy::METADATA_ONLY
            }
            Self::OfflineEvaluate => LoggingPolicy::FULL,
        }
    }
}

impl std::fmt::Display for ModelPurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::ProviderCapabilities;

    fn caps(structured: StructuredOutputCapability) -> ProviderCapabilities {
        ProviderCapabilities::minimal().with_structured_output(structured)
    }

    #[test]
    fn understanding_rejects_prompt_only_json_object_and_none() {
        let requirements = ModelPurpose::Extract.requirements();
        for unsafe_transport in [
            StructuredOutputCapability::PromptOnly,
            StructuredOutputCapability::JsonObject,
            StructuredOutputCapability::None,
        ] {
            assert!(requirements.satisfied_by(&caps(unsafe_transport)).is_err());
        }
        for safe in MUTATION_SAFE_STRUCTURED_OUTPUT {
            assert!(requirements.satisfied_by(&caps(safe)).is_ok());
        }
    }

    #[test]
    fn unsafe_experimental_is_an_explicit_opt_in() {
        let requirements = ModelPurpose::Extract.requirements_in(SafetyMode::UnsafeExperimental);
        assert!(requirements.structured_output.is_empty());
        assert!(
            requirements
                .satisfied_by(&caps(StructuredOutputCapability::PromptOnly))
                .is_ok()
        );
    }

    #[test]
    fn a_read_only_task_accepts_json_object_but_not_prompt_only() {
        for purpose in [ModelPurpose::Investigate, ModelPurpose::Acknowledge] {
            let requirements = purpose.requirements();
            assert_eq!(
                requirements.structured_output,
                READ_ONLY_STRUCTURED_OUTPUT.to_vec()
            );
            assert!(
                requirements
                    .satisfied_by(&caps(StructuredOutputCapability::PromptOnly))
                    .is_err()
            );
        }
    }

    #[test]
    fn narration_logs_metadata_only_and_is_not_critical() {
        assert!(!ModelPurpose::Acknowledge.logging_policy().logs_content());
        assert!(!ModelPurpose::Acknowledge.is_critical());
        assert!(ModelPurpose::Extract.is_critical());
        assert!(
            ModelPurpose::OfflineEvaluate
                .requirements()
                .structured_output
                .is_empty()
        );
    }

    #[test]
    fn labels_are_unique_and_never_retain_raw_bodies() {
        let mut labels: Vec<&str> = ModelPurpose::ALL.iter().map(|p| p.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), ModelPurpose::ALL.len());
        for purpose in ModelPurpose::ALL {
            assert!(!purpose.logging_policy().retain_raw_bodies);
            let json = serde_json::to_string(&purpose).unwrap();
            assert_eq!(json, format!("\"{}\"", purpose.as_str()));
        }
    }
}
