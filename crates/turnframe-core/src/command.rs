//! Typed command envelopes, origins, risk and confirmation policy (spec §14).
//!
//! # Why there is no model-proposal origin
//!
//! [`CommandOrigin`] deliberately has no variant for "the model proposed it".
//! A model output is a proposal (I9); it becomes a command only after evidence
//! validation, target resolution and reduction. When that pipeline produces a
//! low-risk command straight from the user's words, the origin is
//! [`CommandOrigin::DirectSafeUserAct`] carrying the digest of the *validated*
//! evidence, so the authority is the user's text, not the model. Anything more
//! consequential needs a server-issued origin: a confirmed interaction, an
//! internal policy, or a verified external callback (I12).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::case::{CaseKey, CaseRef};
use crate::hash::{Digest, HashError, canonical_digest, derive_uuid};
use crate::ids::{AccountId, BatchId, CommandId, InteractionId, TurnId};
use crate::interaction::{ActionClass, InteractionKind};
use crate::turn::ActorContext;
use crate::understanding::ActId;

/// How a user's answer to an interaction reached the server (spec §15.7).
///
/// The channel is part of the authorization record: a "yes" the interpreter
/// inferred from prose is not the same authority as a click, and I20 requires
/// the replay to say which one happened.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionChannel {
    /// The client posted the stored option id (a click on the card).
    Click,
    /// Understanding read the option from typed text. This channel may never
    /// authorize anything above [`RiskClass::ReversibleLowRisk`].
    ModelInterpreted,
}

impl ResolutionChannel {
    /// Every channel, ordered from strongest to weakest authority.
    pub const ALL: [Self; 2] = [Self::Click, Self::ModelInterpreted];

    /// Returns `true` for a channel the user drove deterministically: a click, never
    /// a reading.
    #[must_use]
    pub fn is_deterministic(self) -> bool {
        matches!(self, Self::Click)
    }
}

/// Who or what authorized a command (spec §14.2).
///
/// The enum grows as new authorization sources appear, so downstream matches
/// need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandOrigin {
    /// A low-risk act grounded directly in validated user evidence.
    DirectSafeUserAct {
        /// Digest of the validated evidence set.
        evidence_digest: Digest,
    },
    /// A stored option of a persisted interaction was chosen.
    ///
    /// The three qualifying fields answer *what* was confirmed: the shape of
    /// the card, what its chosen option authorizes, and how the answer arrived.
    /// Without them a click on a "which trip did you mean?" card would be
    /// indistinguishable from a qualified signature (I12).
    ConfirmedInteraction {
        /// The interaction.
        interaction_id: InteractionId,
        /// Hash of the payload the user saw.
        payload_hash: Digest,
        /// Shape of the card that was answered.
        interaction_kind: InteractionKind,
        /// What the chosen option authorizes.
        action_class: ActionClass,
        /// How the answer reached the server.
        channel: ResolutionChannel,
    },
    /// A server-side policy decided the command (e.g. automatic follow-up, or
    /// a qualified professional's review recorded out of band).
    InternalPolicy {
        /// Key of the policy.
        policy_key: String,
    },
    /// An external system called back.
    ExternalCallback {
        /// Callback identifier.
        callback_id: String,
        /// Whether the signature was verified. Unverified callbacks are untrusted.
        signature_verified: bool,
    },
}

