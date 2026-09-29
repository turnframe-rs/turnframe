//! Runs the persistence conformance suite against the reference in-memory
//! store, and against a deliberately broken one.
//!
//! The suite is developed against `MemoryStores`, so a failure of the first
//! test means the reference implementation and the contract have drifted apart
//! — the only situation in which the suite itself should be suspected before an
//! adapter is. The last test does the opposite job: it introduces a real bug
//! and proves the suite names it instead of passing quietly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::event::EventRedaction;
use turnframe_core::ids::{
    AccountId, CaseRevision, ConversationId, EventId, InteractionId, OptionId, RedactionAuthority,
    TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};
use turnframe_store::conformance::{self, ConformanceFailure};
use turnframe_store::error::StoreError;
use turnframe_store::events::{
    EventBatch, EventCursor, EventJournalReader, EventJournalWriter, EventPage, StoredEvent,
};
use turnframe_store::interaction::{
    InteractionReader, InteractionRecord, InteractionWriter, InvalidationReason, ResolutionOutcome,
};
use turnframe_store::memory::{ManualClock, MemoryStores};
use turnframe_store::stores::Stores;

#[tokio::test]
async fn memory_stores_satisfy_the_contract() {
    let report = conformance::run_all(&Stores::in_memory).await;
    assert!(report.passed(), "{report}");
    assert_eq!(
        report.outcomes.len(),
        conformance::CHECK_COUNT,
        "every check must have run"
    );
}

#[tokio::test]
async fn the_suite_is_deterministic_on_a_stopped_clock() {
    let factory = || {
        Stores::from_memory(Arc::new(MemoryStores::with_clock(Arc::new(
            ManualClock::epoch(),
        ))))
    };
    let first = conformance::run_all(&factory).await;
    let second = conformance::run_all(&factory).await;
    assert!(first.passed(), "{first}");
    assert_eq!(first, second, "two runs must reach the same conclusions");
}

#[tokio::test]
async fn individual_checks_are_callable_on_their_own() {
    // How an adapter author works: one rule at a time, each on a fresh store.
    let results: Vec<Result<(), ConformanceFailure>> = vec![
        conformance::check_blocking_interaction_conflict(&Stores::in_memory()).await,
        conformance::check_journal_idempotency_replay(&Stores::in_memory()).await,
        conformance::check_commit_bundle_atomic_on_invalid_item(&Stores::in_memory()).await,
    ];
    for result in results {
        assert!(result.is_ok(), "{result:?}");
    }
}

/// An interaction store that forgets to scope reads by account — the single
/// most consequential mistake an adapter can make.
struct LeakyInteractions(Arc<MemoryStores>);

#[async_trait]
impl InteractionReader for LeakyInteractions {
    /// The bug: whoever asks is answered as if they were the owning tenant.
    async fn get(
        &self,
        _account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError> {
        self.0
            .get(&AccountId::from("conformance-account"), id)
            .await
    }

    async fn list_open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, StoreError> {
        self.0
            .list_open_for_conversation(account, conversation)
            .await
    }

    async fn list_open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError> {
        self.0.list_open_for_case(account, case_key).await
    }

    async fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: turnframe_core::ids::CaseRevision,
    ) -> Result<bool, StoreError> {
        self.0
            .blocking_answered_at(account, case_key, revision)
            .await
    }
}

#[async_trait]
impl InteractionWriter for LeakyInteractions {
    async fn insert(&self, interaction: Interaction) -> Result<(), StoreError> {
        self.0.insert(interaction).await
    }

    async fn insert_replacing_blocking(
        &self,
        interaction: Interaction,
    ) -> Result<Vec<InteractionId>, StoreError> {
        self.0.insert_replacing_blocking(interaction).await
    }

    async fn begin_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
    ) -> Result<InteractionRecord, StoreError> {
        self.0
            .begin_resolution(account, id, expected_status, option_id, resolved_by)
            .await
    }

    async fn finish_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError> {
        self.0.finish_resolution(account, id, outcome).await
    }

    async fn invalidate_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        self.0
            .invalidate_for_case(account, case_key, new_revision, reason)
            .await
    }

    async fn invalidate_case_cards(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        self.0
            .invalidate_case_cards(account, case_key, reason)
            .await
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<InteractionId>, StoreError> {
        self.0.expire_due(now).await
    }
}

