//! Rolling a workflow back to its previous version, against a live database.
//!
//! `docs/canary-and-rollback.md` states three rules that make a rollback safe,
//! and all three are claims about stored data rather than about code, so the
//! only honest place to check them is a real database.
//!
//! A rollback here means one thing: turns start being projected by the previous
//! version of a workflow again. Nothing is migrated, nothing is rewritten, and
//! no case is touched. What has to hold is that the previous version can still
//! read what the newer one wrote, that the events it produced still say what
//! they said, and that a card the newer version left open is still answerable
//! by a server running the older one — or has been deliberately invalidated,
//! which is the other half of the rule.
//!
//! Each test skips with a printed note when no database is configured, so a
//! green run on a machine without PostgreSQL is never mistaken for a proof.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId};
use turnframe_core::interaction::InteractionStatus;
use turnframe_store::events::{EventBatch, EventJournalReader, EventJournalWriter, StoredEvent};
use turnframe_store::interaction::{
    InteractionReader, InteractionWriter, InvalidationReason, ResolutionOutcome,
};

/// The schema these tests own. Rollback writes and reads whole cases, so it
/// keeps out of the way of the suites that share one.
const SCHEMA: &str = "tf_test_rollback";

/// The workflow being rolled back.
const WORKFLOW: &str = "trip";

/// One event as the newer version of the projector wrote it.
///
/// The identifier is derived from the case so a re-run against the same schema
/// writes the same rows rather than colliding with the last run's.
fn event(
    case: &CaseKey,
    ordinal: u8,
    event_type: &str,
    payload: serde_json::Value,
) -> turnframe_core::event::CommittedEvent<serde_json::Value> {
    let mut seed = [0_u8; 16];
    for (slot, byte) in seed.iter_mut().zip(case.case_id.as_str().bytes().cycle()) {
        *slot = byte;
    }
    seed[15] = ordinal;
    turnframe_core::event::CommittedEvent {
        event_id: EventId::from(uuid::Uuid::from_bytes(seed)),
        event_type: event_type.to_owned(),
        occurred_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant"),
        payload,
    }
}

/// Writes a case's history as version two of the workflow would have, then
/// hands back what a server running version one reads.
async fn history_written_by_the_newer_version(
    store: &turnframe_store_postgres::PgStores,
    account: &AccountId,
    case: &CaseKey,
) -> Vec<StoredEvent> {
    let command = CommandId::new();
    // Each run writes under a fresh command, so a repeat is a new batch rather
    // than a duplicate of the last one.
    let batch = EventBatch::new(
        account.clone(),
        case.clone(),
        command,
        CaseRevision(2),
        vec![
            event(
                case,
                1,
                "trip.name_set",
                serde_json::json!({"value": "Lisbon", "set_by_version": "2"}),
            ),
            event(
                case,
                2,
                "trip.travel_date_set",
                serde_json::json!({"value": "2026-10-31"}),
            ),
        ],
    );
    EventJournalWriter::append(store, batch)
        .await
        .expect("the newer version commits its events");

    EventJournalReader::list_since(store, account, case, CaseRevision(0), 64)
        .await
        .expect("a server running the previous version reads the same ledger")
}

