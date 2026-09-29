//! The Langfuse source against a local HTTP server.
//!
//! Every test here runs against `wiremock`, which binds an ephemeral port on
//! the loopback interface. **Nothing reaches the internet**, and nothing needs
//! a Langfuse account: the point is to prove which request this adapter makes
//! and what it does with each answer, not that Langfuse is up.
//!
//! The request assertions double as the record of which API surface is being
//! spoken: the path is `/api/public/v2/prompts/{name}`, which is the Langfuse
//! **v4** prompt resource, and authentication is HTTP Basic with the project's
//! public key as the user.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use turnframe_prompt::langfuse::{LangfuseConfigError, LangfusePromptSource};
use turnframe_prompt::{PromptError, PromptName, PromptSelector, PromptSource};
use wiremock::matchers::{basic_auth, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The credential every test plants, so an assertion can look for it in
/// whatever the adapter renders.
const PLANTED_SECRET: &str = "sk-lf-planted-abcdefghijklmnop";

const PUBLIC_KEY: &str = "pk-lf-0000";

fn source(server: &MockServer) -> LangfusePromptSource {
    LangfusePromptSource::builder()
        .base_url(server.uri())
        .public_key(PUBLIC_KEY)
        .secret_key(PLANTED_SECRET)
        .timeout(Duration::from_secs(5))
        .build()
        .expect("a complete configuration")
}

fn name() -> PromptName {
    PromptName::from("interpret.system")
}

/// A Langfuse v4 text-prompt document.
fn text_prompt(version: u64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "cm000000000000",
        "name": "interpret.system",
        "version": version,
        "type": "text",
        "prompt": text,
        "labels": ["production"],
        "tags": [],
        "config": {},
        "commitMessage": "tightened the refusal rule",
        "createdAt": "2026-01-01T00:00:00.000Z",
        "updatedAt": "2026-01-01T00:00:00.000Z"
    })
}

/// Everything the adapter could render, for the credential assertions.
fn rendered(source: &LangfusePromptSource, error: Option<&PromptError>) -> String {
    let mut out = format!("{source:?}");
    if let Some(error) = error {
        out.push_str(&format!(" {error} {error:?}"));
    }
    out
}

#[tokio::test]
async fn a_successful_fetch_reads_the_v4_prompt_resource_over_basic_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/public/v2/prompts/interpret.system"))
        .and(query_param("label", "production"))
        .and(basic_auth(PUBLIC_KEY, PLANTED_SECRET))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(text_prompt(4, "Answer with the plan only.")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let source = source(&server);
    let loaded = source
        .load(&name(), &PromptSelector::label("production"))
        .await
        .expect("the server answers");

    assert_eq!(loaded.text(), "Answer with the plan only.");
    assert_eq!(loaded.version().as_str(), "4");
    assert_eq!(loaded.name(), &name());
    // The obligation the trait states, checked against a real answer.
    assert!(loaded.reference().matches(loaded.text()));
    assert_eq!(source.describe(), "langfuse");
    assert!(!rendered(&source, None).contains(PLANTED_SECRET));
}

#[tokio::test]
async fn a_pinned_version_travels_as_a_query_and_is_what_the_reference_cites() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/public/v2/prompts/interpret.system"))
        .and(query_param("version", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(text_prompt(7, "pinned text")))
        .expect(1)
        .mount(&server)
        .await;

    let source = source(&server);
    let loaded = source
        .load(&name(), &PromptSelector::version("7"))
        .await
        .expect("the server answers");
    assert_eq!(loaded.version().as_str(), "7");
    assert_eq!(loaded.reference().version.as_str(), "7");
}

#[tokio::test]
async fn a_registry_that_ignores_the_pin_is_refused_rather_than_served() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/public/v2/prompts/interpret.system"))
        // The pin said 7; the registry answers with 9.
        .respond_with(ResponseTemplate::new(200).set_body_json(text_prompt(9, "some other text")))
        .mount(&server)
        .await;

    let source = source(&server);
    let error = source
        .load(&name(), &PromptSelector::version("7"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        PromptError::VersionNotFound {
            name: name(),
            version: turnframe_prompt::PromptVersion::from("7"),
        }
    );
    assert!(!rendered(&source, Some(&error)).contains(PLANTED_SECRET));
}

#[tokio::test]
async fn a_missing_prompt_is_not_found_and_says_which_selector_missed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "message": "Prompt not found",
            // A body a registry might plausibly echo. It must not reach the
            // error, and it must not reach any rendering of it.
            "detail": PLANTED_SECRET
        })))
        .mount(&server)
        .await;

    let source = source(&server);

    let latest = source
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(latest, PromptError::NotFound { name: name() });

    let labelled = source
        .load(&name(), &PromptSelector::label("production"))
        .await
        .unwrap_err();
    assert_eq!(
        labelled,
        PromptError::LabelNotFound {
            name: name(),
            label: "production".to_owned(),
        }
    );

    for error in [&latest, &labelled] {
        assert!(!rendered(&source, Some(error)).contains(PLANTED_SECRET));
    }
}

