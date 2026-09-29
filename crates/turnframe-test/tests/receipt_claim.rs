//! Proposed values awaiting review, with what already exists.
//!
//! The response to the adopter's request says the view does not need a
//! first-class notion of a proposal, because a proposal is domain state. These
//! tests are the evidence for that answer: one named test per property the
//! request listed, all of them satisfied by a domain that models the proposal
//! itself and a framework that was not changed to accommodate it.
//!
//! The recipe they prove is written on
//! [`turnframe_test::workflows::claim`], so a reader can follow it without
//! this file and check it against this file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::command::{ConfirmationPolicy, RiskClass};
use turnframe_core::flow::{PhaseOwnership, ViewOf, WorkflowDefinition, check_view};
use turnframe_core::hash::{Digest, canonical_digest};
use turnframe_core::ids::{CaseRevision, OptionId};
use turnframe_core::interaction::{ActionClass, InteractionKind, StoredInteractionAction};
use turnframe_core::locale::Locale;
use turnframe_test::workflows::claim::{
    ABANDON_OPTION, ABANDONED_NOTICE, ACCEPT_OPTION, ClaimCommand, ClaimField, ClaimObligation,
    ClaimPhase, ClaimState, ClaimWorkflow, EDITED_NOTICE, NOT_NOW_OPTION, PROPOSED_NOTICE,
    Proposal, SAMPLE_ATTACHMENT, SAMPLE_REFERENCE, apply, complete_proposal, operations,
    partial_proposal, rejection, sample_attachment, under_review, under_review_with_reference,
    validate,
};

const CORRECTED: &str = "Hotel Tejo Lisboa";

fn workflow() -> ClaimWorkflow {
    ClaimWorkflow::default()
}

fn case(revision: u64) -> CaseRef {
    CaseRef::new("claim", "claim-1", CaseRevision(revision))
}

fn view_of(state: &ClaimState, revision: u64) -> ViewOf<ClaimWorkflow> {
    workflow().project(case(revision), Some(state))
}

/// The digest of the card's payload, which is what
/// [`CommandOrigin::ConfirmedInteraction`] carries and what a confirmation is
/// therefore bound to.
fn card_payload_hash(state: &ClaimState, revision: u64) -> Digest {
    let view = view_of(state, revision);
    let requirement = view
        .blocking_interaction
        .as_ref()
        .expect("a review card is open");
    let spec = workflow()
        .build_interaction(Some(state), &view, requirement)
        .expect("the card builds");
    spec.payload.hash().expect("the payload hashes")
}

// ---------------------------------------------------------------------------
// Property 1: the projector can express "these N fields are proposed from
// attachment A"
// ---------------------------------------------------------------------------

#[test]
fn the_projector_says_which_fields_are_proposed_from_which_document() {
    let state = under_review(complete_proposal());
    let view = view_of(&state, 2);

    assert_eq!(view.phase, ClaimPhase::AwaitingReview);
    assert_eq!(
        workflow().phase_ownership(&view.phase),
        PhaseOwnership::User,
        "the phase is user-owned for a reason the user did not initiate"
    );

    // One obligation per proposed field, parameterized: three proposed values
    // are three distinct obligations, not one checkpoint that flickers.
    let reviewing: Vec<ClaimField> = view
        .obligations
        .iter()
        .filter_map(|obligation| match obligation {
            ClaimObligation::ReviewProposedField { field } => Some(*field),
            _ => None,
        })
        .collect();
    assert_eq!(reviewing, ClaimField::ALL.to_vec());

    // The document is named, and the count is stated, in a notice with a stable
    // code rather than in prose the caller would have to parse.
    let notice = view
        .notices
        .iter()
        .find(|notice| notice.code == PROPOSED_NOTICE)
        .expect("the view names the document the values came from");
    let english = notice.text.resolve(&Locale::from("en"));
    assert!(english.contains(SAMPLE_ATTACHMENT), "{english}");
    assert!(english.contains('3'), "{english}");
    assert!(
        !notice
            .text
            .resolve(&Locale::from("it"))
            .eq_ignore_ascii_case(english),
        "the notice is translated"
    );

    check_view(&workflow(), &view).expect("the projection holds the §8.4 invariants");
}

