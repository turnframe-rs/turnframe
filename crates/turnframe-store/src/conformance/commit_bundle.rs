//! Checks for [`CommitStore`](crate::commit::CommitStore).

use turnframe_core::ids::{
    CaseRevision, CommandId, ConversationId, EventId, InteractionId, OptionId, OutboxId, TurnId,
};
use turnframe_core::interaction::InteractionStatus;
use turnframe_core::replay::{ReplayRecord, TurnPhase};

use super::ConformanceFailure;
use super::fixtures::{
    account, at, card, card_spec, case, case_key, ensure, ensure_code, ensure_eq, ensure_error,
    ensure_ok, epoch, event_batch, journal_entry, other_account, other_case, outbox_entry,
    user_turn,
};
use crate::commit::CommitBundle;
use crate::conversation::ConversationRecord;
use crate::error::{StoreError, codes};
use crate::interaction::{InvalidationReason, ResolutionOutcome};
use crate::journal::{CommandJournalStatus, JournalOutcome};
use crate::stores::Stores;

/// A bundle writes every item it carries (spec §16.3, §23 step N).
///
/// One turn's bookkeeping — the journal outcome, the events, the card that
/// authorized them, the cards the new revision invalidates, the new card, the
/// outbox row, the replay record and the phase marker — travels together and
/// must land together.
pub async fn check_commit_bundle_applies_all(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_commit_bundle_applies_all";
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let command = CommandId::new();

    ensure_ok(
        CHECK,
        "creating the conversation",
        stores
            .conversations()
            .create_conversation(ConversationRecord::new(
                conversation,
                account.clone(),
                epoch(),
            ))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "appending the user turn",
        stores
            .conversations()
            .append_user_turn(user_turn(&account, conversation, turn, epoch()))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "admitting the command",
        stores
            .journal()
            .begin(journal_entry(&account, turn, command, "bundle", epoch()))
            .await,
    )?;

    // The card the user answered: it is Resolving while the command runs.
    let answered = card(
        CHECK,
        card_spec("answered", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the answered card",
        stores.interactions().insert(answered.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "starting resolution of the answered card",
        stores
            .interactions()
            .begin_resolution(
                &account,
                &answered.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    // A card left over from the previous revision, which the commit invalidates.
    let stale = card(
        CHECK,
        card_spec("stale", case(1)).non_blocking(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    ensure_ok(
        CHECK,
        "inserting the stale card",
        stores.interactions().insert(stale.clone()).await,
    )?;

    let event_ids = vec![EventId::new(), EventId::new()];
    let next = card(
        CHECK,
        card_spec("next", case(2)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(2),
    )?;
    let outbox_id = OutboxId::new();
    let mut replay_record = ReplayRecord::received(turn, conversation, account.clone(), at(2));
    replay_record.phase = TurnPhase::Committed;
    replay_record.event_ids = event_ids.clone();

    let bundle = CommitBundle::new()
        .with_journal_completion(
            command,
            JournalOutcome::Committed {
                new_revision: CaseRevision(2),
                event_ids: event_ids.clone(),
            },
        )
        .with_events(event_batch(&account, command, 2, &event_ids, at(2)))
        .with_interaction_finish(
            answered.id,
            ResolutionOutcome::Resolved {
                event_ids: event_ids.clone(),
            },
        )
        .with_invalidation(
            case_key(),
            CaseRevision(2),
            InvalidationReason::RevisionChanged,
        )
        .with_interaction_insert(next.clone(), true)
        .with_outbox_entry(outbox_entry(outbox_id, command, "bundle", at(2)))
        .with_replay_record(replay_record.clone())
        .with_turn_phase(turn, TurnPhase::Committed);

    let receipt = ensure_ok(
        CHECK,
        "committing the bundle",
        stores.commit().commit(&account, bundle).await,
    )?;
    ensure_eq(
        CHECK,
        "events reported by the receipt",
        &receipt.event_ids,
        &event_ids,
    )?;
    ensure_eq(
        CHECK,
        "cards inserted by the bundle",
        &receipt.inserted_interactions,
        &vec![next.id],
    )?;
    ensure(
        CHECK,
        receipt.invalidated_interactions.contains(&stale.id),
        "the receipt must report the card the new revision invalidated",
    )?;

    let entry = ensure_ok(
        CHECK,
        "reading the journal entry",
        stores.journal().get(&account, &command).await,
    )?;
    ensure_eq(
        CHECK,
        "the journal entry is settled",
        &entry.status,
        &CommandJournalStatus::Committed,
    )?;
    let committed = ensure_ok(
        CHECK,
        "reading the events back",
        stores.events().get_by_ids(&account, &event_ids).await,
    )?;
    ensure_eq(CHECK, "events in the ledger", &committed.len(), &2)?;
    let settled = ensure_ok(
        CHECK,
        "reading the answered card",
        stores.interactions().get(&account, &answered.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the answered card is resolved",
        &settled.status(),
        &InteractionStatus::Resolved,
    )?;
    let invalidated = ensure_ok(
        CHECK,
        "reading the stale card",
        stores.interactions().get(&account, &stale.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the stale card is invalidated",
        &invalidated.status(),
        &InteractionStatus::Invalidated,
    )?;
    let inserted = ensure_ok(
        CHECK,
        "reading the new card",
        stores.interactions().get(&account, &next.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the new card is active",
        &inserted.status(),
        &InteractionStatus::Active,
    )?;
    ensure_ok(
        CHECK,
        "reading the outbox row",
        stores.outbox().get(&outbox_id).await,
    )?;
    let replayed = ensure_ok(
        CHECK,
        "reading the replay record",
        stores.replay().get(&account, &turn).await,
    )?;
    ensure_eq(CHECK, "the replay record", &replayed, &replay_record)?;
    let marker = ensure_ok(
        CHECK,
        "reading the phase marker",
        stores.conversations().turn_phase(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "the phase marker moved with the bundle",
        &marker.phase,
        &TurnPhase::Committed,
    )
}

/// A bundle that fails writes nothing at all (spec §16.3).
///
/// The failure is injected the way any implementation can reproduce it: the
/// last item of an otherwise valid bundle addresses a turn that does not exist.
/// Everything before it — events, a card, an outbox row, a replay record —
/// would have been written by a store that applied items one at a time, so the
/// check afterwards is that none of them is visible.
pub async fn check_commit_bundle_atomic_on_invalid_item(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_commit_bundle_atomic_on_invalid_item";
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let command = CommandId::new();

    let event_ids = vec![EventId::new()];
    let inserted = card(
        CHECK,
        card_spec("never-written", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    let outbox_id = OutboxId::new();

    let bundle = CommitBundle::new()
        .with_events(event_batch(&account, command, 1, &event_ids, epoch()))
        .with_interaction_insert(inserted.clone(), false)
        .with_outbox_entry(outbox_entry(outbox_id, command, "atomic", epoch()))
        .with_replay_record(ReplayRecord::received(
            turn,
            conversation,
            account.clone(),
            epoch(),
        ))
        // The turn was never appended, so this last item cannot apply.
        .with_turn_phase(turn, TurnPhase::Committed);

    ensure_error(
        CHECK,
        "committing a bundle whose last item is illegal",
        stores.commit().commit(&account, bundle).await,
        &StoreError::NotFound,
    )?;

    let events = ensure_ok(
        CHECK,
        "reading the events of the failed bundle",
        stores.events().get_by_ids(&account, &event_ids).await,
    )?;
    ensure(
        CHECK,
        events.is_empty(),
        "no event of a failed bundle may be visible",
    )?;
    let count = ensure_ok(
        CHECK,
        "counting the events of the case",
        stores.events().count(&account, &case_key()).await,
    )?;
    ensure_eq(CHECK, "events on the case after the failure", &count, &0)?;
    ensure_error(
        CHECK,
        "reading the card of the failed bundle",
        stores.interactions().get(&account, &inserted.id).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading the outbox row of the failed bundle",
        stores.outbox().get(&outbox_id).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading the replay record of the failed bundle",
        stores.replay().get(&account, &turn).await,
        &StoreError::NotFound,
    )?;

    // The same bundle without the illegal item lands in full, which proves the
    // rollback above was the item's fault and not a store that writes nothing.
    let recovered = CommitBundle::new()
        .with_events(event_batch(&account, command, 1, &event_ids, epoch()))
        .with_interaction_insert(inserted.clone(), false)
        .with_outbox_entry(outbox_entry(outbox_id, command, "atomic", epoch()));
    ensure_ok(
        CHECK,
        "committing the same bundle without the illegal item",
        stores.commit().commit(&account, recovered).await,
    )?;
    let replayed_events = ensure_ok(
        CHECK,
        "reading the events after the successful commit",
        stores.events().get_by_ids(&account, &event_ids).await,
    )?;
    ensure_eq(
        CHECK,
        "events visible after the successful commit",
        &replayed_events.len(),
        &1,
    )
}

/// A bundle refuses an item belonging to another tenant, before writing
/// anything (spec §25.4).
pub async fn check_commit_bundle_rejects_foreign_account_items(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_commit_bundle_rejects_foreign_account_items";
    let account = account();
    let intruder = other_account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let command = CommandId::new();
    let event_ids = vec![EventId::new()];

    ensure_code(
        CHECK,
        "a bundle carrying another tenant's events",
        stores
            .commit()
            .commit(
                &account,
                CommitBundle::new().with_events(event_batch(
                    &intruder,
                    command,
                    1,
                    &event_ids,
                    epoch(),
                )),
            )
            .await,
        codes::BUNDLE_ACCOUNT_MISMATCH,
    )?;

    let foreign_card = card(
        CHECK,
        card_spec("foreign", case(1)),
        InteractionId::new(),
        intruder.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_code(
        CHECK,
        "a bundle carrying another tenant's card",
        stores
            .commit()
            .commit(
                &account,
                CommitBundle::new().with_interaction_insert(foreign_card.clone(), false),
            )
            .await,
        codes::BUNDLE_ACCOUNT_MISMATCH,
    )?;
    ensure_code(
        CHECK,
        "a bundle carrying another tenant's replay record",
        stores
            .commit()
            .commit(
                &account,
                CommitBundle::new().with_replay_record(ReplayRecord::received(
                    turn,
                    conversation,
                    intruder.clone(),
                    epoch(),
                )),
            )
            .await,
        codes::BUNDLE_ACCOUNT_MISMATCH,
    )?;
    ensure_code(
        CHECK,
        "a bundle carrying an empty event batch",
        stores
            .commit()
            .commit(
                &account,
                CommitBundle::new().with_events(event_batch(&account, command, 1, &[], epoch())),
            )
            .await,
        codes::INVALID_RECORD,
    )?;

    ensure_error(
        CHECK,
        "a refused bundle must not have written the foreign card",
        stores.interactions().get(&intruder, &foreign_card.id).await,
        &StoreError::NotFound,
    )?;
    let events = ensure_ok(
        CHECK,
        "reading the refused events as their own tenant",
        stores.events().get_by_ids(&intruder, &event_ids).await,
    )?;
    ensure(
        CHECK,
        events.is_empty(),
        "a refused bundle must not have written the foreign events",
    )
}

/// A failed bundle puts back what it changed, not only what it created
/// (spec §16.3).
///
/// A store that only forgets its *inserts* looks atomic until a bundle settles
/// something. This one modifies every record kind a commit can modify — it
/// settles a journal entry, resolves the card that authorized it, invalidates a
/// stale card, supersedes the blocking card of another case, and overwrites an
/// existing replay record — and then fails on the last item. Each of those
/// records must read back exactly as it did before the bundle, down to the
/// fields the change would have set.
pub async fn check_commit_bundle_restores_modified_records(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_commit_bundle_restores_modified_records";
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let command = CommandId::new();

    ensure_ok(
        CHECK,
        "creating the conversation",
        stores
            .conversations()
            .create_conversation(ConversationRecord::new(
                conversation,
                account.clone(),
                epoch(),
            ))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "appending the user turn",
        stores
            .conversations()
            .append_user_turn(user_turn(&account, conversation, turn, epoch()))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "admitting the command",
        stores
            .journal()
            .begin(journal_entry(&account, turn, command, "restore", epoch()))
            .await,
    )?;

    // The card the user answered, mid-resolution.
    let answered = card(
        CHECK,
        card_spec("answered", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the answered card",
        stores.interactions().insert(answered.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "starting resolution of the answered card",
        stores
            .interactions()
            .begin_resolution(
                &account,
                &answered.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    // A revision-bound card the invalidation would sweep.
    let stale = card(
        CHECK,
        card_spec("stale", case(1)).non_blocking(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    ensure_ok(
        CHECK,
        "inserting the stale card",
        stores.interactions().insert(stale.clone()).await,
    )?;
    // The blocking card of another case, which a replacing insert would
    // supersede.
    let occupant = card(
        CHECK,
        card_spec("occupant", other_case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    ensure_ok(
        CHECK,
        "inserting the blocking card of the other case",
        stores.interactions().insert(occupant.clone()).await,
    )?;
    // A replay record the bundle would overwrite.
    let original_replay = ReplayRecord::received(turn, conversation, account.clone(), epoch());
    ensure_ok(
        CHECK,
        "storing the replay record of the turn",
        stores.replay().put(original_replay.clone()).await,
    )?;

    let event_ids = vec![EventId::new()];
    let replacement = card(
        CHECK,
        card_spec("replacement", other_case(2)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(2),
    )?;
    let mut new_replay = original_replay.clone();
    new_replay.phase = TurnPhase::Committed;
    new_replay.event_ids = event_ids.clone();

    let bundle = CommitBundle::new()
        .with_journal_completion(
            command,
            JournalOutcome::Committed {
                new_revision: CaseRevision(2),
                event_ids: event_ids.clone(),
            },
        )
        .with_events(event_batch(&account, command, 2, &event_ids, at(2)))
        .with_interaction_finish(
            answered.id,
            ResolutionOutcome::Resolved {
                event_ids: event_ids.clone(),
            },
        )
        .with_invalidation(
            case_key(),
            CaseRevision(2),
            InvalidationReason::RevisionChanged,
        )
        .with_interaction_insert(replacement.clone(), true)
        .with_outbox_entry(outbox_entry(OutboxId::new(), command, "restore", at(2)))
        .with_replay_record(new_replay)
        // The turn this phase marker names was never appended, so the bundle
        // fails after every modification above has been made.
        .with_turn_phase(TurnId::new(), TurnPhase::Committed);

    ensure_error(
        CHECK,
        "committing a bundle whose last item is illegal",
        stores.commit().commit(&account, bundle).await,
        &StoreError::NotFound,
    )?;

    let entry = ensure_ok(
        CHECK,
        "reading the journal entry after the failure",
        stores.journal().get(&account, &command).await,
    )?;
    ensure_eq(
        CHECK,
        "the journal entry must not have been settled",
        &entry.status,
        &CommandJournalStatus::Pending,
    )?;
    ensure(
        CHECK,
        entry.result.is_none() && entry.completed_at.is_none(),
        "a rolled-back journal entry must carry neither an outcome nor a completion time",
    )?;

    let count = ensure_ok(
        CHECK,
        "counting the events of the case",
        stores.events().count(&account, &case_key()).await,
    )?;
    ensure_eq(CHECK, "events on the case after the failure", &count, &0)?;

    let answered_now = ensure_ok(
        CHECK,
        "reading the answered card after the failure",
        stores.interactions().get(&account, &answered.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the answered card must still be resolving",
        &answered_now.status(),
        &InteractionStatus::Resolving,
    )?;
    ensure(
        CHECK,
        answered_now.resolution_event_ids.is_empty(),
        "a rolled-back resolution must not leave the events it would have cited",
    )?;

    let stale_now = ensure_ok(
        CHECK,
        "reading the stale card after the failure",
        stores.interactions().get(&account, &stale.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the stale card must still be active",
        &stale_now.status(),
        &InteractionStatus::Active,
    )?;
    ensure(
        CHECK,
        stale_now.invalidation.is_none(),
        "a rolled-back invalidation must not leave its reason behind",
    )?;

    let occupant_now = ensure_ok(
        CHECK,
        "reading the blocking card of the other case after the failure",
        stores.interactions().get(&account, &occupant.id).await,
    )?;
    ensure_eq(
        CHECK,
        "the blocking card must still hold its slot",
        &occupant_now.status(),
        &InteractionStatus::Active,
    )?;
    ensure_error(
        CHECK,
        "reading the card the failed bundle would have inserted",
        stores.interactions().get(&account, &replacement.id).await,
        &StoreError::NotFound,
    )?;

    let replay_now = ensure_ok(
        CHECK,
        "reading the replay record after the failure",
        stores.replay().get(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "the replay record must be the one that was there before",
        &replay_now,
        &original_replay,
    )?;

    let marker = ensure_ok(
        CHECK,
        "reading the phase of the turn after the failure",
        stores.conversations().turn_phase(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "the phase of the turn must not have moved",
        &marker.phase,
        &TurnPhase::Received,
    )?;

    // And the store still works: the same bundle, with a phase marker that
    // names the real turn, lands in full.
    let mut committed_replay = original_replay.clone();
    committed_replay.phase = TurnPhase::Committed;
    let good = CommitBundle::new()
        .with_journal_completion(
            command,
            JournalOutcome::Committed {
                new_revision: CaseRevision(2),
                event_ids: event_ids.clone(),
            },
        )
        .with_events(event_batch(&account, command, 2, &event_ids, at(2)))
        .with_interaction_finish(
            answered.id,
            ResolutionOutcome::Resolved {
                event_ids: event_ids.clone(),
            },
        )
        .with_invalidation(
            case_key(),
            CaseRevision(2),
            InvalidationReason::RevisionChanged,
        )
        .with_interaction_insert(replacement.clone(), true)
        .with_replay_record(committed_replay)
        .with_turn_phase(turn, TurnPhase::Committed);
    let receipt = ensure_ok(
        CHECK,
        "committing the same bundle with a phase marker that names the turn",
        stores.commit().commit(&account, good).await,
    )?;
    ensure_eq(
        CHECK,
        "the successful bundle appends its events",
        &receipt.event_ids,
        &event_ids,
    )?;
    ensure_eq(
        CHECK,
        "the successful bundle supersedes the blocking occupant",
        &receipt.invalidated_interactions.contains(&occupant.id),
        &true,
    )?;
    let settled = ensure_ok(
        CHECK,
        "reading the journal entry after the successful commit",
        stores.journal().get(&account, &command).await,
    )?;
    ensure_eq(
        CHECK,
        "the journal entry after the successful commit",
        &settled.status,
        &CommandJournalStatus::Committed,
    )
}
