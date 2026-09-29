//! Deterministic envelope construction and non-panicking assertions for the
//! executor conformance suite.
//!
//! Everything here is derived from the check's own name and a label, so two
//! runs of the same check build byte-identical batches: identifiers, batch ids
//! and idempotency keys included. A suite that minted random keys could not
//! tell "the executor replayed the key" from "the executor never saw it".
//!
//! The helpers replace `assert!`: they return a [`ConformanceFailure`] naming
//! the check, what was expected and what happened, so the suite can run outside
//! a test harness without ever panicking.

use std::fmt;

use turnframe_core::case::CaseRef;
use turnframe_core::command::{
    AtomicityScope, CommandBatch, CommandEnvelope, CommandOrigin, IdempotencyKey, ResolutionChannel,
};
use turnframe_core::error::ExecutionError;
use turnframe_core::event::Commit;
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::hash::{Digest, canonical_digest, derive_uuid};
use turnframe_core::ids::{BatchId, CaseRevision, CommandId, InteractionId, TurnId};
use turnframe_core::interaction::{ActionClass, InteractionKind};

use super::{ConformanceFailure, ExecutorFactory, SeedOf, SeededCase};

/// Domain separation of the turn identifiers the suite derives.
const TURN_DOMAIN: &str = "turnframe.test.executor.conformance.turn.v1";

/// A turn identifier derived from a label, stable across runs and processes.
pub(super) fn turn(label: &str) -> TurnId {
    TurnId::from(derive_uuid(TURN_DOMAIN, &[label]))
}

/// The origin every envelope carries: a click on a confirmation card.
///
/// An executor is not the layer that judges origins — that is the runtime's
/// job — but the origin is part of the idempotency key, so the suite pins one
/// rather than leaving it to the adopter.
pub(super) fn origin() -> CommandOrigin {
    CommandOrigin::ConfirmedInteraction {
        interaction_id: InteractionId::nil(),
        payload_hash: Digest::of_bytes(b"turnframe.executor.conformance"),
        interaction_kind: InteractionKind::ConfirmCommand,
        action_class: ActionClass::AppliesOperation,
        channel: ResolutionChannel::Click,
    }
}

/// One envelope, with derived command id and idempotency key.
fn envelope<C: serde::Serialize + Clone>(
    check: &'static str,
    actor: &turnframe_core::turn::ActorContext,
    turn_id: TurnId,
    position: usize,
    case_ref: &CaseRef,
    command: C,
) -> Result<CommandEnvelope<C>, ConformanceFailure> {
    let value = serde_json::to_value(&command).map_err(|error| {
        ConformanceFailure::new(check, format!("a command did not serialize: {error}"))
    })?;
    let idempotency_key =
        IdempotencyKey::derive(&actor.account_id, &turn_id, case_ref, &origin(), &value).map_err(
            |error| {
                ConformanceFailure::new(
                    check,
                    format!("an idempotency key could not be derived: {error}"),
                )
            },
        )?;
    Ok(CommandEnvelope {
        command_id: CommandId::derive(
            &turn_id,
            turnframe_core::understanding::ActId::new(turnframe_core::understanding::UnitId(1), 1),
            position,
        ),
        turn_id,
        actor: actor.clone(),
        case_ref: case_ref.clone(),
        idempotency_key,
        origin: origin(),
        command,
    })
}

/// A `PerCase` batch of `commands`, all targeting `case_ref`, planned by the
/// turn `label` names.
///
/// Two batches built with the same label and the same commands against the same
/// case reference are the *same* batch, down to the idempotency keys — which is
/// how the suite replays one. Two batches built with different labels are
/// different work that happens to say the same thing, which is how the suite
/// tests a stale revision without tripping the idempotency rule.
pub(super) fn batch<F: ExecutorFactory>(
    check: &'static str,
    seeded: &SeedOf<F>,
    label: &str,
    case_ref: &CaseRef,
    commands: &[<F::Workflow as WorkflowDefinition>::Command],
) -> Result<CommandBatch<<F::Workflow as WorkflowDefinition>::Command>, ConformanceFailure> {
    let turn_id = turn(label);
    let mut envelopes = Vec::with_capacity(commands.len());
    for (position, command) in commands.iter().enumerate() {
        envelopes.push(envelope(
            check,
            &seeded.actor,
            turn_id,
            position,
            case_ref,
            command.clone(),
        )?);
    }
    Ok(CommandBatch {
        batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
        scope: AtomicityScope::PerCase,
        envelopes,
    })
}

