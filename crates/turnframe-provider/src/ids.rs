//! Identity newtypes of the provider layer.
//!
//! [`ProviderKey`] and [`ModelKey`] are **re-exported from `turnframe-core`**:
//! the runtime records provider attempts, signal labels and failures with those
//! exact types, so an adapter's key travels into a replay record without a
//! conversion step. They are configuration labels (`"openai"`,
//! `"gpt-4o-2024-08-06"`), never credentials and never URLs.
//!
//! The rest belongs to this crate because the core contract has no notion of
//! them:
//!
//! * [`ModelRef`] pairs a provider with a model — the unit capabilities, cost
//!   and health are tracked for;
//! * [`RequestId`] is a server-generated UUID v7 that stays **stable across
//!   retries and fallbacks** of the same logical call (spec §20.7), so a
//!   provider can deduplicate and a replay record can correlate attempts;
//! * [`AttemptNumber`] counts attempts within one fallback stage, from one;
//! * [`CallId`] is the provider-assigned identifier of a tool call, echoed back
//!   in the matching tool result.
//!
//! Note that core's `AttemptId` is a different thing: it identifies an attempt
//! at an *external effect* in the outbox (I15), not a model call.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use turnframe_core::ids::{ModelKey, ProviderKey};

/// Provider-assigned identifier of one tool call, echoed in the matching tool
/// result.
///
/// Adapters that cannot preserve the provider's ids synthesize stable ones and
/// declare
/// [`preserves_call_ids = false`](crate::capabilities::ProviderCapabilities::preserves_call_ids).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

impl CallId {
    /// Wraps an existing identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the identifier and returns the owned label.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Returns `true` when the identifier is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for CallId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for CallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<CallId> for String {
    fn from(value: CallId) -> Self {
        value.0
    }
}

impl AsRef<str> for CallId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for CallId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A provider-model pair: the unit capabilities, cost and health are tracked for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelRef {
    /// The provider.
    pub provider: ProviderKey,
    /// The model.
    pub model: ModelKey,
}

impl ModelRef {
    /// Builds a reference.
    ///
    /// ```
    /// use turnframe_provider::ids::ModelRef;
    ///
    /// assert_eq!(ModelRef::new("openai", "gpt-4o").to_string(), "openai/gpt-4o");
    /// ```
    #[must_use]
    pub fn new(provider: impl Into<ProviderKey>, model: impl Into<ModelKey>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// Stable identifier of one logical model call (UUID v7).
///
/// The same id is sent on every retry and every fallback attempt of the call, so
/// the provider can deduplicate and the replay record can correlate attempts
/// (spec §20.7). A new id means a new logical call, not a new try.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub Uuid);

impl RequestId {
    /// Generates a new time-ordered identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The all-zero identifier, for fixtures.
    #[must_use]
    pub const fn nil() -> Self {
        Self(Uuid::nil())
    }

    /// Borrows the UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for RequestId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl std::str::FromStr for RequestId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

/// 1-based attempt counter within one fallback stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptNumber(pub u32);

impl AttemptNumber {
    /// The first attempt.
    pub const FIRST: Self = Self(1);

    /// The attempt that follows this one (saturating).
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// The raw counter.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for AttemptNumber {
    fn default() -> Self {
        Self::FIRST
    }
}

impl fmt::Display for AttemptNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_time_ordered_and_round_trip() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert!(a <= b);
        let json = serde_json::to_string(&a).unwrap();
        let back: RequestId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert_eq!(a.to_string().parse::<RequestId>().unwrap(), a);
        assert_eq!(RequestId::nil().as_uuid(), &Uuid::nil());
    }

    #[test]
    fn keys_are_cores_own_types() {
        // A core key is accepted where a provider key is expected: the
        // re-export must not be a look-alike newtype.
        let core_key = turnframe_core::ids::ProviderKey::from("openai");
        let model_ref = ModelRef::new(core_key.clone(), ModelKey::from("gpt-4o"));
        assert_eq!(model_ref.provider, core_key);
        assert_eq!(model_ref.to_string(), "openai/gpt-4o");
        assert_eq!(serde_json::to_string(&core_key).unwrap(), "\"openai\"");
    }

    #[test]
    fn call_ids_are_transparent_strings() {
        let id = CallId::from("call_123");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"call_123\"");
        assert_eq!(id.as_str(), "call_123");
        assert_eq!(id.to_string(), "call_123");
        assert!(!id.is_empty());
        assert!(CallId::new(String::new()).is_empty());
    }

    #[test]
    fn attempts_start_at_one() {
        assert_eq!(AttemptNumber::default(), AttemptNumber::FIRST);
        assert_eq!(AttemptNumber::FIRST.next().get(), 2);
        assert_eq!(AttemptNumber(u32::MAX).next().get(), u32::MAX);
        assert_eq!(AttemptNumber::FIRST.to_string(), "1");
    }
}
