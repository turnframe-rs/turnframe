//! Whether a command may run now, and if not, what would let it (spec §14.3).
//!
//! [`PolicySnapshot::decide`](turnframe_core::policy::PolicySnapshot::decide) in
//! core answers the narrow question: does this origin satisfy this policy?
//! [`PolicyEngine`] answers the question a turn actually asks, which has three
//! more inputs — the constraints the user put on the turn ("do not submit
//! anything yet"), the orchestration mode, and the configuration — and which has
//! to produce not just a verdict but the *card* that would change it.
//!
//! # Gates, in order
//!
//! 1. **Mode** (§11.4). A sandboxed run is not eligible for destructive,
//!    irreversible or externally regulated commands at all. No card helps.
//! 2. **Constraints** (§10.4). Evaluated in a fixed order so the same set of
//!    constraints always names the same blocker:
//!
//!    | Constraint | Blocks |
//!    | --- | --- |
//!    | [`DoNotSubmit`] | anything that leaves the system: [`RiskClass::ExternalRegulated`], or an [`AtomicityScope::ExternalSaga`] |
//!    | [`DoNotDelete`] | [`RiskClass::Destructive`] |
//!    | [`DraftOnly`] | anything above [`RiskClass::ReversibleLowRisk`] |
//!    | [`NoExternalEffects`] | [`RiskClass::ExternalRegulated`] |
//!    | [`AskBeforeApplying`] | nothing — it *raises* [`ConfirmationPolicy::None`] to [`ConfirmationPolicy::ExplicitClick`] |
//!    | [`ApplyOnlyIf`] | nothing here; the reducer turns it into a clarification |
//!
//! 3. **Policy** (§14.3, I12). The core snapshot decides, against the policy as
//!    the constraints left it.
//!
//! # An absent policy is a conservative policy
//!
//! [`PolicyRequest::policy`] is an `Option` because a domain that has not
//! classified a command is a real situation. It is treated as
//! [`CommandPolicy::conservative`]: irreversible, explicit click, per case,
//! server receipts only. Silence is never permission.
//!
//! [`DoNotSubmit`]: ConstraintKind::DoNotSubmit
//! [`DoNotDelete`]: ConstraintKind::DoNotDelete
//! [`DraftOnly`]: ConstraintKind::DraftOnly
//! [`NoExternalEffects`]: ConstraintKind::NoExternalEffects
//! [`AskBeforeApplying`]: ConstraintKind::AskBeforeApplying
//! [`ApplyOnlyIf`]: ConstraintKind::ApplyOnlyIf

use std::fmt;
use std::sync::Arc;

use turnframe_core::case::CaseRef;
use turnframe_core::command::{
    AtomicityScope, CommandOrigin, CommandPolicy, ConfirmationPolicy, RiskClass,
};
use turnframe_core::flow::ConfirmationSubject;
use turnframe_core::ids::OptionId;
use turnframe_core::interaction::{
    InteractionKind, InteractionOption, InteractionPayload, InteractionSpec, OptionStyle,
    ReviewDiffEntry, StoredInteractionAction, TextResolutionPolicy,
};
use turnframe_core::locale::LocalizedText;
use turnframe_core::policy::{PolicyDecision, PolicySnapshot};
use turnframe_core::reduce::CommandRef;
use turnframe_core::understanding::ConstraintKind;

use crate::config::{InteractionConfig, OrchestrationMode, OrchestratorConfig};

/// Option identifier of the confirming CTA on a card the engine builds.
pub const CONFIRM_OPTION_ID: &str = "confirm";
/// Option identifier of the declining CTA on a card the engine builds.
pub const DECLINE_OPTION_ID: &str = "decline";

/// Reason keys the engine adds to the ones in
/// [`turnframe_core::policy::reason`].
pub mod reason {
    /// A constraint the user placed on the turn blocks the command.
    pub const BLOCKED_BY_CONSTRAINT: &str = "turnframe.policy.blocked_by_constraint";
    /// The orchestration mode is not eligible for this risk class (§11.4).
    pub const BLOCKED_BY_MODE: &str = "turnframe.policy.blocked_by_mode";
}

/// One command as the policy engine sees it.
///
/// It carries no domain knowledge on purpose: everything the engine needs to
/// apply a constraint is already in the [`CommandPolicy`], so a domain cannot
/// accidentally exempt itself from "do not submit" by forgetting a flag.
#[derive(Debug, Clone)]
pub struct PolicyRequest<'a> {
    /// Which command the decision is about.
    pub command_ref: CommandRef,
    /// Stable key for the card the decision may require (e.g.
    /// `"confirm:acts[0]"`). Two requests with the same key describe one card.
    pub interaction_key: String,
    /// The case the command runs on, and the revision a card would bind to.
    pub case_ref: &'a CaseRef,
    /// The domain's policy. `None` means "unclassified", treated as
    /// [`CommandPolicy::conservative`].
    pub policy: Option<&'a CommandPolicy>,
    /// The origin the command would carry.
    pub origin: &'a CommandOrigin,
    /// The erased command, handed to the review diff builder.
    pub command: &'a serde_json::Value,
}

