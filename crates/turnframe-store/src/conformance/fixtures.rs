//! Deterministic fixtures and assertion helpers for the conformance checks.
//!
//! Every value here is fabricated by the suite, so nothing a check reports can
//! carry an adopter's data. Timestamps start at the Unix epoch and move in
//! whole seconds, which keeps "ordered by `created_at` then id" a meaningful
//! assertion instead of a coin flip.
//!
//! The helpers replace `assert!`: they return a [`ConformanceFailure`] naming
//! the check, what was expected and what happened, so the suite can run outside
//! a test harness without ever panicking.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::{CommandOrigin, IdempotencyKey};
use turnframe_core::event::{ArtifactRef, CommittedEvent, OutboxEntry, OutboxStatus};
use turnframe_core::ids::{
    AccountId, BlockId, CaseRevision, CommandId, ConversationId, EventId, InteractionId, OutboxId,
    RedactionAuthority, TurnId, UserId,
};
use turnframe_core::interaction::{
    Interaction, InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
    StoredInteractionAction,
};
use turnframe_core::locale::Locale;
use turnframe_core::response::{
    ArtifactView, AssistantTurn, InteractionBlock, NoticeSeverity, ReplayToken, ResponseBlock,
    ServerNotice,
};
use turnframe_core::turn::{ActorContext, TurnInput};

use super::ConformanceFailure;
use crate::conversation::StoredUserTurn;
use crate::error::StoreError;
use crate::events::EventBatch;
use crate::journal::{CommandJournalEntry, CommandJournalStatus};

/// The instant every fixture starts from.
pub(super) fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

/// `epoch()` plus whole seconds, for fixtures that must sort in a known order.
pub(super) fn at(seconds: i64) -> DateTime<Utc> {
    epoch() + TimeDelta::seconds(seconds)
}

/// The tenant a check writes as.
pub(super) fn account() -> AccountId {
    AccountId::from("conformance-account")
}

/// A second tenant, used to prove isolation.
pub(super) fn other_account() -> AccountId {
    AccountId::from("conformance-other-account")
}

/// The case every fixture card and command targets, at `revision`.
pub(super) fn case(revision: u64) -> CaseRef {
    CaseRef::new("conformance", "case-1", CaseRevision(revision))
}

/// A second case, for checks that must show a sweep stays inside one case.
pub(super) fn other_case(revision: u64) -> CaseRef {
    CaseRef::new("conformance", "case-2", CaseRevision(revision))
}

/// The revision-less identity of [`case`].
pub(super) fn case_key() -> CaseKey {
    case(0).key()
}

/// The revision-less identity of [`other_case`], for checks that must show a
/// read spanning more than one case of the same account.
pub(super) fn other_case_key() -> CaseKey {
    other_case(0).key()
}

/// A card payload with one dismissable option, valid for a `SingleSelect`.
pub(super) fn payload(title: &str) -> InteractionPayload {
    InteractionPayload::new(title).with_option(InteractionOption::new(
        "ack",
        "Got it",
        StoredInteractionAction::Dismiss,
    ))
}

/// A blocking, revision-bound `SingleSelect` spec.
pub(super) fn card_spec(key: &str, case_ref: CaseRef) -> InteractionSpec {
    InteractionSpec::new(
        key,
        case_ref,
        InteractionKind::SingleSelect,
        payload("conformance card"),
    )
}

/// Materializes a spec into an `Active` interaction.
pub(super) fn card(
    check: &'static str,
    spec: InteractionSpec,
    id: InteractionId,
    account_id: AccountId,
    conversation: ConversationId,
    turn: TurnId,
    now: DateTime<Utc>,
) -> Result<Interaction, ConformanceFailure> {
    Interaction::from_spec(spec, id, account_id, conversation, turn, now).map_err(|error| {
        ConformanceFailure::new(check, format!("could not build a fixture card: {error}"))
    })
}

