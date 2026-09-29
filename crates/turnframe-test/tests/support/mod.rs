//! Helpers shared by the integration tests: envelope and batch construction
//! with derived identifiers, so two runs of the same scenario are comparable.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde::Serialize;
use turnframe_core::command::ResolutionChannel;
use turnframe_core::hash::Digest;
use turnframe_core::interaction::ActionClass;
use turnframe_core::prelude::*;

/// The tenant every test runs in.
pub const ACCOUNT: &str = "acct-test";

/// The user every test acts as.
pub const USER: &str = "user-test";

pub fn account() -> AccountId {
    AccountId::from(ACCOUNT)
}

pub fn actor() -> ActorContext {
    ActorContext::new(ACCOUNT, USER)
}

/// A deterministic turn identifier, so idempotency keys are reproducible.
pub fn turn(step: u8) -> TurnId {
    TurnId::from(turnframe_core::hash::derive_uuid(
        "turnframe.test.turn.v1",
        &[&step.to_string()],
    ))
}

/// An origin that satisfies every confirmation a card can give: a click on a
/// `ConfirmCommand` card whose option applies an operation.
pub fn confirmed_click() -> CommandOrigin {
    CommandOrigin::ConfirmedInteraction {
        interaction_id: InteractionId::nil(),
        payload_hash: Digest::of_bytes(b"payload"),
        interaction_kind: InteractionKind::ConfirmCommand,
        action_class: ActionClass::AppliesOperation,
        channel: ResolutionChannel::Click,
    }
}

/// An origin a verified external system supplies.
pub fn verified_callback() -> CommandOrigin {
    CommandOrigin::ExternalCallback {
        callback_id: "cb-1".to_owned(),
        signature_verified: true,
    }
}

/// One per-case batch, with derived batch, command and idempotency identifiers.
pub fn batch<C: Serialize + Clone>(
    turn_id: TurnId,
    case_ref: &CaseRef,
    origin: &CommandOrigin,
    commands: Vec<C>,
) -> CommandBatch<C> {
    let account = account();
    let envelopes = commands
        .into_iter()
        .enumerate()
        .map(|(position, command)| {
            let value = serde_json::to_value(&command).unwrap();
            CommandEnvelope {
                command_id: CommandId::derive(
                    &turn_id,
                    turnframe_core::understanding::ActId::new(
                        turnframe_core::understanding::UnitId(1),
                        1,
                    ),
                    position,
                ),
                turn_id,
                actor: actor(),
                case_ref: case_ref.clone(),
                idempotency_key: IdempotencyKey::derive(
                    &account, &turn_id, case_ref, origin, &value,
                )
                .unwrap(),
                origin: origin.clone(),
                command,
            }
        })
        .collect();
    CommandBatch {
        batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
        scope: AtomicityScope::PerCase,
        envelopes,
    }
}
