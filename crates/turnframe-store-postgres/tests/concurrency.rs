//! What the in-memory store cannot prove.
//!
//! The conformance suite says this adapter obeys the persistence contract; it
//! runs one operation at a time, which is all a single-process store can be
//! asked to do. These tests cover the other half — the claims that are only
//! true because PostgreSQL is underneath, and that would still pass if the
//! adapter had quietly replaced a compare-and-swap with a read followed by a
//! write.
//!
//! Every race here uses two independent pools, so the two sides are two real
//! connections in two real transactions, and repeats itself enough times that a
//! lost update would have to be lucky every round to stay hidden.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use std::sync::Arc;

use tokio::sync::Barrier;
use turnframe_core::ids::{
    CaseRevision, CommandId, EventId, InteractionId, OptionId, OutboxId, RedactionAuthority, TurnId,
};
use turnframe_core::interaction::InteractionStatus;
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_store::commit::{CommitBundle, CommitStore};
use turnframe_store::error::StoreError;
use turnframe_store::events::{EventJournalReader, EventJournalWriter};
use turnframe_store::interaction::{InteractionReader, InteractionWriter, InvalidationReason};
use turnframe_store::outbox::{OutboxReader, OutboxWriter};
use turnframe_store::replay::ReplayReader;

/// The schema the account-scoped tests of this binary share.
const SCHEMA: &str = "tf_test_concurrency";
/// The outbox is a queue with no tenant, so the test that sweeps it needs a
/// schema nobody else writes to.
const OUTBOX_SCHEMA: &str = "tf_test_outbox_claim";
/// How many times a race is repeated before it is believed.
const ROUNDS: usize = 8;

/// Two transactions invalidate the same case at the same expected revision.
///
/// One statement does the selecting and the writing, so the loser does not
/// re-read a stale snapshot and overwrite the winner: it waits on the row lock,
/// re-evaluates `status = 'active'` against the row the winner committed, and
/// updates nothing. Exactly one caller is told it invalidated the card, which is
/// what stops two commits both believing they retired it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_transactions_racing_the_same_expected_revision_only_one_wins() {
    let Some(url) = support::database_url() else {
        support::skipped("two_transactions_racing_the_same_expected_revision_only_one_wins");
        return;
    };
    let first = support::co_tenant_of(&url, SCHEMA).await;
    let second = support::second_pool(&url, SCHEMA).await;
    let account = support::unique_account("revision-race");

    for round in 0..ROUNDS {
        let case_id = format!("case-{round}");
        let bound = support::card(&account, support::case(&case_id, 1), support::at(0));
        first.insert(bound.clone()).await.unwrap();

        let gate = Arc::new(Barrier::new(2));
        let key = support::case(&case_id, 0).key();
        let left = {
            let (store, account, key, gate) =
                (first.clone(), account.clone(), key.clone(), gate.clone());
            tokio::spawn(async move {
                gate.wait().await;
                store
                    .invalidate_for_case(
                        &account,
                        &key,
                        CaseRevision(2),
                        InvalidationReason::RevisionChanged,
                    )
                    .await
            })
        };
        let right = {
            let (store, account, key, gate) = (second.clone(), account.clone(), key, gate);
            tokio::spawn(async move {
                gate.wait().await;
                store
                    .invalidate_for_case(
                        &account,
                        &key,
                        CaseRevision(2),
                        InvalidationReason::RevisionChanged,
                    )
                    .await
            })
        };
        let (left, right) = (left.await.unwrap().unwrap(), right.await.unwrap().unwrap());

        assert_eq!(
            left.len() + right.len(),
            1,
            "round {round}: exactly one caller may invalidate the card, got {left:?} and {right:?}"
        );
        let winner = if left.is_empty() { &right } else { &left };
        assert_eq!(winner, &vec![bound.id], "round {round}");

        // And the card really moved, once, with the revision that retired it.
        let stale = InteractionReader::get(&first, &account, &bound.id)
            .await
            .unwrap();
        assert_eq!(stale.status(), InteractionStatus::Invalidated);
        assert_eq!(
            stale.invalidation.and_then(|record| record.new_revision),
            Some(CaseRevision(2)),
            "round {round}"
        );
    }
}