/// What the case looks like right now: a digest of the loaded state and the
/// revision it was loaded at.
///
/// The digest, not the state: comparing two canonical digests is the same
/// assertion as comparing two rows byte for byte, and it keeps the adopter's
/// data out of a failure message the suite may log.
pub(super) async fn snapshot<W, E>(
    check: &'static str,
    what: &str,
    seeded: &SeededCase<W, E>,
) -> Result<(Digest, CaseRevision), ConformanceFailure>
where
    W: WorkflowDefinition,
    E: WorkflowExecutor<W>,
{
    let loaded = seeded
        .executor
        .load(&seeded.actor.account_id, &seeded.case_ref.case_id)
        .await
        .map_err(|error| {
            ConformanceFailure::new(check, format!("{what}: load failed with {error:?}"))
        })?;
    let digest = canonical_digest(&loaded.value).map_err(|error| {
        ConformanceFailure::new(check, format!("{what}: the state did not digest: {error}"))
    })?;
    Ok((digest, loaded.revision))
}

/// Fails the check unless the case is exactly where `expected` says it was.
pub(super) async fn ensure_unchanged<W, E>(
    check: &'static str,
    what: &str,
    seeded: &SeededCase<W, E>,
    expected: &(Digest, CaseRevision),
) -> Result<(), ConformanceFailure>
where
    W: WorkflowDefinition,
    E: WorkflowExecutor<W>,
{
    let actual = snapshot(check, what, seeded).await?;
    ensure(
        check,
        actual.0 == expected.0 && actual.1 == expected.1,
        format!(
            "{what}: the case had to be untouched, expected revision {} state {}, \
             found revision {} state {}",
            expected.1,
            expected.0.as_str(),
            actual.1,
            actual.0.as_str(),
        ),
    )
}

/// Calls the factory's seed hook, naming the check when it fails.
pub(super) async fn seed<F: ExecutorFactory>(
    check: &'static str,
    factory: &F,
) -> Result<SeedOf<F>, ConformanceFailure> {
    factory.seed().await.map_err(|detail| {
        ConformanceFailure::new(check, format!("the factory could not seed: {detail}"))
    })
}

// ---------------------------------------------------------------------------
// Assertions that return instead of panicking
// ---------------------------------------------------------------------------

/// Fails the check unless `condition` holds.
pub(super) fn ensure(
    check: &'static str,
    condition: bool,
    detail: impl Into<String>,
) -> Result<(), ConformanceFailure> {
    if condition {
        Ok(())
    } else {
        Err(ConformanceFailure::new(check, detail))
    }
}

/// Fails the check unless `actual` equals `expected`.
pub(super) fn ensure_eq<T: PartialEq + fmt::Debug>(
    check: &'static str,
    what: &str,
    actual: &T,
    expected: &T,
) -> Result<(), ConformanceFailure> {
    ensure(
        check,
        actual == expected,
        format!("{what}: expected {expected:?}, got {actual:?}"),
    )
}

/// Unwraps a commit, or fails the check naming the operation.
pub(super) fn ensure_commit<S, E>(
    check: &'static str,
    what: &str,
    result: Result<Commit<S, E>, ExecutionError>,
) -> Result<Commit<S, E>, ConformanceFailure> {
    result.map_err(|error| {
        ConformanceFailure::new(check, format!("{what}: expected a commit, got {error:?}"))
    })
}

/// Fails the check unless the execution was refused, returning the refusal.
pub(super) fn ensure_refused<S, E>(
    check: &'static str,
    what: &str,
    result: Result<Commit<S, E>, ExecutionError>,
) -> Result<ExecutionError, ConformanceFailure> {
    match result {
        Err(error) => Ok(error),
        Ok(commit) => Err(ConformanceFailure::new(
            check,
            format!(
                "{what}: expected a refusal, got a commit at revision {}",
                commit.new_revision
            ),
        )),
    }
}
