//! The trait every adapter implements (spec §20.1).
//!
//! One [`ModelProvider`] instance is one **provider-model profile**: one
//! endpoint, one credential, one model, one honest set of declared
//! capabilities. Two models behind the same vendor are two instances, because
//! conformance is per provider-model combination and a capability declaration
//! that covers "the vendor" is a declaration about nothing.
//!
//! An adapter's whole job is translation: normalized request in, vendor request
//! out, vendor response in, normalized response out, vendor error in,
//! [`ProviderError`] out. It holds no workflow policy, never inspects a
//! workflow view, has no opinion about which acts are safe, and never receives
//! a command handler or an external credential (spec §21.3).
//!
//! # The one rule an adapter can break on its own
//!
//! [`capabilities`](ModelProvider::capabilities) must be true. Everything
//! downstream — routing, the refusal to downgrade a critical stage, the choice
//! to trust a parsed plan — rests on it. An optimistic declaration is the most
//! dangerous misconfiguration in the system, and the conformance suite exists
//! largely to catch it: if the profile says
//! [`NativeJsonSchema`](crate::capabilities::StructuredOutputCapability::NativeJsonSchema),
//! the request that goes on the wire must actually carry the schema.

use async_trait::async_trait;

use crate::capabilities::{
    CapabilityMismatch, CapabilityRequirements, ModelProfile, ProviderCapabilities,
};
use crate::error::ProviderError;
use crate::ids::{ModelKey, ModelRef, ProviderKey};
use crate::request::ModelRequest;
use crate::response::ModelResponse;
use crate::stream::ModelStream;