/// Two clicks on the same card, from two connections, at the same instant.
///
/// The compare-and-swap and the write are one statement, so the second click
/// loses on the row rather than silently re-resolving a card that is already
/// executing its commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_answer_to_the_same_card_loses_the_compare_and_swap() {
    let Some(url) = support::database_url() else {
        support::skipped("a_second_answer_to_the_same_card_loses_the_compare_and_swap");
        return;
    };
    let first = support::co_tenant_of(&url, SCHEMA).await;
    let second = support::second_pool(&url, SCHEMA).await;
    let account = support::unique_account("resolution-race");

    for round in 0..ROUNDS {
        let card = support::card(
            &account,
            support::case(&format!("case-{round}"), 1),
            support::at(0),
        );
        first.insert(card.clone()).await.unwrap();
        let turn = TurnId::new();

        let gate = Arc::new(Barrier::new(2));
        let mut answers = Vec::new();
        for store in [first.clone(), second.clone()] {
            let (account, id, gate) = (account.clone(), card.id, gate.clone());
            answers.push(tokio::spawn(async move {
                gate.wait().await;
                store
                    .begin_resolution(
                        &account,
                        &id,
                        InteractionStatus::Active,
                        OptionId::from("ack"),
                        turn,
                    )
                    .await
            }));
        }
        let mut outcomes = Vec::new();
        for answer in answers {
            outcomes.push(answer.await.unwrap());
        }
        let accepted = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        assert_eq!(accepted, 1, "round {round}: {outcomes:?}");
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, Err(StoreError::Conflict))),
            "round {round}: the loser must be told it lost, got {outcomes:?}"
        );
    }
}

/// Two dispatchers sweep the queue at once and divide it between them.
///
/// `FOR UPDATE SKIP LOCKED` is the whole claim: a row another worker is already
/// taking is passed over rather than waited on, so no external request is ever
/// sent twice, and no row is left behind either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_claiming_the_outbox_never_get_the_same_entry() {
    let Some(url) = support::database_url() else {
        support::skipped("two_workers_claiming_the_outbox_never_get_the_same_entry");
        return;
    };
    let first = support::sole_owner_of(&url, OUTBOX_SCHEMA).await;
    let second = support::second_pool(&url, OUTBOX_SCHEMA).await;
    let command = CommandId::new();

    let mut enqueued = Vec::new();
    for index in 0..40 {
        let id = OutboxId::new();
        enqueued.push(id);
        first
            .enqueue(support::outbox_entry(
                id,
                command,
                "claim-race",
                &format!("key-{index}"),
                support::at(index),
            ))
            .await
            .unwrap();
    }

    let gate = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for (name, store) in [("worker-a", first.clone()), ("worker-b", second.clone())] {
        let gate = gate.clone();
        workers.push(tokio::spawn(async move {
            gate.wait().await;
            let mut claimed = Vec::new();
            loop {
                let batch = store
                    .claim_due(support::at(100), 3, name)
                    .await
                    .expect("claiming is not an error");
                if batch.is_empty() {
                    break;
                }
                claimed.extend(batch.into_iter().map(|entry| entry.outbox_id));
            }
            claimed
        }));
    }
    let mut claimed = Vec::new();
    for worker in workers {
        claimed.extend(worker.await.unwrap());
    }

    let mut sorted = claimed.clone();
    sorted.sort_unstable();
    let mut unique = sorted.clone();
    unique.dedup();
    assert_eq!(
        sorted.len(),
        unique.len(),
        "no row may be handed to two workers"
    );
    let mut expected = enqueued;
    expected.sort_unstable();
    assert_eq!(sorted, expected, "every row must be claimed exactly once");
}

