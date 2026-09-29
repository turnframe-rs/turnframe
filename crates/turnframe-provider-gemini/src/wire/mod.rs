//! Wire conversion, in both directions.
//!
//! Nothing in here is public. Gemini's `GenerateContentRequest` and
//! `GenerateContentResponse` are an implementation detail of this crate; the
//! runtime speaks [`turnframe_provider`]'s normalized vocabulary and never sees
//! a vendor type (spec §0 rule 8). Keeping the wire private is what makes that
//! structural rather than merely intended.
//!
//! The one exception is [`crate::schema`], which is public on purpose: a schema
//! Gemini cannot express is a refusal an application should discover at
//! start-up, not at the first turn.
//!
//! | Module | Direction |
//! |---|---|
//! | [`request`] | [`ModelRequest`](turnframe_provider::request::ModelRequest) → `generateContent` body |
//! | [`response`] | `GenerateContentResponse` → [`ModelResponse`](turnframe_provider::response::ModelResponse) |
//! | [`stream`] | server-sent events → [`StreamEvent`](turnframe_provider::stream::StreamEvent) |

pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod stream;
