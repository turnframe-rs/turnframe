//! The fake stores, driven the way a runtime test drives them (spec §27.7).
//!
//! Three things are proved here and nowhere else in the kit: the store
//! conformance suite really is reachable through this crate, a named crash
//! boundary really does fail the call that reaches it and leave the documented
//! state behind, and the tally really does count the calls the code under test
//! made rather than the ones the assertions made.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use turnframe_core::case::CaseRef;
use turnframe_core::ids::{CaseRevision, CommandId, TurnId};
use turnframe_store::error::StoreError;
use turnframe_store::journal::{CommandJournalEntry, CommandJournalStatus};
use turnframe_test::stores::{
    CRASH_BOUNDARIES, FailurePoint, FakeStores, Stores, boundary_name, conformance, crash_boundary,
};
use turnframe_test::workflows::trip::TripCommand;

fn entry(fake: &FakeStores, turn: TurnId) -> CommandJournalEntry {
    let batch = support::batch(
        turn,
        &CaseRef::new("trip", "trip-1", CaseRevision(0)),
        &support::confirmed_click(),
        vec![TripCommand::Open],
    );
    CommandJournalEntry::from_envelope(&batch.envelopes[0], "trip.open", fake.now())
        .expect("the sample command serializes")
}

#[tokio::test]
async fn the_store_conformance_suite_is_reachable_through_the_kit() {
    // An adopter who depends on `turnframe-test` gets the persistence contract
    // with it, and the reference store passes it.
    let report = conformance::run_all(&Stores::in_memory).await;
    assert!(report.passed(), "{report}");
}

#[tokio::test]
async fn the_fake_stores_pass_the_same_suite() {
    let report = conformance::run_all(&|| FakeStores::new().stores().clone()).await;
    assert!(
        report.passed(),
        "counting and clock control must not change any answer:\n{report}"
    );
}

#[test]
fn every_boundary_of_the_specification_is_addressable_by_name() {
    // Spec §27.7 lists the boundaries a chaos test must inject at; this is that
    // list, as code.
    let names: Vec<&str> = CRASH_BOUNDARIES.iter().map(|(name, _)| *name).collect();
    assert_eq!(names.len(), FailurePoint::ALL.len());
    for name in &names {
        let point = crash_boundary(name).expect("the table is its own index");
        assert_eq!(boundary_name(point), *name);
    }
    assert!(names.contains(&"after_journal_insert_before_commit"));
    assert!(names.contains(&"before_response_persistence"));
}

#[tokio::test]
async fn a_before_boundary_leaves_the_key_free() {
    let fake = FakeStores::new();
    fake.fail_at_boundary("before_journal_insert", StoreError::Unavailable)
        .unwrap();

    let refused = fake
        .stores()
        .journal()
        .begin(entry(&fake, TurnId::nil()))
        .await;
    assert_eq!(refused, Err(StoreError::Unavailable));

    // Nothing was written, so the command can be admitted from scratch.
    let admission = fake
        .stores()
        .journal()
        .begin(entry(&fake, TurnId::nil()))
        .await
        .expect("the second admission goes through");
    assert!(admission.is_fresh(), "the key was never taken");
    assert!(!FailurePoint::BeforeJournalInsert.leaves_write_visible());
}

#[tokio::test]
async fn an_after_boundary_leaves_the_entry_for_recovery_to_find() {
    let fake = FakeStores::new();
    let turn = TurnId::nil();
    fake.fail_at(
        FailurePoint::AfterJournalInsertBeforeCommit,
        StoreError::Timeout,
    )
    .unwrap();

    let refused = fake.stores().journal().begin(entry(&fake, turn)).await;
    assert_eq!(refused, Err(StoreError::Timeout));

    // The write survived: this is the partial state recovery has to resume,
    // by idempotency key rather than by admitting a second command.
    let pending = fake
        .journal_for_turn(&support::account(), &turn)
        .await
        .expect("the turn has journal entries");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, CommandJournalStatus::Pending);
    assert!(FailurePoint::AfterJournalInsertBeforeCommit.leaves_write_visible());
}

#[tokio::test]
async fn the_tally_counts_the_code_under_test_and_not_the_assertions() {
    let fake = FakeStores::new();
    let turn = TurnId::nil();
    let admitted = entry(&fake, turn);
    let command_id = admitted.command_id;

    fake.stores().journal().begin(admitted).await.unwrap();
    fake.stores()
        .journal()
        .mark_executing(&support::account(), &command_id)
        .await
        .unwrap();

    assert_eq!(fake.call_count("journal.begin"), 1);
    assert_eq!(fake.call_count("journal.mark_executing"), 1);
    assert_eq!(fake.call_count("journal.get"), 0);
    assert_eq!(fake.total_calls(), 2);

    // Reading back what was persisted goes around the counter.
    let stored = fake
        .journal_entry(&support::account(), &command_id)
        .await
        .expect("the entry is there");
    assert_eq!(stored.status, CommandJournalStatus::Executing);
    assert_eq!(fake.call_count("journal.get"), 0);
    assert_eq!(fake.total_calls(), 2);

    fake.reset_calls();
    assert!(fake.calls().is_empty());
    assert_eq!(
        fake.journal_entry(&support::account(), &CommandId::nil())
            .await
            .unwrap_err(),
        StoreError::NotFound,
        "an unknown command is indistinguishable from another tenant's"
    );
}