impl PolicyRequest<'_> {
    /// The policy actually applied: the domain's, or the conservative one.
    #[must_use]
    pub fn effective_policy(&self) -> CommandPolicy {
        self.policy
            .cloned()
            .unwrap_or_else(CommandPolicy::conservative)
    }
}

/// Builds the before/after lines of a [`InteractionKind::ReviewChanges`] card.
///
/// The engine cannot know what a domain command changes, so the diff is
/// injected. Returning an empty diff is allowed and safe: the engine then falls
/// back to a plain [`InteractionKind::ConfirmCommand`] card, which
/// [`ConfirmationPolicy::ReviewCard`] also accepts, rather than persisting a
/// review card with nothing to review.
pub trait ReviewDiffBuilder: Send + Sync {
    /// Lines to show for `request`.
    fn diff(&self, request: &PolicyRequest<'_>) -> Vec<ReviewDiffEntry>;
}

/// A diff builder that shows nothing, so review cards degrade to confirmations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoReviewDiff;

impl ReviewDiffBuilder for NoReviewDiff {
    fn diff(&self, _request: &PolicyRequest<'_>) -> Vec<ReviewDiffEntry> {
        Vec::new()
    }
}

impl<F> ReviewDiffBuilder for F
where
    F: Fn(&PolicyRequest<'_>) -> Vec<ReviewDiffEntry> + Send + Sync,
{
    fn diff(&self, request: &PolicyRequest<'_>) -> Vec<ReviewDiffEntry> {
        self(request)
    }
}

/// Server-authored copy for the cards the engine builds.
///
/// Every field is [`LocalizedText`], so an application replaces the English
/// defaults without touching the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfirmationCopy {
    /// Title of a [`InteractionKind::ReviewChanges`] card.
    pub review_title: LocalizedText,
    /// Title of a [`InteractionKind::ConfirmCommand`] card.
    pub confirm_title: LocalizedText,
    /// Title of a [`InteractionKind::Reauthenticate`] card.
    pub reauthenticate_title: LocalizedText,
    /// Title of a [`InteractionKind::ExternalSignature`] card.
    pub signature_title: LocalizedText,
    /// Label of the confirming CTA.
    pub confirm_label: LocalizedText,
    /// Label of the declining CTA.
    pub decline_label: LocalizedText,
}

impl ConfirmationCopy {
    /// The built-in copy: English, with Italian.
    #[must_use]
    pub fn standard() -> Self {
        crate::copy::ServerCopy::translated(Self::english(), "it", CONFIRMATION_ITALIAN)
    }

    /// English alone.
    #[must_use]
    pub fn english() -> Self {
        Self {
            review_title: LocalizedText::new("Review these changes"),
            confirm_title: LocalizedText::new("Confirm this action"),
            reauthenticate_title: LocalizedText::new("Confirm your identity"),
            signature_title: LocalizedText::new("Sign this document"),
            confirm_label: LocalizedText::new("Confirm"),
            decline_label: LocalizedText::new("Cancel"),
        }
    }

    fn title_for(&self, kind: InteractionKind) -> LocalizedText {
        match kind {
            InteractionKind::ReviewChanges => self.review_title.clone(),
            InteractionKind::Reauthenticate => self.reauthenticate_title.clone(),
            InteractionKind::ExternalSignature => self.signature_title.clone(),
            _ => self.confirm_title.clone(),
        }
    }
}

impl Default for ConfirmationCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(
    ConfirmationCopy,
    [
        review_title,
        confirm_title,
        reauthenticate_title,
        signature_title,
        confirm_label,
        decline_label
    ]
);

/// The built-in Italian of [`ConfirmationCopy`], by field.
const CONFIRMATION_ITALIAN: &[(&str, &str)] = &[
    ("review_title", "Controlla queste modifiche"),
    ("confirm_title", "Conferma questa operazione"),
    ("reauthenticate_title", "Conferma la tua identità"),
    ("signature_title", "Firma questo documento"),
    ("confirm_label", "Conferma"),
    ("decline_label", "Annulla"),
];

/// Why a command may not run, when no card the end user can click would help.
///
/// Serializable so a reducer can put it in the `details` of the
/// [`DomainRejection`](turnframe_core::error::DomainRejection) the user's client
/// receives: "blocked" is not an answer, "blocked because you said not to
/// submit" is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockReason {
    /// A constraint the user placed on the turn (§10.4).
    Constraint(ConstraintKind),
    /// The orchestration mode is not eligible for the risk class (§11.4).
    Mode {
        /// The refused class.
        risk: RiskClass,
    },
    /// The policy snapshot forbids the risk class outright.
    ForbiddenRiskClass {
        /// The refused class.
        risk: RiskClass,
    },
    /// Only a qualified human other than the end user may authorize this
    /// ([`ConfirmationPolicy::HumanProfessionalReview`]).
    HumanReview,
}

