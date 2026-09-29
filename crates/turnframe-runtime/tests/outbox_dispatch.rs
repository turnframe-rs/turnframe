//! The reference outbox dispatcher (spec §16.4, §16.5).
//!
//! The three properties an external-effect saga stands or falls on: one row is
//! sent by one dispatcher, a rebooking whose answer never came back is not a retry,
//! and a settled row is never picked up again.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use turnframe_core::command::IdempotencyKey;
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::ids::{CommandId, OutboxId};
use turnframe_runtime::dispatch::{
    DispatchConfig, Dispatched, OutboxDispatcher, OutboxReconciler, OutboxSender, Reconciled, code,
};
use turnframe_store::memory::MemoryStores;
use turnframe_store::outbox::{OutboxReader, OutboxRecord, OutboxStore, OutboxWriter};

/// A sender that counts what it was asked to rebook and answers the same way
/// every time.
#[derive(Debug)]
struct Recording {
    answer: Answer,
    sent: AtomicUsize,
    keys: std::sync::Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
enum Answer {
    Completed,
    Retryable,
    /// Never answers in time.
    Hanging,
}

impl Recording {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            sent: AtomicUsize::new(0),
            keys: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    fn keys(&self) -> Vec<String> {
        self.keys
            .lock()
            .map(|keys| keys.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl OutboxSender for Recording {
    async fn send(&self, entry: &OutboxEntry) -> Dispatched {
        self.sent.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut keys) = self.keys.lock() {
            keys.push(entry.idempotency_key.as_str().to_owned());
        }
        // Give any other dispatcher sharing this store a chance to run while
        // this row is claimed, which is what the exclusivity test needs.
        tokio::task::yield_now().await;
        match self.answer {
            Answer::Completed => Dispatched::completed(),
            Answer::Retryable => Dispatched::Retryable {
                code: "remote_5xx".to_owned(),
            },
            Answer::Hanging => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Dispatched::completed()
            }
        }
    }
}

/// A reconciler with a fixed answer.
struct Says(Reconciled);

#[async_trait]
impl OutboxReconciler for Says {
    async fn reconcile(&self, _record: &OutboxRecord) -> Reconciled {
        self.0.clone()
    }
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
}

fn entry(n: u128) -> OutboxEntry {
    OutboxEntry {
        outbox_id: OutboxId::from(uuid::Uuid::from_u128(n)),
        command_id: CommandId::from(uuid::Uuid::from_u128(n)),
        destination: "airline".to_owned(),
        payload: serde_json::json!({"trip": n}),
        idempotency_key: IdempotencyKey::new(format!("key-{n}")),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        created_at: now(),
        completed_at: None,
    }
}

async fn store_with(rows: &[OutboxEntry]) -> Arc<MemoryStores> {
    let stores = Arc::new(MemoryStores::new());
    for row in rows {
        OutboxWriter::enqueue(stores.as_ref(), row.clone())
            .await
            .expect("the row is new");
    }
    stores
}

fn dispatcher(
    stores: &Arc<MemoryStores>,
    sender: Arc<Recording>,
    worker: &str,
    config: impl FnOnce(DispatchConfig) -> DispatchConfig,
) -> OutboxDispatcher {
    OutboxDispatcher::new(
        Arc::clone(stores) as Arc<dyn OutboxStore>,
        sender,
        config(DispatchConfig::new(worker)),
    )
}

async fn status(stores: &Arc<MemoryStores>, id: &OutboxId) -> OutboxRecord {
    OutboxReader::get(stores.as_ref(), id)
        .await
        .expect("the row is readable")
}

#[tokio::test]
async fn two_dispatchers_never_send_the_same_row() {
    let stores = store_with(&[entry(1)]).await;
    let sender = Recording::new(Answer::Completed);
    let one = dispatcher(&stores, Arc::clone(&sender), "worker-a", |config| config);
    let two = dispatcher(&stores, Arc::clone(&sender), "worker-b", |config| config);

    let (first, second) = tokio::join!(one.run_once(now()), two.run_once(now()));
    let first = first.expect("the claim was made");
    let second = second.expect("the claim was made");

    assert_eq!(
        sender.sent(),
        1,
        "the row left the building exactly once, whoever claimed it"
    );
    assert_eq!(
        first.claimed() + second.claimed(),
        1,
        "and only one dispatcher had anything to settle"
    );
    assert_eq!(
        sender.keys(),
        vec!["key-1"],
        "the sender is handed the idempotency key the command was admitted under"
    );
    assert_eq!(
        status(&stores, &entry(1).outbox_id).await.entry.status,
        OutboxStatus::Completed
    );
}

#[tokio::test]
async fn a_send_that_never_answers_becomes_an_unknown_outcome_and_not_a_retry() {
    let stores = store_with(&[entry(1)]).await;
    let sender = Recording::new(Answer::Hanging);
    let dispatcher = dispatcher(&stores, Arc::clone(&sender), "worker-a", |config| {
        config.with_send_timeout(Duration::from_millis(20))
    });

    let report = dispatcher
        .run_once(now())
        .await
        .expect("the claim was made");

    assert_eq!(sender.sent(), 1);
    assert_eq!(report.unknown, vec![entry(1).outbox_id]);
    assert!(
        report.retried.is_empty() && report.failed.is_empty(),
        "a request that left is never quietly repeated (I15)"
    );
    let record = status(&stores, &entry(1).outbox_id).await;
    assert_eq!(record.entry.status, OutboxStatus::OutcomeUnknown);
    assert!(
        record.entry.next_attempt_at.is_none(),
        "nothing scheduled it for another go"
    );

    // A second sweep does not pick it up either: it is not pending.
    let again = dispatcher
        .run_once(now())
        .await
        .expect("the claim was made");
    assert!(again.is_empty());
    assert_eq!(sender.sent(), 1);
}

#[tokio::test]
async fn an_unknown_outcome_is_settled_by_the_reconciliation_hook() {
    let stores = store_with(&[entry(1)]).await;
    let sender = Recording::new(Answer::Hanging);
    let dispatcher = dispatcher(&stores, Arc::clone(&sender), "worker-a", |config| {
        config.with_send_timeout(Duration::from_millis(20))
    });
    dispatcher
        .run_once(now())
        .await
        .expect("the claim was made");

    let answer = dispatcher
        .reconcile(&entry(1).outbox_id, &Says(Reconciled::Completed), now())
        .await
        .expect("the row is readable");

    assert_eq!(answer, Reconciled::Completed);
    assert_eq!(
        status(&stores, &entry(1).outbox_id).await.entry.status,
        OutboxStatus::Completed
    );
    // A row that is not in doubt is left alone.
    assert_eq!(
        dispatcher
            .reconcile(
                &entry(1).outbox_id,
                &Says(Reconciled::Failed { code: "x".into() }),
                now()
            )
            .await
            .expect("the row is readable"),
        Reconciled::Unresolved
    );
    assert_eq!(sender.sent(), 1, "reconciling never sends anything");
}

#[tokio::test]
async fn a_settled_row_is_never_sent_again() {
    let stores = store_with(&[entry(1)]).await;
    let sender = Recording::new(Answer::Completed);
    let dispatcher = dispatcher(&stores, Arc::clone(&sender), "worker-a", |config| config);

    let first = dispatcher
        .run_once(now())
        .await
        .expect("the claim was made");
    assert_eq!(first.completed, vec![entry(1).outbox_id]);

    for _ in 0..3 {
        let again = dispatcher
            .run_once(now() + TimeDelta::hours(1))
            .await
            .expect("the claim was made");
        assert!(again.is_empty(), "there is nothing due any more");
    }
    assert_eq!(sender.sent(), 1);
}

#[tokio::test]
async fn a_retryable_failure_waits_and_then_gives_up() {
    let stores = store_with(&[entry(1)]).await;
    let sender = Recording::new(Answer::Retryable);
    let dispatcher = dispatcher(&stores, Arc::clone(&sender), "worker-a", |config| {
        config
            .with_max_attempts(2)
            .with_backoff(Duration::from_secs(30), Duration::from_secs(60))
    });

    let first = dispatcher
        .run_once(now())
        .await
        .expect("the claim was made");
    assert_eq!(first.retried, vec![entry(1).outbox_id]);
    let record = status(&stores, &entry(1).outbox_id).await;
    assert_eq!(record.entry.status, OutboxStatus::Pending);
    assert_eq!(
        record.entry.next_attempt_at,
        Some(now() + TimeDelta::seconds(30))
    );

    // Not due yet.
    assert!(
        dispatcher
            .run_once(now())
            .await
            .expect("the claim was made")
            .is_empty()
    );
    assert_eq!(sender.sent(), 1);

    // Due, and this attempt exhausts the budget.
    let later = now() + TimeDelta::minutes(1);
    let second = dispatcher
        .run_once(later)
        .await
        .expect("the claim was made");
    assert_eq!(second.failed, vec![entry(1).outbox_id]);
    assert_eq!(sender.sent(), 2);
    let record = status(&stores, &entry(1).outbox_id).await;
    assert_eq!(record.entry.status, OutboxStatus::Failed);
    assert_eq!(
        record.last_failure.as_deref(),
        Some(code::ATTEMPTS_EXHAUSTED)
    );
}
