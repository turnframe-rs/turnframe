//! Turn input protocol (spec §9).
//!
//! A turn may carry free text, a structured interaction response, attachments
//! and a server-issued origin reference **at the same time**. Mutual exclusion
//! between text and a card reply is forbidden by design: a user may click
//! "Confirm" and ask a question in one message.

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::InvalidInputError;
use crate::hash::Digest;
use crate::ids::{
    AccountId, AttachmentId, CaseRevision, ConversationId, InteractionId, OptionId, OriginToken,
    TurnId, UserId,
};
use crate::locale::Locale;

/// The authenticated actor of a turn, supplied by the application's
/// authentication middleware. Trusted after authentication (spec §25.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorContext {
    /// Tenant the actor belongs to. Every lookup is scoped by it.
    pub account_id: AccountId,
    /// The user within the account.
    pub user_id: UserId,
    /// Application-defined roles.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Application-defined attributes (never shown to the model by the core).
    #[serde(default)]
    pub attributes: IndexMap<String, serde_json::Value>,
}

impl ActorContext {
    /// Builds an actor with no roles or attributes.
    #[must_use]
    pub fn new(account_id: impl Into<AccountId>, user_id: impl Into<UserId>) -> Self {
        Self {
            account_id: account_id.into(),
            user_id: user_id.into(),
            roles: Vec::new(),
            attributes: IndexMap::new(),
        }
    }

    /// Adds a role.
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Returns `true` when the actor holds `role`.
    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// Reference to an attachment uploaded with the turn. The content itself is
/// fetched by the application; the core only tracks identity and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentRef {
    /// Identifier the application assigned to the upload.
    pub attachment_id: AttachmentId,
    /// IANA media type.
    pub media_type: String,
    /// Original file name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Size in bytes, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Content digest, if computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

/// The bytes of one attachment, as the application holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentContent {
    /// IANA media type of what `bytes` are.
    ///
    /// Stated again rather than taken from the [`AttachmentRef`], because the
    /// application is the one that knows: a file uploaded as
    /// `application/octet-stream` may be a PNG, and only whoever stored it can
    /// say so.
    pub media_type: String,
    /// The raw bytes. Encoding for a provider happens downstream.
    pub bytes: Vec<u8>,
}

/// Why an attachment could not be fetched.
///
/// None of these ends a turn. A file the model cannot be shown is a turn that
/// answers with less, not a turn that fails: the user has just done the work of
/// taking a photograph, and losing their question as well is the worse outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AttachmentError {
    /// The application no longer holds the bytes.
    ///
    /// Ordinary rather than exceptional: an application may keep an attachment
    /// for one turn and never write it down, which is a legitimate product
    /// decision and the reason this is a port rather than a store.
    #[error("attachment {attachment_id} is no longer held")]
    Gone {
        /// The attachment.
        attachment_id: AttachmentId,
    },
    /// The actor may not read it.
    #[error("attachment {attachment_id} is not readable by this actor")]
    Unauthorized {
        /// The attachment.
        attachment_id: AttachmentId,
    },
    /// Fetching timed out.
    #[error("fetching attachment {attachment_id} timed out")]
    Timeout {
        /// The attachment.
        attachment_id: AttachmentId,
    },
    /// Anything else the application wants named.
    #[error("attachment {attachment_id} failed: {code}")]
    Other {
        /// The attachment.
        attachment_id: AttachmentId,
        /// Stable code, for a dashboard rather than for a user.
        code: String,
    },
}

