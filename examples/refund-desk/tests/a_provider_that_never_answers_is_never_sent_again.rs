//! The payment provider takes the refund and never answers: the outcome is unknown, nothing
//! is sent again, and its late answer, delivered twice, is recorded once.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;
use turnframe::event::OutboxStatus;

#[tokio::test]
async fn a_provider_that_never_answers_is_never_sent_again() {
    let (run, desk) = runs::timeout().await.unwrap();
    let rows = desk.outbox(2).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entry.attempt_count, 1, "sent once, never again");
    assert_ne!(rows[0].entry.status, OutboxStatus::Pending);
    let events = desk.events(381);
    let recorded = events
        .iter()
        .filter(|e| *e == "order.provider_outcome_recorded")
        .count();
    assert_eq!(recorded, 1, "{events:?}");
    assert!(
        run.frames
            .iter()
            .any(|frame| frame.code.as_deref() == Some("OutcomeUnknown")),
        "the unknown outcome is recorded"
    );
}
