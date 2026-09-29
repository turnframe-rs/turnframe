//! Persistent interactions: cards, confirmations, selections (spec §15).
//!
//! An interaction is a durable, server-owned record. The client only ever
//! sends back an interaction id, an option id and the case revision it saw
//! (I7); the meaning of the click is the [`StoredInteractionAction`] persisted
//! with the option. [`validate_response`] is the pure gate every response
//! passes through before anything executes.

use std::time::Duration;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::case::CaseRef;
use crate::command::{CommandOrigin, ResolutionChannel, RiskClass};
use crate::error::{InteractionError, InteractionSpecError};
use crate::hash::{Digest, HashError, canonical_digest};
use crate::ids::{
    AccountId, CaseRevision, ConversationId, InteractionId, OperationKey, OptionId, TurnId,
};
use crate::locale::LocalizedText;
use crate::reduce::CommandRef;
use crate::turn::{ActorContext, InteractionResponse};

/// The shape of an interaction (spec §15.2).
///
/// New card shapes are expected, so downstream matches need a wildcard arm.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InteractionKind {
    /// Yes/no.
    Boolean,
    /// Pick one option.
    SingleSelect,
    /// Pick several options.
    MultiSelect,
    /// Free text.
    Freeform,
    /// Review a diff before applying.
    ReviewChanges,
    /// Confirm compiled commands.
    ConfirmCommand,
    /// Choose which case an act targets.
    SelectTarget,
    /// Resolve a validation error.
    ResolveValidationError,
    /// Re-authenticate.
    Reauthenticate,
    /// Sign externally.
    ExternalSignature,
}

impl InteractionKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 10] = [
        Self::Boolean,
        Self::SingleSelect,
        Self::MultiSelect,
        Self::Freeform,
        Self::ReviewChanges,
        Self::ConfirmCommand,
        Self::SelectTarget,
        Self::ResolveValidationError,
        Self::Reauthenticate,
        Self::ExternalSignature,
    ];

    /// Returns `true` for cards whose answer authorizes commands.
    ///
    /// Such a card is never resolvable from typed text: the authorization must
    /// be the user's own deterministic answer (spec §15.7, §13.2 rule 8).
    #[must_use]
    pub fn authorizes_commands(self) -> bool {
        matches!(
            self,
            Self::ConfirmCommand
                | Self::ReviewChanges
                | Self::Reauthenticate
                | Self::ExternalSignature
        )
    }
}

/// Lifecycle status of an interaction (spec §15.4).
///
/// Deliberately exhaustive: the set is a closed state machine with an
/// [`ALL`](Self::ALL) table and a
/// [`can_transition`](Self::can_transition) rule for every pair, so code that
/// handles statuses must be forced by the compiler to consider all of them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    /// Displayed and answerable.
    Active,
    /// An answer was accepted and its commands are executing.
    Resolving,
    /// The associated commands committed.
    Resolved,
    /// The user declined.
    Declined,
    /// The user dismissed it without answering.
    Dismissed,
    /// The case moved on; the card is no longer valid.
    Invalidated,
    /// The deadline passed.
    Expired,
    /// The associated commands failed.
    Failed,
}

impl InteractionStatus {
    /// Every status, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Active,
        Self::Resolving,
        Self::Resolved,
        Self::Declined,
        Self::Dismissed,
        Self::Invalidated,
        Self::Expired,
        Self::Failed,
    ];

    /// Returns `true` for statuses with no outgoing transition.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Active | Self::Resolving)
    }

    /// Returns `true` while the interaction still occupies the "one blocking
    /// interaction per case" slot (I5).
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Resolving)
    }

    /// The interaction state machine (spec §15.5).
    ///
    /// * `Active` may move to any other status.
    /// * `Resolving` may move to `Resolved`, `Failed`, or back to `Active` when
    ///   policy restores the card after a failed command.
    /// * Every other status is terminal.
    #[must_use]
    pub fn can_transition(from: Self, to: Self) -> bool {
        match from {
            Self::Active => to != Self::Active,
            Self::Resolving => matches!(to, Self::Resolved | Self::Failed | Self::Active),
            _ => false,
        }
    }
}

/// Whether an option accepts free text (spec §15.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FreeformPolicy {
    /// No free text accepted.
    #[default]
    Forbidden,
    /// Free text accepted up to `max_len` characters.
    Optional {
        /// Maximum length in characters.
        max_len: usize,
    },
    /// Free text required, up to `max_len` characters.
    Required {
        /// Maximum length in characters.
        max_len: usize,
    },
}

impl FreeformPolicy {
    /// Returns `true` when free text may be supplied.
    #[must_use]
    pub fn allows(self) -> bool {
        !matches!(self, Self::Forbidden)
    }

    /// Returns `true` when free text must be supplied.
    #[must_use]
    pub fn requires(self) -> bool {
        matches!(self, Self::Required { .. })
    }

    /// Maximum accepted length, when free text is allowed.
    #[must_use]
    pub fn max_len(self) -> Option<usize> {
        match self {
            Self::Forbidden => None,
            Self::Optional { max_len } | Self::Required { max_len } => Some(max_len),
        }
    }
}

/// Whether and how typed text may resolve an interaction (spec §15.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextResolutionPolicy {
    /// Only the structured CTA resolves it (default for high-risk confirmations).
    #[default]
    Never,
    /// Understanding may read typed text as one of its options, for a low-risk card;
    /// the option id is still checked against the stored options.
    ModelInterpretedLowRisk,
}

impl TextResolutionPolicy {
    /// Returns `true` when typed text may be read as an option.
    #[must_use]
    pub fn allows_model_interpretation(&self) -> bool {
        matches!(self, Self::ModelInterpretedLowRisk)
    }

    /// Returns `true` when the policy admits the channel an answer arrived on.
    ///
    /// [`ResolutionChannel::Click`] is always admitted: the structured CTA
    /// resolves every card.
    #[must_use]
    pub fn admits(&self, channel: ResolutionChannel) -> bool {
        match channel {
            ResolutionChannel::Click => true,
            ResolutionChannel::ModelInterpreted => self.allows_model_interpretation(),
        }
    }
}

/// What a stored option authorizes when it is chosen (spec §15.3).
///
/// This is the half of [`StoredInteractionAction`] that policy cares about: an
/// origin records the class, not the payload, so
/// [`origin_satisfies`](crate::command::origin_satisfies) can tell a
/// confirmation from a clarification without loading the card again.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Executes commands that were compiled and journaled with the card.
    ConfirmsCommands,
    /// Compiles and executes an operation named by the option.
    AppliesOperation,
    /// Answers the card without authorizing any command: a selection, a
    /// clarification, a decline, a dismissal, an application-defined action.
    NoCommands,
}

impl ActionClass {
    /// Returns `true` when choosing the option makes commands run.
    #[must_use]
    pub fn authorizes_commands(self) -> bool {
        matches!(self, Self::ConfirmsCommands | Self::AppliesOperation)
    }
}

