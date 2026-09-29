//! An assistant turn built from a sample's receipts must survive the claim
//! guard (spec §17.3, §18.3, I16).
//!
//! The receipts are not written by hand here: they are rendered by the
//! workflow from the events the in-memory executor really committed. That is
//! the whole point — if a sample's `receipts` implementation ever invented an
//! event id or forgot to copy one, this test fails instead of a production
//! narration claiming something the ledger does not back.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use futures::executor::block_on;
use turnframe_core::event::{
    EventRedaction, OperationalReceipt, ReceiptEvent, ReceiptSeverity, RedactedEvent,
};
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::ids::{BlockId, ConversationId, TurnId};
use turnframe_core::locale::Locale;
use turnframe_core::prelude::{CaseRef, CaseRevision};
use turnframe_core::response::{
    AssistantTurn, GeneratedTransition, NarratableFact, ReceiptBlock, ReplayToken, ResponseBlock,
    claim_guard,
};
use turnframe_test::assertions::{identical_blocks, receipts_backed_by_events};
use turnframe_test::workflows::claim::{
    ClaimCommand, ClaimEvent, ClaimExecutor, ClaimField, ClaimWorkflow, SAMPLE_REFERENCE,
    complete_proposal, sample_attachment,
};
use turnframe_test::workflows::traveler::{
    SAMPLE_EMAIL, SAMPLE_LOYALTY_NUMBER, SAMPLE_NAME as TRAVELER_NAME, TravelerCommand,
    TravelerEvent, TravelerExecutor, TravelerWorkflow,
};
use turnframe_test::workflows::trip::{
    Payer, SAMPLE_NAME, TripCommand, TripEvent, TripExecutor, TripWorkflow, extra_id_for,
    sample_new_extra, sample_quote, sample_travel_date, sample_traveler,
};

/// The commands that take a trip from nothing to a rebooking sent, one per turn.
fn trip_script() -> Vec<TripCommand> {
    vec![
        TripCommand::Open,
        TripCommand::ChangeTraveler {
            traveler: sample_traveler(),
        },
        TripCommand::SetName {
            value: SAMPLE_NAME.to_owned(),
        },
        TripCommand::SetTravelDate {
            value: sample_travel_date(),
        },
        TripCommand::AddExtra {
            extra: sample_new_extra(0),
        },
        TripCommand::AssignPayer {
            extra_id: extra_id_for(0, &sample_new_extra(0).description),
            payer: Payer::Airline,
        },
        sample_quote(1),
        TripCommand::RequestRebooking { leg: 1 },
        TripCommand::Rebook,
    ]
}

fn traveler_script() -> Vec<TravelerCommand> {
    vec![
        TravelerCommand::CreateDraft,
        TravelerCommand::SetName {
            value: TRAVELER_NAME.to_owned(),
        },
        TravelerCommand::ChangeEmail {
            value: SAMPLE_EMAIL.to_owned(),
        },
        TravelerCommand::SetLoyaltyNumber {
            value: SAMPLE_LOYALTY_NUMBER.to_owned(),
        },
        TravelerCommand::Activate,
    ]
}

/// A receipt arrives, is read, one value is corrected, and the reading is
/// accepted: the whole proposal lifecycle in five turns.
fn claim_script() -> Vec<ClaimCommand> {
    vec![
        ClaimCommand::CreateDraft,
        ClaimCommand::SetReference {
            value: SAMPLE_REFERENCE.to_owned(),
        },
        ClaimCommand::AttachReceipt {
            attachment_id: sample_attachment(),
        },
        ClaimCommand::ProposeFields {
            attachment_id: sample_attachment(),
            fields: complete_proposal().fields,
        },
        ClaimCommand::ReviseProposedField {
            field: ClaimField::Merchant,
            value: "Hotel Tejo Lisboa".to_owned(),
        },
        ClaimCommand::AcceptProposal,
    ]
}

