//! Credentials and redaction (spec §25.2).
//!
//! Two rules shape this module.
//!
//! **A credential is never printable.** [`ApiKey`] wraps a
//! [`secrecy::SecretString`]. Its `Debug` renders `ApiKey(REDACTED)` and it has
//! no `Display` and no `Serialize` at all, so `format!("{key}")` and
//! `serde_json::to_string(&key)` do not compile. The only way out is
//! [`ApiKey::expose`], whose name shows up in review.
//!
//! **A credential that escaped anyway is masked on the way to a log.**
//! [`Redactor`] is the hook adapters run text through before it reaches a log
//! line, a trace attribute or an error report. [`DefaultRedactor`] masks
//! bearer tokens, values that follow a credential-shaped key name, the token
//! shapes the major providers issue, and any literal secret registered with
//! [`DefaultRedactor::with_secret`].
//!
//! Redaction is defence in depth, not the primary control: the primary control
//! is that [`ProviderError`](crate::error::ProviderError) has no field a body
//! can be put into, and that request and response logging is bounded by
//! [`LoggingPolicy`](crate::purpose::LoggingPolicy).
//!
//! ```
//! use turnframe_provider::secret::{ApiKey, DefaultRedactor, Redactor};
//!
//! let key = ApiKey::new("sk-live-000111222333444555666777");
//! assert_eq!(format!("{key:?}"), "ApiKey(REDACTED)");
//!
//! let redactor = DefaultRedactor::new().with_secret(&key);
//! let line = redactor.redact("authorization: Bearer sk-live-000111222333444555666777");
//! assert_eq!(line, "authorization: Bearer [REDACTED]");
//! ```

use std::fmt;

use secrecy::{ExposeSecret, SecretString};

/// The placeholder every redaction leaves behind.
pub const MASK: &str = "[REDACTED]";

/// Shortest run of token characters [`DefaultRedactor`] will mask on its own.
///
/// Below this, a match is far more likely to be a word than a credential.
pub const MIN_TOKEN_LEN: usize = 12;

/// Token prefixes that identify a provider credential by shape.
///
/// A token starting with one of these and at least [`MIN_TOKEN_LEN`] characters
/// long is masked wherever it appears.
pub const CREDENTIAL_PREFIXES: &[&str] = &[
    "sk-",         // OpenAI, Anthropic (sk-ant-...), many compatible gateways
    "sk_",         // Stripe-style and several gateways
    "rk-",         // OpenAI restricted keys
    "AIza",        // Google API keys
    "ya29.",       // Google OAuth access tokens
    "xai-",        // xAI
    "gsk_",        // Groq
    "r8_",         // Replicate
    "hf_",         // Hugging Face
    "ghp_",        // GitHub personal access tokens
    "github_pat_", // GitHub fine-grained tokens
    "AKIA",        // AWS access key id
    "ASIA",        // AWS temporary access key id
    "glpat-",      // GitLab
    "nvapi-",      // NVIDIA NIM
    "co-",         // Cohere trial keys
];

/// Key names whose value is a credential.
///
/// Matching is case-insensitive and ignores `-` and `_`, so `X-Api-Key`,
/// `x_api_key` and `apikey` are all recognized.
pub const CREDENTIAL_KEY_NAMES: &[&str] = &[
    "authorization",
    "apikey",
    "xapikey",
    "apisecret",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "bearer",
    "token",
    "secret",
    "password",
    "credential",
    "credentials",
    "sessiontoken",
];

/// An API credential.
///
/// Construct it as early as possible — at configuration parsing time — so the
/// plain `String` never lives long enough to be logged.
pub struct ApiKey(SecretString);

impl Clone for ApiKey {
    /// Copies the credential into a fresh [`SecretString`]; the old box is
    /// still zeroized when it drops.
    fn clone(&self) -> Self {
        Self::new(self.expose())
    }
}

impl ApiKey {
    /// Wraps a credential.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Borrows the credential. The only way to read it; grep for it in review.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }

    /// Returns `true` when the credential is empty, without exposing it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }

    /// Length in bytes, without exposing the value.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.expose_secret().len()
    }

    /// A short, non-reversible hint that identifies *which* key is configured
    /// without revealing it: the digest of the value, truncated to 8 hex
    /// characters.
    ///
    /// Safe to log and to compare across processes; useless for authenticating.
    ///
    /// ```
    /// use turnframe_provider::secret::ApiKey;
    ///
    /// let key = ApiKey::new("sk-live-42");
    /// assert_eq!(key.fingerprint().len(), 8);
    /// assert_eq!(key.fingerprint(), ApiKey::new("sk-live-42").fingerprint());
    /// assert_ne!(key.fingerprint(), ApiKey::new("sk-live-43").fingerprint());
    /// ```
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = turnframe_core::hash::Digest::of_bytes(self.0.expose_secret().as_bytes());
        digest.as_str().chars().take(8).collect()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(REDACTED)")
    }
}

