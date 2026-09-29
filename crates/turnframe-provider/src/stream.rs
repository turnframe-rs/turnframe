//! Incremental output and its deterministic reassembly (spec §18.5, §20.8).
//!
//! [`ModelStream`] is what [`ModelProvider::stream`](crate::provider::ModelProvider::stream)
//! returns: a pinned, boxed, `Send` stream of [`StreamEvent`]s. It exists so the
//! UI can show prose as it arrives — and only prose. Invariant I16 and spec
//! §18.5 are blunt about the rest: an operational claim is never streamed
//! before commit, and interactions and receipts are emitted as whole typed
//! events, not as text the user watches assemble.
//!
//! # Reassembly is a contract, not a convenience
//!
//! [`reconstruct`] rebuilds the same [`ModelResponse`] the non-streaming call
//! would have produced. The conformance suite asserts the two are equal for the
//! same fixture (spec §20.8), which is what makes it safe for the runtime to
//! stream one purpose and not another without maintaining two parsers.
//!
//! Reassembly is deterministic and total:
//!
//! * text deltas are concatenated in arrival order into **one**
//!   [`ContentPart::Text`], placed before the tool calls;
//! * tool-call argument fragments are concatenated **per call id**, in arrival
//!   order, and the result must parse as JSON;
//! * tool calls appear in the order their [`StreamEvent::ToolCallStart`]
//!   arrived;
//! * a fragment for an unannounced call, a duplicated announcement, a second
//!   finish event or a stream that ends without one is a
//!   [`Malformed`](crate::error::ProviderErrorKind::Malformed) failure — never
//!   a best-effort partial response (I18);
//! * a [`StreamEvent::ResponseId`] sets the provider's response identifier and
//!   a [`StreamEvent::Warning`] adds a warning, so the streamed path reports
//!   the same identifier and the same dropped features the whole path does
//!   instead of the caller having to seed them out of band.
//!
//! ```
//! # use turnframe_provider::prelude::*;
//! # use turnframe_provider::stream::StreamAccumulator;
//! # async fn demo() -> Result<(), ProviderError> {
//! let events = vec![
//!     StreamEvent::text("Ho preparato "),
//!     StreamEvent::text("la modifica."),
//!     StreamEvent::Finish { reason: FinishReason::Stop },
//! ];
//! let stream = ModelStream::from_events(events);
//! let seed = StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o");
//! let response = turnframe_provider::stream::reconstruct(stream, seed).await?;
//! assert_eq!(response.text(), "Ho preparato la modifica.");
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::ids::{CallId, ModelKey, ProviderKey, RequestId};
use crate::request::{ContentPart, ToolCall};
use crate::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

/// One increment of a streamed answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamEvent {
    /// More prose.
    TextDelta {
        /// The fragment. Concatenated verbatim; adapters do not trim.
        text: String,
    },
    /// A tool call begins. Must precede any fragment for the same id.
    ToolCallStart {
        /// Provider-assigned id.
        id: CallId,
        /// Tool name.
        name: String,
    },
    /// More argument bytes for a call already started.
    ToolCallDelta {
        /// Which call.
        id: CallId,
        /// A fragment of the JSON arguments. Rarely valid JSON on its own.
        arguments_fragment: String,
    },
    /// A tool call's arguments are complete.
    ToolCallEnd {
        /// Which call.
        id: CallId,
    },
    /// Reported token usage. Providers send it once, at the end; a later
    /// report replaces an earlier one.
    Usage {
        /// The counts.
        usage: TokenUsage,
    },
    /// The provider's own response identifier, as soon as the wire reveals it.
    ///
    /// Most vendors put it on the first frame — an OpenAI chunk's `id`, an
    /// Anthropic `message_start`, a Gemini `responseId` — so the caller no
    /// longer has to seed it out of band to have it on the rebuilt response.
    /// A later identifier replaces an earlier one.
    ResponseId {
        /// The identifier. For a support ticket; never a body, never a header.
        id: String,
    },
    /// Something the adapter wants the caller to know without failing the call.
    ///
    /// The streamed path reports exactly what the whole path reports. Without
    /// this event an adapter that had to drop a stop sequence or a cache hint
    /// could say so in [`ModelResponse::warnings`](crate::response::ModelResponse::warnings)
    /// when called whole, and had nowhere to say it when called streamed —
    /// which made the same call honest one way and silent the other.
    Warning {
        /// What was given up.
        warning: ResponseWarning,
    },
    /// Generation stopped. Exactly one per stream, and it must arrive.
    Finish {
        /// Why.
        reason: FinishReason,
    },
}

