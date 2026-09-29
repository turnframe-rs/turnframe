//! Policy decisions and the policy snapshot the reducer evaluates against.
//!
//! Policy is deterministic: given a command's [`CommandPolicy`] and its
//! [`CommandOrigin`], [`PolicySnapshot::decide`] says whether the command may
//! execute now and, if not, which interaction would authorize it.

use serde::{Deserialize, Serialize};

use crate::command::{CommandOrigin, CommandPolicy, RiskClass, origin_satisfies};
use crate::interaction::InteractionKind;
use crate::reduce::CommandRef;

/// Reason keys used by [`PolicySnapshot::decide`].
pub mod reason {
    /// The command may execute with its origin.
    pub const ALLOWED: &str = "turnframe.policy.allowed";
    /// A confirmation interaction is required first.
    pub const CONFIRMATION_REQUIRED: &str = "turnframe.policy.confirmation_required";
    /// The risk class is forbidden in this configuration.
    pub const FORBIDDEN_RISK_CLASS: &str = "turnframe.policy.forbidden_risk_class";
    /// The confirmation must come from somebody other than the end user, so no
    /// card can unblock the command.
    pub const HUMAN_REVIEW_REQUIRED: &str = "turnframe.policy.human_review_required";
}

/// The outcome of evaluating policy for one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    /// The command.
    pub command_ref: CommandRef,
    /// The policy that applied.
    pub policy: CommandPolicy,
    /// Interaction that would authorize the command when not allowed now.
    pub requires_interaction: Option<InteractionKind>,
    /// Whether the command may execute with its current origin.
    pub allowed: bool,
    /// Key of the user-facing reason (see [`reason`]).
    pub reason_key: String,
}

/// Point-in-time policy configuration the reducer evaluates against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PolicySnapshot {
    /// Policy for commands the domain did not classify.
    pub default_policy: CommandPolicy,
    /// Risk classes that may never execute (e.g. in a sandbox: everything above
    /// `ReversibleLowRisk`).
    pub forbidden_risk_classes: Vec<RiskClass>,
    /// Highest risk a command may carry while originating from an untrusted
    /// origin such as [`CommandOrigin::DirectSafeUserAct`]. Default
    /// `ReversibleLowRisk` (I12).
    ///
    /// The setting may only tighten: [`origin_satisfies`] refuses anything
    /// above `ReversibleLowRisk` from an untrusted origin whatever the
    /// snapshot says.
    pub max_direct_risk: RiskClass,
    /// Whether low-risk interactions may be resolved from interpreted text
    /// when their stored policy permits it (spec §13.2 rule 8).
    pub allow_text_resolution_for_low_risk: bool,
}

impl PolicySnapshot {
    /// The default snapshot: conservative default policy, nothing forbidden,
    /// direct acts up to `ReversibleLowRisk`, text resolution allowed for
    /// low-risk cards.
    ///
    /// There is no "fail open" setting: when the policy source cannot be
    /// consulted the runtime has no snapshot to consult either and must return
    /// [`PolicyError::Unavailable`](crate::error::PolicyError::Unavailable)
    /// (I19).
    #[must_use]
    pub fn conservative() -> Self {
        Self {
            default_policy: CommandPolicy::conservative(),
            forbidden_risk_classes: Vec::new(),
            max_direct_risk: RiskClass::ReversibleLowRisk,
            allow_text_resolution_for_low_risk: true,
        }
    }

    /// A snapshot for sandboxed, reversible domains: everything above
    /// `ReversibleLowRisk` is forbidden (spec §11.4).
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            forbidden_risk_classes: vec![
                RiskClass::SensitiveDataChange,
                RiskClass::Destructive,
                RiskClass::Irreversible,
                RiskClass::ExternalRegulated,
            ],
            ..Self::conservative()
        }
    }

    /// Returns `true` when the risk class may never execute.
    #[must_use]
    pub fn is_risk_forbidden(&self, risk: RiskClass) -> bool {
        self.forbidden_risk_classes.contains(&risk)
    }

    /// Evaluates one command.
    ///
    /// The decision answers two questions: may this command execute with the
    /// origin it carries, and if not, which card would authorize it. A policy
    /// only somebody other than the end user can satisfy
    /// ([`ConfirmationPolicy::HumanProfessionalReview`](crate::command::ConfirmationPolicy::HumanProfessionalReview))
    /// names no card at all.
    #[must_use]
    pub fn decide(
        &self,
        command_ref: CommandRef,
        policy: &CommandPolicy,
        origin: &CommandOrigin,
    ) -> PolicyDecision {
        if self.is_risk_forbidden(policy.risk) {
            return PolicyDecision {
                command_ref,
                policy: policy.clone(),
                requires_interaction: None,
                allowed: false,
                reason_key: reason::FORBIDDEN_RISK_CLASS.to_owned(),
            };
        }
        let direct_too_risky = !origin.is_trusted() && policy.risk > self.max_direct_risk;
        if origin_satisfies(origin, policy) && !direct_too_risky {
            return PolicyDecision {
                command_ref,
                policy: policy.clone(),
                requires_interaction: None,
                allowed: true,
                reason_key: reason::ALLOWED.to_owned(),
            };
        }
        let requires_interaction = policy.confirmation.interaction_kind().or({
            if policy.confirmation.is_server_side_only() {
                None
            } else {
                Some(InteractionKind::ConfirmCommand)
            }
        });
        let reason_key = if policy.confirmation.is_server_side_only() {
            reason::HUMAN_REVIEW_REQUIRED
        } else {
            reason::CONFIRMATION_REQUIRED
        };
        PolicyDecision {
            command_ref,
            policy: policy.clone(),
            requires_interaction,
            allowed: false,
            reason_key: reason_key.to_owned(),
        }
    }
}

