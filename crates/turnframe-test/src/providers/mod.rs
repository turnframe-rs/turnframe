//! Scripted model providers, grounded plan building, and the conformance
//! harness an adapter crate runs (spec §6.1, §20.8, §27.5).
//!
//! # Why a scripted double and not the static one
//!
//! `turnframe-provider` already ships a
//! [`StaticProvider`](turnframe_provider::testing::StaticProvider): queue a few
//! answers, then repeat a default for ever. That is the right shape for testing
//! *the provider layer* — retry, fallback, routing — where "and then it keeps
//! working" is exactly what you mean.
//!
//! It is the wrong shape for testing a **turn**. A turn calls a model a
//! specific number of times, for specific purposes, with a specific catalog,
//! and the interesting bugs are the extra call nobody expected and the call
//! that carried the wrong thing. A double that answers for ever cannot report
//! either of them. [`ScriptedProvider`] therefore:
//!
//! * consumes its steps in order and **fails loudly** on a call the script did
//!   not anticipate, instead of improvising one more answer;
//! * can bind a step to a [`ModelPurpose`](turnframe_provider::purpose::ModelPurpose),
//!   so an interpretation answer cannot silently satisfy a narration call;
//! * records every request, so a test asserts what the runtime actually *sent*
//!   — which catalog, which schema, which messages — and not only what it did
//!   with the answer;
//! * declares whatever capabilities the test gives it, including dishonest
//!   ones, because that is what a test of the no-silent-downgrade rule needs.
//!
//! Loud means three things at once: the call fails with
//! [`UNEXPECTED_CALL_CODE`], the violation is recorded, and
//! [`ScriptedProvider::verify`] returns it at the end of the test. The last one
//! matters most: a runtime that swallowed the error cannot hide the extra call
//! from the test that wrote the script.
//!
//! # Understandings without word counting
//!
//! An [`Understanding`](turnframe_core::understanding::Understanding) points at the
//! user's words by index. [`UnderstandingBuilder`] takes the quotes and computes the
//! ranges, and [`ScriptedUnderstanding`] hands the understandings to a runtime in order,
//! failing loudly on a turn nobody scripted.
//!
//! # Conformance
//!
//! [`conformance`] re-exports the whole of the provider crate's harness and
//! adds the three things every adapter crate would otherwise write by hand: a
//! blocking runner, a judgement about skipped rows, and one table for the rows
//! a deployment genuinely cannot put on the wire — which the harness asks about
//! through two different hooks and a gate has to be told a third time. The
//! [`provider_conformance_suite!`](crate::provider_conformance_suite) macro
//! turns all three into one declaration.
//!
//! ```
//! use turnframe_provider::prelude::*;
//! use turnframe_test::providers::{ScriptedProvider, ScriptedReply};
//!
//! # futures::executor::block_on(async {
//! let provider = ScriptedProvider::builder("scripted", "model-1")
//!     .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Done."))
//!     .build();
//! let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("go"));
//! let response = provider.generate(request).await.unwrap();
//!
//! assert_eq!(response.text(), r#"{"text":"Done."}"#);
//! provider.verify().expect("the script was followed exactly");
//! # });
//! ```

pub mod conformance;
mod script;
mod understanding;

pub use script::{
    RecordedCall, ScriptStep, ScriptViolation, ScriptedProvider, ScriptedProviderBuilder,
    ScriptedReply, UNEXPECTED_CALL_CODE, UNSERIALIZABLE_REPLY_CODE, WRONG_PURPOSE_CODE,
    response_text,
};
pub use understanding::{QuoteNotFound, ScriptedUnderstanding, UnderstandingBuilder};
