//! Wire conversion, in both directions.
//!
//! Nothing in here is public. Ollama's `/api/chat` envelope is an
//! implementation detail of this crate; the runtime speaks
//! [`turnframe_provider`]'s normalized vocabulary and never sees a vendor type
//! (spec §0 rule 8). Keeping the wire private is what makes that true rather
//! than merely intended.
//!
//! | Module | Direction |
//! |---|---|
//! | [`request`] | [`ModelRequest`](turnframe_provider::request::ModelRequest) → `/api/chat` body |
//! | [`response`] | `/api/chat` body → [`ModelResponse`](turnframe_provider::response::ModelResponse) |
//! | [`stream`] | newline-delimited JSON → [`StreamEvent`](turnframe_provider::stream::StreamEvent) |

pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod stream;