/// Runs one command per batch and returns every event committed, in order, in
/// the form the receipt renderer takes.
fn run<W, E>(
    executor: &E,
    workflow: &str,
    case: &str,
    script: Vec<W::Command>,
) -> Vec<ReceiptEvent<W::Event>>
where
    W: WorkflowDefinition,
    W::Command: Clone,
    E: WorkflowExecutor<W>,
{
    let mut events = Vec::new();
    for (step, command) in script.into_iter().enumerate() {
        let case_ref = CaseRef::new(workflow, case, CaseRevision(step as u64));
        let batch = support::batch(
            support::turn(u8::try_from(step).unwrap()),
            &case_ref,
            &support::confirmed_click(),
            vec![command],
        );
        let commit = block_on(executor.execute(batch)).expect("the scripted command commits");
        assert!(!commit.idempotency_replay);
        events.extend(commit.receipt_events());
    }
    events
}

/// Renders the receipts as response blocks, plus one model-authored transition
/// that cites the last outcome. Block ids are derived from the receipt ids so
/// two runs of the same script produce the same turn.
fn assistant_turn(receipts: &[OperationalReceipt]) -> AssistantTurn {
    let mut blocks: Vec<ResponseBlock> = receipts
        .iter()
        .map(|receipt| {
            ResponseBlock::Receipt(ReceiptBlock {
                block_id: BlockId::new(format!("receipt-{}", receipt.receipt_id)),
                receipt: receipt.clone(),
            })
        })
        .collect();
    if let Some(last) = receipts.last() {
        blocks.push(ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("transition-1"),
            text: "Ecco cosa ho fatto.".to_owned(),
            facts_used: vec![NarratableFact::OperationalOutcome {
                receipt_id: last.receipt_id,
                event_ids: last.event_ids.clone(),
                status_code: last.status_code.clone(),
            }],
        }));
    }
    AssistantTurn {
        turn_id: TurnId::nil(),
        conversation_id: ConversationId::nil(),
        blocks,
        subjects: Vec::new(),
        expectations: Vec::new(),
        replay_token: ReplayToken::from("replay-1"),
        done: Vec::new(),
    }
}

#[test]
fn a_trip_turn_passes_the_claim_guard() {
    let workflow = TripWorkflow::default();
    let executor = TripExecutor::default();
    let events: Vec<ReceiptEvent<TripEvent>> =
        run::<TripWorkflow, _>(&executor, "trip", "trip-1", trip_script());
    let receipts = workflow.receipts(&events, &Locale::from("it-IT"));

    assert_eq!(receipts.len(), events.len());
    receipts_backed_by_events(&receipts, &events).expect("every receipt cites committed events");
    claim_guard::verify(&assistant_turn(&receipts)).expect("the turn claims nothing extra");
}

#[test]
fn a_traveler_turn_passes_the_claim_guard() {
    let workflow = TravelerWorkflow::new().with_cards();
    let executor = TravelerExecutor::new(workflow);
    let events: Vec<ReceiptEvent<TravelerEvent>> =
        run::<TravelerWorkflow, _>(&executor, "traveler", "trav-1", traveler_script());
    let receipts = workflow.receipts(&events, &Locale::from("en"));

    assert_eq!(receipts.len(), events.len());
    receipts_backed_by_events(&receipts, &events).expect("every receipt cites committed events");
    claim_guard::verify(&assistant_turn(&receipts)).expect("the turn claims nothing extra");
}