impl StreamEvent {
    /// A text delta.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::TextDelta { text: text.into() }
    }

    /// A tool-call announcement.
    #[must_use]
    pub fn tool_call_start(id: impl Into<CallId>, name: impl Into<String>) -> Self {
        Self::ToolCallStart {
            id: id.into(),
            name: name.into(),
        }
    }

    /// An argument fragment.
    #[must_use]
    pub fn tool_call_delta(id: impl Into<CallId>, fragment: impl Into<String>) -> Self {
        Self::ToolCallDelta {
            id: id.into(),
            arguments_fragment: fragment.into(),
        }
    }

    /// A tool-call terminator.
    #[must_use]
    pub fn tool_call_end(id: impl Into<CallId>) -> Self {
        Self::ToolCallEnd { id: id.into() }
    }

    /// The provider's response identifier.
    #[must_use]
    pub fn response_id(id: impl Into<String>) -> Self {
        Self::ResponseId { id: id.into() }
    }

    /// A warning the adapter wants carried into the rebuilt response.
    #[must_use]
    pub const fn warning(warning: ResponseWarning) -> Self {
        Self::Warning { warning }
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::TextDelta { .. } => "text_delta",
            Self::ToolCallStart { .. } => "tool_call_start",
            Self::ToolCallDelta { .. } => "tool_call_delta",
            Self::ToolCallEnd { .. } => "tool_call_end",
            Self::Usage { .. } => "usage",
            Self::ResponseId { .. } => "response_id",
            Self::Warning { .. } => "warning",
            Self::Finish { .. } => "finish",
        }
    }

    /// Returns `true` for the events a UI may render directly (spec §18.5:
    /// only model-authored prose is streamed to the user).
    #[must_use]
    pub const fn is_user_visible(&self) -> bool {
        matches!(self, Self::TextDelta { .. })
    }
}

/// The item type a [`ModelStream`] yields.
pub type StreamItem = Result<StreamEvent, ProviderError>;

/// A stream of [`StreamEvent`]s from one model call.
///
/// Dropping it cancels the call: an adapter must not leave a task running
/// behind a dropped stream, and the conformance suite checks that dropping is
/// clean.
pub struct ModelStream {
    inner: Pin<Box<dyn Stream<Item = StreamItem> + Send>>,
}

impl ModelStream {
    /// Wraps any `Send` stream of events.
    #[must_use]
    pub fn new(stream: impl Stream<Item = StreamItem> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// A stream that yields a fixed sequence of events. For fixtures and tests.
    #[must_use]
    pub fn from_events(events: Vec<StreamEvent>) -> Self {
        Self::new(futures::stream::iter(events.into_iter().map(Ok)))
    }

    /// A stream that yields the given items, failures included.
    #[must_use]
    pub fn from_items(items: Vec<StreamItem>) -> Self {
        Self::new(futures::stream::iter(items))
    }

    /// A stream that fails immediately.
    #[must_use]
    pub fn failed(error: ProviderError) -> Self {
        Self::from_items(vec![Err(error)])
    }

    /// Collects every item, for tests and fixtures.
    pub async fn collect_items(self) -> Vec<StreamItem> {
        self.inner.collect().await
    }
}

impl fmt::Debug for ModelStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ModelStream(..)")
    }
}

