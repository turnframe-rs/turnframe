//! The traveler sample: the flat onboarding slice.
//!
//! Where the trip proves the awkward case, the traveler proves the ordinary one: three
//! fields, no parameterized obligation, every value given in text, and a card that confirms
//! the activation once every field is settled. [`TravelerWorkflow::with_cards`] also puts a
//! card in front of deletion and a new contact address, for testing cards.
//!
//! A collected field has three states: never asked, answered, and declined. [`FieldState`]
//! makes the third real, the projection closes the obligation and keeps the reason in a
//! notice code, and [`DeclineReason::worth_asking_again`] says which refusals can change.
//! The recipe is in
//! [`docs/recipes.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/recipes.md),
//! and `tests/three_valued_field.rs` proves it.
//!
//! ```
//! use turnframe_core::case::CaseRef;
//! use turnframe_core::flow::{PhaseOwnership, WorkflowDefinition};
//! use turnframe_core::ids::CaseRevision;
//! use turnframe_test::workflows::traveler::{TravelerPhase, TravelerWorkflow, awaiting_activation};
//!
//! let workflow = TravelerWorkflow::new();
//! let view = workflow.project(
//!     CaseRef::new("traveler", "c-1", CaseRevision(3)),
//!     Some(&awaiting_activation()),
//! );
//!
//! assert_eq!(view.phase, TravelerPhase::AwaitingActivation);
//! assert_eq!(workflow.phase_ownership(&view.phase), PhaseOwnership::User);
//! assert!(view.blocking_interaction.is_some());
//! ```
//!

pub mod command;
pub mod definition;
pub mod model;
pub mod state;

pub use command::{
    CreateDraftArgs, DeclineArgs, TravelerCommand, TravelerEvent, ValueArgs, operations,
};
pub use definition::{
    ACTIVATE_OPTION, ACTIVATION_KEY, KEEP_DRAFT_OPTION, LOYALTY_NUMBER_MAX_DIGITS,
    LOYALTY_NUMBER_MIN_DIGITS, MAX_NAME_CHARS, TravelerWorkflow, apply, looks_like_email,
    looks_like_loyalty_number, rejection, validate,
};
pub use model::{
    OTHER_EMAIL, SAMPLE_EMAIL, SAMPLE_LOYALTY_NUMBER, SAMPLE_NAME, TravelerModel, active_traveler,
    awaiting_activation, declined_loyalty_number, incomplete_draft,
};
pub use state::{
    DeclineReason, FieldState, TravelerObligation, TravelerOutcome, TravelerPhase, TravelerState,
    TravelerStatus,
};

/// An in-memory executor for the traveler workflow.
pub type TravelerExecutor = crate::workflows::InMemoryExecutor<TravelerWorkflow>;