impl From<String> for ApiKey {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for ApiKey {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl<'de> serde::Deserialize<'de> for ApiKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// Masks credentials in text on its way to a log, a trace or a report.
pub trait Redactor: Send + Sync + fmt::Debug {
    /// Returns `text` with every recognized credential replaced by [`MASK`].
    fn redact(&self, text: &str) -> String;

    /// Returns `true` when redacting `text` would change it. The default
    /// implementation redacts and compares.
    fn would_redact(&self, text: &str) -> bool {
        self.redact(text) != text
    }
}

/// The redactor adapters get when they do not configure one.
///
/// It applies three passes, in order:
///
/// 1. **Registered literals.** Every credential passed to
///    [`with_secret`](Self::with_secret) or [`with_literal`](Self::with_literal)
///    is replaced wherever it occurs. This is the only exact pass; the other two
///    are heuristics.
/// 2. **Key-directed values.** A token preceded by one of
///    [`CREDENTIAL_KEY_NAMES`] and a separator (`:`, `=`, whitespace, quotes) is
///    masked whatever it looks like. This is what catches
///    `Authorization: Bearer …` and `{"api_key": "…"}`.
/// 3. **Token shapes.** A token starting with one of [`CREDENTIAL_PREFIXES`]
///    and at least [`MIN_TOKEN_LEN`] characters long is masked wherever it
///    appears, even in prose.
///
/// It deliberately does not mask arbitrary long tokens: a base64 attachment id
/// or a UUID is not a credential, and a redactor that eats every identifier
/// makes logs useless and gets switched off.
#[derive(Debug, Clone, Default)]
pub struct DefaultRedactor {
    literals: Vec<String>,
}

impl DefaultRedactor {
    /// A redactor with no registered literals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a credential so its exact value is masked wherever it occurs.
    #[must_use]
    pub fn with_secret(mut self, key: &ApiKey) -> Self {
        let value = key.expose();
        if !value.is_empty() {
            self.literals.push(value.to_owned());
            self.sort_literals();
        }
        self
    }

    /// Registers a literal string to mask (a session token, a signed URL).
    #[must_use]
    pub fn with_literal(mut self, literal: impl Into<String>) -> Self {
        let literal = literal.into();
        if !literal.is_empty() {
            self.literals.push(literal);
            self.sort_literals();
        }
        self
    }

    /// Longest first, so a key that contains another key still masks fully.
    fn sort_literals(&mut self) {
        self.literals
            .sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        self.literals.dedup();
    }

    /// Number of registered literals, for diagnostics.
    #[must_use]
    pub fn literal_count(&self) -> usize {
        self.literals.len()
    }
}

/// Characters that may appear inside a credential token.
///
/// `=` is deliberately excluded even though base64 pads with it: treating it as
/// a separator is what makes `api_key=value` parse as a key and a value, and a
/// padded secret still loses its body to the mask.
fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '+' | '/')
}

/// Normalizes a key name for comparison against [`CREDENTIAL_KEY_NAMES`].
fn normalize_key_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// Returns `true` when `token` has a credential-issuer shape.
fn has_credential_shape(token: &str) -> bool {
    token.len() >= MIN_TOKEN_LEN
        && CREDENTIAL_PREFIXES
            .iter()
            .any(|prefix| token.starts_with(prefix))
}

/// Splits `text` into `(token, separator)` runs, preserving every byte.
fn tokenize(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        let mut token = String::new();
        while chars.peek().is_some_and(|ch| is_token_char(*ch)) {
            if let Some(ch) = chars.next() {
                token.push(ch);
            }
        }
        let mut separator = String::new();
        while chars.peek().is_some_and(|ch| !is_token_char(*ch)) {
            if let Some(ch) = chars.next() {
                separator.push(ch);
            }
        }
        if token.is_empty() && separator.is_empty() {
            break;
        }
        out.push((token, separator));
    }
    out
}

/// Returns `true` when `separator` only holds characters that can sit between a
/// key name and its value.
fn is_assignment_separator(separator: &str) -> bool {
    !separator.is_empty()
        && separator
            .chars()
            .all(|ch| matches!(ch, ':' | '=' | ' ' | '\t' | '"' | '\'' | ',' | '{' | '>'))
}