/// The blocking slot is held by an index, not by a check in the adapter.
///
/// The adapter refuses a second open blocking card, and so does a raw insert
/// that goes around the adapter entirely: the rule survives a bug in this crate,
/// a migration script, or a `psql` session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_partial_unique_index_refuses_a_second_blocking_card() {
    let Some(url) = support::database_url() else {
        support::skipped("the_partial_unique_index_refuses_a_second_blocking_card");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("blocking-slot");
    let case_ref = support::case("case-1", 1);

    let occupant = support::card(&account, case_ref.clone(), support::at(0));
    store.insert(occupant.clone()).await.unwrap();
    assert_eq!(
        store
            .insert(support::card(&account, case_ref.clone(), support::at(1)))
            .await,
        Err(StoreError::Conflict),
        "the adapter refuses a second blocking card"
    );

    // Now around the adapter: the index itself must refuse the row.
    let error = sqlx::query(
        "INSERT INTO tf_interaction (
             account_id, interaction_id, conversation_id, workflow_key, case_id, case_revision,
             kind, blocking, revision_independent, payload_hash, interaction, status, created_at
         ) VALUES ($1, $2, $3, $4, $5, 1, 'single_select', true, false, 'x', '{}'::jsonb,
                   'active', now())",
    )
    .bind(account.as_str())
    .bind(InteractionId::new().as_uuid())
    .bind(occupant.conversation_id.as_uuid())
    .bind(case_ref.workflow.as_str())
    .bind(case_ref.case_id.as_str())
    .execute(store.pool())
    .await
    .expect_err("the index must refuse a second open blocking card");
    let database = error.as_database_error().expect("a server-side refusal");
    assert_eq!(
        database.constraint(),
        Some("tf_one_open_blocking_interaction_per_case"),
        "the refusal must come from the partial index, not from something else"
    );

    // The index is partial on purpose: it covers open blocking cards and
    // nothing else.
    let aside = support::card(&account, case_ref.clone(), support::at(2));
    let non_blocking = turnframe_core::interaction::Interaction {
        blocking: false,
        ..aside
    };
    store
        .insert(non_blocking)
        .await
        .expect("a non-blocking card does not take the slot");

    let other_case = support::card(&account, support::case("case-2", 1), support::at(3));
    store
        .insert(other_case)
        .await
        .expect("another case has a slot of its own");

    // And once the occupant leaves the slot, the next card may take it.
    store
        .begin_resolution(
            &account,
            &occupant.id,
            InteractionStatus::Active,
            OptionId::from("ack"),
            TurnId::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .insert(support::card(&account, case_ref.clone(), support::at(4)))
            .await,
        Err(StoreError::Conflict),
        "a card that is resolving still holds the slot"
    );
    store
        .finish_resolution(
            &account,
            &occupant.id,
            turnframe_store::interaction::ResolutionOutcome::Resolved {
                event_ids: Vec::new(),
            },
        )
        .await
        .unwrap();
    store
        .insert(support::card(&account, case_ref, support::at(5)))
        .await
        .expect("a settled card has left the slot");
}

/// A bundle in flight is invisible, and a bundle rolled back never happened.
///
/// The first half holds a real transaction open and looks at the database
/// through a second connection: nothing the bundle wrote is there. The second
/// half lets a bundle fail on its last item the way the contract requires and
/// checks, again from the other connection, that the items before it are gone
/// too — and then that the same bundle without the bad item lands in full, so
/// the rollback was the item's doing and not a store that writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bundle_rolled_back_mid_flight_is_never_visible_to_another_connection() {
    let Some(url) = support::database_url() else {
        support::skipped("a_bundle_rolled_back_mid_flight_is_never_visible_to_another_connection");
        return;
    };
    let writer = support::co_tenant_of(&url, SCHEMA).await;
    let reader = support::second_pool(&url, SCHEMA).await;
    let account = support::unique_account("rollback");
    let command = CommandId::new();

    let inserted = support::card(&account, support::case("case-1", 1), support::at(0));
    let outbox_id = OutboxId::new();
    let turn = TurnId::new();
    let event_ids = vec![EventId::new()];
    let bundle = || {
        CommitBundle::new()
            .with_events(support::event_batch(
                &account, "case-1", command, 1, &event_ids,
            ))
            .with_interaction_insert(inserted.clone(), false)
            .with_outbox_entry(support::outbox_entry(
                outbox_id,
                command,
                "rollback-test",
                &format!("key-{outbox_id}"),
                support::at(0),
            ))
            .with_replay_record(ReplayRecord::received(
                turn,
                inserted.conversation_id,
                account.clone(),
                support::at(0),
            ))
    };

    // Held open, not committed: another connection sees none of it.
    let mut transaction = writer.pool().begin().await.unwrap();
    writer
        .commit_in(&mut transaction, &account, bundle())
        .await
        .expect("every item of the bundle applies");
    assert_eq!(
        InteractionReader::get(&reader, &account, &inserted.id).await,
        Err(StoreError::NotFound),
        "an uncommitted card must not be visible to another connection"
    );
    assert!(
        reader
            .get_by_ids(&account, &event_ids)
            .await
            .unwrap()
            .is_empty()
    );
    transaction.rollback().await.unwrap();

    assert_eq!(
        InteractionReader::get(&reader, &account, &inserted.id).await,
        Err(StoreError::NotFound),
        "a rolled-back card must never appear"
    );
    assert_eq!(
        OutboxReader::get(&reader, &outbox_id).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        ReplayReader::get(&reader, &account, &turn).await,
        Err(StoreError::NotFound)
    );

    // The contract's own failure mode: the last item names a turn that does not
    // exist, so everything before it must roll back with it.
    assert_eq!(
        writer
            .commit(
                &account,
                bundle().with_turn_phase(TurnId::new(), TurnPhase::Committed)
            )
            .await
            .map(|receipt| receipt.event_ids),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        InteractionReader::get(&reader, &account, &inserted.id).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        reader
            .count(&account, &support::case("case-1", 0).key())
            .await
            .unwrap(),
        0,
        "no event of a failed bundle may be visible"
    );

    // The same bundle without the illegal item lands in full.
    let receipt = writer.commit(&account, bundle()).await.unwrap();
    assert_eq!(receipt.event_ids, event_ids);
    assert_eq!(
        InteractionReader::get(&reader, &account, &inserted.id)
            .await
            .unwrap()
            .status(),
        InteractionStatus::Active
    );
    assert_eq!(
        reader.get_by_ids(&account, &event_ids).await.unwrap().len(),
        1
    );
    OutboxReader::get(&reader, &outbox_id).await.unwrap();
    ReplayReader::get(&reader, &account, &turn).await.unwrap();
}