/// The time-to-live used by the expiry check.
pub(super) fn ttl(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// A `Pending` journal entry for `turn`, keyed by `key`.
pub(super) fn journal_entry(
    account_id: &AccountId,
    turn: TurnId,
    command_id: CommandId,
    key: &str,
    created_at: DateTime<Utc>,
) -> CommandJournalEntry {
    CommandJournalEntry {
        command_id,
        account_id: account_id.clone(),
        idempotency_key: IdempotencyKey::new(key),
        turn_id: turn,
        case_ref: case(1),
        command_type: "conformance.noop".to_owned(),
        command_payload: serde_json::json!({ "key": key }),
        origin: CommandOrigin::InternalPolicy {
            policy_key: "conformance".to_owned(),
        },
        status: CommandJournalStatus::Pending,
        result: None,
        created_at,
        completed_at: None,
    }
}

/// A batch carrying `ids` for `command_id` on [`case_key`], at `revision`.
pub(super) fn event_batch(
    account_id: &AccountId,
    command_id: CommandId,
    revision: u64,
    ids: &[EventId],
    occurred_at: DateTime<Utc>,
) -> EventBatch {
    event_batch_for(
        account_id,
        &case_key(),
        command_id,
        revision,
        ids,
        occurred_at,
    )
}

/// The same, on a case the caller names.
pub(super) fn event_batch_for(
    account_id: &AccountId,
    case_key: &CaseKey,
    command_id: CommandId,
    revision: u64,
    ids: &[EventId],
    occurred_at: DateTime<Utc>,
) -> EventBatch {
    EventBatch::new(
        account_id.clone(),
        case_key.clone(),
        command_id,
        CaseRevision(revision),
        ids.iter()
            .enumerate()
            .map(|(index, id)| CommittedEvent {
                event_id: *id,
                event_type: "conformance.happened".to_owned(),
                occurred_at,
                payload: serde_json::json!({ "index": index }),
            })
            .collect(),
    )
}

/// The string the erasure checks plant in a payload.
///
/// It stands in for a name or a passport number: after a redaction it must not
/// be readable anywhere the store hands back — not in the payload, and not in
/// the erasure record, which is supposed to say *that* something was removed
/// and never *what*.
pub(super) const PERSONAL_DATA: &str = "conformance-personal-data";

/// The authority the erasure checks redact under.
pub(super) fn redaction_authority() -> RedactionAuthority {
    RedactionAuthority::from("conformance-erasure-request")
}

/// A second authority, to prove a repeated erasure does not rewrite the first.
pub(super) fn other_redaction_authority() -> RedactionAuthority {
    RedactionAuthority::from("conformance-second-erasure-request")
}

/// A batch whose payloads carry [`PERSONAL_DATA`], on [`case_key`].
pub(super) fn event_batch_with_personal_data(
    account_id: &AccountId,
    command_id: CommandId,
    revision: u64,
    ids: &[EventId],
    occurred_at: DateTime<Utc>,
) -> EventBatch {
    let mut batch = event_batch(account_id, command_id, revision, ids, occurred_at);
    for (index, event) in batch.events.iter_mut().enumerate() {
        event.payload = serde_json::json!({
            "index": index,
            "full_name": PERSONAL_DATA,
        });
    }
    batch
}

/// A `Pending` outbox row.
pub(super) fn outbox_entry(
    outbox_id: OutboxId,
    command_id: CommandId,
    key: &str,
    created_at: DateTime<Utc>,
) -> OutboxEntry {
    OutboxEntry {
        outbox_id,
        command_id,
        destination: "conformance-destination".to_owned(),
        payload: serde_json::json!({ "key": key }),
        idempotency_key: IdempotencyKey::new(key),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        created_at,
        completed_at: None,
    }
}

/// A user turn carrying plain text.
pub(super) fn user_turn(
    account_id: &AccountId,
    conversation: ConversationId,
    turn: TurnId,
    received_at: DateTime<Utc>,
) -> StoredUserTurn {
    StoredUserTurn::new(
        TurnInput {
            turn_id: turn,
            conversation_id: conversation,
            actor: ActorContext::new(account_id.clone(), UserId::from("conformance-user")),
            text: Some("conformance turn".to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("en"),
            effort: None,
        },
        received_at,
    )
}

/// An assistant turn with three blocks of different kinds, one of them a card.
///
/// The card matters: spec §22.3 forbids rebuilding an interaction from free
/// text on reload, so a round-trip that only carried prose would prove nothing.
pub(super) fn assistant_turn(
    conversation: ConversationId,
    turn: TurnId,
    interaction: &Interaction,
) -> AssistantTurn {
    AssistantTurn {
        turn_id: turn,
        conversation_id: conversation,
        blocks: vec![
            ResponseBlock::Notice(ServerNotice {
                block_id: BlockId::from("block-notice"),
                code: "turnframe.notice.conformance".to_owned(),
                severity: NoticeSeverity::Info,
                text: "a server-authored notice".into(),
            }),
            ResponseBlock::Interaction(InteractionBlock {
                block_id: BlockId::from("block-card"),
                view: interaction.view(),
            }),
            ResponseBlock::Artifact(ArtifactView {
                block_id: BlockId::from("block-artifact"),
                artifact: ArtifactRef {
                    artifact_id: "conformance-artifact".to_owned(),
                    kind: "conformance".to_owned(),
                    label: "an artifact".into(),
                    uri: None,
                    media_type: None,
                },
            }),
        ],
        subjects: Vec::new(),
        expectations: Vec::new(),
        replay_token: ReplayToken::new("conformance-replay-token"),
        done: Vec::new(),
        offers: Vec::new(),
    }
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

/// Unwraps a store result, or fails the check naming the operation.
pub(super) fn ensure_ok<T>(
    check: &'static str,
    what: &str,
    result: Result<T, StoreError>,
) -> Result<T, ConformanceFailure> {
    result.map_err(|error| {
        ConformanceFailure::new(check, format!("{what}: expected success, got {error:?}"))
    })
}

/// Fails the check unless the operation refused with exactly `expected`.
pub(super) fn ensure_error<T: fmt::Debug>(
    check: &'static str,
    what: &str,
    result: Result<T, StoreError>,
    expected: &StoreError,
) -> Result<(), ConformanceFailure> {
    match result {
        Err(ref error) if error == expected => Ok(()),
        other => Err(ConformanceFailure::new(
            check,
            format!("{what}: expected {expected:?}, got {other:?}"),
        )),
    }
}

/// Fails the check unless the operation refused with `Other { code }`.
pub(super) fn ensure_code<T: fmt::Debug>(
    check: &'static str,
    what: &str,
    result: Result<T, StoreError>,
    code: &str,
) -> Result<(), ConformanceFailure> {
    match result {
        Err(ref error) if crate::error::has_code(error, code) => Ok(()),
        other => Err(ConformanceFailure::new(
            check,
            format!("{what}: expected store code {code}, got {other:?}"),
        )),
    }
}