impl Stream for ModelStream {
    type Item = StreamItem;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// A tool call being assembled.
#[derive(Debug, Clone)]
struct PendingCall {
    order: usize,
    name: String,
    fragments: String,
    ended: bool,
}

/// Rebuilds a [`ModelResponse`] from [`StreamEvent`]s.
///
/// Seed it with the identity of the call, feed it every event with
/// [`push`](Self::push), then call [`finish`](Self::finish). [`reconstruct`]
/// does exactly that over a whole [`ModelStream`].
#[derive(Debug, Clone)]
pub struct StreamAccumulator {
    request_id: RequestId,
    provider: ProviderKey,
    model: ModelKey,
    raw_id: Option<String>,
    latency: Duration,
    text: String,
    calls: HashMap<CallId, PendingCall>,
    usage: TokenUsage,
    finish: Option<FinishReason>,
    warnings: Vec<ResponseWarning>,
}

impl StreamAccumulator {
    /// A fresh accumulator for one call.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        provider: impl Into<ProviderKey>,
        model: impl Into<ModelKey>,
    ) -> Self {
        Self {
            request_id,
            provider: provider.into(),
            model: model.into(),
            raw_id: None,
            latency: Duration::ZERO,
            text: String::new(),
            calls: HashMap::new(),
            usage: TokenUsage::none(),
            finish: None,
            warnings: Vec::new(),
        }
    }

    /// Records the provider's response identifier.
    ///
    /// Only needed when the identifier is known before the stream opens — from
    /// a response header, say. An adapter that reads it off the wire emits a
    /// [`StreamEvent::ResponseId`] instead, and the last one wins.
    #[must_use]
    pub fn with_raw_id(mut self, raw_id: impl Into<String>) -> Self {
        self.raw_id = Some(raw_id.into());
        self
    }

    /// Records the measured latency of the whole stream.
    #[must_use]
    pub const fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Adds a warning the adapter already knows about.
    ///
    /// For what the request conversion gave up before the call. A warning the
    /// stream itself reveals travels as a [`StreamEvent::Warning`], and a
    /// warning that arrives both ways is recorded once.
    #[must_use]
    pub fn with_warning(mut self, warning: ResponseWarning) -> Self {
        self.warnings.push(warning);
        self
    }

    /// Absorbs one event.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderErrorKind::Malformed`](crate::error::ProviderErrorKind::Malformed)
    /// for a fragment or terminator that names a call which was never started,
    /// a call started twice, a fragment after the call ended, or a second
    /// finish event.
    pub fn push(&mut self, event: StreamEvent) -> Result<(), ProviderError> {
        match event {
            StreamEvent::TextDelta { text } => self.text.push_str(&text),
            StreamEvent::ToolCallStart { id, name } => {
                let order = self.calls.len();
                if self.calls.contains_key(&id) {
                    return Err(self.malformed("tool_call_started_twice"));
                }
                self.calls.insert(
                    id,
                    PendingCall {
                        order,
                        name,
                        fragments: String::new(),
                        ended: false,
                    },
                );
            }
            StreamEvent::ToolCallDelta {
                id,
                arguments_fragment,
            } => {
                let Some(call) = self.calls.get_mut(&id) else {
                    return Err(self.malformed("tool_call_delta_without_start"));
                };
                if call.ended {
                    return Err(self.malformed("tool_call_delta_after_end"));
                }
                call.fragments.push_str(&arguments_fragment);
            }
            StreamEvent::ToolCallEnd { id } => {
                let Some(call) = self.calls.get_mut(&id) else {
                    return Err(self.malformed("tool_call_end_without_start"));
                };
                call.ended = true;
            }
            StreamEvent::Usage { usage } => self.usage = usage,
            StreamEvent::ResponseId { id } => self.raw_id = Some(id),
            StreamEvent::Warning { warning } => {
                if !self.warnings.contains(&warning) {
                    self.warnings.push(warning);
                }
            }
            StreamEvent::Finish { reason } => {
                if self.finish.is_some() {
                    return Err(self.malformed("duplicate_finish"));
                }
                self.finish = Some(reason);
            }
        }
        Ok(())
    }