/// The server-side meaning of an option (spec §15.3). Never supplied by the client.
///
/// Applications add meanings through [`Self::Custom`] and the library adds
/// variants, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoredInteractionAction {
    /// Execute commands that were compiled and journaled when the card was made.
    ConfirmCommands {
        /// The commands.
        command_refs: Vec<CommandRef>,
    },
    /// Drop the pending commands.
    DeclineCommands,
    /// Bind an ambiguous act to this case.
    SelectTarget {
        /// The chosen case.
        case_ref: CaseRef,
    },
    /// Answer a clarification with a stable key.
    ResolveClarification {
        /// Application-defined answer key.
        answer_key: String,
    },
    /// Compile and execute an operation when clicked (the option is the origin).
    ApplyOperation {
        /// Operation to compile through the workflow.
        operation: OperationKey,
        /// Arguments for the operation.
        arguments: serde_json::Value,
        /// Where the option's free text goes in those arguments, as a JSON
        /// pointer (RFC 6901), for a card that asks for a VALUE rather than a
        /// choice.
        ///
        /// `None` is every card that only offers options, which is all of them
        /// until one asks a question whose answer is a word.
        ///
        /// # Why a card at all, when the user could just type
        ///
        /// Because a question in prose is not a state: the next message arrives as
        /// free text, and what to do with it is a guess among every operation that
        /// accepts a name. Asked on a card, the question is the state: the answer
        /// arrives bound to the case and to the operation, and nothing is chosen.
        ///
        /// A JSON pointer into the operation's arguments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        freeform_argument: Option<String>,
    },
    /// Drop the pending commands, and record that the card was answered.
    ///
    /// # The trace a "no" could not leave
    ///
    /// A confirmation must carry an option that declines, and both declining
    /// actions end the card without effect. So the only shape a "no" could take
    /// wrote nothing — and a workflow whose next sentence depends on whether the
    /// user has already answered could not tell that they had.
    ///
    /// A send confirmation offered "Send" and "Edit". The user pressed Edit;
    /// nothing was written, so the projection was unchanged, so the phase's own
    /// instruction to the writing stage was served again, and the reply was word
    /// for word the sentence the user had just answered. The writer was obeying.
    ///
    /// The runtime already remembers the answer well enough not to raise the
    /// card again, and that memory decides whether the **card** is rendered. It
    /// does not reach what the workflow tells the stage that speaks, which is
    /// where the repetition lives.
    ///
    /// # What it is not
    ///
    /// It does not run what it declined. The journaled commands are dropped
    /// exactly as [`Self::DeclineCommands`] drops them, [`Self::declines`] is
    /// true of it, and it satisfies the requirement that a confirmation offer a
    /// way to say no. What it adds is one operation, named by the server that
    /// wrote the card, compiled through `compile_act` and validated like any
    /// other — so a workflow that records nothing is unaffected, and one that
    /// records something cannot smuggle a confirmation through this door,
    /// because no pending command executes on it.
    ///
    /// The operation is the domain's bookkeeping, so its `command_policy` had
    /// better not ask for a confirmation of its own.
    DeclineAndRecord {
        /// Operation to compile through the workflow, on the case the card
        /// belongs to.
        operation: OperationKey,
    },
    /// Close the card without effect.
    Dismiss,
    /// Application-defined action.
    Custom {
        /// Application key.
        key: String,
        /// Application payload.
        payload: serde_json::Value,
    },
}

impl StoredInteractionAction {
    /// What choosing this option authorizes.
    ///
    /// [`Self::Custom`] is deliberately [`ActionClass::NoCommands`]: an
    /// application-defined action cannot be a confirmation, because the library
    /// cannot know what it does.
    #[must_use]
    pub fn action_class(&self) -> ActionClass {
        match self {
            Self::ConfirmCommands { .. } => ActionClass::ConfirmsCommands,
            Self::ApplyOperation { .. } => ActionClass::AppliesOperation,
            // It compiles an operation, and it is still not a confirmation:
            // what it authorizes is the record of a refusal, on the path where
            // nothing pending runs.
            Self::DeclineCommands
            | Self::DeclineAndRecord { .. }
            | Self::SelectTarget { .. }
            | Self::ResolveClarification { .. }
            | Self::Dismiss
            | Self::Custom { .. } => ActionClass::NoCommands,
        }
    }

    /// Returns `true` when choosing this option ends the card without effect.
    #[must_use]
    pub fn declines(&self) -> bool {
        matches!(
            self,
            Self::DeclineCommands | Self::DeclineAndRecord { .. } | Self::Dismiss
        )
    }

    /// The operation a decline records, when it records one.
    #[must_use]
    pub fn records(&self) -> Option<&OperationKey> {
        match self {
            Self::DeclineAndRecord { operation } => Some(operation),
            _ => None,
        }
    }
}

/// Visual emphasis of an option, for clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptionStyle {
    /// The suggested action.
    Primary,
    /// A neutral action.
    #[default]
    Secondary,
    /// A destructive or declining action.
    Danger,
}

/// A stored option (spec §15.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionOption {
    /// Identifier echoed by the client.
    pub id: OptionId,
    /// Label copy.
    pub label: LocalizedText,
    /// Server-side meaning.
    pub action: StoredInteractionAction,
    /// Whether free text may accompany the click.
    #[serde(default)]
    pub freeform_policy: FreeformPolicy,
    /// Visual emphasis.
    #[serde(default)]
    pub style: OptionStyle,
}

impl InteractionOption {
    /// Builds an option that forbids free text with secondary style.
    #[must_use]
    pub fn new(
        id: impl Into<OptionId>,
        label: impl Into<LocalizedText>,
        action: StoredInteractionAction,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            action,
            freeform_policy: FreeformPolicy::Forbidden,
            style: OptionStyle::Secondary,
        }
    }

    /// Sets the free-form policy.
    #[must_use]
    pub fn with_freeform(mut self, policy: FreeformPolicy) -> Self {
        self.freeform_policy = policy;
        self
    }

    /// Sets the style.
    #[must_use]
    pub fn with_style(mut self, style: OptionStyle) -> Self {
        self.style = style;
        self
    }

    /// The client-facing projection (no action).
    #[must_use]
    pub fn view(&self) -> InteractionOptionView {
        InteractionOptionView {
            id: self.id.clone(),
            label: self.label.clone(),
            freeform_policy: self.freeform_policy,
            style: self.style,
        }
    }
}

/// One side of a field change: absent, or an explicit JSON value.
///
/// `Option<serde_json::Value>` cannot express this: with
/// `skip_serializing_if = "Option::is_none"` an explicit `Some(Value::Null)` —
/// "this field is being cleared" — is written as a missing key and reads back
/// as `None`, so a review card that clears a field fails
/// [`Interaction::verify_payload_hash`] after a round trip through any JSON
/// store and its confirmation can never match. `FieldValue` keeps the two
/// apart: [`Self::Absent`] omits the key, [`Self::Present`] writes the value,
/// `null` included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldValue {
    /// The field is not part of this side of the change.
    #[default]
    Absent,
    /// An explicit value. [`serde_json::Value::Null`] means "cleared".
    Present(serde_json::Value),
}

impl FieldValue {
    /// A present value.
    #[must_use]
    pub fn present(value: impl Into<serde_json::Value>) -> Self {
        Self::Present(value.into())
    }

    /// An explicit JSON `null`: the field is being cleared.
    #[must_use]
    pub fn cleared() -> Self {
        Self::Present(serde_json::Value::Null)
    }

    /// Returns `true` when the field is not part of this side.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// The value, when present.
    #[must_use]
    pub fn value(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Absent => None,
            Self::Present(value) => Some(value),
        }
    }
}

impl From<serde_json::Value> for FieldValue {
    fn from(value: serde_json::Value) -> Self {
        Self::Present(value)
    }
}

impl Serialize for FieldValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // `Absent` is skipped by the container; serializing it standalone still
        // has to produce something, and `null` is the closest reading.
        match self {
            Self::Absent => serializer.serialize_unit(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FieldValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Only called when the key is present, so an explicit `null` arrives
        // here and stays `Present(Null)`; a missing key uses `Default`.
        serde_json::Value::deserialize(deserializer).map(Self::Present)
    }
}

/// One line of a review card: a field before and after the proposed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiffEntry {
    /// Stable field path.
    pub field: String,
    /// Label copy.
    pub label: LocalizedText,
    /// Current value.
    #[serde(default, skip_serializing_if = "FieldValue::is_absent")]
    pub before: FieldValue,
    /// Proposed value.
    #[serde(default, skip_serializing_if = "FieldValue::is_absent")]
    pub after: FieldValue,
}