#[test]
fn a_field_the_document_did_not_yield_is_a_different_obligation() {
    let state = under_review(partial_proposal());
    let view = view_of(&state, 2);

    let reviewing = view
        .obligations
        .iter()
        .filter(|o| matches!(o, ClaimObligation::ReviewProposedField { .. }))
        .count();
    let missing: Vec<ClaimField> = view
        .obligations
        .iter()
        .filter_map(|obligation| match obligation {
            ClaimObligation::ProvideField { field } => Some(*field),
            _ => None,
        })
        .collect();

    assert_eq!(reviewing, 2, "two values were read");
    assert_eq!(
        missing,
        vec![ClaimField::ReceiptDate],
        "the field nobody has a value for is its own obligation"
    );
    // A proposal that cannot be accepted does not offer to be accepted.
    let requirement = view.blocking_interaction.as_ref().unwrap();
    let payload = requirement.payload.as_ref().unwrap();
    assert!(payload.option(&OptionId::from(ACCEPT_OPTION)).is_none());
    assert_eq!(
        validate(Some(&state), &ClaimCommand::AcceptProposal)
            .unwrap_err()
            .code
            .as_str(),
        rejection::PROPOSAL_INCOMPLETE
    );
    check_view(&workflow(), &view).expect("an incomplete proposal still projects legally");
}

// ---------------------------------------------------------------------------
// Property 2: the card's payload hash covers the proposal rather than the whole
// state
// ---------------------------------------------------------------------------

#[test]
fn the_card_payload_hash_covers_the_proposal_not_the_whole_state() {
    let plain = under_review(complete_proposal());
    let with_reference = under_review_with_reference(complete_proposal(), SAMPLE_REFERENCE);

    // The reference is state, and it is not part of the proposal, so the card
    // is not about it: the hash does not move.
    assert_eq!(
        card_payload_hash(&plain, 2),
        card_payload_hash(&with_reference, 3),
        "editing state the card does not display must not change what the user confirmed"
    );

    // A proposed value is what the card is about: the hash moves.
    let corrected = apply(
        Some(&plain),
        &ClaimCommand::ReviseProposedField {
            field: ClaimField::Merchant,
            value: CORRECTED.to_owned(),
        },
    )
    .expect("revising applies")
    .state;
    assert_ne!(
        card_payload_hash(&plain, 2),
        card_payload_hash(&corrected, 3),
        "a card must not keep hashing to content the proposal no longer holds"
    );

    // And the binding is explicit: the metadata carries a digest of the
    // proposal itself, not of the case.
    let view = view_of(&plain, 2);
    let spec = workflow()
        .build_interaction(
            Some(&plain),
            &view,
            view.blocking_interaction.as_ref().unwrap(),
        )
        .unwrap();
    let expected = canonical_digest(plain.proposal.as_ref().unwrap()).unwrap();
    assert_eq!(
        spec.payload.metadata.get("proposal_hash").unwrap(),
        &serde_json::Value::String(expected.as_str().to_owned())
    );
    assert_eq!(
        spec.payload.metadata.get("attachment_id").unwrap(),
        &serde_json::Value::String(SAMPLE_ATTACHMENT.to_owned())
    );
    assert!(
        spec.payload.metadata.get("bound_revision").is_none(),
        "the revision is the interaction's own binding, not payload content: \
         copying it into the payload would make the hash move on every unrelated edit"
    );
    assert!(
        spec.binds_to_revision,
        "the card is still bound to the revision it was rendered at"
    );
}

// ---------------------------------------------------------------------------
// Property 3: an act that edits a proposed field is distinguishable from an act
// that answers the review
// ---------------------------------------------------------------------------

