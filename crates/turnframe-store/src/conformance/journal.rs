//! Checks for [`CommandJournal`](crate::journal::CommandJournal).

use turnframe_core::ids::{CaseRevision, CommandId, EventId, TurnId};

use super::ConformanceFailure;
use super::fixtures::{
    account, at, ensure, ensure_eq, ensure_error, ensure_ok, epoch, journal_entry, other_account,
};
use crate::error::StoreError;
use crate::journal::{CommandJournalStatus, JournalAdmission, JournalOutcome};
use crate::stores::Stores;

/// One `Fresh` per idempotency key, ever; every repeat replays the persisted
/// entry and its outcome (I14, spec §16.2).
///
/// This is the rule that stops a retried request from charging a traveler
/// twice, so it is checked from every angle: before the outcome exists, after
/// it exists, and with a key deliberately reused for a different command.
pub async fn check_journal_idempotency_replay(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_journal_idempotency_replay";
    let journal = stores.journal();
    let account = account();
    let turn = TurnId::new();
    let first_id = CommandId::new();
    let first = journal_entry(&account, turn, first_id, "shared-key", epoch());

    let admission = ensure_ok(
        CHECK,
        "admitting the command",
        journal.begin(first.clone()).await,
    )?;
    ensure_eq(
        CHECK,
        "the first admission of a key",
        &admission,
        &JournalAdmission::Fresh,
    )?;

    // Before any outcome exists, the key already replays.
    let second_id = CommandId::new();
    let second = journal_entry(&account, turn, second_id, "shared-key", at(1));
    let replay = ensure_ok(
        CHECK,
        "admitting the same key again",
        journal.begin(second.clone()).await,
    )?;
    match &replay {
        JournalAdmission::Fresh => {
            return Err(ConformanceFailure::new(
                CHECK,
                "a key was admitted Fresh twice; there must be exactly one Fresh per key",
            ));
        }
        JournalAdmission::Replay(entry) => {
            ensure_eq(
                CHECK,
                "the replayed entry keeps the original command id",
                &entry.command_id,
                &first_id,
            )?;
            ensure(
                CHECK,
                entry.same_command(&second),
                "the replayed entry describes the same command as the fixture",
            )?;
        }
    }

    let outcome = JournalOutcome::Committed {
        new_revision: CaseRevision(2),
        event_ids: vec![EventId::new()],
    };
    ensure_ok(
        CHECK,
        "marking the command executing",
        journal.mark_executing(&account, &first_id).await,
    )?;
    ensure_ok(
        CHECK,
        "recording the outcome",
        journal.complete(&account, &first_id, outcome.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "recording the same outcome again",
        journal.complete(&account, &first_id, outcome.clone()).await,
    )?;
    ensure_error(
        CHECK,
        "recording a different outcome",
        journal
            .complete(
                &account,
                &first_id,
                JournalOutcome::Failed {
                    code: "late".to_owned(),
                },
            )
            .await,
        &StoreError::Conflict,
    )?;

    // After the outcome exists, the replay must carry it: that is what lets the
    // caller answer a retry without executing anything.
    let after = ensure_ok(
        CHECK,
        "admitting the key once the outcome exists",
        journal.begin(second).await,
    )?;
    let replayed = after.replayed().ok_or_else(|| {
        ConformanceFailure::new(CHECK, "a settled key must replay, never admit again")
    })?;
    ensure_eq(
        CHECK,
        "the replayed outcome",
        &replayed.result,
        &Some(outcome),
    )?;
    ensure_eq(
        CHECK,
        "the replayed status",
        &replayed.status,
        &CommandJournalStatus::Committed,
    )?;

    // A key reused for a different command still replays; detecting the misuse
    // is the runtime's job, through `same_command`.
    let mut different = journal_entry(&account, turn, CommandId::new(), "shared-key", at(2));
    different.command_payload = serde_json::json!({ "key": "something-else" });
    let mismatch = ensure_ok(
        CHECK,
        "admitting the key with a different payload",
        journal.begin(different.clone()).await,
    )?;
    let mismatched = mismatch.replayed().ok_or_else(|| {
        ConformanceFailure::new(CHECK, "a reused key must replay, never admit again")
    })?;
    ensure(
        CHECK,
        !mismatched.same_command(&different),
        "a key reused for another payload must be detectable with same_command",
    )?;

    // The same identifier under a different key is a different command.
    let clashing = journal_entry(&account, turn, first_id, "another-key", at(3));
    ensure_error(
        CHECK,
        "reusing a command id under another key",
        journal.begin(clashing).await,
        &StoreError::Conflict,
    )?;

    // Keys are scoped per account: the same key in another tenant is fresh.
    let elsewhere = journal_entry(
        &other_account(),
        turn,
        CommandId::new(),
        "shared-key",
        at(4),
    );
    let foreign = ensure_ok(
        CHECK,
        "admitting the same key in another tenant",
        journal.begin(elsewhere).await,
    )?;
    ensure_eq(
        CHECK,
        "idempotency keys are scoped per account",
        &foreign,
        &JournalAdmission::Fresh,
    )
}

/// A turn interrupted between admission and outcome is found by its unfinished
/// entries (spec §23.1).
///
/// The crash is simulated the way any store can reproduce it: entries are
/// admitted and left in `Pending` and `Executing` while a third one settles.
/// Recovery must see exactly the first two, in a stable order, so it can resume
/// them by idempotency key instead of re-interpreting the turn.
pub async fn check_pending_for_turn_after_partial_write(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_pending_for_turn_after_partial_write";
    let journal = stores.journal();
    let account = account();
    let turn = TurnId::new();
    let other_turn = TurnId::new();

    let pending = CommandId::new();
    let executing = CommandId::new();
    let committed = CommandId::new();
    for (id, key, seconds) in [
        (pending, "pending", 0_i64),
        (executing, "executing", 1),
        (committed, "committed", 2),
    ] {
        ensure_ok(
            CHECK,
            &format!("admitting the {key} command"),
            journal
                .begin(journal_entry(&account, turn, id, key, at(seconds)))
                .await,
        )?;
    }
    ensure_ok(
        CHECK,
        "admitting a command of another turn",
        journal
            .begin(journal_entry(
                &account,
                other_turn,
                CommandId::new(),
                "other-turn",
                at(9),
            ))
            .await,
    )?;

    ensure_ok(
        CHECK,
        "marking the second command executing",
        journal.mark_executing(&account, &executing).await,
    )?;
    ensure_ok(
        CHECK,
        "marking it executing again",
        journal.mark_executing(&account, &executing).await,
    )?;
    ensure_ok(
        CHECK,
        "settling the third command",
        journal
            .complete(
                &account,
                &committed,
                JournalOutcome::Committed {
                    new_revision: CaseRevision(1),
                    event_ids: Vec::new(),
                },
            )
            .await,
    )?;

    let unfinished = ensure_ok(
        CHECK,
        "listing the unfinished entries of the turn",
        journal.pending_for_turn(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "unfinished entries of the interrupted turn",
        &unfinished
            .iter()
            .map(|entry| entry.command_id)
            .collect::<Vec<_>>(),
        &vec![pending, executing],
    )?;
    ensure_eq(
        CHECK,
        "status of the first unfinished entry",
        &unfinished[0].status,
        &CommandJournalStatus::Pending,
    )?;
    ensure_eq(
        CHECK,
        "status of the second unfinished entry",
        &unfinished[1].status,
        &CommandJournalStatus::Executing,
    )?;

    let all = ensure_ok(
        CHECK,
        "listing every entry of the turn",
        journal.for_turn(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "every entry of the turn, in creation order",
        &all.iter().map(|entry| entry.command_id).collect::<Vec<_>>(),
        &vec![pending, executing, committed],
    )?;

    // A settled command must never be resumed.
    ensure_error(
        CHECK,
        "moving a settled entry back to Executing",
        journal.mark_executing(&account, &committed).await,
        &StoreError::Conflict,
    )?;

    let foreign = ensure_ok(
        CHECK,
        "listing the unfinished entries as another tenant",
        journal.pending_for_turn(&other_account(), &turn).await,
    )?;
    ensure(
        CHECK,
        foreign.is_empty(),
        "recovery of one tenant must not see another tenant's entries",
    )
}

/// An entry journaled for a confirmation card is listed with its turn but never
/// as unfinished work, and confirming it moves it to `Executing` like any other.
pub async fn check_awaiting_confirmation_is_never_resumed(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_awaiting_confirmation_is_never_resumed";
    let journal = stores.journal();
    let account = account();
    let turn = TurnId::new();
    let awaiting = CommandId::new();
    let mut entry = journal_entry(&account, turn, awaiting, "awaiting", epoch());
    entry.status = CommandJournalStatus::AwaitingConfirmation;
    ensure_ok(
        CHECK,
        "journaling a command for a card",
        journal.begin(entry).await,
    )?;

    let unfinished = ensure_ok(
        CHECK,
        "listing the unfinished entries of the turn",
        journal.pending_for_turn(&account, &turn).await,
    )?;
    ensure(
        CHECK,
        unfinished.is_empty(),
        "a command awaiting its confirmation was listed as unfinished work",
    )?;
    let all = ensure_ok(
        CHECK,
        "listing every entry of the turn",
        journal.for_turn(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "its status as stored",
        &all.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        &vec![CommandJournalStatus::AwaitingConfirmation],
    )?;

    ensure_ok(
        CHECK,
        "moving it to Executing once confirmed",
        journal.mark_executing(&account, &awaiting).await,
    )?;
    let resumed = ensure_ok(
        CHECK,
        "listing the unfinished entries again",
        journal.pending_for_turn(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "a confirmed command in flight is unfinished work",
        &resumed
            .iter()
            .map(|entry| entry.command_id)
            .collect::<Vec<_>>(),
        &vec![awaiting],
    )
}