/// Two erasure requests for the same event, at the same moment.
///
/// An erasure arrives from a support tool, a queue and a retry, so two of them
/// racing is the normal case rather than the exotic one. Both must be told the
/// same thing, and what they are told has to be the record that is actually in
/// the row: a second caller that saw its own authority stamped while the row
/// kept the first one would report an erasure that never happened under that
/// name. `COALESCE` over a locked row is what settles it — the loser waits, re-
/// reads the row the winner committed, and keeps that record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_erasure_requests_for_the_same_event_agree_on_one_record() {
    let Some(url) = support::database_url() else {
        support::skipped("two_erasure_requests_for_the_same_event_agree_on_one_record");
        return;
    };
    let first = support::co_tenant_of(&url, SCHEMA).await;
    let second = support::second_pool(&url, SCHEMA).await;
    let account = support::unique_account("erasure-race");

    for round in 0..ROUNDS {
        let case_id = format!("erasure-{round}");
        let event_id = EventId::new();
        EventJournalWriter::append(
            &first,
            support::event_batch(&account, &case_id, CommandId::new(), 1, &[event_id]),
        )
        .await
        .unwrap();

        let gate = Arc::new(Barrier::new(2));
        let left = {
            let (store, account, gate) = (first.clone(), account.clone(), gate.clone());
            tokio::spawn(async move {
                gate.wait().await;
                store
                    .redact_payload(
                        &account,
                        &event_id,
                        &RedactionAuthority::from("erasure-request-left"),
                    )
                    .await
            })
        };
        let right = {
            let (store, account, gate) = (second.clone(), account.clone(), gate.clone());
            tokio::spawn(async move {
                gate.wait().await;
                store
                    .redact_payload(
                        &account,
                        &event_id,
                        &RedactionAuthority::from("erasure-request-right"),
                    )
                    .await
            })
        };
        let (left, right) = (left.await.unwrap(), right.await.unwrap());
        let left = left.expect("the first erasure request");
        let right = right.expect("the second erasure request");
        assert_eq!(
            left, right,
            "both callers must be told about the same erasure"
        );

        let stored = EventJournalReader::get_by_ids(&first, &account, &[event_id])
            .await
            .unwrap();
        let stored = stored.first().expect("the event is still in the ledger");
        assert_eq!(
            stored.redaction.as_ref(),
            Some(&left),
            "the row must carry the record both callers were given"
        );
        assert!(stored.payload.is_null(), "the payload must be gone");
    }
}