impl ReviewDiffEntry {
    /// Builds an entry with both sides absent.
    #[must_use]
    pub fn new(field: impl Into<String>, label: impl Into<LocalizedText>) -> Self {
        Self {
            field: field.into(),
            label: label.into(),
            before: FieldValue::Absent,
            after: FieldValue::Absent,
        }
    }

    /// Sets the current value.
    #[must_use]
    pub fn with_before(mut self, before: FieldValue) -> Self {
        self.before = before;
        self
    }

    /// Sets the proposed value.
    #[must_use]
    pub fn with_after(mut self, after: FieldValue) -> Self {
        self.after = after;
        self
    }
}

/// Immutable content of an interaction (spec §15.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionPayload {
    /// Title copy.
    pub title: LocalizedText,
    /// Body copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<LocalizedText>,
    /// Stored options.
    #[serde(default)]
    pub options: Vec<InteractionOption>,
    /// Diff entries for review cards.
    #[serde(default)]
    pub review_entries: Vec<ReviewDiffEntry>,
    /// Prompt for free-form interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform_prompt: Option<LocalizedText>,
    /// Application metadata (rendering hints, preview hash...).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl InteractionPayload {
    /// Builds a payload with only a title.
    #[must_use]
    pub fn new(title: impl Into<LocalizedText>) -> Self {
        Self {
            title: title.into(),
            body: None,
            options: Vec::new(),
            review_entries: Vec::new(),
            freeform_prompt: None,
            metadata: serde_json::Value::Null,
        }
    }

    /// Sets the body.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<LocalizedText>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Appends an option.
    #[must_use]
    pub fn with_option(mut self, option: InteractionOption) -> Self {
        self.options.push(option);
        self
    }

    /// Appends a review entry.
    #[must_use]
    pub fn with_review_entry(mut self, entry: ReviewDiffEntry) -> Self {
        self.review_entries.push(entry);
        self
    }

    /// Sets metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// BLAKE3 over the canonical JSON of the payload. Stored on the interaction
    /// and embedded in [`CommandOrigin::ConfirmedInteraction`] so a command can
    /// prove which content the user confirmed.
    pub fn hash(&self) -> Result<Digest, HashError> {
        canonical_digest(self)
    }

    /// Finds a stored option.
    #[must_use]
    pub fn option(&self, id: &OptionId) -> Option<&InteractionOption> {
        self.options.iter().find(|o| &o.id == id)
    }

    /// Identifiers of all options.
    #[must_use]
    pub fn option_ids(&self) -> Vec<OptionId> {
        self.options.iter().map(|o| o.id.clone()).collect()
    }

    /// Sets the free-form prompt.
    #[must_use]
    pub fn with_freeform_prompt(mut self, prompt: impl Into<LocalizedText>) -> Self {
        self.freeform_prompt = Some(prompt.into());
        self
    }

    /// Checks that a card of this `kind` can actually be answered (I6).
    ///
    /// A user-owned phase whose card carries no usable option is a dead end:
    /// the case blocks on an answer nobody can give. The rules are:
    ///
    /// * option ids are unique, whatever the kind;
    /// * `Boolean` has exactly two options;
    /// * `SingleSelect`, `SelectTarget` and `ResolveValidationError` have at
    ///   least one option;
    /// * `ConfirmCommand` and `ReviewChanges` have at least one option that
    ///   authorizes commands **and** one that declines, so refusing is always
    ///   possible; `ReviewChanges` also has at least one diff entry;
    /// * `Reauthenticate` and `ExternalSignature` have at least one option that
    ///   authorizes commands, since their whole purpose is to carry that
    ///   authority;
    /// * `Freeform` has a prompt and an option that requires free text;
    /// * `MultiSelect` is refused outright: the input protocol carries one
    ///   option id ([`InteractionResponse::option_id`]), so a persisted
    ///   multi-select card could only ever be answered as a single select.
    pub fn validate_for(&self, kind: InteractionKind) -> Result<(), InteractionSpecError> {
        let mut seen = std::collections::BTreeSet::new();
        for option in &self.options {
            if !seen.insert(&option.id) {
                return Err(InteractionSpecError::DuplicateOptionId {
                    option_id: option.id.clone(),
                });
            }
        }
        let authorizing = self
            .options
            .iter()
            .filter(|o| o.action.action_class().authorizes_commands())
            .count();
        let declining = self.options.iter().filter(|o| o.action.declines()).count();
        let required_freeform = self
            .options
            .iter()
            .filter(|o| o.freeform_policy.requires())
            .count();
        let require_options = |expected: usize| {
            if self.options.len() < expected {
                Err(InteractionSpecError::NotEnoughOptions {
                    interaction_kind: kind,
                    required: expected,
                    found: self.options.len(),
                })
            } else {
                Ok(())
            }
        };
        match kind {
            InteractionKind::MultiSelect => {
                return Err(InteractionSpecError::UnsupportedKind {
                    interaction_kind: kind,
                });
            }
            InteractionKind::Boolean => {
                if self.options.len() != 2 {
                    return Err(InteractionSpecError::NotEnoughOptions {
                        interaction_kind: kind,
                        required: 2,
                        found: self.options.len(),
                    });
                }
            }
            InteractionKind::SingleSelect
            | InteractionKind::SelectTarget
            | InteractionKind::ResolveValidationError => require_options(1)?,
            InteractionKind::ConfirmCommand | InteractionKind::ReviewChanges => {
                require_options(2)?;
                if authorizing == 0 {
                    return Err(InteractionSpecError::MissingAuthorizingOption {
                        interaction_kind: kind,
                    });
                }
                if declining == 0 {
                    return Err(InteractionSpecError::MissingDeclineOption {
                        interaction_kind: kind,
                    });
                }
                if kind == InteractionKind::ReviewChanges && self.review_entries.is_empty() {
                    return Err(InteractionSpecError::MissingReviewEntries);
                }
            }
            InteractionKind::Reauthenticate | InteractionKind::ExternalSignature => {
                require_options(1)?;
                if authorizing == 0 {
                    return Err(InteractionSpecError::MissingAuthorizingOption {
                        interaction_kind: kind,
                    });
                }
            }
            InteractionKind::Freeform => {
                require_options(1)?;
                if self.freeform_prompt.is_none() {
                    return Err(InteractionSpecError::MissingFreeformPrompt);
                }
                if required_freeform == 0 {
                    return Err(InteractionSpecError::MissingFreeformOption);
                }
            }
        }
        Ok(())
    }
}

/// What a reducer or workflow asks the engine to create (spec §13.3, I6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionSpec {
    /// Stable key within a turn/plan (e.g. `"select_target:acts[1]"`). Two
    /// specs with the same key describe the same interaction.
    pub key: String,
    /// Case the interaction belongs to; its revision is the bound revision.
    pub case_ref: CaseRef,
    /// Shape.
    pub kind: InteractionKind,
    /// Whether it owns unqualified answers for the case (I5).
    pub blocking: bool,
    /// Content.
    pub payload: InteractionPayload,
    /// Time to live, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<Duration>,
    /// Whether typed text may resolve it.
    #[serde(default)]
    pub text_resolution: TextResolutionPolicy,
    /// Highest risk class of the commands an answer to this card authorizes.
    ///
    /// The default is [`RiskClass::Irreversible`], so a card that forgets to
    /// declare it is treated as consequential and can never be resolved from
    /// typed text (spec §13.2 rule 8, §15.7).
    #[serde(default = "RiskClass::conservative")]
    pub confirms_risk: RiskClass,
    /// `true` when the interaction is invalidated by a case revision change.
    pub binds_to_revision: bool,
}