/// The claim sample narrates a proposal without ever claiming it was recorded.
///
/// This is where a receipt-reading domain is most likely to over-claim: the
/// values are on screen, the card is open, and a receipt that said "recorded"
/// would be wrong for exactly as long as the review is open. The receipts are
/// rendered from the events the executor really committed, so a receipt that
/// got ahead of the ledger fails here.
#[test]
fn a_receipt_turn_passes_the_claim_guard() {
    let workflow = ClaimWorkflow::default();
    let executor = ClaimExecutor::default();
    let events: Vec<ReceiptEvent<ClaimEvent>> =
        run::<ClaimWorkflow, _>(&executor, "claim", "claim-1", claim_script());
    let receipts = workflow.receipts(&events, &Locale::from("it-IT"));

    assert_eq!(receipts.len(), events.len());
    receipts_backed_by_events(&receipts, &events).expect("every receipt cites committed events");
    claim_guard::verify(&assistant_turn(&receipts)).expect("the turn claims nothing extra");

    // The receipt for the reading says values were read and nothing recorded;
    // only the acceptance is allowed to say otherwise.
    let proposed = receipts
        .iter()
        .find(|receipt| receipt.status_code == "claim.fields_proposed")
        .expect("the reading produced a receipt");
    let english = proposed.body.resolve(&Locale::from("en"));
    assert!(english.contains("waiting for review"), "{english}");
    assert!(english.contains("Nothing has been recorded"), "{english}");
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.status_code == "claim.proposal_accepted"),
        "the acceptance is the event that turns a proposal into the record"
    );
}

#[test]
fn the_same_script_renders_the_same_ordered_blocks() {
    let workflow = TripWorkflow::default();
    let first = assistant_turn(&workflow.receipts(
        &run::<TripWorkflow, _>(&TripExecutor::default(), "trip", "trip-1", trip_script()),
        &Locale::from("it-IT"),
    ));
    let second = assistant_turn(&workflow.receipts(
        &run::<TripWorkflow, _>(&TripExecutor::default(), "trip", "trip-1", trip_script()),
        &Locale::from("it-IT"),
    ));

    identical_blocks(&first, &second).expect("receipts are deterministic");
}

#[test]
fn a_claim_without_its_receipt_block_is_refused() {
    let workflow = TripWorkflow::default();
    let events = run::<TripWorkflow, _>(&TripExecutor::default(), "trip", "trip-1", trip_script());
    let receipts = workflow.receipts(&events, &Locale::from("it-IT"));
    let mut turn = assistant_turn(&receipts);
    turn.blocks
        .retain(|block| !matches!(block, ResponseBlock::Receipt(_)));

    assert!(matches!(
        claim_guard::verify(&turn),
        Err(claim_guard::ClaimViolation::UnbackedOperationalOutcome { .. })
    ));
}

#[test]
fn receipts_are_localized_in_italian_and_english() {
    let workflow = TripWorkflow::default();
    let events = run::<TripWorkflow, _>(
        &TripExecutor::default(),
        "trip",
        "trip-1",
        vec![TripCommand::Open],
    );
    let receipts = workflow.receipts(&events, &Locale::from("it-IT"));
    let receipt = receipts.first().expect("one event, one receipt");

    assert_eq!(receipt.title.resolve(&Locale::from("en")), "Case opened");
    assert_eq!(
        receipt.title.resolve(&Locale::from("it-IT")),
        "Pratica aperta"
    );
    assert_eq!(receipt.title.resolve(&Locale::from("fr")), "Case opened");
}

/// Erases one event of a run, the way the store's redaction does: identity,
/// type and instant survive, the payload does not.
fn erase(event: &ReceiptEvent<TripEvent>) -> ReceiptEvent<TripEvent> {
    ReceiptEvent::Redacted(RedactedEvent {
        event_id: event.event_id(),
        event_type: event.event_type().to_owned(),
        occurred_at: event.occurred_at(),
        redaction: EventRedaction {
            redacted_at: event.occurred_at(),
            authority: "erasure-request-8842".into(),
        },
    })
}