#[tokio::test]
async fn the_previous_version_reads_a_case_the_newer_one_wrote() {
    let Some(url) = support::database_url() else {
        support::skipped("the_previous_version_reads_a_case_the_newer_one_wrote");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("acct-rollback-reads");
    let case = CaseKey::new(WORKFLOW, "trip-1");

    let read_back = history_written_by_the_newer_version(&store, &account, &case).await;

    assert_eq!(
        read_back.len(),
        2,
        "a rollback reads the whole history, not the part its own version wrote"
    );
    assert_eq!(
        read_back
            .iter()
            .map(|stored| stored.event_type.clone())
            .collect::<Vec<_>>(),
        vec!["trip.name_set", "trip.travel_date_set"],
        "in the order they were committed"
    );
    assert_eq!(
        read_back[0].payload["value"], "Lisbon",
        "with the payload unchanged: an event's meaning is not edited retroactively"
    );
    assert_eq!(
        read_back[0].payload["set_by_version"], "2",
        "including a field the previous version does not know, which it ignores rather than fails on"
    );
}

#[tokio::test]
async fn a_rollback_neither_rewrites_nor_removes_an_event() {
    let Some(url) = support::database_url() else {
        support::skipped("a_rollback_neither_rewrites_nor_removes_an_event");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("acct-rollback-immutable");
    let case = CaseKey::new(WORKFLOW, "trip-2");

    let before = history_written_by_the_newer_version(&store, &account, &case).await;

    // The rollback itself: a server running the previous version starts reading.
    // It appends nothing and rewrites nothing, so a second read is the first.
    let after = EventJournalReader::list_since(&store, &account, &case, CaseRevision(0), 64)
        .await
        .expect("the ledger answers");

    assert_eq!(
        before.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        after.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        "the same events, with the same identifiers, in the same order"
    );
    assert_eq!(
        before.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        after.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        "and at the same positions, so a receipt rendered before the rollback still cites what it cited"
    );
}

#[tokio::test]
async fn a_card_the_newer_version_left_open_is_still_answerable_afterwards() {
    let Some(url) = support::database_url() else {
        support::skipped("a_card_the_newer_version_left_open_is_still_answerable_afterwards");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("acct-rollback-card-open");
    let case = CaseRef::new(WORKFLOW, "trip-3", CaseRevision(2));

    let card = support::card(&account, case.clone(), support::epoch());
    let id = card.id;
    InteractionWriter::insert(&store, card)
        .await
        .expect("the newer version wrote a card");

    // Rollback. A server running the previous version now reads the card.
    let found = InteractionReader::get(&store, &account, &id)
        .await
        .expect("the card survives the rollback");
    assert_eq!(
        found.interaction.status,
        InteractionStatus::Active,
        "and it is still open, so the user is not left holding an unanswerable card"
    );

    // And it can still be answered, because the option's meaning was stored
    // with the card rather than being recompiled by whichever version is live.
    let option = found
        .interaction
        .payload
        .options
        .first()
        .expect("the stored card carries its options")
        .id
        .clone();
    InteractionWriter::begin_resolution(
        &store,
        &account,
        &id,
        InteractionStatus::Active,
        option,
        turnframe_core::ids::TurnId::new(),
    )
    .await
    .expect("the previous version can answer a card the newer one wrote");
    InteractionWriter::finish_resolution(
        &store,
        &account,
        &id,
        ResolutionOutcome::Resolved { event_ids: vec![] },
    )
    .await
    .expect("and settle it");

    let settled = InteractionReader::get(&store, &account, &id)
        .await
        .expect("the card is readable after settling");
    assert_eq!(settled.interaction.status, InteractionStatus::Resolved);
}

#[tokio::test]
async fn the_other_half_of_the_rule_is_invalidating_the_cards_instead() {
    let Some(url) = support::database_url() else {
        support::skipped("the_other_half_of_the_rule_is_invalidating_the_cards_instead");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("acct-rollback-card-invalidated");
    let case = CaseRef::new(WORKFLOW, "trip-4", CaseRevision(2));

    let card = support::card(&account, case.clone(), support::epoch());
    let id = card.id;
    InteractionWriter::insert(&store, card)
        .await
        .expect("the newer version wrote a card");

    // When the previous version cannot compile the operation the card's option
    // names, the honest rollback step is to invalidate it rather than leave the
    // user an option nothing will honour.
    let invalidated = InteractionWriter::invalidate_case_cards(
        &store,
        &account,
        &case.key(),
        InvalidationReason::Administrative {
            code: String::from("workflow_rolled_back"),
        },
    )
    .await
    .expect("the rollback invalidates what it cannot honour");
    assert!(
        invalidated.contains(&id),
        "the card the newer version wrote is named as invalidated"
    );

    let found = InteractionReader::get(&store, &account, &id)
        .await
        .expect("the card is still there to read");
    assert_eq!(
        found.interaction.status,
        InteractionStatus::Invalidated,
        "invalidated rather than deleted, so the record of it having existed survives"
    );
}

#[tokio::test]
async fn rolling_one_workflow_back_leaves_another_alone() {
    let Some(url) = support::database_url() else {
        support::skipped("rolling_one_workflow_back_leaves_another_alone");
        return;
    };
    let store = support::co_tenant_of(&url, SCHEMA).await;
    let account = support::unique_account("acct-rollback-scoped");
    let trip = CaseRef::new(WORKFLOW, "trip-5", CaseRevision(2));
    let traveler = CaseRef::new("traveler", "trav-1", CaseRevision(2));

    let trip_card = support::card(&account, trip.clone(), support::epoch());
    let traveler_card = support::card(&account, traveler.clone(), support::epoch());
    let traveler_id = traveler_card.id;
    InteractionWriter::insert(&store, trip_card)
        .await
        .expect("a card on the workflow being rolled back");
    InteractionWriter::insert(&store, traveler_card)
        .await
        .expect("a card on a workflow that is not");

    InteractionWriter::invalidate_case_cards(
        &store,
        &account,
        &trip.key(),
        InvalidationReason::Administrative {
            code: String::from("workflow_rolled_back"),
        },
    )
    .await
    .expect("the rollback runs against one workflow's case");

    let untouched = InteractionReader::get(&store, &account, &traveler_id)
        .await
        .expect("the other workflow's card is still there");
    assert_eq!(
        untouched.interaction.status,
        InteractionStatus::Active,
        "rolling one workflow back leaves another's cards open: versions are independent"
    );
}