impl InteractionSpec {
    /// Builds a blocking, revision-bound spec with `Never` text resolution and
    /// the conservative [`RiskClass::Irreversible`] confirmation risk.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        case_ref: CaseRef,
        kind: InteractionKind,
        payload: InteractionPayload,
    ) -> Self {
        Self {
            key: key.into(),
            case_ref,
            kind,
            blocking: true,
            payload,
            expires_in: None,
            text_resolution: TextResolutionPolicy::Never,
            confirms_risk: RiskClass::conservative(),
            binds_to_revision: true,
        }
    }

    /// Declares the highest risk class an answer authorizes.
    #[must_use]
    pub fn with_confirms_risk(mut self, risk: RiskClass) -> Self {
        self.confirms_risk = risk;
        self
    }

    /// Checks that the card can be answered and that its text-resolution
    /// policy is allowed for what it confirms.
    ///
    /// Beyond [`InteractionPayload::validate_for`], any policy other than
    /// [`TextResolutionPolicy::Never`] is refused when the card authorizes
    /// commands above [`RiskClass::ReversibleLowRisk`] or is one of the
    /// authorizing kinds ([`InteractionKind::authorizes_commands`]): a
    /// confirmation inferred from prose is not a confirmation.
    pub fn validate(&self) -> Result<(), InteractionSpecError> {
        self.payload.validate_for(self.kind)?;
        if self.text_resolution != TextResolutionPolicy::Never
            && (self.confirms_risk > RiskClass::ReversibleLowRisk
                || self.kind.authorizes_commands())
        {
            return Err(InteractionSpecError::TextResolutionNotAllowed {
                interaction_kind: self.kind,
                confirms_risk: self.confirms_risk,
            });
        }
        Ok(())
    }

    /// Marks the spec non-blocking.
    #[must_use]
    pub fn non_blocking(mut self) -> Self {
        self.blocking = false;
        self
    }

    /// Sets the text resolution policy.
    #[must_use]
    pub fn with_text_resolution(mut self, policy: TextResolutionPolicy) -> Self {
        self.text_resolution = policy;
        self
    }

    /// Sets the time to live.
    #[must_use]
    pub fn expires_in(mut self, ttl: Duration) -> Self {
        self.expires_in = Some(ttl);
        self
    }

    /// Makes the interaction survive revision changes.
    #[must_use]
    pub fn revision_independent(mut self) -> Self {
        self.binds_to_revision = false;
        self
    }
}

/// A persisted interaction (spec §15.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interaction {
    /// Identifier.
    pub id: InteractionId,
    /// Owning tenant.
    pub account_id: AccountId,
    /// Owning conversation.
    pub conversation_id: ConversationId,
    /// Case and the revision the card was rendered against (the bound revision).
    pub case_ref: CaseRef,
    /// Turn that created it.
    pub created_by_turn: TurnId,
    /// Shape.
    pub kind: InteractionKind,
    /// Whether it owns unqualified answers for the case.
    pub blocking: bool,
    /// Immutable content.
    pub payload: InteractionPayload,
    /// Hash of `payload` at creation.
    pub payload_hash: Digest,
    /// Lifecycle status.
    pub status: InteractionStatus,
    /// `true` when a revision change does not invalidate it.
    pub revision_independent: bool,
    /// Whether typed text may resolve it.
    pub text_resolution: TextResolutionPolicy,
    /// Highest risk class of the commands an answer authorizes. Conservative
    /// ([`RiskClass::Irreversible`]) when a stored record predates the field.
    #[serde(default = "RiskClass::conservative")]
    pub confirms_risk: RiskClass,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Expiry, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Resolution time, if resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Option chosen, if resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_option_id: Option<OptionId>,
}

impl Interaction {
    /// Materializes a spec into a new `Active` interaction, computing the
    /// payload hash and the expiry.
    ///
    /// Fails closed: a card that cannot be answered
    /// ([`InteractionSpec::validate`]) or whose time to live cannot be applied
    /// is never persisted. Silently dropping an out-of-range TTL would produce
    /// a card that never expires, which is a fail-open on a safety timeout.
    pub fn from_spec(
        spec: InteractionSpec,
        id: InteractionId,
        account_id: AccountId,
        conversation_id: ConversationId,
        created_by_turn: TurnId,
        now: DateTime<Utc>,
    ) -> Result<Self, InteractionError> {
        spec.validate()?;
        let payload_hash = spec.payload.hash().map_err(|_| InteractionError::Hash)?;
        let expires_at = spec
            .expires_in
            .map(|ttl| {
                chrono::Duration::from_std(ttl)
                    .ok()
                    .and_then(|ttl| now.checked_add_signed(ttl))
                    .ok_or(InteractionError::InvalidTtl)
            })
            .transpose()?;
        Ok(Self {
            id,
            account_id,
            conversation_id,
            case_ref: spec.case_ref,
            created_by_turn,
            kind: spec.kind,
            blocking: spec.blocking,
            payload: spec.payload,
            payload_hash,
            status: InteractionStatus::Active,
            revision_independent: !spec.binds_to_revision,
            text_resolution: spec.text_resolution,
            confirms_risk: spec.confirms_risk,
            created_at: now,
            expires_at,
            resolved_at: None,
            resolved_option_id: None,
        })
    }

    /// The revision the card is bound to, or `None` when revision independent.
    #[must_use]
    pub fn bound_revision(&self) -> Option<CaseRevision> {
        (!self.revision_independent).then_some(self.case_ref.expected_revision)
    }

    /// Returns `true` when `now` is past the expiry.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    /// Recomputes the payload hash and compares it with the stored one.
    pub fn verify_payload_hash(&self) -> Result<bool, HashError> {
        Ok(self.payload.hash()? == self.payload_hash)
    }

    /// Applies a status transition, refusing illegal ones.
    pub fn transition(&mut self, to: InteractionStatus) -> Result<(), InteractionError> {
        if !InteractionStatus::can_transition(self.status, to) {
            return Err(InteractionError::InvalidTransition {
                interaction_id: self.id,
                from: self.status,
                to,
            });
        }
        self.status = to;
        Ok(())
    }

    /// Records the chosen option and moves to `Resolving`.
    pub fn begin_resolution(
        &mut self,
        option_id: OptionId,
        now: DateTime<Utc>,
    ) -> Result<(), InteractionError> {
        self.transition(InteractionStatus::Resolving)?;
        self.resolved_option_id = Some(option_id);
        self.resolved_at = Some(now);
        Ok(())
    }