/// Where the bytes of a turn's attachments come from.
///
/// # Why this is a port and not a store
///
/// [`AttachmentRef`] carries identity and metadata and says plainly that the
/// content is fetched by the application. That is deliberate: where a file
/// lives, how long it lives and who may read it are product decisions, and at
/// least one adopter's answer is that the bytes exist for one turn and are never
/// written down. A library that stored them would be wrong for that deployment
/// and could not be made right by configuration.
///
/// # What it is for
///
/// Attachments already travel end to end at the *plan* level: a turn carries
/// them, the schema offers `attachment_extraction` as evidence, and an act can
/// name a file so an application's own extractor fetches the bytes and rewrites
/// the command — which is how a document's numbers reach a record without
/// passing through a model at all. That half needs nothing from here.
///
/// The other half is the model **seeing** the file. Without it a turn carrying a
/// photograph reaches the model as a sentence mentioning one, so a domain
/// extractor that returns nothing — a receipt photographed at an angle, a scan
/// of a scan, a document that is not what the prompt asks for — has no fallback
/// but "I cannot read this", on the turn where the user has just done the work
/// of taking the picture.
///
/// # What the runtime does with a failure
///
/// Nothing that costs the turn. The file is absent from the request, the user is
/// told which ones were left out, and the writing stage is told too, so the
/// prose cannot answer about a document nobody looked at.
#[async_trait::async_trait]
pub trait AttachmentSource: Send + Sync {
    /// The bytes of one attachment of `turn_id`.
    ///
    /// The whole [`AttachmentRef`] is passed rather than only its identifier,
    /// because an implementation usually wants the media type or the size it
    /// already declared, and asking it to look them up again would be the
    /// library making a call cheaper for itself.
    async fn fetch(
        &self,
        turn_id: &TurnId,
        attachment: &AttachmentRef,
    ) -> Result<AttachmentContent, AttachmentError>;
}

/// A server-validated origin reference (spec §12.4): the UI surface that knows
/// the exact record the user is looking at names it, so the model does not have
/// to rediscover it from prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OriginRef {
    /// Opaque token issued by the server for the record.
    pub origin_token: OriginToken,
    /// Optional signature the runtime verifies before trusting the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Which UI surface produced the token (e.g. `"trip_detail"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
}

/// A structured reply to a persisted interaction (spec §9).
///
/// The client sends identifiers only; the server derives the meaning from the
/// stored option (I7). `freeform_input` is accepted only when the stored option
/// permits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractionResponse {
    /// The interaction being answered.
    pub interaction_id: InteractionId,
    /// The stored option chosen.
    pub option_id: OptionId,
    /// Case revision the card was rendered against.
    pub expected_case_revision: CaseRevision,
    /// Free-form value; only valid when the stored option explicitly permits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform_input: Option<String>,
}

/// One user turn as accepted by the runtime (spec §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInput {
    /// Identifier of this turn.
    pub turn_id: TurnId,
    /// Conversation the turn belongs to.
    pub conversation_id: ConversationId,
    /// Authenticated actor.
    pub actor: ActorContext,
    /// Free text, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Structured card reply, if any. May coexist with `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_response: Option<InteractionResponse>,
    /// Attachments supplied with the turn.
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    /// Server-validated origin, if the UI supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<OriginRef>,
    /// Locale of the user.
    pub locale: Locale,
    /// The effort this turn runs at, forced by the application; `None` takes the
    /// configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<crate::effort::Effort>,
}

/// Size limits on one turn's input, checked before anything is interpreted.
///
/// Both are `None` by default, which means unlimited. What counts as an
/// oversized message depends on the product — a chat box and a document upload
/// are not the same thing — so the library ships no answer and refuses nothing
/// until a deployment says what it wants refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnLimits {
    /// Maximum length of the user's text in bytes, or `None` for no limit.
    pub max_text_bytes: Option<usize>,
    /// Maximum number of attachments, or `None` for no limit.
    pub max_attachments: Option<usize>,
}

impl TurnLimits {
    /// No limits at all, which is what ships.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_text_bytes: None,
            max_attachments: None,
        }
    }

    /// Returns a copy with another text limit.
    #[must_use]
    pub const fn with_max_text_bytes(mut self, max_text_bytes: Option<usize>) -> Self {
        self.max_text_bytes = max_text_bytes;
        self
    }

    /// Returns a copy with another attachment limit.
    #[must_use]
    pub const fn with_max_attachments(mut self, max_attachments: Option<usize>) -> Self {
        self.max_attachments = max_attachments;
        self
    }
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self::conservative()
    }
}

impl TurnInput {
    /// Checks the structural rules of a turn against [`TurnLimits::conservative`].
    ///
    /// See [`Self::validate_shape_within`].
    pub fn validate_shape(&self) -> Result<(), InvalidInputError> {
        self.validate_shape_within(&TurnLimits::conservative())
    }

