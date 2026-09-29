//! `proptest` strategies over the core types, for every crate to reuse.
//!
//! Writing these once matters more than it looks. Generating a `TurnInput` that
//! actually passes `validate_shape`, or an understanding whose word ranges are
//! words of the text, is fiddly enough that every crate
//! doing it again would get it subtly wrong in a different way — and a
//! property test that generates invalid inputs proves nothing.
//!
//! Identifiers come from bytes, never from a clock or a random source, so a
//! failing case shrinks and reproduces from its seed.
//!
//! ```
//! use proptest::strategy::{Strategy, ValueTree};
//! use proptest::test_runner::TestRunner;
//! use turnframe_test::strategies;
//!
//! let text = "Set the name to Lisbon offsite and rebook it tomorrow";
//! let mut runner = TestRunner::deterministic();
//! let understanding = strategies::grounded_understanding(text)
//!     .new_tree(&mut runner)
//!     .unwrap()
//!     .current();
//! for act in &understanding.acts {
//!     assert!(act.words.end <= text.len());
//! }
//! ```

pub mod command;
pub mod ids;
pub mod interaction;
pub mod turn;
pub mod understanding;

pub use command::{
    atomicity_scope, claim_mode, command_origin, command_policy, confirmation_policy,
    consequential_policy, risk_class, trusted_origin,
};
pub use ids::{
    account_id, attachment_id, case_id, case_key, case_ref, case_revision, command_id,
    conversation_id, digest, event_id, instant, interaction_id, label, locale, operation_key,
    option_id, origin_token, receipt_id, target_token, turn_id, user_id, uuid, workflow_key,
};
pub use interaction::{
    answerable_payload, interaction, interaction_kind, interaction_spec, text_resolution_policy,
};
pub use turn::{
    actor_context, attachment_ref, interaction_response, origin_ref, turn_input, user_text,
};
pub use understanding::{grounded_understanding, word_range};