impl CommandOrigin {
    /// Returns `true` when the origin may authorize commands above
    /// [`RiskClass::ReversibleLowRisk`] (I12).
    ///
    /// A confirmed interaction is trusted only when the chosen option actually
    /// authorizes commands and the answer did not come from an inference: a
    /// dismissed card and a model-interpreted "yes" are both untrusted.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        match self {
            Self::DirectSafeUserAct { .. } => false,
            Self::ConfirmedInteraction {
                action_class,
                channel,
                ..
            } => action_class.authorizes_commands() && channel.is_deterministic(),
            Self::InternalPolicy { .. } => true,
            Self::ExternalCallback {
                signature_verified, ..
            } => *signature_verified,
        }
    }

    /// The interaction kind behind a confirmed origin, when the option
    /// authorizes commands.
    #[must_use]
    fn confirming_kind(&self) -> Option<InteractionKind> {
        match self {
            Self::ConfirmedInteraction {
                interaction_kind,
                action_class,
                channel,
                ..
            } if action_class.authorizes_commands() && channel.is_deterministic() => {
                Some(*interaction_kind)
            }
            _ => None,
        }
    }

    /// Returns `true` for an external callback whose signature was verified.
    #[must_use]
    fn is_verified_callback(&self) -> bool {
        matches!(
            self,
            Self::ExternalCallback {
                signature_verified: true,
                ..
            }
        )
    }

    /// Returns `true` when this origin is the *specific* authorization
    /// [`ConfirmationPolicy`] demands (spec §14.3, §15.7).
    ///
    /// The mapping is deliberately narrow, because a confirmation is a
    /// statement about one act by one authority:
    ///
    /// | Policy | Accepted origin |
    /// | --- | --- |
    /// | `None` | any |
    /// | `ReviewCard`, `ExplicitClick` | a `ConfirmCommand` or `ReviewChanges` card whose chosen option authorizes commands |
    /// | `Reauthentication` | a `Reauthenticate` card, or a verified external callback |
    /// | `QualifiedSignature` | an `ExternalSignature` card, or a verified external callback |
    /// | `HumanProfessionalReview` | an internal policy or a verified external callback — never the end user's own click |
    ///
    /// A [`ResolutionChannel::ModelInterpreted`] answer satisfies no policy but
    /// [`ConfirmationPolicy::None`].
    #[must_use]
    pub fn satisfies_confirmation(&self, confirmation: ConfirmationPolicy) -> bool {
        use ConfirmationPolicy as Policy;
        use InteractionKind as Kind;
        match confirmation {
            Policy::None => true,
            Policy::ReviewCard | Policy::ExplicitClick => matches!(
                self.confirming_kind(),
                Some(Kind::ConfirmCommand | Kind::ReviewChanges)
            ),
            Policy::Reauthentication => {
                self.confirming_kind() == Some(Kind::Reauthenticate) || self.is_verified_callback()
            }
            Policy::QualifiedSignature => {
                self.confirming_kind() == Some(Kind::ExternalSignature)
                    || self.is_verified_callback()
            }
            Policy::HumanProfessionalReview => {
                matches!(self, Self::InternalPolicy { .. }) || self.is_verified_callback()
            }
        }
    }
}

/// Risk class of a command, ordered from harmless to regulated (spec §14.3).
///
/// Deliberately exhaustive: the ladder is a closed, ordered vocabulary and
/// callers compare against it rather than match it open-endedly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// No mutation.
    ReadOnly,
    /// Easily reversible, low impact.
    ReversibleLowRisk,
    /// Changes sensitive data.
    SensitiveDataChange,
    /// Deletes or destroys data.
    Destructive,
    /// Cannot be undone.
    Irreversible,
    /// An external effect another party decides (an airline, a bank, a regulator).
    ExternalRegulated,
}

impl RiskClass {
    /// The class assumed when nothing declares one: [`Self::Irreversible`].
    ///
    /// Used as the serde default wherever an unannotated record would
    /// otherwise be read as harmless.
    #[must_use]
    pub const fn conservative() -> Self {
        Self::Irreversible
    }

    /// Returns `true` when the class needs a trusted origin on its own,
    /// regardless of the confirmation policy (I12).
    #[must_use]
    pub fn needs_trusted_origin(self) -> bool {
        self > Self::ReversibleLowRisk
    }
}

/// Confirmation a command requires before execution (spec §14.3).
///
/// New confirmation kinds are expected, so downstream matches need a wildcard
/// arm.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfirmationPolicy {
    /// No confirmation.
    None,
    /// The user must review a diff card.
    ReviewCard,
    /// The user must click an explicit confirmation CTA.
    ExplicitClick,
    /// The user must re-authenticate.
    Reauthentication,
    /// A qualified electronic signature is required.
    QualifiedSignature,
    /// A qualified human other than the end user must review. No card the user
    /// can click satisfies it.
    HumanProfessionalReview,
}