    /// The client-facing projection without server actions.
    #[must_use]
    pub fn view(&self) -> InteractionView {
        InteractionView {
            id: self.id,
            kind: self.kind,
            status: self.status,
            blocking: self.blocking,
            case_ref: self.case_ref.clone(),
            title: self.payload.title.clone(),
            body: self.payload.body.clone(),
            options: self
                .payload
                .options
                .iter()
                .map(InteractionOption::view)
                .collect(),
            review_entries: self.payload.review_entries.clone(),
            freeform_prompt: self.payload.freeform_prompt.clone(),
            metadata: self.payload.metadata.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// Client-facing option: no server action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionOptionView {
    /// Identifier to echo back.
    pub id: OptionId,
    /// Label copy.
    pub label: LocalizedText,
    /// Whether free text may accompany the click.
    pub freeform_policy: FreeformPolicy,
    /// Visual emphasis.
    pub style: OptionStyle,
}

/// Client-facing projection of an interaction (spec §18.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionView {
    /// Identifier.
    pub id: InteractionId,
    /// Shape.
    pub kind: InteractionKind,
    /// Status.
    pub status: InteractionStatus,
    /// Whether it blocks the case.
    pub blocking: bool,
    /// Case and bound revision the client must echo back.
    pub case_ref: CaseRef,
    /// Title copy.
    pub title: LocalizedText,
    /// Body copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<LocalizedText>,
    /// Options without actions.
    pub options: Vec<InteractionOptionView>,
    /// Diff entries.
    #[serde(default)]
    pub review_entries: Vec<ReviewDiffEntry>,
    /// Free-form prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform_prompt: Option<LocalizedText>,
    /// Application metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Why a response was not accepted (spec §15.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InteractionRejection {
    /// Wrong id, account or conversation. Deliberately indistinguishable so a
    /// guessed id reveals nothing about other tenants (spec §25.4).
    #[error("interaction not found")]
    NotFound,
    /// The interaction is not `Active`.
    #[error("interaction is not active ({status:?})")]
    NotActive {
        /// Current status.
        status: InteractionStatus,
    },
    /// The interaction was already resolved; the original option is returned so
    /// the caller can replay the original result (I14).
    #[error("interaction already resolved")]
    AlreadyResolved {
        /// The option chosen originally.
        option_id: Option<OptionId>,
    },
    /// The interaction expired.
    #[error("interaction expired")]
    Expired,
    /// The case moved past the bound revision, or the client echoed another one.
    #[error("interaction stale: bound {bound_revision}, current {current_revision}")]
    Stale {
        /// Revision the card was rendered against.
        bound_revision: CaseRevision,
        /// Revision now.
        current_revision: CaseRevision,
    },
    /// The option id is not stored on the interaction.
    #[error("unknown option")]
    UnknownOption,
    /// Free text was supplied but the option forbids it.
    #[error("freeform input not allowed")]
    FreeformNotAllowed,
    /// The option requires free text and none was supplied.
    #[error("freeform input required")]
    FreeformRequired,
    /// Free text exceeds the option's limit.
    #[error("freeform input exceeds {max_len} characters")]
    FreeformTooLong {
        /// The limit.
        max_len: usize,
    },
    /// The stored payload no longer hashes to the stored `payload_hash`, so the
    /// option's meaning is not the one the user saw.
    #[error("interaction payload does not match its stored hash")]
    PayloadCorrupt,
    /// The answer arrived on a channel the card's
    /// [`TextResolutionPolicy`] does not admit (e.g. an interpreted "yes" on a
    /// card that only the CTA resolves).
    #[error("interaction cannot be resolved through the {channel:?} channel")]
    ChannelNotAllowed {
        /// The channel the answer arrived on.
        channel: ResolutionChannel,
    },
}

/// A response that passed every §15.5 rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedResponse {
    /// The interaction.
    pub interaction_id: InteractionId,
    /// Case and bound revision.
    pub case_ref: CaseRef,
    /// Shape of the card that was answered.
    pub kind: InteractionKind,
    /// The chosen option.
    pub option_id: OptionId,
    /// Server-side meaning of the option.
    pub action: StoredInteractionAction,
    /// How the answer reached the server.
    pub channel: ResolutionChannel,
    /// Free text, when permitted and supplied.
    pub freeform_input: Option<String>,
    /// Hash of the payload the user saw.
    pub payload_hash: Digest,
}

impl AcceptedResponse {
    /// The command origin this response authorizes (I12), or `None` when it
    /// authorizes nothing.
    ///
    /// Only an option that actually confirms or applies commands mints an
    /// origin. Picking a case on a `SelectTarget` card, answering a
    /// clarification or dismissing a card resolves the interaction without
    /// authorizing anything: the runtime must re-run policy for the original
    /// act and raise a fresh confirmation when one is required, instead of
    /// treating the click as consent.
    #[must_use]
    pub fn origin(&self) -> Option<CommandOrigin> {
        let action_class = self.action.action_class();
        action_class
            .authorizes_commands()
            .then(|| CommandOrigin::ConfirmedInteraction {
                interaction_id: self.interaction_id,
                payload_hash: self.payload_hash.clone(),
                interaction_kind: self.kind,
                action_class,
                channel: self.channel,
            })
    }
}

