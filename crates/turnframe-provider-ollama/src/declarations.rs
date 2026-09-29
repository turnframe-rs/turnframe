//! Per-**model** capability declarations (spec §20.3).
//!
//! This module exists to make one sentence structural rather than advisory:
//! **what an Ollama profile can do is a property of the model, not of Ollama.**
//!
//! The daemon is the same program whatever it is serving. It will accept a JSON
//! Schema in `format` for a 0.5B model exactly as it will for a 70B one, and it
//! will stream either. But a small instruction-tuned model routinely ignores a
//! schema it was told to satisfy, produces prose where a tool call was asked
//! for, and has never seen an image in its life. Two models behind one daemon
//! are therefore two conformance runs and two declarations, and a report that
//! passed for `qwen3:8b` says nothing whatsoever about `smollm2:135m`.
//!
//! So there is exactly one starting point here — [`baseline`] — and it declares
//! only what the *daemon* backs regardless of the model. Everything a model has
//! to earn is raised from there with the `with_*` methods of
//! [`ProviderCapabilities`], and the only thing that licenses raising one is a
//! passing run of the conformance suite against that model.
//!
//! ```
//! use turnframe_provider::capabilities::{StructuredOutputCapability, ToolCallingCapability};
//! use turnframe_provider_ollama::declarations::baseline;
//!
//! // What any model behind the daemon backs: it streams, and `format: "json"`
//! // gets valid JSON out of it.
//! let small = baseline();
//! assert!(small.streaming);
//! assert_eq!(small.structured_output, StructuredOutputCapability::JsonObject);
//! assert!(!small.supports_tools());
//! // And no model behind this endpoint reads a document: there is no channel
//! // for one on `/api/chat`.
//! assert!(!small.documents);
//!
//! // What a model earns after a passing conformance run against it.
//! let measured = baseline()
//!     .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
//!     .with_tool_calling(ToolCallingCapability::Parallel)
//!     .with_vision(true)
//!     .with_max_context_tokens(32_768);
//! assert!(measured.structured_output.enforces_schema());
//! ```

use turnframe_provider::capabilities::{ProviderCapabilities, StructuredOutputCapability};

/// The declaration any model behind an Ollama daemon backs.
///
/// Three fields are decided by the daemon and are true for every model:
///
/// * `streaming` is **true**. `/api/chat` streams by default and every model is
///   served through the same newline-delimited framing.
/// * `structured_output` is [`JsonObject`](StructuredOutputCapability::JsonObject).
///   `format: "json"` constrains decoding to valid JSON syntax for anything the
///   daemon runs. It is deliberately *not*
///   [`NativeJsonSchema`](StructuredOutputCapability::NativeJsonSchema): the
///   daemon will happily take a schema, but whether the model it is driving
///   honours the constraint is exactly the thing a conformance run measures,
///   and this crate refuses to claim it on a model's behalf.
/// * `preserves_call_ids` is **false**, and cannot honestly be anything else.
///   Ollama's chat format has no call id at all — a tool call is a name and an
///   arguments object — so the ids come from this adapter and the builder
///   refuses a profile that claims otherwise.
///
/// Everything else starts off: no tools, no vision, no audio, no prompt caching
/// the daemon will report, no reasoning controls, and no declared context
/// window. Raise each one only for a model that has proven it.
///
/// One of them can never be raised honestly. `documents` stays **false** for
/// every model behind this endpoint: `/api/chat` has a single attachment
/// channel, `images`, and no document one, so a document part is refused rather
/// than sent as a picture of a PDF.
#[must_use]
pub fn baseline() -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::JsonObject)
        .with_streaming(true)
        .with_preserves_call_ids(false)
        .with_temperature(true)
        .with_seed(true)
}