impl ConfirmationPolicy {
    /// The interaction kind whose answer satisfies this confirmation, if the
    /// end user can give it at all.
    ///
    /// [`Self::HumanProfessionalReview`] returns `None`: the review is somebody
    /// else's, recorded through [`CommandOrigin::InternalPolicy`] or a verified
    /// [`CommandOrigin::ExternalCallback`], so offering the user a confirmation
    /// card would let them approve themselves.
    #[must_use]
    pub fn interaction_kind(self) -> Option<InteractionKind> {
        match self {
            Self::None | Self::HumanProfessionalReview => None,
            Self::ReviewCard => Some(InteractionKind::ReviewChanges),
            Self::ExplicitClick => Some(InteractionKind::ConfirmCommand),
            Self::Reauthentication => Some(InteractionKind::Reauthenticate),
            Self::QualifiedSignature => Some(InteractionKind::ExternalSignature),
        }
    }

    /// Returns `true` when only the server can supply the confirmation.
    #[must_use]
    pub fn is_server_side_only(self) -> bool {
        matches!(self, Self::HumanProfessionalReview)
    }
}

/// How commands are grouped for all-or-nothing execution (spec §13.4).
///
/// New grouping strategies are expected, so downstream matches need a wildcard
/// arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AtomicityScope {
    /// Each command commits alone.
    PerCommand,
    /// All commands on the same case commit together (the default for mutations).
    PerCase,
    /// A named group commits together.
    ExplicitGroup {
        /// Group name.
        group: String,
    },
    /// The command starts an external saga tracked through the outbox.
    ExternalSaga {
        /// Saga name.
        saga: String,
    },
}

impl AtomicityScope {
    /// Stable snake-case name of the variant, as it appears in JSON.
    #[must_use]
    pub fn discriminant(&self) -> &'static str {
        match self {
            Self::PerCommand => "per_command",
            Self::PerCase => "per_case",
            Self::ExplicitGroup { .. } => "explicit_group",
            Self::ExternalSaga { .. } => "external_saga",
        }
    }

    /// The group or saga name, for the named scopes.
    #[must_use]
    pub fn group_name(&self) -> Option<&str> {
        match self {
            Self::PerCommand | Self::PerCase => None,
            Self::ExplicitGroup { group } => Some(group),
            Self::ExternalSaga { saga } => Some(saga),
        }
    }
}

/// How the assistant may talk about the outcome of a command (spec §17.2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMode {
    /// Only the server-rendered receipt may state the outcome.
    ServerReceiptOnly,
    /// The narrator may paraphrase, citing event ids.
    EventReferencedParaphrase,
    /// The narrator may explain freely (read-only or trivial outcomes).
    FreeExplanation,
}

/// The policy attached to a command (spec §14.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CommandPolicy {
    /// Risk class.
    pub risk: RiskClass,
    /// Required confirmation.
    pub confirmation: ConfirmationPolicy,
    /// Grouping for execution.
    pub atomicity: AtomicityScope,
    /// Claim mode for receipts and narration.
    pub claim_mode: ClaimMode,
}

impl CommandPolicy {
    /// The policy applied to anything unknown: irreversible, explicit click,
    /// per-case atomicity, server receipts only.
    #[must_use]
    pub fn conservative() -> Self {
        Self {
            risk: RiskClass::Irreversible,
            confirmation: ConfirmationPolicy::ExplicitClick,
            atomicity: AtomicityScope::PerCase,
            claim_mode: ClaimMode::ServerReceiptOnly,
        }
    }

    /// Policy for commands that mutate nothing.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            risk: RiskClass::ReadOnly,
            confirmation: ConfirmationPolicy::None,
            atomicity: AtomicityScope::PerCommand,
            claim_mode: ClaimMode::FreeExplanation,
        }
    }

    /// Policy for reversible low-risk edits applied directly from validated
    /// user evidence (e.g. setting a draft field).
    #[must_use]
    pub fn low_risk() -> Self {
        Self {
            risk: RiskClass::ReversibleLowRisk,
            confirmation: ConfirmationPolicy::None,
            atomicity: AtomicityScope::PerCase,
            claim_mode: ClaimMode::EventReferencedParaphrase,
        }
    }

    /// Returns `true` when the policy demands a trusted origin (I12): any risk
    /// above [`RiskClass::ReversibleLowRisk`] or any confirmation other than
    /// [`ConfirmationPolicy::None`].
    #[must_use]
    pub fn requires_trusted_origin(&self) -> bool {
        self.risk > RiskClass::ReversibleLowRisk || self.confirmation != ConfirmationPolicy::None
    }
}