/// Pure validation of a client response against the stored interaction
/// (spec §15.5).
///
/// Checks, in order: identity (id, account, conversation → `NotFound`),
/// status (`AlreadyResolved` / `NotActive`), expiry, revision binding
/// (`Stale`), payload integrity (`PayloadCorrupt`), the resolution channel
/// against the stored [`TextResolutionPolicy`] (`ChannelNotAllowed`), option
/// existence (`UnknownOption`), free-form rules.
///
/// `channel` says how the answer arrived: [`ResolutionChannel::Click`] for a
/// structured client response, the text channels when the runtime resolved
/// typed text to an option. It is carried into the accepted response and from
/// there into the [`CommandOrigin`], because an inferred "yes" and a click are
/// not the same authority (I12, I20).
pub fn validate_response(
    interaction: &Interaction,
    response: &InteractionResponse,
    channel: ResolutionChannel,
    actor: &ActorContext,
    conversation_id: &ConversationId,
    current_revision: CaseRevision,
    now: DateTime<Utc>,
) -> Result<AcceptedResponse, InteractionRejection> {
    if response.interaction_id != interaction.id
        || actor.account_id != interaction.account_id
        || *conversation_id != interaction.conversation_id
    {
        return Err(InteractionRejection::NotFound);
    }
    match interaction.status {
        InteractionStatus::Active => {}
        InteractionStatus::Resolved => {
            return Err(InteractionRejection::AlreadyResolved {
                option_id: interaction.resolved_option_id.clone(),
            });
        }
        status => return Err(InteractionRejection::NotActive { status }),
    }
    if interaction.is_expired(now) {
        return Err(InteractionRejection::Expired);
    }
    if let Some(bound) = interaction.bound_revision()
        && (response.expected_case_revision != bound || current_revision != bound)
    {
        return Err(InteractionRejection::Stale {
            bound_revision: bound,
            current_revision,
        });
    }
    // The stored payload is what the user saw and what the origin's hash will
    // claim; if the two disagree, the option's meaning is not the one that was
    // shown, so nothing may execute (I19, §15.5).
    if !matches!(interaction.verify_payload_hash(), Ok(true)) {
        return Err(InteractionRejection::PayloadCorrupt);
    }
    if !interaction.text_resolution.admits(channel) {
        return Err(InteractionRejection::ChannelNotAllowed { channel });
    }
    let option = interaction
        .payload
        .option(&response.option_id)
        .ok_or(InteractionRejection::UnknownOption)?;
    let freeform_input = match (&response.freeform_input, option.freeform_policy) {
        (Some(_), FreeformPolicy::Forbidden) => {
            return Err(InteractionRejection::FreeformNotAllowed);
        }
        (None, FreeformPolicy::Required { .. }) => {
            return Err(InteractionRejection::FreeformRequired);
        }
        (
            Some(text),
            FreeformPolicy::Optional { max_len } | FreeformPolicy::Required { max_len },
        ) => {
            if text.chars().count() > max_len {
                return Err(InteractionRejection::FreeformTooLong { max_len });
            }
            // Blank text is no text: required free text must carry content.
            if text.trim().is_empty() {
                if option.freeform_policy.requires() {
                    return Err(InteractionRejection::FreeformRequired);
                }
                None
            } else {
                Some(text.clone())
            }
        }
        (None, _) => None,
    };
    Ok(AcceptedResponse {
        interaction_id: interaction.id,
        case_ref: interaction.case_ref.clone(),
        kind: interaction.kind,
        option_id: option.id.clone(),
        action: option.action.clone(),
        channel,
        freeform_input,
        payload_hash: interaction.payload_hash.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::ConfirmationPolicy;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn confirm_option() -> InteractionOption {
        InteractionOption::new(
            "confirm",
            "Confirm",
            StoredInteractionAction::ApplyOperation {
                operation: OperationKey::from("trip.rebook"),
                arguments: serde_json::Value::Null,
                freeform_argument: None,
            },
        )
    }

    fn decline_option() -> InteractionOption {
        InteractionOption::new("no", "Cancel", StoredInteractionAction::DeclineCommands)
    }

    fn payload() -> InteractionPayload {
        InteractionPayload::new("Rebook this flight?")
            .with_option(confirm_option())
            .with_option(decline_option())
            .with_option(
                InteractionOption::new("note", "Add a note", StoredInteractionAction::Dismiss)
                    .with_freeform(FreeformPolicy::Optional { max_len: 5 }),
            )
            .with_option(
                InteractionOption::new("why", "Explain", StoredInteractionAction::Dismiss)
                    .with_freeform(FreeformPolicy::Required { max_len: 100 }),
            )
    }

    fn spec() -> InteractionSpec {
        InteractionSpec::new(
            "send",
            CaseRef::new("trip", "i1", CaseRevision(12)),
            InteractionKind::ConfirmCommand,
            payload(),
        )
    }

    fn interaction() -> Interaction {
        Interaction::from_spec(
            spec(),
            InteractionId::nil(),
            AccountId::from("acct"),
            ConversationId::nil(),
            TurnId::nil(),
            now(),
        )
        .unwrap()
    }

    fn response(option: &str) -> InteractionResponse {
        InteractionResponse {
            interaction_id: InteractionId::nil(),
            option_id: OptionId::from(option),
            expected_case_revision: CaseRevision(12),
            freeform_input: None,
        }
    }

    fn actor() -> ActorContext {
        ActorContext::new("acct", "u1")
    }

    fn validate(
        i: &Interaction,
        r: &InteractionResponse,
    ) -> Result<AcceptedResponse, InteractionRejection> {
        validate_response(
            i,
            r,
            ResolutionChannel::Click,
            &actor(),
            &ConversationId::nil(),
            CaseRevision(12),
            now(),
        )
    }

    #[test]
    fn happy_path_yields_confirmed_origin() {
        let i = interaction();
        let accepted = validate(&i, &response("confirm")).unwrap();
        assert_eq!(accepted.option_id, OptionId::from("confirm"));
        assert_eq!(
            accepted.origin(),
            Some(CommandOrigin::ConfirmedInteraction {
                interaction_id: i.id,
                payload_hash: i.payload_hash.clone(),
                interaction_kind: InteractionKind::ConfirmCommand,
                action_class: ActionClass::AppliesOperation,
                channel: ResolutionChannel::Click,
            })
        );
        assert!(crate::command::origin_satisfies(
            &accepted.origin().unwrap(),
            &crate::command::CommandPolicy::conservative()
        ));
    }

    #[test]
    fn an_option_that_authorizes_nothing_mints_no_origin() {
        let i = interaction();
        // Declining and dismissing resolve the card without authorizing a
        // command; the runtime must not treat either as consent.
        for option in ["no", "note"] {
            let accepted = validate(&i, &response(option)).unwrap();
            assert_eq!(accepted.origin(), None, "{option}");
        }
        let selection = Interaction::from_spec(
            InteractionSpec::new(
                "which",
                CaseRef::new("trip", "i1", CaseRevision(12)),
                InteractionKind::SelectTarget,
                InteractionPayload::new("Which trip?").with_option(InteractionOption::new(
                    "a",
                    "Trip A",
                    StoredInteractionAction::SelectTarget {
                        case_ref: CaseRef::new("trip", "a", CaseRevision(1)),
                    },
                )),
            ),
            InteractionId::nil(),
            AccountId::from("acct"),
            ConversationId::nil(),
            TurnId::nil(),
            now(),
        )
        .unwrap();
        let accepted = validate(&selection, &response("a")).unwrap();
        assert_eq!(accepted.origin(), None);
    }

    #[test]
    fn wrong_account_and_conversation_are_indistinguishable() {
        let i = interaction();
        let other_actor = ActorContext::new("other", "u1");
        let err_account = validate_response(
            &i,
            &response("confirm"),
            ResolutionChannel::Click,
            &other_actor,
            &ConversationId::nil(),
            CaseRevision(12),
            now(),
        )
        .unwrap_err();
        let err_conv = validate_response(
            &i,
            &response("confirm"),
            ResolutionChannel::Click,
            &actor(),
            &ConversationId::new(),
            CaseRevision(12),
            now(),
        )
        .unwrap_err();
        let mut r = response("confirm");
        r.interaction_id = InteractionId::new();
        let err_id = validate(&i, &r).unwrap_err();
        assert_eq!(err_account, InteractionRejection::NotFound);
        assert_eq!(err_conv, InteractionRejection::NotFound);
        assert_eq!(err_id, InteractionRejection::NotFound);
    }

    #[test]
    fn identity_is_checked_before_status_and_expiry() {
        // A foreign tenant must not learn that the card exists, whatever state
        // it is in.
        let mut resolved = interaction();
        resolved
            .begin_resolution(OptionId::from("confirm"), now())
            .unwrap();
        resolved.transition(InteractionStatus::Resolved).unwrap();
        let mut expired = interaction();
        expired.expires_at = Some(now() - chrono::Duration::seconds(1));
        for card in [&resolved, &expired] {
            let err = validate_response(
                card,
                &response("confirm"),
                ResolutionChannel::Click,
                &ActorContext::new("other", "u1"),
                &ConversationId::nil(),
                CaseRevision(12),
                now(),
            )
            .unwrap_err();
            assert_eq!(err, InteractionRejection::NotFound);
        }
    }

    #[test]
    fn unknown_option_rejected() {
        assert_eq!(
            validate(&interaction(), &response("nope")).unwrap_err(),
            InteractionRejection::UnknownOption
        );
    }

    #[test]
    fn freeform_rules() {
        let i = interaction();
        let mut r = response("confirm");
        r.freeform_input = Some("x".into());
        assert_eq!(
            validate(&i, &r).unwrap_err(),
            InteractionRejection::FreeformNotAllowed
        );
        let mut r = response("note");
        r.freeform_input = Some("toolong".into());
        assert_eq!(
            validate(&i, &r).unwrap_err(),
            InteractionRejection::FreeformTooLong { max_len: 5 }
        );
        r.freeform_input = Some("exact".into());
        assert_eq!(
            validate(&i, &r).unwrap().freeform_input.as_deref(),
            Some("exact"),
            "a string of exactly max_len characters is accepted"
        );
        r.freeform_input = Some("ok".into());
        assert_eq!(
            validate(&i, &r).unwrap().freeform_input.as_deref(),
            Some("ok")
        );
        assert!(validate(&i, &response("note")).is_ok());
        assert_eq!(
            validate(&i, &response("why")).unwrap_err(),
            InteractionRejection::FreeformRequired
        );
    }

    #[test]
    fn blank_text_does_not_satisfy_a_required_freeform() {
        let i = interaction();
        for blank in ["", "   ", "\n\t "] {
            let mut r = response("why");
            r.freeform_input = Some(blank.into());
            assert_eq!(
                validate(&i, &r).unwrap_err(),
                InteractionRejection::FreeformRequired,
                "{blank:?}"
            );
        }
        // Blank optional text is simply no text.
        let mut r = response("note");
        r.freeform_input = Some("  ".into());
        assert_eq!(validate(&i, &r).unwrap().freeform_input, None);
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let mut i = interaction();
        i.payload.options[0].action = StoredInteractionAction::ApplyOperation {
            operation: OperationKey::from("trip.withdraw"),
            arguments: serde_json::Value::Null,
            freeform_argument: None,
        };
        assert!(!i.verify_payload_hash().unwrap());
        assert_eq!(
            validate(&i, &response("confirm")).unwrap_err(),
            InteractionRejection::PayloadCorrupt
        );
    }

    #[test]
    fn a_card_only_answers_on_the_channels_it_admits() {
        let i = interaction();
        let channel = ResolutionChannel::ModelInterpreted;
        let err = validate_response(
            &i,
            &response("confirm"),
            channel,
            &actor(),
            &ConversationId::nil(),
            CaseRevision(12),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, InteractionRejection::ChannelNotAllowed { channel });
        let mut aliased = Interaction::from_spec(
            InteractionSpec::new(
                "pick",
                CaseRef::new("trip", "i1", CaseRevision(12)),
                InteractionKind::SingleSelect,
                InteractionPayload::new("Which?").with_option(InteractionOption::new(
                    "a",
                    "A",
                    StoredInteractionAction::ResolveClarification {
                        answer_key: "a".into(),
                    },
                )),
            )
            .with_confirms_risk(RiskClass::ReversibleLowRisk)
            .with_text_resolution(TextResolutionPolicy::Never),
            InteractionId::nil(),
            AccountId::from("acct"),
            ConversationId::nil(),
            TurnId::nil(),
            now(),
        )
        .unwrap();
        aliased.text_resolution = TextResolutionPolicy::ModelInterpretedLowRisk;
        let accepted = validate_response(
            &aliased,
            &response("a"),
            ResolutionChannel::ModelInterpreted,
            &actor(),
            &ConversationId::nil(),
            CaseRevision(12),
            now(),
        )
        .unwrap();
        assert_eq!(accepted.channel, ResolutionChannel::ModelInterpreted);
        assert_eq!(
            accepted.origin(),
            None,
            "a clarification authorizes nothing"
        );
    }

    #[test]
    fn stale_revision_rejected_both_ways() {
        let i = interaction();
        let mut r = response("confirm");
        r.expected_case_revision = CaseRevision(11);
        assert_eq!(
            validate(&i, &r).unwrap_err(),
            InteractionRejection::Stale {
                bound_revision: CaseRevision(12),
                current_revision: CaseRevision(12)
            }
        );
        let err = validate_response(
            &i,
            &response("confirm"),
            ResolutionChannel::Click,
            &actor(),
            &ConversationId::nil(),
            CaseRevision(13),
            now(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            InteractionRejection::Stale {
                bound_revision: CaseRevision(12),
                current_revision: CaseRevision(13)
            }
        );
        let mut independent = interaction();
        independent.revision_independent = true;
        assert!(
            validate_response(
                &independent,
                &response("confirm"),
                ResolutionChannel::Click,
                &actor(),
                &ConversationId::nil(),
                CaseRevision(99),
                now(),
            )
            .is_ok()
        );
    }

    #[test]
    fn already_resolved_returns_original_option() {
        let mut i = interaction();
        i.begin_resolution(OptionId::from("confirm"), now())
            .unwrap();
        i.transition(InteractionStatus::Resolved).unwrap();
        assert_eq!(
            validate(&i, &response("confirm")).unwrap_err(),
            InteractionRejection::AlreadyResolved {
                option_id: Some(OptionId::from("confirm"))
            }
        );
    }

    #[test]
    fn expired_and_not_active() {
        let mut i = interaction();
        i.expires_at = Some(now() - chrono::Duration::seconds(1));
        assert_eq!(
            validate(&i, &response("confirm")).unwrap_err(),
            InteractionRejection::Expired
        );
        let mut i = interaction();
        i.status = InteractionStatus::Invalidated;
        assert_eq!(
            validate(&i, &response("confirm")).unwrap_err(),
            InteractionRejection::NotActive {
                status: InteractionStatus::Invalidated
            }
        );
        let mut i = interaction();
        i.status = InteractionStatus::Resolving;
        assert!(matches!(
            validate(&i, &response("confirm")).unwrap_err(),
            InteractionRejection::NotActive { .. }
        ));
    }

    #[test]
    fn state_machine_table() {
        use InteractionStatus as S;
        let allowed: &[(S, S)] = &[
            (S::Active, S::Resolving),
            (S::Active, S::Resolved),
            (S::Active, S::Declined),
            (S::Active, S::Dismissed),
            (S::Active, S::Invalidated),
            (S::Active, S::Expired),
            (S::Active, S::Failed),
            (S::Resolving, S::Resolved),
            (S::Resolving, S::Failed),
            (S::Resolving, S::Active),
        ];
        for from in S::ALL {
            for to in S::ALL {
                let expected = allowed.contains(&(from, to));
                assert_eq!(S::can_transition(from, to), expected, "{from:?} -> {to:?}");
            }
        }
        for terminal in [
            S::Resolved,
            S::Declined,
            S::Dismissed,
            S::Invalidated,
            S::Expired,
            S::Failed,
        ] {
            assert!(terminal.is_terminal());
            assert!(!terminal.is_open());
        }
    }

    #[test]
    fn transition_refuses_what_the_table_forbids() {
        let mut i = interaction();
        i.transition(InteractionStatus::Resolving).unwrap();
        i.transition(InteractionStatus::Resolved).unwrap();
        let err = i.transition(InteractionStatus::Active).unwrap_err();
        assert!(matches!(
            err,
            InteractionError::InvalidTransition {
                from: InteractionStatus::Resolved,
                to: InteractionStatus::Active,
                ..
            }
        ));
        // `begin_resolution` refuses too, instead of recording the option.
        let mut done = interaction();
        done.status = InteractionStatus::Expired;
        assert!(
            done.begin_resolution(OptionId::from("confirm"), now())
                .is_err()
        );
        assert_eq!(done.resolved_option_id, None);
    }

    #[test]
    fn view_hides_actions() {
        let json = serde_json::to_value(interaction().view()).unwrap();
        assert!(json["options"][0].get("action").is_none());
        assert_eq!(json["options"][0]["id"], "confirm");
    }

    #[test]
    fn a_card_must_be_answerable_for_its_kind() {
        let title_only = InteractionPayload::new("Anything?");
        for kind in InteractionKind::ALL {
            assert!(
                title_only.validate_for(kind).is_err(),
                "{kind:?} accepted a card with no options"
            );
        }
        let boolean = InteractionPayload::new("Ready?")
            .with_option(InteractionOption::new(
                "yes",
                "Yes",
                StoredInteractionAction::ConfirmCommands {
                    command_refs: vec![],
                },
            ))
            .with_option(decline_option());
        assert_eq!(boolean.validate_for(InteractionKind::Boolean), Ok(()));
        let three = boolean.clone().with_option(InteractionOption::new(
            "maybe",
            "Maybe",
            StoredInteractionAction::Dismiss,
        ));
        assert!(matches!(
            three.validate_for(InteractionKind::Boolean),
            Err(InteractionSpecError::NotEnoughOptions { .. })
        ));
        // A confirmation the user cannot refuse is not a confirmation.
        let no_decline = InteractionPayload::new("Send?")
            .with_option(confirm_option())
            .with_option(InteractionOption::new(
                "later",
                "Later",
                StoredInteractionAction::ResolveClarification {
                    answer_key: "later".into(),
                },
            ));
        assert!(matches!(
            no_decline.validate_for(InteractionKind::ConfirmCommand),
            Err(InteractionSpecError::MissingDeclineOption { .. })
        ));
        let no_authority = InteractionPayload::new("Send?")
            .with_option(decline_option())
            .with_option(InteractionOption::new(
                "dismiss",
                "Close",
                StoredInteractionAction::Dismiss,
            ));
        assert!(matches!(
            no_authority.validate_for(InteractionKind::ConfirmCommand),
            Err(InteractionSpecError::MissingAuthorizingOption { .. })
        ));
        // A review card with nothing to review.
        assert!(matches!(
            boolean.validate_for(InteractionKind::ReviewChanges),
            Err(InteractionSpecError::MissingReviewEntries)
        ));
        let review = boolean.clone().with_review_entry(
            ReviewDiffEntry::new("total", "Total")
                .with_after(FieldValue::present(serde_json::Value::from(10))),
        );
        assert_eq!(review.validate_for(InteractionKind::ReviewChanges), Ok(()));
        // Free-form cards need somewhere to type.
        let freeform_option =
            InteractionOption::new("text", "Send", StoredInteractionAction::Dismiss)
                .with_freeform(FreeformPolicy::Required { max_len: 200 });
        let no_prompt = InteractionPayload::new("Note").with_option(freeform_option.clone());
        assert!(matches!(
            no_prompt.validate_for(InteractionKind::Freeform),
            Err(InteractionSpecError::MissingFreeformPrompt)
        ));
        let no_field = InteractionPayload::new("Note")
            .with_freeform_prompt("Write the note")
            .with_option(decline_option());
        assert!(matches!(
            no_field.validate_for(InteractionKind::Freeform),
            Err(InteractionSpecError::MissingFreeformOption)
        ));
        let good = InteractionPayload::new("Note")
            .with_freeform_prompt("Write the note")
            .with_option(freeform_option);
        assert_eq!(good.validate_for(InteractionKind::Freeform), Ok(()));
        // Duplicate ids make the answer ambiguous.
        let duplicated = InteractionPayload::new("Pick")
            .with_option(InteractionOption::new(
                "a",
                "A",
                StoredInteractionAction::Dismiss,
            ))
            .with_option(InteractionOption::new(
                "a",
                "A again",
                StoredInteractionAction::Dismiss,
            ));
        assert!(matches!(
            duplicated.validate_for(InteractionKind::SingleSelect),
            Err(InteractionSpecError::DuplicateOptionId { .. })
        ));
    }

    #[test]
    fn multi_select_cannot_be_persisted() {
        // The input protocol carries one option id, so a stored multi-select
        // card could only ever be answered as a single select.
        let payload = InteractionPayload::new("Pick some")
            .with_option(InteractionOption::new(
                "a",
                "A",
                StoredInteractionAction::Dismiss,
            ))
            .with_option(InteractionOption::new(
                "b",
                "B",
                StoredInteractionAction::Dismiss,
            ));
        assert!(matches!(
            payload.validate_for(InteractionKind::MultiSelect),
            Err(InteractionSpecError::UnsupportedKind {
                interaction_kind: InteractionKind::MultiSelect
            })
        ));
        let spec = InteractionSpec::new(
            "pick",
            CaseRef::new("trip", "i1", CaseRevision(1)),
            InteractionKind::MultiSelect,
            payload,
        );
        assert!(matches!(
            Interaction::from_spec(
                spec,
                InteractionId::nil(),
                AccountId::from("acct"),
                ConversationId::nil(),
                TurnId::nil(),
                now()
            ),
            Err(InteractionError::InvalidSpec(_))
        ));
    }

    #[test]
    fn a_confirming_card_may_never_be_resolved_from_text() {
        let base = spec();
        let policy = TextResolutionPolicy::ModelInterpretedLowRisk;
        let risky = base.clone().with_text_resolution(policy.clone());
        assert!(
            matches!(
                risky.validate(),
                Err(InteractionSpecError::TextResolutionNotAllowed { .. })
            ),
            "a ConfirmCommand card is authorizing whatever its risk"
        );
        // Even a low-risk kind is refused while it confirms something
        // consequential.
        let mut low_kind = base.clone().with_text_resolution(policy);
        low_kind.kind = InteractionKind::SingleSelect;
        low_kind.payload = InteractionPayload::new("Which?").with_option(InteractionOption::new(
            "a",
            "A",
            StoredInteractionAction::Dismiss,
        ));
        assert!(matches!(
            low_kind.validate(),
            Err(InteractionSpecError::TextResolutionNotAllowed { .. })
        ));
        assert_eq!(
            low_kind
                .with_confirms_risk(RiskClass::ReversibleLowRisk)
                .validate(),
            Ok(())
        );
        assert_eq!(base.validate(), Ok(()));
        assert_eq!(
            spec().confirms_risk,
            RiskClass::Irreversible,
            "a card that declares nothing is treated as consequential"
        );
    }

    #[test]
    fn an_unusable_time_to_live_is_an_error_not_an_immortal_card() {
        let ttl = spec().expires_in(Duration::from_secs(60));
        let card = Interaction::from_spec(
            ttl,
            InteractionId::nil(),
            AccountId::from("acct"),
            ConversationId::nil(),
            TurnId::nil(),
            now(),
        )
        .unwrap();
        assert_eq!(card.expires_at, Some(now() + chrono::Duration::seconds(60)));
        assert!(!card.is_expired(now()));
        assert!(card.is_expired(now() + chrono::Duration::seconds(61)));
        let absurd = spec().expires_in(Duration::from_secs(u64::MAX));
        assert!(matches!(
            Interaction::from_spec(
                absurd,
                InteractionId::nil(),
                AccountId::from("acct"),
                ConversationId::nil(),
                TurnId::nil(),
                now()
            ),
            Err(InteractionError::InvalidTtl)
        ));
    }

    #[test]
    fn an_explicit_null_survives_the_round_trip() {
        let entry = ReviewDiffEntry::new("traveler.email", "Email")
            .with_before(FieldValue::present(serde_json::Value::from("a@b.it")))
            .with_after(FieldValue::cleared());
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["after"], serde_json::Value::Null);
        let back: ReviewDiffEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back, entry);
        assert_eq!(back.after, FieldValue::Present(serde_json::Value::Null));

        let absent = ReviewDiffEntry::new("traveler.email", "Email");
        let json = serde_json::to_value(&absent).unwrap();
        assert!(json.get("after").is_none(), "an absent side omits the key");
        assert_eq!(
            serde_json::from_value::<ReviewDiffEntry>(json).unwrap(),
            absent
        );

        // The whole point: a card that clears a field still verifies after a
        // round trip through a JSON store, so its confirmation can match.
        let payload = InteractionPayload::new("Clear the email?")
            .with_option(confirm_option())
            .with_option(decline_option())
            .with_review_entry(entry);
        let card = Interaction::from_spec(
            InteractionSpec::new(
                "clear",
                CaseRef::new("trip", "i1", CaseRevision(1)),
                InteractionKind::ReviewChanges,
                payload,
            ),
            InteractionId::nil(),
            AccountId::from("acct"),
            ConversationId::nil(),
            TurnId::nil(),
            now(),
        )
        .unwrap();
        assert!(card.verify_payload_hash().unwrap());
        let reloaded: Interaction =
            serde_json::from_str(&serde_json::to_string(&card).unwrap()).unwrap();
        assert_eq!(reloaded, card);
        assert!(reloaded.verify_payload_hash().unwrap());
    }

    #[test]
    fn action_classes_follow_the_stored_action() {
        assert_eq!(
            StoredInteractionAction::ConfirmCommands {
                command_refs: vec![]
            }
            .action_class(),
            ActionClass::ConfirmsCommands
        );
        assert_eq!(
            StoredInteractionAction::ApplyOperation {
                operation: OperationKey::from("x"),
                arguments: serde_json::Value::Null,
                freeform_argument: None,
            }
            .action_class(),
            ActionClass::AppliesOperation
        );
        for action in [
            StoredInteractionAction::DeclineCommands,
            StoredInteractionAction::Dismiss,
            StoredInteractionAction::SelectTarget {
                case_ref: CaseRef::new("w", "c", CaseRevision(1)),
            },
            StoredInteractionAction::ResolveClarification {
                answer_key: "k".into(),
            },
            StoredInteractionAction::Custom {
                key: "k".into(),
                payload: serde_json::Value::Null,
            },
        ] {
            assert_eq!(action.action_class(), ActionClass::NoCommands);
            assert!(!action.action_class().authorizes_commands());
        }
        assert!(InteractionKind::ConfirmCommand.authorizes_commands());
        assert!(InteractionKind::ExternalSignature.authorizes_commands());
        assert!(!InteractionKind::SelectTarget.authorizes_commands());
        assert_eq!(
            ConfirmationPolicy::ExplicitClick.interaction_kind(),
            Some(InteractionKind::ConfirmCommand)
        );
    }
}
