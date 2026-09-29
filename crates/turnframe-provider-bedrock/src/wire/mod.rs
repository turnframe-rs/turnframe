//! Conversion between the normalized vocabulary and the Converse API.
//!
//! Deliberately private: the runtime never sees a vendor type (spec §0 rule 8),
//! and keeping the SDK's types out of this crate's public API is what makes
//! that structural rather than aspirational.

pub(crate) mod document;
pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod stream;