impl Default for CommandPolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Pure check of I12: does `origin` satisfy `policy`?
///
/// Two independent gates, both of which must pass:
///
/// 1. **Risk.** Anything above [`RiskClass::ReversibleLowRisk`] needs a trusted
///    origin ([`CommandOrigin::is_trusted`]).
/// 2. **Confirmation.** The origin must be the specific authorization
///    `policy.confirmation` names
///    ([`CommandOrigin::satisfies_confirmation`]) — a click on a target
///    selection card is not a confirmation of the command it disambiguates.
///
/// # Examples
///
/// ```
/// use turnframe_core::prelude::*;
///
/// let picked_a_case = CommandOrigin::ConfirmedInteraction {
///     interaction_id: InteractionId::nil(),
///     payload_hash: Digest::of_bytes(b"payload"),
///     interaction_kind: InteractionKind::SelectTarget,
///     action_class: ActionClass::NoCommands,
///     channel: ResolutionChannel::Click,
/// };
/// let confirmed_the_send = CommandOrigin::ConfirmedInteraction {
///     interaction_id: InteractionId::nil(),
///     payload_hash: Digest::of_bytes(b"payload"),
///     interaction_kind: InteractionKind::ConfirmCommand,
///     action_class: ActionClass::ConfirmsCommands,
///     channel: ResolutionChannel::Click,
/// };
/// let send = CommandPolicy::conservative();
/// assert!(!origin_satisfies(&picked_a_case, &send));
/// assert!(origin_satisfies(&confirmed_the_send, &send));
/// ```
#[must_use]
pub fn origin_satisfies(origin: &CommandOrigin, policy: &CommandPolicy) -> bool {
    if policy.risk.needs_trusted_origin() && !origin.is_trusted() {
        return false;
    }
    origin.satisfies_confirmation(policy.confirmation)
}

/// Domain-separation prefix of [`CommandId::derive`].
const COMMAND_ID_DOMAIN: &str = "turnframe.command_id.v2";

/// Domain-separation prefix of [`BatchId::derive`].
const BATCH_ID_DOMAIN: &str = "turnframe.batch_id.v1";

impl CommandId {
    /// Derives the identifier of the `position`-th command compiled for `act` of
    /// `turn_id`.
    ///
    /// Shipped reducers **must** use this instead of [`CommandId::new`]: the
    /// identifier is part of the [`ReductionPlan`](crate::reduce::ReductionPlan)
    /// hash, so a random one would make two reductions of the same inputs
    /// disagree and defeat replay (I20).
    #[must_use]
    pub fn derive(turn_id: &TurnId, act: ActId, position: usize) -> Self {
        Self(derive_uuid(
            COMMAND_ID_DOMAIN,
            &[
                &turn_id.to_string(),
                &act.to_string(),
                &position.to_string(),
            ],
        ))
    }
}

impl BatchId {
    /// Derives the identifier of the batch that groups the commands of
    /// `case_key` under `scope` within `turn_id`.
    ///
    /// Deterministic for the same reason as [`CommandId::derive`].
    #[must_use]
    pub fn derive(turn_id: &TurnId, case_key: &CaseKey, scope: &AtomicityScope) -> Self {
        Self(derive_uuid(
            BATCH_ID_DOMAIN,
            &[
                &turn_id.to_string(),
                case_key.workflow.as_str(),
                case_key.case_id.as_str(),
                scope.discriminant(),
                scope.group_name().unwrap_or(""),
            ],
        ))
    }
}

/// Stable idempotency key of a command (I14).
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct IdempotencyKey(pub String);

/// Domain-separation prefix of the idempotency derivation. Bump when the
/// derivation input changes.
const IDEMPOTENCY_DOMAIN: &str = "turnframe.idempotency.v1";

#[derive(Serialize)]
struct IdempotencyInput<'a> {
    domain: &'static str,
    account: &'a AccountId,
    turn_id: &'a TurnId,
    case_ref: &'a CaseRef,
    origin: &'a CommandOrigin,
    command: &'a serde_json::Value,
}