#[test]
fn editing_a_proposed_field_is_a_different_operation_from_answering_the_review() {
    let state = under_review(complete_proposal());
    let view = view_of(&state, 2);

    // Two keys in the catalog, not one act with a mode flag.
    let keys: Vec<String> = workflow()
        .operations(&view)
        .into_iter()
        .map(|act| act.key.as_str().to_owned())
        .collect();
    assert!(keys.contains(&operations::REVISE_PROPOSED_FIELD.to_owned()));
    assert!(keys.contains(&operations::ACCEPT_PROPOSAL.to_owned()));
    assert_ne!(
        operations::REVISE_PROPOSED_FIELD,
        operations::ACCEPT_PROPOSAL
    );
    // Reading a document is not an act a user can ask for, in any phase.
    for phase_state in [
        None,
        Some(ClaimState::default()),
        Some(under_review(complete_proposal())),
        Some(under_review(partial_proposal())),
    ] {
        let phase_view = workflow().project(case(2), phase_state.as_ref());
        assert!(
            workflow()
                .operations(&phase_view)
                .iter()
                .all(|act| !act.key.as_str().ends_with(".propose_fields")),
            "extraction must never be in the interpretation catalog, phase {:?}",
            phase_view.phase
        );
    }

    // Two policies. Editing a proposal touches nothing outside it; answering
    // the review writes derived values into the record.
    let revise = workflow().command_policy(
        Some(&state),
        &ClaimCommand::ReviseProposedField {
            field: ClaimField::Merchant,
            value: CORRECTED.to_owned(),
        },
    );
    let accept = workflow().command_policy(Some(&state), &ClaimCommand::AcceptProposal);
    assert_eq!(revise.risk, RiskClass::ReversibleLowRisk);
    assert_eq!(revise.confirmation, ConfirmationPolicy::None);
    assert_eq!(accept.risk, RiskClass::SensitiveDataChange);
    assert_eq!(accept.confirmation, ConfirmationPolicy::ReviewCard);

    // Two effects. Revising leaves the review open and marks the value edited.
    let revised = apply(
        Some(&state),
        &ClaimCommand::ReviseProposedField {
            field: ClaimField::Merchant,
            value: CORRECTED.to_owned(),
        },
    )
    .expect("revising applies");
    let proposal = revised.state.proposal.as_ref().expect("still a proposal");
    assert_eq!(
        ClaimWorkflow::phase_of(&revised.state),
        ClaimPhase::AwaitingReview
    );
    assert!(revised.state.recorded.is_empty(), "nothing was recorded");
    let merchant = proposal.field(ClaimField::Merchant).unwrap();
    assert_eq!(merchant.value, CORRECTED);
    assert!(merchant.edited, "the human correction is marked as one");
    assert!(
        view_of(&revised.state, 3)
            .notices
            .iter()
            .any(|notice| notice.code == EDITED_NOTICE),
        "the view says a value was changed by hand"
    );

    // Answering the review ends it and turns the proposal into the record.
    let accepted = apply(Some(&revised.state), &ClaimCommand::AcceptProposal)
        .expect("accepting applies")
        .state;
    assert_eq!(ClaimWorkflow::phase_of(&accepted), ClaimPhase::Recorded);
    assert!(accepted.proposal.is_none(), "the proposal is spent");
    assert_eq!(accepted.recorded.len(), 3);
    let recorded = accepted
        .recorded
        .iter()
        .find(|recorded| recorded.field == ClaimField::Merchant)
        .unwrap();
    assert_eq!(recorded.value, CORRECTED);
    assert_eq!(recorded.from_attachment, sample_attachment());
    assert!(
        recorded.corrected,
        "provenance survives the review: this value came from a human, not the extractor"
    );

    // The card never offers to revise: revising is typed, answering is clicked.
    let payload = view.blocking_interaction.unwrap().payload.unwrap();
    for option in &payload.options {
        if let StoredInteractionAction::ApplyOperation { operation, .. } = &option.action {
            assert_ne!(operation.as_str(), operations::REVISE_PROPOSED_FIELD);
        }
    }
}

// ---------------------------------------------------------------------------
// Property 4: abandoning the review has a defined effect on the proposal
// ---------------------------------------------------------------------------