/// What the engine decided about one command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyOutcome {
    /// The command may execute with the origin it carries.
    Allowed {
        /// The recorded decision.
        decision: PolicyDecision,
    },
    /// The command needs a specific confirmation first; here is the card.
    ///
    /// The card is boxed because it is by far the largest thing an outcome can
    /// carry and most outcomes are not this one.
    NeedsConfirmation {
        /// The recorded decision.
        decision: PolicyDecision,
        /// The card to persist before anything executes.
        interaction: Box<InteractionSpec>,
    },
    /// The command may not execute, and no card the user can click changes that.
    Blocked {
        /// The recorded decision.
        decision: PolicyDecision,
        /// Why.
        reason: BlockReason,
    },
}

impl PolicyOutcome {
    /// The decision, whatever the outcome.
    #[must_use]
    pub fn decision(&self) -> &PolicyDecision {
        match self {
            Self::Allowed { decision }
            | Self::NeedsConfirmation { decision, .. }
            | Self::Blocked { decision, .. } => decision,
        }
    }

    /// Consumes the outcome and returns the decision.
    #[must_use]
    pub fn into_decision(self) -> PolicyDecision {
        match self {
            Self::Allowed { decision }
            | Self::NeedsConfirmation { decision, .. }
            | Self::Blocked { decision, .. } => decision,
        }
    }

    /// The card the outcome requires, when there is one.
    #[must_use]
    pub fn interaction(&self) -> Option<&InteractionSpec> {
        match self {
            Self::NeedsConfirmation { interaction, .. } => Some(interaction.as_ref()),
            _ => None,
        }
    }

    /// Returns `true` for [`Self::Allowed`].
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }
}

/// Applies constraints, mode and policy to one command, and builds the card
/// that would unblock it.
#[derive(Clone)]
pub struct PolicyEngine {
    mode: OrchestrationMode,
    interaction: InteractionConfig,
    copy: ConfirmationCopy,
    diff: Arc<dyn ReviewDiffBuilder>,
}

impl fmt::Debug for PolicyEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyEngine")
            .field("mode", &self.mode)
            .field("interaction", &self.interaction)
            .finish_non_exhaustive()
    }
}

impl PolicyEngine {
    /// Builds an engine from a configuration, with the built-in copy and no review
    /// diff.
    #[must_use]
    pub fn new(config: &OrchestratorConfig) -> Self {
        Self {
            mode: config.mode.clone(),
            interaction: config.interaction,
            copy: ConfirmationCopy::standard(),
            diff: Arc::new(NoReviewDiff),
        }
    }

    /// Replaces the copy on the cards the policy engine raises.
    ///
    /// # The shipped default is English, and only English
    ///
    /// Not a placeholder: it is real copy, it renders, and a deployment that
    /// never calls this ships it to every user in every locale. That is the
    /// failure worth naming here, because nothing goes wrong until a user in
    /// another language reads a sentence the rest of the product would never
    /// have written — which is how it was found.
    ///
    /// A [`LocalizedText`] carries a default plus one string per locale, so
    /// supplying your own is additive rather than a rewrite: start from
    /// `english()` and use
    /// [`LocalizedText::with`](turnframe_core::locale::LocalizedText::with) to
    /// add the languages you answer in.
    #[must_use]
    pub fn with_copy(mut self, copy: ConfirmationCopy) -> Self {
        self.copy = copy;
        self
    }

    /// Replaces the review diff builder.
    ///
    /// ```
    /// use turnframe_core::interaction::ReviewDiffEntry;
    /// use turnframe_runtime::config::OrchestratorConfig;
    /// use turnframe_runtime::policy::PolicyEngine;
    /// use std::sync::Arc;
    ///
    /// let engine = PolicyEngine::new(&OrchestratorConfig::conservative())
    ///     .with_review_diff(Arc::new(|_: &turnframe_runtime::policy::PolicyRequest<'_>| {
    ///         vec![ReviewDiffEntry::new("name", "Name")]
    ///     }));
    /// assert!(format!("{engine:?}").starts_with("PolicyEngine"));
    /// ```
    #[must_use]
    pub fn with_review_diff(mut self, diff: Arc<dyn ReviewDiffBuilder>) -> Self {
        self.diff = diff;
        self
    }

    /// The orchestration mode the engine gates on.
    #[must_use]
    pub fn mode(&self) -> &OrchestrationMode {
        &self.mode
    }

    /// The diff a review card would show for `request`.
    #[must_use]
    pub fn review_diff(&self, request: &PolicyRequest<'_>) -> Vec<ReviewDiffEntry> {
        self.diff.diff(request)
    }