impl Redactor for DefaultRedactor {
    fn redact(&self, text: &str) -> String {
        // Pass 1: registered literals, longest first.
        let mut work = text.to_owned();
        for literal in &self.literals {
            if work.contains(literal.as_str()) {
                work = work.replace(literal.as_str(), MASK);
            }
        }

        // Passes 2 and 3 share one walk over the token runs.
        let runs = tokenize(&work);
        let mut out = String::with_capacity(work.len());
        let mut expect_value = false;
        for (token, separator) in runs {
            if token.is_empty() {
                out.push_str(&separator);
                continue;
            }
            let normalized = normalize_key_name(&token);
            let is_key_name = CREDENTIAL_KEY_NAMES.contains(&normalized.as_str());
            let masked = (expect_value && !is_key_name) || has_credential_shape(&token);
            if masked {
                out.push_str(MASK);
                expect_value = false;
            } else {
                out.push_str(&token);
            }
            if is_key_name {
                expect_value = is_assignment_separator(&separator);
            } else if expect_value {
                // The key name was followed by something that is not a value.
                expect_value = false;
            }
            out.push_str(&separator);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLANTED: &str = "sk-live-abcdefghijklmnopqrstuvwxyz0123456789";

    #[test]
    fn api_key_never_renders_its_value() {
        let key = ApiKey::new(PLANTED);
        let debug = format!("{key:?}");
        assert_eq!(debug, "ApiKey(REDACTED)");
        assert!(!debug.contains("sk-live"));
        // Debug of a container holding the key is safe too.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            key: ApiKey,
            endpoint: &'static str,
        }
        let rendered = format!(
            "{:?}",
            Config {
                key,
                endpoint: "https://example.test"
            }
        );
        assert!(!rendered.contains("abcdefg"), "{rendered}");
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn fingerprint_identifies_without_revealing() {
        let key = ApiKey::new(PLANTED);
        let fingerprint = key.fingerprint();
        assert_eq!(fingerprint.len(), 8);
        assert!(!PLANTED.contains(fingerprint.as_str()));
        assert_eq!(key.len(), PLANTED.len());
        assert!(!key.is_empty());
        assert!(ApiKey::new("").is_empty());
    }

    #[test]
    fn api_key_deserializes_from_a_plain_string() {
        let key: ApiKey = serde_json::from_str("\"sk-from-config\"").unwrap();
        assert_eq!(key.expose(), "sk-from-config");
    }

    #[test]
    fn registered_literals_are_masked_anywhere() {
        let redactor = DefaultRedactor::new().with_secret(&ApiKey::new("plain-word-secret"));
        assert_eq!(redactor.literal_count(), 1);
        let masked = redactor.redact("the value plain-word-secret appears mid sentence");
        assert_eq!(masked, "the value [REDACTED] appears mid sentence");
        assert!(redactor.would_redact("plain-word-secret"));
        assert!(!redactor.would_redact("nothing to see"));
    }

    #[test]
    fn bearer_tokens_are_masked_by_key_name() {
        let redactor = DefaultRedactor::new();
        assert_eq!(
            redactor.redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            redactor.redact("authorization: bearer shortish"),
            "authorization: bearer [REDACTED]"
        );
        assert_eq!(
            redactor.redact("{\"x-api-key\": \"opaque-value-1\"}"),
            "{\"x-api-key\": \"[REDACTED]\"}"
        );
        assert_eq!(
            redactor.redact("api_key=opaque&model=gpt-4o"),
            "api_key=[REDACTED]&model=gpt-4o"
        );
    }

    #[test]
    fn provider_key_shapes_are_masked_without_a_key_name() {
        let redactor = DefaultRedactor::new();
        for planted in [
            "sk-ant-api03-0123456789abcdef",
            "sk-proj-0123456789abcdef",
            "AIzaSyA0123456789abcdef",
            "gsk_0123456789abcdef",
            "AKIAIOSFODNN7EXAMPLE",
            "ghp_0123456789abcdefghij",
        ] {
            let line = format!("call failed with {planted} configured");
            let masked = redactor.redact(&line);
            assert!(!masked.contains(planted), "{planted} survived: {masked}");
            assert!(masked.contains(MASK));
        }
    }

    #[test]
    fn ordinary_identifiers_survive() {
        let redactor = DefaultRedactor::new();
        for benign in [
            "turnframe.provider.latency_ms",
            "request_id=0192f0aa-1b2c-7def-8000-0123456789ab",
            "model=gpt-4o-2024-08-06 provider=openai",
            "finish=tool_calls usage.input=1234",
            "the sky is blue",
        ] {
            assert_eq!(redactor.redact(benign), benign, "over-redacted {benign}");
        }
    }

    #[test]
    fn redaction_preserves_everything_that_is_not_a_secret() {
        let redactor = DefaultRedactor::new().with_literal("hunter2");
        let line = "level=warn provider=openai attempt=2 password=hunter2 latency_ms=134";
        assert_eq!(
            redactor.redact(line),
            "level=warn provider=openai attempt=2 password=[REDACTED] latency_ms=134"
        );
    }

    #[test]
    fn a_key_name_without_a_value_does_not_eat_the_next_word() {
        let redactor = DefaultRedactor::new();
        assert_eq!(
            redactor.redact("the token\nwas rejected"),
            "the token\nwas rejected"
        );
    }

    #[test]
    fn longest_literal_wins() {
        let redactor = DefaultRedactor::new()
            .with_literal("abc")
            .with_literal("abcdef");
        assert_eq!(redactor.redact("abcdef"), MASK);
    }
}