impl IdempotencyKey {
    /// Derives the key as `blake3(canonical_json({domain, account, turn_id,
    /// case_ref, origin, command}))`.
    ///
    /// The key is stable for the same inputs across processes and library
    /// versions that share the domain prefix. It changes when any input changes,
    /// including the expected revision inside `case_ref`: a command re-planned
    /// against a newer revision is a new command, while a crash-recovery replay
    /// of the same turn reproduces the same key.
    ///
    /// # Examples
    ///
    /// ```
    /// use turnframe_core::prelude::*;
    /// use serde_json::json;
    ///
    /// let account = AccountId::from("acct");
    /// let turn = TurnId::nil();
    /// let case_ref = CaseRef::new("trip", "i1", CaseRevision(3));
    /// let origin = CommandOrigin::DirectSafeUserAct {
    ///     evidence_digest: Digest::of_bytes(b"evidence"),
    /// };
    /// let command = json!({ "set_name": { "value": "Lisbon" } });
    /// let key = IdempotencyKey::derive(&account, &turn, &case_ref, &origin, &command)?;
    /// // The same turn replayed after a crash derives the same key ...
    /// assert_eq!(
    ///     key,
    ///     IdempotencyKey::derive(&account, &turn, &case_ref, &origin, &command)?
    /// );
    /// // ... but a command planned against a newer revision is a new command.
    /// let newer = case_ref.with_revision(CaseRevision(4));
    /// assert_ne!(
    ///     key,
    ///     IdempotencyKey::derive(&account, &turn, &newer, &origin, &command)?
    /// );
    /// # Ok::<(), turnframe_core::hash::HashError>(())
    /// ```
    pub fn derive(
        account: &AccountId,
        turn_id: &TurnId,
        case_ref: &CaseRef,
        origin: &CommandOrigin,
        command: &serde_json::Value,
    ) -> Result<Self, HashError> {
        let input = IdempotencyInput {
            domain: IDEMPOTENCY_DOMAIN,
            account,
            turn_id,
            case_ref,
            origin,
            command,
        };
        canonical_digest(&input).map(|digest| Self(digest.0))
    }

    /// Wraps an externally supplied key (e.g. from a callback).
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A typed command with everything the executor and the journal need (spec §14.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope<C> {
    /// Identifier of this envelope.
    pub command_id: CommandId,
    /// Turn that produced it.
    pub turn_id: TurnId,
    /// Actor on whose behalf it runs.
    pub actor: ActorContext,
    /// Target case and expected revision.
    pub case_ref: CaseRef,
    /// Idempotency key (I14).
    pub idempotency_key: IdempotencyKey,
    /// Authorizing origin.
    pub origin: CommandOrigin,
    /// The domain command.
    pub command: C,
}

impl<C> CommandEnvelope<C> {
    /// Account the command runs in.
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.actor.account_id
    }

    /// Transforms the command payload, keeping every other field.
    pub fn try_map_command<D, E>(
        self,
        f: impl FnOnce(C) -> Result<D, E>,
    ) -> Result<CommandEnvelope<D>, E> {
        Ok(CommandEnvelope {
            command_id: self.command_id,
            turn_id: self.turn_id,
            actor: self.actor,
            case_ref: self.case_ref,
            idempotency_key: self.idempotency_key,
            origin: self.origin,
            command: f(self.command)?,
        })
    }
}

/// A group of envelopes executed under one atomicity scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandBatch<C> {
    /// Identifier of the batch.
    pub batch_id: BatchId,
    /// Scope under which the envelopes commit.
    pub scope: AtomicityScope,
    /// The envelopes, in execution order.
    pub envelopes: Vec<CommandEnvelope<C>>,
}