    /// Decides one command against `snapshot` and the turn's `constraints`.
    ///
    /// See the module documentation for the order of the gates.
    #[must_use]
    pub fn decide(
        &self,
        snapshot: &PolicySnapshot,
        request: &PolicyRequest<'_>,
        constraints: &[ConstraintKind],
    ) -> PolicyOutcome {
        let declared = request.effective_policy();

        if !self.mode.allows_risk(declared.risk) {
            return PolicyOutcome::Blocked {
                decision: refusal(request, &declared, reason::BLOCKED_BY_MODE),
                reason: BlockReason::Mode {
                    risk: declared.risk,
                },
            };
        }

        if let Some(constraint) = blocking_constraint(&declared, constraints) {
            return PolicyOutcome::Blocked {
                decision: refusal(request, &declared, reason::BLOCKED_BY_CONSTRAINT),
                reason: BlockReason::Constraint(constraint),
            };
        }

        let effective = apply_ask_before_applying(declared, constraints);
        let decision = snapshot.decide(request.command_ref, &effective, request.origin);
        if decision.allowed {
            return PolicyOutcome::Allowed { decision };
        }
        if snapshot.is_risk_forbidden(effective.risk) {
            return PolicyOutcome::Blocked {
                decision,
                reason: BlockReason::ForbiddenRiskClass {
                    risk: effective.risk,
                },
            };
        }
        match self.confirmation_card(
            request.interaction_key.clone(),
            request.case_ref,
            &effective,
            vec![request.command_ref],
            self.diff.diff(request),
            // This path is the engine on its own, with no workflow in reach to
            // ask; the reducer's is the one that carries a domain's answer.
            None,
        ) {
            Some(interaction) => PolicyOutcome::NeedsConfirmation {
                decision,
                interaction: Box::new(interaction),
            },
            None => PolicyOutcome::Blocked {
                decision,
                reason: BlockReason::HumanReview,
            },
        }
    }

    /// The card whose answer satisfies `policy`, or `None` when the end user
    /// cannot supply the confirmation at all.
    ///
    /// `None` comes back for [`ConfirmationPolicy::None`] (nothing to confirm)
    /// and for [`ConfirmationPolicy::HumanProfessionalReview`] (offering the
    /// user a card would let them approve themselves).
    ///
    /// The card always carries a confirming CTA that authorizes exactly
    /// `command_refs` and a declining CTA, so refusing is always possible (I6).
    /// A [`InteractionKind::ReviewChanges`] card with an empty `diff` degrades
    /// to [`InteractionKind::ConfirmCommand`], which
    /// [`ConfirmationPolicy::ReviewCard`] accepts just as well.
    #[must_use]
    pub fn confirmation_card(
        &self,
        key: impl Into<String>,
        case_ref: &CaseRef,
        policy: &CommandPolicy,
        command_refs: Vec<CommandRef>,
        diff: Vec<ReviewDiffEntry>,
        subject: Option<ConfirmationSubject>,
    ) -> Option<InteractionSpec> {
        let mut kind = policy.confirmation.interaction_kind()?;
        if kind == InteractionKind::ReviewChanges && diff.is_empty() {
            kind = InteractionKind::ConfirmCommand;
        }
        // The engine knows a confirmation is needed; only the domain knows what
        // it is about. Where it says nothing this is the per-kind box exactly as
        // before, which is what a card with no subject has always been.
        let subject = subject.unwrap_or_default();
        let title = subject.title.unwrap_or_else(|| self.copy.title_for(kind));
        // Only the confirming option carries it. Declining is the end of the
        // matter by definition, so a turn that declines has nothing to go on
        // with — and the option that says "no" must never be the one that lets
        // a plan run.
        let confirm = InteractionOption::new(
            OptionId::from(CONFIRM_OPTION_ID),
            self.copy.confirm_label.clone(),
            StoredInteractionAction::ConfirmCommands { command_refs },
        )
        .with_style(OptionStyle::Primary);
        let mut payload = InteractionPayload::new(title)
            .with_option(confirm)
            .with_option(
                InteractionOption::new(
                    OptionId::from(DECLINE_OPTION_ID),
                    self.copy.decline_label.clone(),
                    StoredInteractionAction::DeclineCommands,
                )
                .with_style(OptionStyle::Danger),
            );
        if let Some(body) = subject.body {
            payload = payload.with_body(body);
        }
        if kind == InteractionKind::ReviewChanges {
            for entry in diff {
                payload = payload.with_review_entry(entry);
            }
        }
        let mut spec = InteractionSpec::new(key, case_ref.clone(), kind, payload)
            .with_confirms_risk(policy.risk)
            // Every kind this method builds authorizes commands, so typed text
            // may never resolve it (§15.7): an inferred "yes" is not consent.
            .with_text_resolution(TextResolutionPolicy::Never);
        if let Some(ttl) = self.interaction.default_ttl {
            spec = spec.expires_in(ttl);
        }
        if !self.interaction.confirmation_cards_bind_to_revision {
            spec = spec.revision_independent();
        }
        Some(spec)
    }