    /// Checks the structural rules of a turn: at least one of text,
    /// interaction response or attachment must be present, the locale and the
    /// account must be non-empty, and the text and attachments must fit
    /// `limits`.
    ///
    /// Text and interaction response may coexist; this never rejects that.
    pub fn validate_shape_within(&self, limits: &TurnLimits) -> Result<(), InvalidInputError> {
        if !self.has_text() && self.interaction_response.is_none() && self.attachments.is_empty() {
            return Err(InvalidInputError::EmptyTurn);
        }
        if self.locale.as_str().trim().is_empty() {
            return Err(InvalidInputError::EmptyLocale);
        }
        if self.actor.account_id.is_empty() {
            return Err(InvalidInputError::EmptyAccount);
        }
        if let Some(max_bytes) = limits.max_text_bytes
            && self.text.as_ref().is_some_and(|t| t.len() > max_bytes)
        {
            return Err(InvalidInputError::TextTooLong { max_bytes });
        }
        if let Some(max) = limits.max_attachments
            && self.attachments.len() > max
        {
            return Err(InvalidInputError::TooManyAttachments { max });
        }
        Ok(())
    }

    /// Returns `true` when the turn carries non-blank text.
    #[must_use]
    pub fn has_text(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.trim().is_empty())
    }

    /// Returns `true` when the turn is a pure card click: an interaction
    /// response with no text and no attachments. Such turns need no model call
    /// (spec §9).
    #[must_use]
    pub fn is_button_only(&self) -> bool {
        self.interaction_response.is_some() && !self.has_text() && self.attachments.is_empty()
    }

    /// Convenience accessor for the account.
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.actor.account_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TurnInput {
        TurnInput {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            actor: ActorContext::new("acct", "user"),
            text: None,
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("it-IT"),
            effort: None,
        }
    }

    #[test]
    fn empty_turn_is_rejected() {
        assert_eq!(base().validate_shape(), Err(InvalidInputError::EmptyTurn));
        let mut blank = base();
        blank.text = Some("   ".into());
        assert_eq!(blank.validate_shape(), Err(InvalidInputError::EmptyTurn));
    }

    #[test]
    fn text_and_interaction_response_coexist() {
        let mut turn = base();
        turn.text = Some("confirm and tell me why".into());
        turn.interaction_response = Some(InteractionResponse {
            interaction_id: InteractionId::nil(),
            option_id: OptionId::from("confirm"),
            expected_case_revision: CaseRevision(12),
            freeform_input: None,
        });
        assert_eq!(turn.validate_shape(), Ok(()));
        assert!(!turn.is_button_only());
        turn.text = None;
        assert!(turn.is_button_only());
    }

    #[test]
    fn limits_are_enforced_where_they_are_declared() {
        let mut long = base();
        long.text = Some("x".repeat(64));
        assert_eq!(
            long.validate_shape_within(&TurnLimits::conservative().with_max_text_bytes(Some(32))),
            Err(InvalidInputError::TextTooLong { max_bytes: 32 })
        );
        assert_eq!(long.validate_shape(), Ok(()));
        let mut many = base();
        many.text = Some("hi".into());
        many.attachments = (0..3)
            .map(|n| AttachmentRef {
                attachment_id: AttachmentId::from(format!("a{n}")),
                media_type: "application/pdf".into(),
                filename: None,
                size_bytes: None,
                digest: None,
            })
            .collect();
        assert_eq!(
            many.validate_shape_within(&TurnLimits::conservative().with_max_attachments(Some(2))),
            Err(InvalidInputError::TooManyAttachments { max: 2 })
        );
        assert_eq!(many.validate_shape(), Ok(()));
    }

    #[test]
    fn client_payload_rejects_unknown_fields() {
        let json = r#"{"interaction_id":"00000000-0000-0000-0000-000000000000","option_id":"a","expected_case_revision":1,"value":"evil"}"#;
        assert!(serde_json::from_str::<InteractionResponse>(json).is_err());
    }
}
