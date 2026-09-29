//! A failed command refreshes projected facts whether or not the revision moved.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, narration, notice_codes, receipt_codes};
use turnframe_core::error::{DomainRejection, ExecutionError};
use turnframe_core::hash::canonical_digest;
use turnframe_core::ids::{CaseRevision, TurnId};
use turnframe_core::locale::Locale;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{
    REBOOK_CONFIRM_OPTION, awaiting_rebooking_confirmation, incomplete_case,
};

async fn failed_click(error: ExecutionError, fail_refresh: bool) {
    let first = TurnId::from(uuid::Uuid::from_u128(1));
    let second = TurnId::from(uuid::Uuid::from_u128(2));
    let text = "show the rebooking card";
    let says_nothing = UnderstandingBuilder::of(text).build().unwrap();
    let original = awaiting_rebooking_confirmation();
    let mut current = original.clone();
    current.offer.as_mut().unwrap().fare_difference_cents = 13_200;
    let expected_hash = canonical_digest(&current).unwrap();
    let refreshed_revision = match &error {
        ExecutionError::RevisionConflict(conflict) => conflict.current_revision,
        _ => CaseRevision(3),
    };
    let provider = narrating()
        .acknowledging("Ready.")
        .acknowledging("Not sent.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, original)
        .trip("trip-2", "Trip 2", 3, incomplete_case())
        .trip_fails_once_armed(error)
        .trip_refresh_after_failure(current, refreshed_revision, fail_refresh)
        .understands(says_nothing)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness.handle(harness.turn(first, text)).await.unwrap();
    let old_card = harness.blocking_card("trip", "trip-1").await;
    let target_loads = harness.trip_load_count("trip-1");
    let untouched_loads = harness.trip_load_count("trip-2");
    harness.arm_trip_failure();
    let reply = harness
        .handle(harness.click(second, old_card.id, REBOOK_CONFIRM_OPTION, 3))
        .await
        .unwrap();

    assert!(harness.events("trip", "trip-1").await.is_empty());
    assert!(receipt_codes(&reply).is_empty());
    assert_eq!(harness.trip_revision("trip-1"), refreshed_revision);
    assert_eq!(
        harness.trip_load_count("trip-1") - target_loads,
        2,
        "the attempted case is loaded once normally and once after execution"
    );
    assert_eq!(
        harness.trip_load_count("trip-2") - untouched_loads,
        1,
        "an untouched case must not be reloaded after execution"
    );
    let open = harness.open_cards("trip", "trip-1").await;
    if fail_refresh {
        assert!(
            open.is_empty(),
            "an unavailable refresh must not revive the old confirmation"
        );
        assert_eq!(reply.interactions().count(), 0);
        assert!(narration(&reply).is_empty());
        assert_eq!(provider.calls_for(ModelPurpose::Acknowledge).len(), 1);
        assert!(
            notice_codes(&reply)
                .contains(&turnframe_runtime::compose::notice::CASE_REFRESH_UNAVAILABLE.to_owned())
        );
    } else {
        assert_eq!(open.len(), 1, "I5: only the replacement is active");
        let fresh = &open[0];
        assert_ne!(fresh.id, old_card.id);
        assert_eq!(fresh.case_ref.expected_revision, refreshed_revision);
        assert_eq!(
            fresh.payload.metadata["preview_hash"],
            expected_hash.as_str()
        );
        assert!(
            fresh
                .payload
                .body
                .as_ref()
                .unwrap()
                .resolve(&Locale::from("en-GB"))
                .contains("€132.00")
        );
        assert_eq!(reply.interactions().count(), 1);
        assert_eq!(provider.calls_for(ModelPurpose::Acknowledge).len(), 2);
    }
}

#[tokio::test]
async fn a_refused_click_rebuilds_the_card_from_current_facts_at_the_same_revision() {
    failed_click(
        ExecutionError::Rejected(DomainRejection::new(
            "assessment.changed",
            "assessment.changed",
        )),
        false,
    )
    .await;
}

#[tokio::test]
async fn a_revision_conflict_also_refreshes_the_case_before_rebuilding() {
    failed_click(
        ExecutionError::RevisionConflict(turnframe_core::error::RevisionConflict {
            expected: turnframe_core::case::CaseRef::new("trip", "trip-1", CaseRevision(3)),
            current_revision: CaseRevision(4),
        }),
        false,
    )
    .await;
}

#[tokio::test]
async fn a_failed_refresh_never_falls_back_to_the_opening_confirmation() {
    failed_click(
        ExecutionError::Rejected(DomainRejection::new(
            "assessment.changed",
            "assessment.changed",
        )),
        true,
    )
    .await;
}