    /// The card that lets the user pick between ambiguous targets (§12.3).
    ///
    /// Returns `None` when there are fewer than two candidates — a selection
    /// with one answer is not a selection — or more than
    /// [`InteractionConfig::max_selection_candidates`], because truncating the
    /// list would let list order decide which cases the user may reach (I8).
    #[must_use]
    pub fn selection_card(
        &self,
        key: impl Into<String>,
        case_ref: &CaseRef,
        title: LocalizedText,
        cancel_label: LocalizedText,
        candidates: &[turnframe_core::target::TargetCandidate],
    ) -> Option<InteractionSpec> {
        if candidates.len() < 2 || candidates.len() > self.interaction.max_selection_candidates {
            return None;
        }
        let mut payload = InteractionPayload::new(title);
        for candidate in candidates {
            payload = payload.with_option(InteractionOption::new(
                OptionId::from(candidate.token.as_str()),
                LocalizedText::new(candidate.label.clone()),
                StoredInteractionAction::SelectTarget {
                    case_ref: candidate.case_ref.clone(),
                },
            ));
        }
        payload = payload.with_option(
            InteractionOption::new(
                OptionId::from(DECLINE_OPTION_ID),
                cancel_label,
                StoredInteractionAction::Dismiss,
            )
            .with_style(OptionStyle::Danger),
        );
        let mut spec = InteractionSpec::new(
            key,
            case_ref.clone(),
            InteractionKind::SelectTarget,
            payload,
        )
        // Picking a case authorizes nothing, which is exactly why it is safe to
        // let the answer arrive as low risk.
        .with_confirms_risk(RiskClass::ReversibleLowRisk)
        // "Which of these did you mean?" does not stop being a fair question
        // when one of the candidates moves on, so the card outlives a revision
        // change; the act it unblocks is re-planned against the state of the
        // day it is answered.
        .revision_independent();
        if !self.interaction.selection_cards_block_the_case {
            spec = spec.non_blocking();
        }
        if let Some(ttl) = self.interaction.default_ttl {
            spec = spec.expires_in(ttl);
        }
        Some(spec)
    }
}

/// Whether the constraint set blocks a command with this policy, and which
/// constraint did it. Evaluated in a fixed order, not in plan order, so the
/// same set always names the same blocker.
fn blocking_constraint(
    policy: &CommandPolicy,
    constraints: &[ConstraintKind],
) -> Option<ConstraintKind> {
    let submits = policy.risk == RiskClass::ExternalRegulated
        || matches!(policy.atomicity, AtomicityScope::ExternalSaga { .. });
    let external = policy.risk == RiskClass::ExternalRegulated;
    let has = |wanted: &ConstraintKind| constraints.iter().any(|c| c == wanted);
    if submits && has(&ConstraintKind::DoNotSubmit) {
        return Some(ConstraintKind::DoNotSubmit);
    }
    if policy.risk == RiskClass::Destructive && has(&ConstraintKind::DoNotDelete) {
        return Some(ConstraintKind::DoNotDelete);
    }
    if policy.risk > RiskClass::ReversibleLowRisk && has(&ConstraintKind::DraftOnly) {
        return Some(ConstraintKind::DraftOnly);
    }
    if external && has(&ConstraintKind::NoExternalEffects) {
        return Some(ConstraintKind::NoExternalEffects);
    }
    None
}

/// `AskBeforeApplying` never blocks; it turns a command that needed no
/// confirmation into one that does.
fn apply_ask_before_applying(
    mut policy: CommandPolicy,
    constraints: &[ConstraintKind],
) -> CommandPolicy {
    if policy.confirmation == ConfirmationPolicy::None
        && constraints
            .iter()
            .any(|c| c == &ConstraintKind::AskBeforeApplying)
    {
        policy.confirmation = ConfirmationPolicy::ExplicitClick;
    }
    policy
}

