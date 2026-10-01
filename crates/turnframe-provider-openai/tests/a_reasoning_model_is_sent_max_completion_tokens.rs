//! OpenAI's reasoning models refuse `max_tokens`; they are sent `max_completion_tokens`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::{Message, ModelRequest};
use turnframe_provider::secret::ApiKey;
use turnframe_provider_openai::OpenAiProvider;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn sent_body(model: &str) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::openai()
        .api_key(ApiKey::new("sk-test"))
        .base_url(server.uri())
        .model(model)
        .build()
        .unwrap();
    let request = ModelRequest::new(ModelPurpose::Acknowledge)
        .with_message(Message::user("hello"))
        .with_max_output_tokens(64);
    provider.generate(request).await.unwrap();
    let received = server.received_requests().await.unwrap();
    serde_json::from_slice(&received[0].body).unwrap()
}

#[tokio::test]
async fn a_reasoning_model_gets_max_completion_tokens() {
    for model in ["gpt-5.4-mini", "gpt-6-luna", "gpt-6.1-sol"] {
        let body = sent_body(model).await;
        assert_eq!(body["max_completion_tokens"], 64, "{model}");
        assert!(body.get("max_tokens").is_none(), "{model}: {body}");
    }
}

#[tokio::test]
async fn an_older_model_keeps_max_tokens() {
    let body = sent_body("gpt-4o-mini").await;
    assert_eq!(body["max_tokens"], 64);
    assert!(body.get("max_completion_tokens").is_none(), "{body}");
}