impl<C> CommandBatch<C> {
    /// Number of envelopes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.envelopes.len()
    }

    /// Returns `true` when the batch has no envelopes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.envelopes.is_empty()
    }

    /// Returns `true` when every envelope targets the same case (required for
    /// [`AtomicityScope::PerCase`]).
    #[must_use]
    pub fn is_single_case(&self) -> bool {
        match self.envelopes.split_first() {
            None => true,
            Some((first, rest)) => rest.iter().all(|e| e.case_ref.same_case(&first.case_ref)),
        }
    }

    /// Transforms every command payload, keeping identifiers and scope.
    pub fn try_map<D, E>(self, mut f: impl FnMut(C) -> Result<D, E>) -> Result<CommandBatch<D>, E> {
        let mut envelopes = Vec::with_capacity(self.envelopes.len());
        for envelope in self.envelopes {
            envelopes.push(envelope.try_map_command(&mut f)?);
        }
        Ok(CommandBatch {
            batch_id: self.batch_id,
            scope: self.scope,
            envelopes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CaseRevision;
    use crate::understanding::UnitId;

    fn card(kind: InteractionKind, action_class: ActionClass) -> CommandOrigin {
        CommandOrigin::ConfirmedInteraction {
            interaction_id: InteractionId::nil(),
            payload_hash: Digest::of_bytes(b"p"),
            interaction_kind: kind,
            action_class,
            channel: ResolutionChannel::Click,
        }
    }

    fn confirmed() -> CommandOrigin {
        card(
            InteractionKind::ConfirmCommand,
            ActionClass::ConfirmsCommands,
        )
    }

    fn direct() -> CommandOrigin {
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: Digest::of_bytes(b"e"),
        }
    }

    fn internal() -> CommandOrigin {
        CommandOrigin::InternalPolicy {
            policy_key: "auto".into(),
        }
    }

    fn callback(signature_verified: bool) -> CommandOrigin {
        CommandOrigin::ExternalCallback {
            callback_id: "c".into(),
            signature_verified,
        }
    }

    fn with_confirmation(confirmation: ConfirmationPolicy) -> CommandPolicy {
        CommandPolicy {
            confirmation,
            ..CommandPolicy::conservative()
        }
    }

    #[test]
    fn conservative_is_default_and_requires_trust() {
        assert_eq!(CommandPolicy::default(), CommandPolicy::conservative());
        assert!(CommandPolicy::conservative().requires_trusted_origin());
        assert!(!CommandPolicy::low_risk().requires_trusted_origin());
        assert!(!CommandPolicy::read_only().requires_trusted_origin());
    }

    #[test]
    fn origin_satisfies_rules() {
        assert!(origin_satisfies(&direct(), &CommandPolicy::low_risk()));
        assert!(!origin_satisfies(&direct(), &CommandPolicy::conservative()));
        assert!(origin_satisfies(
            &confirmed(),
            &CommandPolicy::conservative()
        ));
        let mut low_with_review = CommandPolicy::low_risk();
        low_with_review.confirmation = ConfirmationPolicy::ReviewCard;
        assert!(!origin_satisfies(&direct(), &low_with_review));
        assert!(!origin_satisfies(
            &callback(false),
            &CommandPolicy::conservative()
        ));
        // A verified callback stands in for the user's click only where the
        // policy says an out-of-band authority may answer.
        assert!(!origin_satisfies(
            &callback(true),
            &CommandPolicy::conservative()
        ));
        assert!(origin_satisfies(
            &callback(true),
            &with_confirmation(ConfirmationPolicy::QualifiedSignature)
        ));
        assert!(!origin_satisfies(
            &internal(),
            &CommandPolicy::conservative()
        ));
        assert!(origin_satisfies(
            &internal(),
            &with_confirmation(ConfirmationPolicy::HumanProfessionalReview)
        ));
    }

    #[test]
    fn answering_a_selection_card_confirms_nothing() {
        // The user picked which trip they meant; that is not a confirmation
        // of an irreversible command (CORE-SPEC-001).
        let selection = card(InteractionKind::SelectTarget, ActionClass::NoCommands);
        assert!(!selection.is_trusted());
        assert!(!origin_satisfies(
            &selection,
            &CommandPolicy::conservative()
        ));
        assert!(origin_satisfies(&selection, &CommandPolicy::low_risk()));
        let dismissed = card(InteractionKind::ConfirmCommand, ActionClass::NoCommands);
        assert!(!origin_satisfies(
            &dismissed,
            &CommandPolicy::conservative()
        ));
    }

    #[test]
    fn each_confirmation_policy_accepts_only_its_own_authority() {
        use ConfirmationPolicy as P;
        use InteractionKind as K;
        let cases: &[(P, &[K])] = &[
            (P::ReviewCard, &[K::ConfirmCommand, K::ReviewChanges]),
            (P::ExplicitClick, &[K::ConfirmCommand, K::ReviewChanges]),
            (P::Reauthentication, &[K::Reauthenticate]),
            (P::QualifiedSignature, &[K::ExternalSignature]),
            (P::HumanProfessionalReview, &[]),
        ];
        let every_kind = [
            K::Boolean,
            K::SingleSelect,
            K::MultiSelect,
            K::Freeform,
            K::ReviewChanges,
            K::ConfirmCommand,
            K::SelectTarget,
            K::ResolveValidationError,
            K::Reauthenticate,
            K::ExternalSignature,
        ];
        for (confirmation, accepted) in cases {
            let policy = with_confirmation(*confirmation);
            for kind in every_kind {
                let origin = card(kind, ActionClass::ConfirmsCommands);
                assert_eq!(
                    origin_satisfies(&origin, &policy),
                    accepted.contains(&kind),
                    "{confirmation:?} vs {kind:?}"
                );
            }
        }
    }

    #[test]
    fn human_professional_review_is_not_the_users_own_click() {
        let policy = with_confirmation(ConfirmationPolicy::HumanProfessionalReview);
        assert!(!origin_satisfies(&confirmed(), &policy));
        assert_eq!(
            ConfirmationPolicy::HumanProfessionalReview.interaction_kind(),
            None
        );
        assert!(ConfirmationPolicy::HumanProfessionalReview.is_server_side_only());
        assert!(origin_satisfies(&internal(), &policy));
        assert!(origin_satisfies(&callback(true), &policy));
        assert!(!origin_satisfies(&callback(false), &policy));
    }

    #[test]
    fn model_interpreted_answers_never_authorize_above_low_risk() {
        let interpreted = CommandOrigin::ConfirmedInteraction {
            interaction_id: InteractionId::nil(),
            payload_hash: Digest::of_bytes(b"p"),
            interaction_kind: InteractionKind::ConfirmCommand,
            action_class: ActionClass::ConfirmsCommands,
            channel: ResolutionChannel::ModelInterpreted,
        };
        assert!(!interpreted.is_trusted());
        assert!(!origin_satisfies(
            &interpreted,
            &CommandPolicy::conservative()
        ));
        assert!(origin_satisfies(&interpreted, &CommandPolicy::low_risk()));
        let mut low_but_confirmed = CommandPolicy::low_risk();
        low_but_confirmed.confirmation = ConfirmationPolicy::ExplicitClick;
        assert!(!origin_satisfies(&interpreted, &low_but_confirmed));
    }

    #[test]
    fn derived_ids_are_deterministic_and_positional() {
        let turn = TurnId::nil();
        let (first, second) = (ActId::new(UnitId(1), 1), ActId::new(UnitId(2), 1));
        let a = CommandId::derive(&turn, first, 0);
        assert_eq!(a, CommandId::derive(&turn, first, 0));
        assert_ne!(a, CommandId::derive(&turn, first, 1));
        assert_ne!(a, CommandId::derive(&turn, second, 0));
        assert_ne!(a, CommandId::derive(&TurnId::new(), first, 0));
        let key = CaseKey::new("trip", "i1");
        let b = BatchId::derive(&turn, &key, &AtomicityScope::PerCase);
        assert_eq!(b, BatchId::derive(&turn, &key, &AtomicityScope::PerCase));
        assert_ne!(b, BatchId::derive(&turn, &key, &AtomicityScope::PerCommand));
        assert_ne!(
            b,
            BatchId::derive(&turn, &CaseKey::new("trip", "i2"), &AtomicityScope::PerCase)
        );
        assert_ne!(
            BatchId::derive(
                &turn,
                &key,
                &AtomicityScope::ExplicitGroup { group: "a".into() }
            ),
            BatchId::derive(
                &turn,
                &key,
                &AtomicityScope::ExplicitGroup { group: "b".into() }
            )
        );
    }

    #[test]
    fn risk_ordering_matches_declaration() {
        assert!(RiskClass::ReadOnly < RiskClass::ReversibleLowRisk);
        assert!(RiskClass::Irreversible < RiskClass::ExternalRegulated);
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        let account = AccountId::from("acct");
        let turn = TurnId::nil();
        let case_ref = CaseRef::new("trip", "i1", CaseRevision(1));
        let cmd = serde_json::json!({"set_subject": {"value": "x"}});
        let a = IdempotencyKey::derive(&account, &turn, &case_ref, &direct(), &cmd).unwrap();
        let b = IdempotencyKey::derive(&account, &turn, &case_ref, &direct(), &cmd).unwrap();
        assert_eq!(a, b);
        let other_rev = case_ref.with_revision(CaseRevision(2));
        let c = IdempotencyKey::derive(&account, &turn, &other_rev, &direct(), &cmd).unwrap();
        assert_ne!(a, c);
    }
}