#[test]
fn abandoning_the_review_discards_the_reading_and_keeps_the_document() {
    let state = under_review(complete_proposal());
    let applied = apply(Some(&state), &ClaimCommand::AbandonReview).expect("abandoning applies");
    let after = applied.state;

    // The domain's explicit choice, in three parts.
    assert!(after.proposal.is_none(), "the reading is gone");
    assert_eq!(
        after.attachment,
        Some(sample_attachment()),
        "the document stays: only the reading of it was thrown away"
    );
    assert_eq!(after.abandoned_from, Some(sample_attachment()));
    assert!(after.recorded.is_empty(), "nothing was recorded");
    assert_eq!(ClaimWorkflow::phase_of(&after), ClaimPhase::Extracting);

    // The view says so, with a stable code.
    let view = view_of(&after, 3);
    assert!(view.blocking_interaction.is_none(), "the card is gone");
    assert!(
        view.notices
            .iter()
            .any(|notice| notice.code == ABANDONED_NOTICE),
        "the reason the proposal vanished must survive in the view"
    );
    assert_eq!(
        view.obligations,
        vec![ClaimObligation::ExtractFields {
            attachment_id: sample_attachment()
        }]
    );

    // And the effect is that the same document can be read again.
    let reread = apply(
        Some(&after),
        &ClaimCommand::ProposeFields {
            attachment_id: sample_attachment(),
            fields: complete_proposal().fields,
        },
    )
    .expect("the document can be read again")
    .state;
    assert!(reread.proposal.is_some());
    assert!(
        reread.abandoned_from.is_none(),
        "a fresh reading supersedes the memory of the abandoned one"
    );
    check_view(&workflow(), &view_of(&reread, 4)).expect("the new review projects legally");
}

#[test]
fn declining_the_card_is_not_abandoning_the_proposal() {
    let state = under_review(complete_proposal());
    let payload = view_of(&state, 2)
        .blocking_interaction
        .unwrap()
        .payload
        .unwrap();

    // Three options, three different meanings, and only one of them is an
    // abandonment.
    let not_now = payload.option(&OptionId::from(NOT_NOW_OPTION)).unwrap();
    assert_eq!(not_now.action, StoredInteractionAction::DeclineCommands);
    assert_eq!(not_now.action.action_class(), ActionClass::NoCommands);

    let abandon = payload.option(&OptionId::from(ABANDON_OPTION)).unwrap();
    assert!(matches!(
        &abandon.action,
        StoredInteractionAction::ApplyOperation { operation, .. }
            if operation.as_str() == operations::ABANDON_REVIEW
    ));
    let accept = payload.option(&OptionId::from(ACCEPT_OPTION)).unwrap();
    assert!(matches!(
        &accept.action,
        StoredInteractionAction::ApplyOperation { operation, .. }
            if operation.as_str() == operations::ACCEPT_PROPOSAL
    ));

    // Declining commits nothing, so the card comes back from the same state on
    // the next turn: the projection is a pure function of state, and the state
    // still holds the proposal.
    assert!(
        view_of(&state, 9).blocking_interaction.is_some(),
        "a declined card is re-derived while the proposal is still there"
    );
    payload
        .validate_for(InteractionKind::ReviewChanges)
        .expect("a review card must be answerable: one way to authorize, one way to decline");
}

// ---------------------------------------------------------------------------
// The rules that keep the sample honest
// ---------------------------------------------------------------------------

#[test]
fn a_second_reading_cannot_replace_a_proposal_the_user_is_looking_at() {
    let state = under_review(complete_proposal());
    let refused = validate(
        Some(&state),
        &ClaimCommand::ProposeFields {
            attachment_id: sample_attachment(),
            fields: partial_proposal().fields,
        },
    )
    .unwrap_err();
    assert_eq!(refused.code.as_str(), rejection::REVIEW_ALREADY_OPEN);
}

#[test]
fn a_proposal_holds_its_fields_in_a_canonical_order() {
    let mut shuffled = complete_proposal().fields;
    shuffled.reverse();
    let rebuilt = Proposal::new(sample_attachment(), shuffled);
    assert_eq!(
        rebuilt.fields.iter().map(|f| f.field).collect::<Vec<_>>(),
        ClaimField::ALL.to_vec(),
        "two readings that found the same values must hash the same"
    );
    assert!(rebuilt.is_complete());
    assert!(
        partial_proposal()
            .missing()
            .contains(&ClaimField::ReceiptDate)
    );
}