#[tokio::test]
async fn a_rejected_credential_is_an_authentication_failure_that_never_prints_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "message": "Invalid credentials",
            // The worst case: the service reflects the key back at us.
            "receivedKey": PLANTED_SECRET
        })))
        .mount(&server)
        .await;

    let source = source(&server);
    let error = source
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(error, PromptError::Unauthorized);
    assert!(!error.is_transient(), "a wrong key stays wrong");

    let all = rendered(&source, Some(&error));
    assert!(!all.contains(PLANTED_SECRET), "{all}");
    assert!(!all.contains("planted"), "{all}");
    assert!(all.contains("REDACTED"));
}

#[tokio::test]
async fn an_unentitled_credential_is_told_apart_from_a_wrong_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let error = source(&server)
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(error, PromptError::Forbidden { name: name() });
}

#[tokio::test]
async fn a_malformed_payload_is_rejected_whole_and_carries_none_of_it() {
    let server = MockServer::start().await;

    // Not JSON at all.
    let not_json = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("<html>{PLANTED_SECRET}</html>"), "text/html"),
        )
        .mount(&not_json)
        .await;
    let html_source = source(&not_json);
    let error = html_source
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        PromptError::Malformed {
            code: "not_json_prompt",
        }
    );
    assert!(!rendered(&html_source, Some(&error)).contains(PLANTED_SECRET));

    // JSON, but not a prompt document this adapter can read.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "interpret.system",
            "type": "text",
            "prompt": "there is text but no version"
        })))
        .mount(&server)
        .await;
    let json_source = source(&server);
    let error = json_source
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        PromptError::Malformed {
            code: "missing_version",
        }
    );
    assert!(!error.is_transient());
}

#[tokio::test]
async fn a_chat_prompt_is_refused_because_flattening_it_would_change_its_meaning() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "interpret.system",
            "version": 2,
            "type": "chat",
            "prompt": [
                {"role": "system", "content": "You are a critic."},
                {"role": "user", "content": "{{movie}}"}
            ]
        })))
        .mount(&server)
        .await;

    let error = source(&server)
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        PromptError::Unsupported {
            reason: "chat_prompt",
        }
    );
}

#[tokio::test]
async fn a_server_fault_and_a_rate_limit_are_transient_and_a_cache_can_survive_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let error = source(&server)
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(error, PromptError::Transport { code: "status_503" });
    assert!(error.is_transient());

    let limited = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&limited)
        .await;
    let error = source(&limited)
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert_eq!(error, PromptError::RateLimited);
    assert!(error.is_transient());
}

#[tokio::test]
async fn the_cache_composes_with_it_and_holds_a_version_across_an_outage() {
    let server = MockServer::start().await;
    // One good answer, then the registry goes away.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(text_prompt(1, "held text")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let cache = source(&server).into_cached().with_freshness(Duration::ZERO);

    let first = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
    assert_eq!(first.text(), "held text");

    // The window is zero, so this goes to the registry, which is now down.
    let second = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
    assert_eq!(second.text(), "held text", "the held version answers");
    assert_eq!(cache.stats().stale_hits, 1);
}

#[tokio::test]
async fn an_unreachable_server_is_a_transport_failure() {
    // Loopback, on a port nothing listens on: the connection is refused
    // without a packet leaving the machine, which is the closest thing to
    // "the registry is down" that needs no network.
    let source = LangfusePromptSource::builder()
        .base_url("http://127.0.0.1:1")
        .public_key(PUBLIC_KEY)
        .secret_key(PLANTED_SECRET)
        .timeout(Duration::from_millis(500))
        .build()
        .expect("a complete configuration");

    let error = source
        .load(&name(), &PromptSelector::Latest)
        .await
        .unwrap_err();
    assert!(error.is_transient(), "got {error:?}");
    assert!(!rendered(&source, Some(&error)).contains(PLANTED_SECRET));
}

#[test]
fn a_base_url_carrying_a_credential_is_refused_at_configuration_time() {
    let error = LangfusePromptSource::builder()
        .base_url(format!(
            "https://{PUBLIC_KEY}:{PLANTED_SECRET}@example.test"
        ))
        .public_key(PUBLIC_KEY)
        .secret_key(PLANTED_SECRET)
        .build()
        .unwrap_err();
    assert_eq!(error, LangfuseConfigError::CredentialInBaseUrl);
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(PLANTED_SECRET), "{rendered}");
}
