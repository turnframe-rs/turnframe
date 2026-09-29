//! Strategies for command origins and policies (spec §14).

use proptest::prelude::*;
use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandOrigin, CommandPolicy, ConfirmationPolicy, ResolutionChannel,
    RiskClass,
};
use turnframe_core::interaction::{ActionClass, InteractionKind};

use crate::strategies::ids;

/// An arbitrary answer channel.
pub fn resolution_channel() -> impl Strategy<Value = ResolutionChannel> {
    prop_oneof![
        Just(ResolutionChannel::Click),
        Just(ResolutionChannel::ModelInterpreted),
    ]
}

/// An arbitrary action class.
pub fn action_class() -> impl Strategy<Value = ActionClass> {
    prop_oneof![
        Just(ActionClass::ConfirmsCommands),
        Just(ActionClass::AppliesOperation),
        Just(ActionClass::NoCommands),
    ]
}

/// An arbitrary origin, trusted or not.
///
/// There is deliberately no "the model proposed it" variant to generate: a
/// model proposal is not an origin (spec §14.2).
pub fn command_origin() -> impl Strategy<Value = CommandOrigin> {
    prop_oneof![
        ids::digest()
            .prop_map(|evidence_digest| CommandOrigin::DirectSafeUserAct { evidence_digest }),
        (
            ids::interaction_id(),
            ids::digest(),
            crate::strategies::interaction::interaction_kind(),
            action_class(),
            resolution_channel(),
        )
            .prop_map(
                |(interaction_id, payload_hash, interaction_kind, action_class, channel)| {
                    CommandOrigin::ConfirmedInteraction {
                        interaction_id,
                        payload_hash,
                        interaction_kind,
                        action_class,
                        channel,
                    }
                },
            ),
        ids::label().prop_map(|policy_key| CommandOrigin::InternalPolicy { policy_key }),
        (ids::label(), any::<bool>()).prop_map(|(callback_id, signature_verified)| {
            CommandOrigin::ExternalCallback {
                callback_id,
                signature_verified,
            }
        }),
    ]
}

/// An origin that may authorize a consequential command (I12): a confirmed
/// interaction, an internal policy, or a callback whose signature was verified.
pub fn trusted_origin() -> impl Strategy<Value = CommandOrigin> {
    prop_oneof![
        (
            ids::interaction_id(),
            ids::digest(),
            prop_oneof![
                Just(InteractionKind::ConfirmCommand),
                Just(InteractionKind::ReviewChanges),
            ],
            prop_oneof![
                Just(ActionClass::ConfirmsCommands),
                Just(ActionClass::AppliesOperation),
            ],
            Just(ResolutionChannel::Click),
        )
            .prop_map(
                |(interaction_id, payload_hash, interaction_kind, action_class, channel)| {
                    CommandOrigin::ConfirmedInteraction {
                        interaction_id,
                        payload_hash,
                        interaction_kind,
                        action_class,
                        channel,
                    }
                },
            ),
        ids::label().prop_map(|policy_key| CommandOrigin::InternalPolicy { policy_key }),
        ids::label().prop_map(|callback_id| CommandOrigin::ExternalCallback {
            callback_id,
            signature_verified: true,
        }),
    ]
}

/// An arbitrary risk class.
pub fn risk_class() -> impl Strategy<Value = RiskClass> {
    prop_oneof![
        Just(RiskClass::ReadOnly),
        Just(RiskClass::ReversibleLowRisk),
        Just(RiskClass::SensitiveDataChange),
        Just(RiskClass::Destructive),
        Just(RiskClass::Irreversible),
        Just(RiskClass::ExternalRegulated),
    ]
}

/// An arbitrary confirmation policy.
pub fn confirmation_policy() -> impl Strategy<Value = ConfirmationPolicy> {
    prop_oneof![
        Just(ConfirmationPolicy::None),
        Just(ConfirmationPolicy::ReviewCard),
        Just(ConfirmationPolicy::ExplicitClick),
        Just(ConfirmationPolicy::Reauthentication),
        Just(ConfirmationPolicy::QualifiedSignature),
        Just(ConfirmationPolicy::HumanProfessionalReview),
    ]
}

/// An arbitrary atomicity scope.
pub fn atomicity_scope() -> impl Strategy<Value = AtomicityScope> {
    prop_oneof![
        Just(AtomicityScope::PerCommand),
        Just(AtomicityScope::PerCase),
        ids::label().prop_map(|group| AtomicityScope::ExplicitGroup { group }),
        ids::label().prop_map(|saga| AtomicityScope::ExternalSaga { saga }),
    ]
}

/// An arbitrary claim mode.
pub fn claim_mode() -> impl Strategy<Value = ClaimMode> {
    prop_oneof![
        Just(ClaimMode::ServerReceiptOnly),
        Just(ClaimMode::EventReferencedParaphrase),
        Just(ClaimMode::FreeExplanation),
    ]
}

/// An arbitrary command policy. Combinations are free on purpose: the point of
/// a property test is to try the ones a domain author would not think of.
pub fn command_policy() -> impl Strategy<Value = CommandPolicy> {
    (
        risk_class(),
        confirmation_policy(),
        atomicity_scope(),
        claim_mode(),
    )
        .prop_map(
            |(risk, confirmation, atomicity, claim_mode)| CommandPolicy {
                risk,
                confirmation,
                atomicity,
                claim_mode,
            },
        )
}

/// A policy that demands a trusted origin, i.e. one where
/// [`CommandPolicy::requires_trusted_origin`] holds.
pub fn consequential_policy() -> impl Strategy<Value = CommandPolicy> {
    command_policy().prop_filter("policy must require a trusted origin", |policy| {
        policy.requires_trusted_origin()
    })
}