fn refusal(
    request: &PolicyRequest<'_>,
    policy: &CommandPolicy,
    reason_key: &str,
) -> PolicyDecision {
    PolicyDecision {
        command_ref: request.command_ref,
        policy: policy.clone(),
        requires_interaction: None,
        allowed: false,
        reason_key: reason_key.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_core::command::{ClaimMode, ResolutionChannel};
    use turnframe_core::hash::Digest;
    use turnframe_core::ids::{BatchId, CaseRevision, CommandId, InteractionId, TargetToken};
    use turnframe_core::interaction::{ActionClass, FieldValue};
    use turnframe_core::policy::reason as core_reason;
    use turnframe_core::target::TargetCandidate;

    use crate::config::{ResourceBudget, SandboxAcknowledgement};

    fn case() -> CaseRef {
        CaseRef::new("trip", "i1", CaseRevision(3))
    }

    fn command_ref() -> CommandRef {
        CommandRef {
            batch_id: BatchId::nil(),
            command_id: CommandId::nil(),
        }
    }

    fn direct() -> CommandOrigin {
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: Digest::of_bytes(b"e"),
        }
    }

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

    fn engine() -> PolicyEngine {
        PolicyEngine::new(&OrchestratorConfig::conservative())
    }

    fn request<'a>(policy: &'a CommandPolicy, origin: &'a CommandOrigin) -> PolicyRequest<'a> {
        PolicyRequest {
            command_ref: command_ref(),
            interaction_key: "confirm:acts[0]".into(),
            case_ref: CASE.get_or_init(case),
            policy: Some(policy),
            origin,
            command: COMMAND.get_or_init(|| json!({"do": true})),
        }
    }

    static CASE: std::sync::OnceLock<CaseRef> = std::sync::OnceLock::new();
    static COMMAND: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

    fn with(risk: RiskClass, confirmation: ConfirmationPolicy) -> CommandPolicy {
        CommandPolicy {
            risk,
            confirmation,
            atomicity: AtomicityScope::PerCase,
            claim_mode: ClaimMode::ServerReceiptOnly,
        }
    }

    #[test]
    fn an_unclassified_command_is_treated_as_irreversible() {
        let origin = direct();
        let unclassified = PolicyRequest {
            command_ref: command_ref(),
            interaction_key: "confirm:acts[0]".into(),
            case_ref: CASE.get_or_init(case),
            policy: None,
            origin: &origin,
            command: COMMAND.get_or_init(|| json!({"do": true})),
        };
        assert_eq!(
            unclassified.effective_policy(),
            CommandPolicy::conservative()
        );
        let outcome = engine().decide(&PolicySnapshot::conservative(), &unclassified, &[]);
        assert!(!outcome.is_allowed());
        assert_eq!(
            outcome.interaction().map(|s| s.kind),
            Some(InteractionKind::ConfirmCommand)
        );
    }

    #[test]
    fn confirmation_policy_maps_to_the_card_that_satisfies_it() {
        let engine = engine();
        let snapshot = PolicySnapshot::conservative();
        let origin = direct();
        // (confirmation, expected card kind)
        let table = [
            (
                ConfirmationPolicy::ReviewCard,
                Some(InteractionKind::ConfirmCommand),
            ),
            (
                ConfirmationPolicy::ExplicitClick,
                Some(InteractionKind::ConfirmCommand),
            ),
            (
                ConfirmationPolicy::Reauthentication,
                Some(InteractionKind::Reauthenticate),
            ),
            (
                ConfirmationPolicy::QualifiedSignature,
                Some(InteractionKind::ExternalSignature),
            ),
            (ConfirmationPolicy::HumanProfessionalReview, None),
        ];
        for (confirmation, expected) in table {
            let policy = with(RiskClass::Irreversible, confirmation);
            let outcome = engine.decide(&snapshot, &request(&policy, &origin), &[]);
            assert_eq!(
                outcome.interaction().map(|spec| spec.kind),
                expected,
                "{confirmation:?}"
            );
            if expected.is_none() {
                assert_eq!(
                    outcome,
                    PolicyOutcome::Blocked {
                        decision: outcome.decision().clone(),
                        reason: BlockReason::HumanReview
                    }
                );
                assert_eq!(
                    outcome.decision().reason_key,
                    core_reason::HUMAN_REVIEW_REQUIRED
                );
            }
            // Whatever the engine builds, it must be a card somebody can answer.
            if let Some(spec) = outcome.interaction() {
                assert_eq!(spec.validate(), Ok(()));
                assert_eq!(spec.confirms_risk, RiskClass::Irreversible);
                assert_eq!(spec.text_resolution, TextResolutionPolicy::Never);
                assert_eq!(spec.key, "confirm:acts[0]");
                assert!(spec.blocking && spec.binds_to_revision);
            }
        }
    }

    #[test]
    fn a_review_card_shows_its_diff_and_degrades_when_there_is_none() {
        let with_diff = engine().with_review_diff(Arc::new(|_: &PolicyRequest<'_>| {
            vec![
                ReviewDiffEntry::new("name", "Name")
                    .with_before(FieldValue::present("Old"))
                    .with_after(FieldValue::present("New")),
            ]
        }));
        let policy = with(
            RiskClass::SensitiveDataChange,
            ConfirmationPolicy::ReviewCard,
        );
        let origin = direct();
        let outcome = with_diff.decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[],
        );
        let spec = outcome.interaction().unwrap();
        assert_eq!(spec.kind, InteractionKind::ReviewChanges);
        assert_eq!(spec.payload.review_entries.len(), 1);
        assert_eq!(spec.validate(), Ok(()));
        assert_eq!(with_diff.review_diff(&request(&policy, &origin)).len(), 1);
        // Without a diff, a review card would be unanswerable, so it becomes a
        // plain confirmation — which `ReviewCard` accepts anyway.
        let outcome = engine().decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[],
        );
        let spec = outcome.interaction().unwrap();
        assert_eq!(spec.kind, InteractionKind::ConfirmCommand);
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn origins_are_judged_against_the_specific_confirmation_they_claim() {
        let engine = engine();
        let snapshot = PolicySnapshot::conservative();
        let irreversible = with(RiskClass::Irreversible, ConfirmationPolicy::ExplicitClick);
        let low = CommandPolicy::low_risk();
        let selection = card(InteractionKind::SelectTarget, ActionClass::NoCommands);
        let signature = card(
            InteractionKind::ExternalSignature,
            ActionClass::ConfirmsCommands,
        );
        let interpreted = CommandOrigin::ConfirmedInteraction {
            interaction_id: InteractionId::nil(),
            payload_hash: Digest::of_bytes(b"p"),
            interaction_kind: InteractionKind::ConfirmCommand,
            action_class: ActionClass::ConfirmsCommands,
            channel: ResolutionChannel::ModelInterpreted,
        };
        let confirmed_origin = confirmed();
        let direct_origin = direct();
        // (name, policy, origin, allowed)
        let table: [(&str, &CommandPolicy, &CommandOrigin, bool); 7] = [
            ("direct low risk", &low, &direct_origin, true),
            ("direct irreversible", &irreversible, &direct_origin, false),
            (
                "confirmed irreversible",
                &irreversible,
                &confirmed_origin,
                true,
            ),
            // The user said which trip they meant. That is not consent to
            // send one (CORE-SPEC-001).
            ("disambiguation click", &irreversible, &selection, false),
            (
                "signature on a click policy",
                &irreversible,
                &signature,
                false,
            ),
            ("inferred yes", &irreversible, &interpreted, false),
            ("disambiguation click, low risk", &low, &selection, true),
        ];
        for (name, policy, origin, allowed) in table {
            let outcome = engine.decide(&snapshot, &request(policy, origin), &[]);
            assert_eq!(outcome.is_allowed(), allowed, "case {name}");
        }
    }

    #[test]
    fn constraints_block_what_they_name_and_nothing_else() {
        let engine = engine();
        let snapshot = PolicySnapshot::conservative();
        let origin = confirmed();
        let submission = CommandPolicy {
            risk: RiskClass::ExternalRegulated,
            ..CommandPolicy::conservative()
        };
        let saga = CommandPolicy {
            risk: RiskClass::ReversibleLowRisk,
            confirmation: ConfirmationPolicy::None,
            atomicity: AtomicityScope::ExternalSaga {
                saga: "airline".into(),
            },
            claim_mode: ClaimMode::ServerReceiptOnly,
        };
        let deletion = CommandPolicy {
            risk: RiskClass::Destructive,
            ..CommandPolicy::conservative()
        };
        let edit = CommandPolicy::low_risk();
        // (name, policy, constraints, blocked by)
        let table: [(
            &str,
            &CommandPolicy,
            &[ConstraintKind],
            Option<ConstraintKind>,
        ); 8] = [
            (
                "submission vs do not submit",
                &submission,
                &[ConstraintKind::DoNotSubmit],
                Some(ConstraintKind::DoNotSubmit),
            ),
            (
                "saga vs do not submit",
                &saga,
                &[ConstraintKind::DoNotSubmit],
                Some(ConstraintKind::DoNotSubmit),
            ),
            (
                "edit vs do not submit",
                &edit,
                &[ConstraintKind::DoNotSubmit],
                None,
            ),
            (
                "deletion vs do not delete",
                &deletion,
                &[ConstraintKind::DoNotDelete],
                Some(ConstraintKind::DoNotDelete),
            ),
            (
                "edit vs do not delete",
                &edit,
                &[ConstraintKind::DoNotDelete],
                None,
            ),
            (
                "deletion vs draft only",
                &deletion,
                &[ConstraintKind::DraftOnly],
                Some(ConstraintKind::DraftOnly),
            ),
            (
                "edit vs draft only",
                &edit,
                &[ConstraintKind::DraftOnly],
                None,
            ),
            (
                "submission vs no external effects",
                &submission,
                &[ConstraintKind::NoExternalEffects],
                Some(ConstraintKind::NoExternalEffects),
            ),
        ];
        for (name, policy, constraints, expected) in table {
            let outcome = engine.decide(&snapshot, &request(policy, &origin), constraints);
            match expected {
                Some(constraint) => {
                    assert_eq!(
                        outcome,
                        PolicyOutcome::Blocked {
                            decision: outcome.decision().clone(),
                            reason: BlockReason::Constraint(constraint)
                        },
                        "case {name}"
                    );
                    assert_eq!(
                        outcome.decision().reason_key,
                        reason::BLOCKED_BY_CONSTRAINT,
                        "case {name}"
                    );
                }
                None => assert!(outcome.is_allowed(), "case {name}"),
            }
        }
    }

    #[test]
    fn ask_before_applying_turns_a_free_edit_into_a_confirmation() {
        let engine = engine();
        let origin = direct();
        let policy = CommandPolicy::low_risk();
        let free = engine.decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[],
        );
        assert!(free.is_allowed());
        let asked = engine.decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[ConstraintKind::AskBeforeApplying],
        );
        let spec = asked.interaction().unwrap();
        assert_eq!(spec.kind, InteractionKind::ConfirmCommand);
        assert_eq!(
            asked.decision().policy.confirmation,
            ConfirmationPolicy::ExplicitClick,
            "the recorded decision must show the policy that was actually applied"
        );
        // A click then satisfies it.
        let confirmed_origin = confirmed();
        assert!(
            engine
                .decide(
                    &PolicySnapshot::conservative(),
                    &request(&policy, &confirmed_origin),
                    &[ConstraintKind::AskBeforeApplying]
                )
                .is_allowed()
        );
    }

    #[test]
    fn the_sandbox_refuses_before_any_card_is_offered() {
        let config =
            OrchestratorConfig::conservative().with_mode(OrchestrationMode::sandboxed_autonomous(
                ResourceBudget::conservative(),
                SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
            ));
        let engine = PolicyEngine::new(&config);
        assert!(engine.mode().is_sandboxed());
        let origin = confirmed();
        let policy = with(RiskClass::Irreversible, ConfirmationPolicy::ExplicitClick);
        let outcome = engine.decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[],
        );
        assert_eq!(
            outcome,
            PolicyOutcome::Blocked {
                decision: outcome.decision().clone(),
                reason: BlockReason::Mode {
                    risk: RiskClass::Irreversible
                }
            }
        );
        assert_eq!(outcome.decision().reason_key, reason::BLOCKED_BY_MODE);
        assert!(outcome.interaction().is_none());
        // What the sandbox does allow still goes through the normal gates.
        let low = CommandPolicy::low_risk();
        let direct_origin = direct();
        assert!(
            engine
                .decide(
                    &PolicySnapshot::conservative(),
                    &request(&low, &direct_origin),
                    &[]
                )
                .is_allowed()
        );
    }

    #[test]
    fn a_snapshot_that_forbids_a_class_offers_no_card_either() {
        let policy = with(RiskClass::Destructive, ConfirmationPolicy::ExplicitClick);
        let origin = confirmed();
        let outcome = engine().decide(&PolicySnapshot::sandbox(), &request(&policy, &origin), &[]);
        assert_eq!(
            outcome,
            PolicyOutcome::Blocked {
                decision: outcome.decision().clone(),
                reason: BlockReason::ForbiddenRiskClass {
                    risk: RiskClass::Destructive
                }
            }
        );
        assert_eq!(
            outcome.decision().reason_key,
            core_reason::FORBIDDEN_RISK_CLASS
        );
    }

    #[test]
    fn a_selection_card_is_built_only_for_a_real_choice() {
        let engine = engine();
        let candidate = |id: &str, label: &str| TargetCandidate {
            token: TargetToken::from(format!("t_{id}")),
            case_ref: CaseRef::new("trip", id, CaseRevision(1)),
            label: label.to_owned(),
        };
        let two = [candidate("i1", "Ferri"), candidate("i2", "Luca Ferri")];
        let spec = engine
            .selection_card(
                "select_target:acts[0]",
                &case(),
                LocalizedText::new("Which one?"),
                LocalizedText::new("Neither"),
                &two,
            )
            .unwrap();
        assert_eq!(spec.kind, InteractionKind::SelectTarget);
        assert_eq!(spec.validate(), Ok(()));
        assert_eq!(
            spec.payload.options.len(),
            3,
            "two candidates plus a way out"
        );
        assert_eq!(spec.confirms_risk, RiskClass::ReversibleLowRisk);
        assert!(!spec.binds_to_revision);
        assert!(
            !spec.blocking,
            "a card about a case nobody has identified yet must not wedge one"
        );
        assert!(
            engine
                .selection_card(
                    "k",
                    &case(),
                    LocalizedText::new("t"),
                    LocalizedText::new("c"),
                    &two[..1]
                )
                .is_none()
        );
        let many: Vec<_> = (0..99)
            .map(|n| candidate(&format!("i{n}"), "Ferri"))
            .collect();
        assert!(
            engine
                .selection_card(
                    "k",
                    &case(),
                    LocalizedText::new("t"),
                    LocalizedText::new("c"),
                    &many
                )
                .is_none(),
            "a truncated list would let order decide which cases are reachable"
        );
    }

    #[test]
    fn outcome_accessors_and_copy() {
        let policy = CommandPolicy::conservative();
        let origin = direct();
        let outcome = engine().decide(
            &PolicySnapshot::conservative(),
            &request(&policy, &origin),
            &[],
        );
        assert_eq!(outcome.decision().command_ref, command_ref());
        assert_eq!(outcome.clone().into_decision().command_ref, command_ref());
        let custom = ConfirmationCopy {
            confirm_title: LocalizedText::new("Sicuro?"),
            ..ConfirmationCopy::default()
        };
        let engine = engine().with_copy(custom);
        let spec = engine
            .decide(
                &PolicySnapshot::conservative(),
                &request(&policy, &origin),
                &[],
            )
            .into_decision();
        assert!(!spec.allowed);
        assert_eq!(NoReviewDiff.diff(&request(&policy, &origin)), vec![]);
    }
}