/// One configured provider-model pair the runtime can call.
///
/// ```
/// use async_trait::async_trait;
/// use turnframe_provider::prelude::*;
/// use turnframe_provider::stream::ModelStream;
///
/// struct EchoProvider;
///
/// #[async_trait]
/// impl ModelProvider for EchoProvider {
///     fn provider_key(&self) -> ProviderKey {
///         ProviderKey::from("echo")
///     }
///     fn model_key(&self) -> ModelKey {
///         ModelKey::from("echo-1")
///     }
///     fn capabilities(&self) -> ProviderCapabilities {
///         ProviderCapabilities::minimal()
///     }
///     async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
///         let text = request.messages.last().map(Message::text).unwrap_or_default();
///         Ok(ModelResponse::new(request.request_id, self.provider_key(), self.model_key())
///             .with_text(text))
///     }
/// }
///
/// futures::executor::block_on(async {
///     let provider = EchoProvider;
///     let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao"));
///     let response = provider.generate(request).await.unwrap();
///     assert_eq!(response.text(), "ciao");
///
///     // Streaming is not implemented, and the default says so instead of
///     // chunking a complete answer to look like a stream.
///     let refused = provider.stream(ModelRequest::new(ModelPurpose::Acknowledge)).await;
///     assert!(refused.is_err());
/// });
/// ```
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// The configured provider key, e.g. `"openai"`. Labels every metric,
    /// attempt record and replay entry this provider produces.
    fn provider_key(&self) -> ProviderKey;

    /// The configured model key, e.g. `"gpt-4o-2024-08-06"`.
    fn model_key(&self) -> ModelKey;

    /// What this provider-model pair can actually do (spec §20.3).
    ///
    /// Configured or probed, never inferred from the brand.
    fn capabilities(&self) -> ProviderCapabilities;

    /// The full routing profile: keys, capabilities, and the cost, region and
    /// tags routing filters on.
    ///
    /// The default builds a profile with no cost, no region and no tags, which
    /// is enough for a router that only filters on capabilities. An adapter
    /// that knows its price or its region overrides this.
    fn profile(&self) -> ModelProfile {
        ModelProfile::new(self.provider_key(), self.model_key(), self.capabilities())
    }

    /// The provider-model pair, as routing and health tracking key it.
    fn reference(&self) -> ModelRef {
        ModelRef {
            provider: self.provider_key(),
            model: self.model_key(),
        }
    }

    /// Checks the declared capabilities against `requirements`.
    ///
    /// The single place a caller asks "may this profile serve this stage?".
    ///
    /// # Errors
    ///
    /// Returns the [`CapabilityMismatch`] naming every unmet requirement.
    fn supports(&self, requirements: &CapabilityRequirements) -> Result<(), CapabilityMismatch> {
        requirements.satisfied_by(&self.capabilities())
    }

    /// Runs one model call and returns the whole answer.
    ///
    /// The implementation must honour [`ModelRequest::timeout`], must send
    /// [`ModelRequest::request_id`] as an idempotency hint where the vendor
    /// supports one, and must be cancel-safe: dropping the returned future
    /// aborts the call and leaves nothing running.
    ///
    /// # Errors
    ///
    /// Returns the normalized [`ProviderError`] for the vendor failure.
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError>;

    /// Runs one model call and returns its answer incrementally.
    ///
    /// The default refuses with
    /// [`Unsupported`](crate::error::ProviderErrorKind::Unsupported), which is
    /// the right answer for an adapter whose profile declares
    /// [`streaming: false`](crate::capabilities::ProviderCapabilities::streaming).
    /// Emulating a stream by chunking a complete response would make the
    /// declaration a lie and defeat the point of streaming.
    ///
    /// An implementation must reassemble, through
    /// [`reconstruct`](crate::stream::reconstruct), to the same response
    /// [`generate`](Self::generate) returns for the same exchange.
    ///
    /// # Errors
    ///
    /// Returns the normalized [`ProviderError`] for the vendor failure, before
    /// the stream starts or as an item within it.
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let _ = request;
        Err(ProviderError::unsupported("streaming").with_model(&self.reference()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::StructuredOutputCapability;
    use crate::ids::RequestId;
    use crate::purpose::ModelPurpose;
    use crate::request::Message;

    struct Fixed {
        capabilities: ProviderCapabilities,
    }

    #[async_trait]
    impl ModelProvider for Fixed {
        fn provider_key(&self) -> ProviderKey {
            ProviderKey::from("fixed")
        }

        fn model_key(&self) -> ModelKey {
            ModelKey::from("fixed-1")
        }

        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
            Ok(
                ModelResponse::new(request.request_id, self.provider_key(), self.model_key())
                    .with_text("ok"),
            )
        }
    }

    fn provider() -> Fixed {
        Fixed {
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::NativeJsonSchema),
        }
    }

    #[tokio::test]
    async fn the_default_profile_mirrors_the_declared_capabilities() {
        let provider = provider();
        let profile = provider.profile();
        assert_eq!(profile.provider, provider.provider_key());
        assert_eq!(profile.model, provider.model_key());
        assert_eq!(profile.capabilities, provider.capabilities());
        assert_eq!(provider.reference().to_string(), "fixed/fixed-1");
        assert!(profile.max_cost_per_million().is_none());
    }

    #[tokio::test]
    async fn supports_answers_with_the_mismatch() {
        let provider = provider();
        let ok = ModelPurpose::Extract.requirements();
        assert!(provider.supports(&ok).is_ok());

        let needs_streaming = CapabilityRequirements::none().with_streaming();
        let mismatch = provider.supports(&needs_streaming).unwrap_err();
        assert_eq!(mismatch.missing.len(), 1);
        assert!(!mismatch.structured_output_unmet());
    }

    #[tokio::test]
    async fn streaming_defaults_to_an_honest_refusal() {
        let provider = provider();
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("hi"));
        let error = provider.stream(request).await.unwrap_err();
        assert!(matches!(
            error.kind(),
            crate::error::ProviderErrorKind::Unsupported { .. }
        ));
        assert_eq!(error.retry_class(), crate::error::RetryClass::Fallback);
        assert_eq!(error.provider().map(ProviderKey::as_str), Some("fixed"));
    }

    #[tokio::test]
    async fn generate_echoes_the_request_id() {
        let provider = provider();
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_request_id(RequestId::nil());
        let response = provider.generate(request).await.unwrap();
        assert_eq!(response.request_id, RequestId::nil());
    }

    #[tokio::test]
    async fn the_trait_is_object_safe() {
        let provider: Box<dyn ModelProvider> = Box::new(provider());
        assert_eq!(provider.provider_key().as_str(), "fixed");
        let response = provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap();
        assert_eq!(response.text(), "ok");
    }
}
