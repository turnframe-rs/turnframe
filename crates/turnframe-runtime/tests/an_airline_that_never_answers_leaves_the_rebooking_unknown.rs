//! A rebooking the airline never answers was sent and is not confirmed: its outbox row
//! ends as an unknown outcome, never a retry, the trip waits on the airline, and the reply
//! says it was sent, never that it is done.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use support::{Harness, narrating, now, rebooking_requested, receipt_codes};
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::response::claim_guard;
use turnframe_runtime::dispatch::{DispatchConfig, Dispatched, OutboxDispatcher, OutboxSender};
use turnframe_store::outbox::OutboxStore;
use turnframe_test::workflows::trip::{
    AirlineMode, REBOOK_CONFIRM_OPTION, TripStatus, airline_answer, with_offer,
};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// The sample airline behind the outbox, answering as its mode says or not at all.
struct Airline(AirlineMode);

#[async_trait]
impl OutboxSender for Airline {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        if airline_answer(self.0).is_none() {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Dispatched::completed()
    }
}

#[tokio::test]
async fn an_airline_that_never_answers_leaves_the_rebooking_unknown() {
    let (text, requested) = rebooking_requested(turn(1));
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(requested)
        .provider(
            narrating()
                .acknowledging("Here is the card.")
                .acknowledging("Sent to the airline; they have not answered yet.")
                .build_shared(),
        )
        .build()
        .await;
    harness.handle(harness.turn(turn(1), text)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();
    let reply = harness
        .handle(harness.click(turn(2), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();

    let dispatcher = OutboxDispatcher::new(
        Arc::clone(harness.stores.memory()) as Arc<dyn OutboxStore>,
        Arc::new(Airline(AirlineMode::NeverAnswers)),
        DispatchConfig::new("desk").with_send_timeout(Duration::from_millis(20)),
    );
    dispatcher.run_once(now()).await.unwrap();

    let journal = harness.journal(turn(2)).await;
    let rows = harness
        .stores
        .outbox_for_command(&journal[0].command_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entry.status, OutboxStatus::OutcomeUnknown);
    let trip = harness.trip_state("trip-1").unwrap();
    assert_eq!(trip.status, TripStatus::Rebooking, "waiting on the airline");
    assert_eq!(trip.ticket_number, None);

    assert_eq!(
        receipt_codes(&reply),
        vec!["trip.rebooking_sent".to_owned()]
    );
    let said: Vec<String> = reply
        .receipts()
        .map(|receipt| receipt.body.resolve(&Locale::from("en-GB")).to_owned())
        .collect();
    assert!(
        said.iter().all(|body| body.contains("not confirmed")),
        "{said:?}"
    );
    claim_guard::verify(&reply).expect("every claim in the reply rests on a receipt");
}