impl Default for PolicySnapshot {
    fn default() -> Self {
        Self::conservative()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{ConfirmationPolicy, ResolutionChannel};
    use crate::hash::Digest;
    use crate::ids::{BatchId, CommandId, InteractionId};
    use crate::interaction::ActionClass;

    fn cref() -> CommandRef {
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

    #[test]
    fn direct_low_risk_allowed_high_risk_needs_card() {
        let snap = PolicySnapshot::conservative();
        let low = snap.decide(cref(), &CommandPolicy::low_risk(), &direct());
        assert!(low.allowed);
        let high = snap.decide(cref(), &CommandPolicy::conservative(), &direct());
        assert!(!high.allowed);
        assert_eq!(
            high.requires_interaction,
            Some(InteractionKind::ConfirmCommand)
        );
        assert!(
            snap.decide(cref(), &CommandPolicy::conservative(), &confirmed())
                .allowed
        );
    }

    #[test]
    fn review_card_maps_to_review_changes() {
        let snap = PolicySnapshot::conservative();
        let mut policy = CommandPolicy::low_risk();
        policy.confirmation = ConfirmationPolicy::ReviewCard;
        let d = snap.decide(cref(), &policy, &direct());
        assert_eq!(d.requires_interaction, Some(InteractionKind::ReviewChanges));
    }

    #[test]
    fn sandbox_forbids_destructive_even_when_confirmed() {
        let snap = PolicySnapshot::sandbox();
        let mut policy = CommandPolicy::conservative();
        policy.risk = RiskClass::Destructive;
        let d = snap.decide(cref(), &policy, &confirmed());
        assert!(!d.allowed);
        assert_eq!(d.reason_key, reason::FORBIDDEN_RISK_CLASS);
        assert_eq!(d.requires_interaction, None);
    }

    #[test]
    fn a_selection_click_does_not_authorize_the_command_it_disambiguates() {
        let snap = PolicySnapshot::conservative();
        let selection = card(InteractionKind::SelectTarget, ActionClass::NoCommands);
        let d = snap.decide(cref(), &CommandPolicy::conservative(), &selection);
        assert!(!d.allowed);
        assert_eq!(d.reason_key, reason::CONFIRMATION_REQUIRED);
        assert_eq!(
            d.requires_interaction,
            Some(InteractionKind::ConfirmCommand)
        );
    }

    #[test]
    fn an_internal_policy_key_is_not_a_qualified_signature() {
        let snap = PolicySnapshot::conservative();
        let policy = CommandPolicy {
            confirmation: ConfirmationPolicy::QualifiedSignature,
            ..CommandPolicy::conservative()
        };
        let internal = CommandOrigin::InternalPolicy {
            policy_key: "auto".into(),
        };
        assert!(!snap.decide(cref(), &policy, &internal).allowed);
        assert!(
            !snap.decide(cref(), &policy, &confirmed()).allowed,
            "a plain confirmation card is not a signature"
        );
        let signed = card(
            InteractionKind::ExternalSignature,
            ActionClass::ConfirmsCommands,
        );
        assert!(snap.decide(cref(), &policy, &signed).allowed);
    }

    #[test]
    fn human_review_names_no_card_the_user_could_click() {
        let snap = PolicySnapshot::conservative();
        let policy = CommandPolicy {
            confirmation: ConfirmationPolicy::HumanProfessionalReview,
            ..CommandPolicy::conservative()
        };
        let d = snap.decide(cref(), &policy, &confirmed());
        assert!(!d.allowed);
        assert_eq!(d.requires_interaction, None);
        assert_eq!(d.reason_key, reason::HUMAN_REVIEW_REQUIRED);
        assert!(
            snap.decide(
                cref(),
                &policy,
                &CommandOrigin::InternalPolicy {
                    policy_key: "reviewed_by_accountant".into()
                }
            )
            .allowed
        );
    }
}