/// A turn rendered after an erasure still holds together, and still tells the
/// truth (spec §17.3, I16).
///
/// This is the shape the whole redaction path exists for. The event that
/// carried a traveler's name has had its payload erased; it is still in the
/// ledger, at its position, so the claim it authorized is still backed and the
/// guard still passes. What must change is what the receipt *says*: it may no
/// longer describe the change, because the values that described it are gone,
/// and it may not silently disappear either — a turn that quietly drops a
/// receipt reads as a turn in which nothing happened.
#[test]
fn a_receipt_over_an_erased_event_is_honest_and_still_backed() {
    let workflow = TripWorkflow::default();
    let events: Vec<ReceiptEvent<TripEvent>> =
        run::<TripWorkflow, _>(&TripExecutor::default(), "trip", "trip-1", trip_script());
    let intact = workflow.receipts(&events, &Locale::from("en"));

    // The extra is where the personal data is: a description, a quantity and a
    // price, quoted verbatim in the receipt that announced it.
    let position = events
        .iter()
        .position(|event| event.event_type() == "trip.extra_added")
        .expect("the script adds an extra");
    let quoted = intact[position].body.resolve(&Locale::from("en"));
    assert!(
        quoted.contains(&sample_new_extra(0).description),
        "before the erasure the receipt quotes the value: {quoted}"
    );

    let mut erased = events.clone();
    erased[position] = erase(&events[position]);
    let after = workflow.receipts(&erased, &Locale::from("en"));

    // Nothing was dropped and nothing moved: one receipt per event, still.
    assert_eq!(after.len(), intact.len(), "no receipt may go missing");
    assert_eq!(
        after
            .iter()
            .map(|receipt| receipt.event_ids.clone())
            .collect::<Vec<_>>(),
        intact
            .iter()
            .map(|receipt| receipt.event_ids.clone())
            .collect::<Vec<_>>(),
        "every receipt still cites the event it always cited"
    );

    // The one receipt that changed says something true of an erased event, in
    // both languages, and does not quote what is gone.
    let redacted = &after[position];
    assert_eq!(redacted.status_code, "trip.detail_erased");
    assert_eq!(redacted.severity, ReceiptSeverity::Info);
    for locale in ["en", "it"] {
        let body = redacted.body.resolve(&Locale::from(locale));
        assert!(
            !body.contains(&sample_new_extra(0).description),
            "the erased value must not survive in the copy ({locale}): {body}"
        );
        assert!(
            !body.is_empty(),
            "an erased event still gets a sentence ({locale})"
        );
    }
    assert!(
        redacted.is_event_backed(),
        "the event is still in the ledger, so the receipt is still backed"
    );

    // And every other receipt is untouched: erasing one payload is not a
    // reason to re-render the rest of the turn.
    for (index, (was, is)) in intact.iter().zip(after.iter()).enumerate() {
        if index != position {
            assert_eq!(was, is, "receipt {index} must be unchanged");
        }
    }

    // The two checks the library actually runs over a turn.
    receipts_backed_by_events(&after, &erased)
        .expect("a receipt over an erased event still cites a real event");
    claim_guard::verify(&assistant_turn(&after)).expect("the turn claims nothing extra");
}

/// Erasing every payload of a turn does not produce a turn that claims nothing.
///
/// The failure this rules out is the quiet one: a domain that rendered `None`
/// for an erased event would leave a turn with a narration and no receipts,
/// which the guard would refuse only because the narration cites an outcome. A
/// turn with neither reads as success and is backed by nothing at all.
#[test]
fn a_turn_whose_events_were_all_erased_still_renders_receipts() {
    let workflow = TripWorkflow::default();
    let events: Vec<ReceiptEvent<TripEvent>> =
        run::<TripWorkflow, _>(&TripExecutor::default(), "trip", "trip-1", trip_script());
    let erased: Vec<ReceiptEvent<TripEvent>> = events.iter().map(erase).collect();
    let receipts = workflow.receipts(&erased, &Locale::from("it"));

    assert_eq!(receipts.len(), events.len(), "one receipt per event, still");
    assert!(
        receipts
            .iter()
            .all(turnframe_core::event::OperationalReceipt::is_event_backed)
    );
    receipts_backed_by_events(&receipts, &erased).expect("every receipt cites a real event");
    claim_guard::verify(&assistant_turn(&receipts)).expect("the turn claims nothing extra");
}
