//! Checks for [`ReplayStore`](crate::replay::ReplayStore).

use turnframe_core::ids::{ConversationId, TurnId};
use turnframe_core::replay::{ReplayRecord, TurnPhase};

use super::ConformanceFailure;
use super::fixtures::{account, at, ensure_eq, ensure_error, ensure_ok, epoch, other_account};
use crate::error::StoreError;
use crate::stores::Stores;

/// One record per turn, upserted as the turn advances (I20, spec §23.1).
///
/// The runtime writes the record at `Received` and rewrites it with more detail
/// at every step, so `put` must replace rather than accumulate: two records for
/// one turn would make "why did the assistant do that?" ambiguous.
pub async fn check_replay_put_get(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_replay_put_get";
    let replay = stores.replay();
    let account = account();
    let conversation = ConversationId::new();
    let other_conversation = ConversationId::new();
    let turn = TurnId::new();

    let received = ReplayRecord::received(turn, conversation, account.clone(), epoch());
    ensure_ok(
        CHECK,
        "writing the record at Received",
        replay.put(received.clone()).await,
    )?;
    let loaded = ensure_ok(CHECK, "reading it back", replay.get(&account, &turn).await)?;
    ensure_eq(CHECK, "the record read back", &loaded, &received)?;

    let mut advanced = received.clone();
    advanced.phase = TurnPhase::Committed;
    advanced.recorded_at = at(1);
    advanced.event_ids = vec![turnframe_core::ids::EventId::new()];
    ensure_ok(
        CHECK,
        "rewriting the record as the turn advances",
        replay.put(advanced.clone()).await,
    )?;
    let rewritten = ensure_ok(
        CHECK,
        "reading the rewritten record",
        replay.get(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "put must replace the previous version of the record",
        &rewritten,
        &advanced,
    )?;

    let listed = ensure_ok(
        CHECK,
        "listing the records of the conversation",
        replay
            .list_for_conversation(&account, &conversation, 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "one record per turn, not one per write",
        &listed.len(),
        &1,
    )?;

    // Ordering and the limit: the most recent records, oldest of them first.
    let older = TurnId::new();
    let newer = TurnId::new();
    for (id, seconds) in [(older, 2), (newer, 3)] {
        ensure_ok(
            CHECK,
            "writing another record of the conversation",
            replay
                .put(ReplayRecord::received(
                    id,
                    conversation,
                    account.clone(),
                    at(seconds),
                ))
                .await,
        )?;
    }
    ensure_ok(
        CHECK,
        "writing a record of another conversation",
        replay
            .put(ReplayRecord::received(
                TurnId::new(),
                other_conversation,
                account.clone(),
                at(4),
            ))
            .await,
    )?;

    let recent = ensure_ok(
        CHECK,
        "listing the two most recent records",
        replay
            .list_for_conversation(&account, &conversation, 2)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the most recent records, in chronological order",
        &recent
            .iter()
            .map(|record| record.turn_id)
            .collect::<Vec<_>>(),
        &vec![older, newer],
    )?;

    ensure_error(
        CHECK,
        "reading a record that does not exist",
        replay.get(&account, &TurnId::new()).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading the record as another tenant",
        replay.get(&other_account(), &turn).await,
        &StoreError::NotFound,
    )
}