#[tokio::test]
async fn a_broken_store_is_reported_rather_than_panicking() {
    let factory = || {
        let backend = Arc::new(MemoryStores::new());
        Stores::builder()
            .conversations(backend.clone())
            .interactions(Arc::new(LeakyInteractions(backend.clone())))
            .journal(backend.clone())
            .events(backend.clone())
            .outbox(backend.clone())
            .replay(backend.clone())
            .commit(backend)
            .build()
            .expect("every role supplied")
    };

    let report = conformance::run_all(&factory).await;
    assert!(!report.passed(), "the leaky store must not pass");
    let failed: Vec<&str> = report.failures().map(|failure| failure.check).collect();
    assert!(
        failed.contains(&"check_cross_tenant_isolation"),
        "the isolation check must catch it, got {failed:?}"
    );
    assert!(
        report
            .to_string()
            .contains("FAIL  check_cross_tenant_isolation"),
        "{report}"
    );
}

/// An event journal that meets an erasure request by deleting the row.
///
/// This is the implementation an adopter reaches for when the obligation lands
/// and the contract says nothing, and it is the one the whole architecture
/// rests on not happening: the event still authorizes receipts that were
/// already rendered, and every consumer paging by sequence silently skips a
/// position it will never be handed again. It is also invisible from most
/// angles — appends, counts and by-identifier reads of *other* events all still
/// look right — which is why it needs a check of its own rather than a review.
struct DeletingJournal {
    inner: Arc<MemoryStores>,
    deleted: Mutex<BTreeSet<EventId>>,
}

#[async_trait]
impl EventJournalReader for DeletingJournal {
    async fn list_since(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let events = self
            .inner
            .list_since(account, case_key, since, limit)
            .await?;
        Ok(self.surviving(events))
    }

    async fn read_from(
        &self,
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> Result<EventPage, StoreError> {
        let page = self.inner.read_from(account, after, limit).await?;
        Ok(EventPage::new(self.surviving(page.events), after))
    }

    async fn get_by_ids(
        &self,
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let events = self.inner.get_by_ids(account, ids).await?;
        Ok(self.surviving(events))
    }

    async fn count(&self, account: &AccountId, case_key: &CaseKey) -> Result<u64, StoreError> {
        let all = self
            .inner
            .list_since(account, case_key, CaseRevision::ZERO, usize::MAX)
            .await?;
        Ok(self.surviving(all).len() as u64)
    }
}

#[async_trait]
impl EventJournalWriter for DeletingJournal {
    async fn append(&self, batch: EventBatch) -> Result<Vec<EventId>, StoreError> {
        self.inner.append(batch).await
    }

    /// The bug: the payload goes, and so does the event.
    async fn redact_payload(
        &self,
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
    ) -> Result<EventRedaction, StoreError> {
        let record = self
            .inner
            .redact_payload(account, event_id, authority)
            .await?;
        self.deleted
            .lock()
            .expect("the deletion set is not poisoned")
            .insert(*event_id);
        Ok(record)
    }
}

impl DeletingJournal {
    fn surviving(&self, events: Vec<StoredEvent>) -> Vec<StoredEvent> {
        let deleted = self
            .deleted
            .lock()
            .expect("the deletion set is not poisoned");
        events
            .into_iter()
            .filter(|event| !deleted.contains(&event.event_id))
            .collect()
    }
}

#[tokio::test]
async fn a_store_that_erases_by_deleting_is_caught() {
    let factory = || {
        let backend = Arc::new(MemoryStores::new());
        Stores::builder()
            .conversations(backend.clone())
            .interactions(backend.clone())
            .journal(backend.clone())
            .events(Arc::new(DeletingJournal {
                inner: backend.clone(),
                deleted: Mutex::new(BTreeSet::new()),
            }))
            .outbox(backend.clone())
            .replay(backend.clone())
            .commit(backend)
            .build()
            .expect("every role supplied")
    };

    let report = conformance::run_all(&factory).await;
    assert!(!report.passed(), "a delete must not pass as an erasure");
    let failed: Vec<&str> = report.failures().map(|failure| failure.check).collect();
    assert!(
        failed.contains(&"check_event_redaction_preserves_identity_and_order"),
        "the check written for exactly this mistake must name it, got {failed:?}"
    );
    // Nothing else may fail: a delete has to be caught by the rule it breaks,
    // not by collateral damage somewhere in the suite.
    assert!(
        failed
            .iter()
            .all(|check| check.starts_with("check_event_redaction_")),
        "only the erasure checks may fail on this store, got {failed:?}"
    );
    assert!(
        report
            .to_string()
            .contains("an erasure must not add, remove or move an event"),
        "{report}"
    );
}