    /// Builds the response.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderErrorKind::Malformed`](crate::error::ProviderErrorKind::Malformed)
    /// when no finish event arrived — a truncated stream is not a short answer
    /// — or when a call's concatenated fragments are not valid JSON.
    pub fn finish(self) -> Result<ModelResponse, ProviderError> {
        let Some(finish) = self.finish else {
            return Err(self.malformed("stream_ended_without_finish"));
        };

        let mut content = Vec::with_capacity(self.calls.len() + 1);
        if !self.text.is_empty() {
            content.push(ContentPart::text(self.text.clone()));
        }

        let mut ordered: Vec<(CallId, PendingCall)> = self.calls.clone().into_iter().collect();
        ordered.sort_by_key(|(_, call)| call.order);
        for (id, call) in ordered {
            let raw = call.fragments.trim();
            let arguments: serde_json::Value = if raw.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(raw)
                    .map_err(|_| self.malformed("tool_call_arguments_not_json"))?
            };
            content.push(ContentPart::ToolCall(ToolCall::new(
                id, call.name, arguments,
            )));
        }

        let mut warnings = self.warnings.clone();
        warnings.push(ResponseWarning::Reconstructed);
        if self.usage.is_unreported() {
            warnings.push(ResponseWarning::UsageUnreported);
        }

        Ok(ModelResponse {
            request_id: self.request_id,
            provider: self.provider,
            model: self.model,
            content,
            finish,
            usage: self.usage,
            raw_id: self.raw_id,
            latency: self.latency,
            warnings,
        })
    }

    /// Builds a malformed failure already labelled with the call's identity.
    fn malformed(&self, code: &str) -> ProviderError {
        ProviderError::malformed(code).with_model(&crate::ids::ModelRef {
            provider: self.provider.clone(),
            model: self.model.clone(),
        })
    }
}

