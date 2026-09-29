//! An airline that answers after the rebooking's outcome was left unknown settles it once:
//! the outbox row is reconciled, never sent again, and the answer delivered twice moves the
//! trip once, to a ticket.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use support::{Harness, now, rebooking_requested};
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::flow::WorkflowExecutor;
use turnframe_core::ids::TurnId;
use turnframe_runtime::dispatch::{
    DispatchConfig, Dispatched, OutboxDispatcher, OutboxReconciler, OutboxSender, Reconciled,
};
use turnframe_store::outbox::{OutboxRecord, OutboxStore};
use turnframe_test::workflows::trip::{
    AirlineMode, REBOOK_CONFIRM_OPTION, SAMPLE_TICKET_NUMBER, TripStatus, airline_answer,
    with_offer,
};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// An airline that does not answer the send, counting every send it is handed.
#[derive(Default)]
struct Silent(AtomicUsize);

#[async_trait]
impl OutboxSender for Silent {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        Dispatched::completed()
    }
}

/// Asked later, the airline says it has the rebooking.
struct HasIt;

#[async_trait]
impl OutboxReconciler for HasIt {
    async fn reconcile(&self, _record: &OutboxRecord) -> Reconciled {
        Reconciled::Completed
    }
}

#[tokio::test]
async fn a_late_airline_answer_is_recorded_once() {
    let (text, requested) = rebooking_requested(turn(1));
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(requested)
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(turn(1), text)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();
    harness
        .handle(harness.click(turn(2), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();
    let airline = Arc::new(Silent::default());
    let dispatcher = OutboxDispatcher::new(
        Arc::clone(harness.stores.memory()) as Arc<dyn OutboxStore>,
        Arc::clone(&airline) as Arc<dyn OutboxSender>,
        DispatchConfig::new("desk").with_send_timeout(Duration::from_millis(20)),
    );
    dispatcher.run_once(now()).await.unwrap();
    let journal = harness.journal(turn(2)).await;
    let row = harness
        .stores
        .outbox_for_command(&journal[0].command_id)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(row.entry.status, OutboxStatus::OutcomeUnknown);

    let settled = dispatcher
        .reconcile(&row.entry.outbox_id, &HasIt, now())
        .await
        .unwrap();
    assert_eq!(settled, Reconciled::Completed);
    let answer = airline_answer(AirlineMode::Answers).expect("the airline answers");
    let delivered = harness.outside_batch("trip-1", "airline-answer-1", answer);
    let first = harness.trip.execute(delivered.clone()).await.unwrap();
    let after_first = harness.trip_revision("trip-1");
    let again = harness.trip.execute(delivered).await.unwrap();
    dispatcher.run_once(now()).await.unwrap();

    assert!(!first.idempotency_replay);
    assert_eq!(first.events.len(), 1);
    assert!(
        again.idempotency_replay,
        "the second delivery repeats nothing"
    );
    assert_eq!(harness.trip_revision("trip-1"), after_first);
    let trip = harness.trip_state("trip-1").unwrap();
    assert_eq!(trip.status, TripStatus::Ticketed);
    assert_eq!(trip.ticket_number.as_deref(), Some(SAMPLE_TICKET_NUMBER));
    assert_eq!(
        airline.0.load(Ordering::SeqCst),
        1,
        "sent once, never again"
    );
}
