//! The receipt-claim sample: **proposed values awaiting review**, done with
//! what already exists.
//!
//! A document arrives, something reads values out of it, those values are
//! proposed but not applied, a card asks the user to confirm them, and only then
//! do they become state. It is a fair question whether the view should grow a
//! word for a value that is neither set nor unset. The answer this sample
//! argues is **no**: a proposal is domain state, and a workflow that models it
//! as such gets every property out of the vocabulary already there, while a
//! framework that grew the concept would force every projector — including the
//! ones for domains that never see a document — to reason about a state they do
//! not have.
//!
//! The five steps of the recipe, and what it costs, are in
//! [`docs/recipes.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/recipes.md).
//! Each step is proved by a test in `tests/receipt_claim.rs`.
//!
//! ```
//! use turnframe_core::case::CaseRef;
//! use turnframe_core::flow::{PhaseOwnership, WorkflowDefinition, check_view};
//! use turnframe_core::ids::CaseRevision;
//! use turnframe_test::workflows::claim::{
//!     ClaimPhase, ClaimWorkflow, complete_proposal, under_review,
//! };
//!
//! let workflow = ClaimWorkflow::default();
//! let state = under_review(complete_proposal());
//! let view = workflow.project(CaseRef::new("claim", "doc-1", CaseRevision(2)), Some(&state));
//!
//! assert_eq!(view.phase, ClaimPhase::AwaitingReview);
//! assert_eq!(workflow.phase_ownership(&view.phase), PhaseOwnership::User);
//! // One obligation per proposed value, and a card to answer.
//! assert_eq!(view.obligations.len(), 3);
//! assert!(view.blocking_interaction.is_some());
//! assert!(check_view(&workflow, &view).is_ok());
//! ```

pub mod command;
pub mod definition;
pub mod model;
pub mod state;

pub use command::{AttachArgs, ClaimCommand, ClaimEvent, ReferenceArgs, ReviseArgs, operations};
pub use definition::{
    ABANDON_OPTION, ABANDONED_NOTICE, ACCEPT_OPTION, ClaimWorkflow, EDITED_NOTICE, MAX_VALUE_CHARS,
    NOT_NOW_OPTION, PROPOSED_NOTICE, REVIEW_KEY, apply, rejection, validate,
};
pub use model::{
    CORRECTED_MERCHANT, ClaimModel, EXTRACTED_DATE, EXTRACTED_MERCHANT, EXTRACTED_TOTAL,
    OTHER_REFERENCE, SAMPLE_ATTACHMENT, SAMPLE_REFERENCE, abandoned, awaiting_extraction,
    complete_proposal, partial_proposal, sample_attachment, under_review,
    under_review_with_reference,
};
pub use state::{
    ClaimField, ClaimObligation, ClaimOutcome, ClaimPhase, ClaimState, ClaimStatus, Proposal,
    ProposedField, RecordedField,
};

/// An in-memory executor for the claim workflow.
pub type ClaimExecutor = crate::workflows::InMemoryExecutor<ClaimWorkflow>;