/// Drains `stream` into the response it describes.
///
/// The `seed` carries the identity and timing the events do not: request id,
/// provider and model keys, and the measured latency. The provider's response
/// identifier and any warning may come either from the seed or from the stream
/// itself, through [`StreamEvent::ResponseId`] and [`StreamEvent::Warning`].
///
/// # Errors
///
/// Propagates the first [`ProviderError`] the stream yields, and returns the
/// reassembly failures documented on [`StreamAccumulator::push`] and
/// [`StreamAccumulator::finish`].
pub async fn reconstruct(
    mut stream: ModelStream,
    mut seed: StreamAccumulator,
) -> Result<ModelResponse, ProviderError> {
    while let Some(item) = stream.next().await {
        seed.push(item?)?;
    }
    seed.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed() -> StreamAccumulator {
        StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o")
    }

    async fn rebuild(events: Vec<StreamEvent>) -> Result<ModelResponse, ProviderError> {
        reconstruct(ModelStream::from_events(events), seed()).await
    }

    #[tokio::test]
    async fn text_deltas_concatenate_in_order() {
        let response = rebuild(vec![
            StreamEvent::text("Ho "),
            StreamEvent::text("preparato "),
            StreamEvent::text("la modifica."),
            StreamEvent::Usage {
                usage: TokenUsage::new(10, 4),
            },
            StreamEvent::Finish {
                reason: FinishReason::Stop,
            },
        ])
        .await
        .unwrap();
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.content.len(), 1, "one text part, not three");
        assert_eq!(response.usage, TokenUsage::new(10, 4));
        assert_eq!(response.finish, FinishReason::Stop);
        assert!(response.warnings.contains(&ResponseWarning::Reconstructed));
    }

    #[tokio::test]
    async fn tool_call_fragments_concatenate_per_id() {
        let response = rebuild(vec![
            StreamEvent::tool_call_start("call_a", "plan"),
            StreamEvent::tool_call_start("call_b", "plan"),
            StreamEvent::tool_call_delta("call_a", "{\"n\":"),
            StreamEvent::tool_call_delta("call_b", "{\"n\":"),
            StreamEvent::tool_call_delta("call_a", "1}"),
            StreamEvent::tool_call_delta("call_b", "2}"),
            StreamEvent::tool_call_end("call_a"),
            StreamEvent::tool_call_end("call_b"),
            StreamEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ])
        .await
        .unwrap();
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "call_a");
        assert_eq!(calls[0].arguments, json!({"n": 1}));
        assert_eq!(calls[1].id.as_str(), "call_b");
        assert_eq!(calls[1].arguments, json!({"n": 2}));
    }

    #[tokio::test]
    async fn text_comes_before_tool_calls() {
        let response = rebuild(vec![
            StreamEvent::tool_call_start("c", "plan"),
            StreamEvent::text("preambolo"),
            StreamEvent::tool_call_delta("c", "{}"),
            StreamEvent::tool_call_end("c"),
            StreamEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ])
        .await
        .unwrap();
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.content[0].kind(), "text");
        assert_eq!(response.content[1].kind(), "tool_call");
    }

    #[tokio::test]
    async fn a_call_with_no_fragments_gets_an_empty_object() {
        let response = rebuild(vec![
            StreamEvent::tool_call_start("c", "ping"),
            StreamEvent::tool_call_end("c"),
            StreamEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ])
        .await
        .unwrap();
        assert_eq!(response.tool_calls()[0].arguments, json!({}));
    }

    #[tokio::test]
    async fn reassembly_failures_are_malformed_and_never_partial() {
        let cases = [
            (
                vec![
                    StreamEvent::tool_call_delta("ghost", "{}"),
                    StreamEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ],
                "tool_call_delta_without_start",
            ),
            (
                vec![
                    StreamEvent::tool_call_end("ghost"),
                    StreamEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ],
                "tool_call_end_without_start",
            ),
            (
                vec![
                    StreamEvent::tool_call_start("c", "plan"),
                    StreamEvent::tool_call_start("c", "plan"),
                    StreamEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ],
                "tool_call_started_twice",
            ),
            (
                vec![
                    StreamEvent::tool_call_start("c", "plan"),
                    StreamEvent::tool_call_end("c"),
                    StreamEvent::tool_call_delta("c", "{}"),
                    StreamEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ],
                "tool_call_delta_after_end",
            ),
            (
                vec![
                    StreamEvent::Finish {
                        reason: FinishReason::Stop,
                    },
                    StreamEvent::Finish {
                        reason: FinishReason::Stop,
                    },
                ],
                "duplicate_finish",
            ),
            (
                vec![StreamEvent::text("ciao")],
                "stream_ended_without_finish",
            ),
            (
                vec![
                    StreamEvent::tool_call_start("c", "plan"),
                    StreamEvent::tool_call_delta("c", "{\"n\":"),
                    StreamEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ],
                "tool_call_arguments_not_json",
            ),
        ];
        for (events, expected) in cases {
            let error = rebuild(events).await.unwrap_err();
            assert_eq!(
                error.code().map(|code| code.as_str().to_owned()),
                Some(expected.to_owned()),
                "{error}"
            );
            assert!(matches!(
                error.kind(),
                crate::error::ProviderErrorKind::Malformed
            ));
            assert_eq!(error.provider().map(ProviderKey::as_str), Some("openai"));
        }
    }

    #[tokio::test]
    async fn an_error_item_stops_reassembly_immediately() {
        let stream = ModelStream::from_items(vec![
            Ok(StreamEvent::text("half")),
            Err(ProviderError::transport("connection_reset")),
            Ok(StreamEvent::Finish {
                reason: FinishReason::Stop,
            }),
        ]);
        let error = reconstruct(stream, seed()).await.unwrap_err();
        assert!(matches!(
            error.kind(),
            crate::error::ProviderErrorKind::Transport
        ));
    }

    #[tokio::test]
    async fn a_failed_stream_surfaces_its_error() {
        let stream = ModelStream::failed(ProviderError::rate_limited(None));
        let error = reconstruct(stream, seed()).await.unwrap_err();
        assert!(matches!(
            error.kind(),
            crate::error::ProviderErrorKind::RateLimited { .. }
        ));
        assert_eq!(
            format!("{:?}", ModelStream::from_events(vec![])),
            "ModelStream(..)"
        );
    }

    #[tokio::test]
    async fn the_seed_supplies_identity_and_timing() {
        let seed = StreamAccumulator::new(RequestId::nil(), "anthropic", "claude")
            .with_raw_id("msg_01")
            .with_latency(Duration::from_millis(250))
            .with_warning(ResponseWarning::SynthesizedCallIds);
        let response = reconstruct(
            ModelStream::from_events(vec![
                StreamEvent::text("x"),
                StreamEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]),
            seed,
        )
        .await
        .unwrap();
        assert_eq!(response.provider.as_str(), "anthropic");
        assert_eq!(response.raw_id.as_deref(), Some("msg_01"));
        assert_eq!(response.latency, Duration::from_millis(250));
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
    }

    #[tokio::test]
    async fn the_stream_can_carry_the_identifier_and_the_warning_itself() {
        // The gap this closes: an adapter that had to drop a feature said so on
        // the whole path and had nowhere to say it on the streamed one, and the
        // response identifier had to be seeded before the first frame arrived.
        let response = rebuild(vec![
            StreamEvent::response_id("chatcmpl-1"),
            StreamEvent::warning(ResponseWarning::FeatureDropped {
                feature: "stop_sequences".to_owned(),
            }),
            StreamEvent::text("ok"),
            StreamEvent::response_id("chatcmpl-final"),
            StreamEvent::Finish {
                reason: FinishReason::Stop,
            },
        ])
        .await
        .unwrap();
        assert_eq!(
            response.raw_id.as_deref(),
            Some("chatcmpl-final"),
            "a later identifier replaces an earlier one"
        );
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "stop_sequences".to_owned(),
                })
        );
        assert!(response.warnings.contains(&ResponseWarning::Reconstructed));
    }

    #[tokio::test]
    async fn a_warning_the_seed_and_the_stream_both_carry_is_recorded_once() {
        let dropped = ResponseWarning::FeatureDropped {
            feature: "cache_hint".to_owned(),
        };
        let seed = StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o")
            .with_warning(dropped.clone());
        let response = reconstruct(
            ModelStream::from_events(vec![
                StreamEvent::warning(dropped.clone()),
                StreamEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]),
            seed,
        )
        .await
        .unwrap();
        assert_eq!(
            response
                .warnings
                .iter()
                .filter(|warning| **warning == dropped)
                .count(),
            1
        );
    }

    #[test]
    fn events_round_trip_and_only_text_is_user_visible() {
        let events = [
            StreamEvent::text("a"),
            StreamEvent::tool_call_start("c", "plan"),
            StreamEvent::tool_call_delta("c", "{}"),
            StreamEvent::tool_call_end("c"),
            StreamEvent::Usage {
                usage: TokenUsage::new(1, 1),
            },
            StreamEvent::response_id("resp_1"),
            StreamEvent::warning(ResponseWarning::UsageUnreported),
            StreamEvent::Finish {
                reason: FinishReason::Stop,
            },
        ];
        let mut kinds: Vec<&str> = events.iter().map(StreamEvent::kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), 8);
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let back: StreamEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, event);
        }
        assert!(events[0].is_user_visible());
        assert!(!events[1].is_user_visible());
    }
}
