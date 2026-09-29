//! Where prompt text comes from, and which prompt text produced a turn.
//!
//! A replayed turn can already say which workflow versions were in force, which
//! schema the plan was validated against and which provider attempt answered.
//! What it could not say is which instructions the model was given, because the
//! prompt reached the provider layer as an opaque string.
//!
//! This module closes that gap with two things and nothing else:
//!
//! * a [`PromptRef`] — a name, a version and the digest of the exact text —
//!   which [`ReplayRecord`](crate::replay::ReplayRecord) and
//!   [`ProviderAttemptRecord`](crate::replay::ProviderAttemptRecord) carry, so
//!   the record can be falsified rather than merely believed;
//! * a [`PromptSource`], the trait an application implements or configures when
//!   it wants the runtime to fetch prompt text instead of using the
//!   instructions compiled into the library.
//!
//! # Why the contract is here and the implementations are not
//!
//! The trait lives in the contract crate so the runtime can accept an
//! `Arc<dyn PromptSource>` without depending on any particular way of getting
//! prompts. Concrete sources — prompts compiled in from the adopter's
//! repository, a bounded cache, an optional registry adapter — live in
//! `turnframe-prompt`, which nothing in this crate and nothing in the runtime
//! depends on.
//!
//! That split is the point rather than a tidiness preference. **Prompt
//! management must never become a framework dependency.** An application that
//! configures no source pulls no source code, reaches no network, and runs on
//! the instructions in its own binary; the dependency graph says so, not the
//! documentation.
//!
//! ```
//! use turnframe_core::prompt::PromptRef;
//!
//! let reference = PromptRef::of_text("interpret.system", "v3", "Answer with the plan only.");
//! assert!(reference.matches("Answer with the plan only."));
//! assert!(!reference.matches("Answer with anything you like."));
//! ```

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hash::Digest;
use crate::ids::string_id;

string_id! {
    /// Name of a prompt, as the application asks for it.
    ///
    /// An opaque label owned by the adopter — `"interpret.system"`,
    /// `"narrate.transition"` — never free user text, so it is safe in a log
    /// line and in a metric label.
    PromptName
}

string_id! {
    /// Version of a prompt, as its source names it.
    ///
    /// Deliberately a string and not a number, because the two sources that
    /// matter spell it differently: a registry hands out ordinal versions
    /// (`"3"`), and a repository-served prompt derives its version from the
    /// content of the file, so nobody has to maintain a counter. Comparing two
    /// versions for equality is meaningful; ordering them is not, which is why
    /// this type is not asked to be an integer.
    PromptVersion
}

/// Which prompt text produced a turn.
///
/// Small and cheap to clone: three owned strings, no borrowed lifetimes and no
/// content. It is safe to log and to store, because a name, a version and a
/// digest are identifiers and never the prompt body.
///
/// # The obligation
///
/// A reference is only worth what its hash is worth. Whatever produces one owes
/// the reader this: **[`hash`](Self::hash) is the digest of the text that was
/// actually used**, computed with [`Digest::of_bytes`] over the UTF-8 bytes.
/// Build one with [`PromptRef::of_text`] and the obligation is met by
/// construction; build one field by field and [`PromptRef::matches`] is how a
/// test proves it.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct PromptRef {
    /// The name the application asked for.
    pub name: PromptName,
    /// The version its source returned.
    pub version: PromptVersion,
    /// BLAKE3 digest of the UTF-8 bytes of the prompt text.
    pub hash: Digest,
}

impl PromptRef {
    /// A reference to `text`, with its hash computed here so it cannot be wrong.
    #[must_use]
    pub fn of_text(
        name: impl Into<PromptName>,
        version: impl Into<PromptVersion>,
        text: &str,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            hash: Digest::of_bytes(text.as_bytes()),
        }
    }

    /// A reference assembled from parts, for a source that already holds a
    /// digest it trusts.
    ///
    /// Prefer [`PromptRef::of_text`]; this constructor exists for the
    /// deserialization-shaped cases, and it is the caller who then owes the
    /// obligation described on the type.
    #[must_use]
    pub fn new(
        name: impl Into<PromptName>,
        version: impl Into<PromptVersion>,
        hash: Digest,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            hash,
        }
    }

    /// Returns `true` when `text` is the text this reference stands for.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        self.hash == Digest::of_bytes(text.as_bytes())
    }

    /// A short, stable label for a metric or a span attribute: `name@version`.
    ///
    /// Without the digest, which is 64 characters and belongs in the record
    /// rather than in a dashboard legend.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

impl fmt::Display for PromptRef {
    /// `name@version#12345678` — the label plus the first eight hex characters
    /// of the digest, which is enough to tell two texts apart in a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short: String = self.hash.as_str().chars().take(8).collect();
        write!(f, "{}@{}#{short}", self.name, self.version)
    }
}

/// Which version of a prompt is wanted.
///
/// The three arms are three different postures towards a moving registry, and
/// they are worth choosing between deliberately:
///
/// * [`Latest`](Self::Latest) takes whatever the source considers current. Fine
///   for a compiled-in source, where "current" means "what is in this binary".
/// * [`Label`](Self::Label) takes the version somebody has marked — `production`
///   being the usual one. Convenient, and it means an edit in the registry
///   changes a running system without a deploy, which is the trade being made.
/// * [`Version`](Self::Version) pins. A registry edit then cannot change what
///   the system does; only a deploy can, and the pinned version is what the
///   replay record cites. This is the arm a deployment that cares about
///   reproducibility uses.
///
/// Deliberately **not** `#[non_exhaustive]`, unlike most growable enums in this
/// crate. A [`PromptSource`] has to decide what every selector means for it,
/// and a wildcard arm decides by accident: it would serve *something* for a
/// selector the source has never heard of, which is the one behaviour a pin
/// exists to rule out. Adding an arm here should break every source and make
/// each of them say what it does.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum PromptSelector {
    /// Whatever the source considers current.
    #[default]
    Latest,
    /// The version carrying this deployment label.
    Label(String),
    /// This exact version, and no other.
    Version(PromptVersion),
}

impl PromptSelector {
    /// A label selector.
    #[must_use]
    pub fn label(label: impl Into<String>) -> Self {
        Self::Label(label.into())
    }

    /// A pinned-version selector.
    #[must_use]
    pub fn version(version: impl Into<PromptVersion>) -> Self {
        Self::Version(version.into())
    }

    /// A short stable rendering, used as part of a cache key and safe to log.
    #[must_use]
    pub fn as_key(&self) -> String {
        match self {
            Self::Latest => "latest".to_owned(),
            Self::Label(label) => format!("label:{label}"),
            Self::Version(version) => format!("version:{version}"),
        }
    }

    /// Returns `true` when this selector names one immutable version, so what
    /// comes back cannot change under a running system.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        matches!(self, Self::Version(_))
    }
}

impl fmt::Display for PromptSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_key())
    }
}

/// A prompt, and the reference that names it.
///
/// The two fields are private and the only constructors compute or verify the
/// hash, so a value of this type carries the [`PromptSource`] promise by
/// construction: `reference().hash` *is* the digest of `text()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPrompt {
    reference: PromptRef,
    text: String,
}

impl LoadedPrompt {
    /// Builds a prompt and derives its reference from the text.
    ///
    /// This is the constructor a source should reach for: there is no way to
    /// get the hash wrong.
    #[must_use]
    pub fn new(
        name: impl Into<PromptName>,
        version: impl Into<PromptVersion>,
        text: impl Into<String>,
    ) -> Self {
        let text = text.into();
        let reference = PromptRef::of_text(name, version, &text);
        Self { reference, text }
    }

    /// Builds a prompt from a reference somebody else produced, checking that
    /// the reference is telling the truth.
    ///
    /// # Errors
    ///
    /// [`PromptError::InconsistentReference`] when `reference.hash` is not the
    /// digest of `text`.
    pub fn from_parts(reference: PromptRef, text: impl Into<String>) -> Result<Self, PromptError> {
        let text = text.into();
        if !reference.matches(&text) {
            return Err(PromptError::InconsistentReference {
                name: reference.name,
            });
        }
        Ok(Self { reference, text })
    }

    /// The prompt text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The reference that names this text.
    #[must_use]
    pub fn reference(&self) -> &PromptRef {
        &self.reference
    }

    /// The name it was loaded under.
    #[must_use]
    pub fn name(&self) -> &PromptName {
        &self.reference.name
    }

    /// The version it came back at.
    #[must_use]
    pub fn version(&self) -> &PromptVersion {
        &self.reference.version
    }

    /// Consumes the value and returns the text alone.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    /// Consumes the value and returns both halves.
    #[must_use]
    pub fn into_parts(self) -> (PromptRef, String) {
        (self.reference, self.text)
    }
}

/// A prompt could not be loaded.
///
/// Two rules shape this family, and they are the two the provider layer
/// follows.
///
/// **Nothing here can print a secret.** No variant has a field a credential, a
/// response body or a prompt text could be put into: every field is a name, a
/// label, a version or a short stable code. `Display` is therefore safe in a
/// log line.
///
/// **Every failure says whether it is worth trying again**, through
/// [`is_transient`](Self::is_transient), because that is the difference between
/// a cache that serves a held version for a moment and one that serves it
/// forever.
///
/// The family grows as sources learn to distinguish more, so a downstream match
/// needs a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PromptError {
    /// The source has no prompt by that name.
    #[error("no prompt named {name}")]
    NotFound {
        /// The name that was asked for.
        name: PromptName,
    },
    /// The prompt exists, but not at the version that was pinned.
    ///
    /// A deployment that pins a version wants this to be loud: quietly serving
    /// a different version is exactly the failure pinning exists to prevent.
    #[error("prompt {name} has no version {version}")]
    VersionNotFound {
        /// The name.
        name: PromptName,
        /// The version that was pinned and is not there.
        version: PromptVersion,
    },
    /// The prompt exists, but no version carries that label.
    #[error("prompt {name} has no version labelled {label}")]
    LabelNotFound {
        /// The name.
        name: PromptName,
        /// The label. A deployment label such as `production`, never user text.
        label: String,
    },
    /// The credentials were rejected.
    ///
    /// The credential itself is deliberately absent from every field, so this
    /// value cannot leak it however it is rendered.
    #[error("the prompt source rejected the credentials")]
    Unauthorized,
    /// The credentials are valid but not entitled to this project or prompt.
    #[error("the credentials are not entitled to prompt {name}")]
    Forbidden {
        /// The name that was asked for.
        name: PromptName,
    },
    /// The source asked for the request to be made again later.
    #[error("the prompt source rate-limited the request")]
    RateLimited,
    /// The request never got a complete answer, or the answer was a server
    /// error.
    #[error("the prompt source could not be reached: {code}")]
    Transport {
        /// A short stable code — `"timeout"`, `"connect"`, `"status_503"` —
        /// never a URL, a body or a header.
        code: &'static str,
    },
    /// The answer arrived and was not the shape the source expects.
    ///
    /// Never carries the payload: a malformed answer from a registry is exactly
    /// the kind of thing that turns out to contain a token.
    #[error("the prompt source returned an answer this adapter cannot read: {code}")]
    Malformed {
        /// A short stable code naming what was wrong — `"not_json"`,
        /// `"missing_field"`, `"version_not_a_number"`.
        code: &'static str,
    },
    /// The source can reach the prompt but this adapter will not serve it.
    ///
    /// Used where guessing would be worse than refusing: a chat-shaped prompt
    /// that would have to be flattened into one string to fit this trait, or an
    /// endpoint whose current API shape an adapter cannot verify.
    #[error("the prompt source cannot serve this prompt here: {reason}")]
    Unsupported {
        /// A short stable code naming what is unsupported.
        reason: &'static str,
    },
    /// The source returned a reference whose hash is not the hash of the text
    /// it returned, which breaks the obligation [`PromptSource`] states.
    #[error("prompt {name} came back with a reference that does not match its text")]
    InconsistentReference {
        /// The name that was asked for.
        name: PromptName,
    },
}

impl PromptError {
    /// A stable snake-case label, for a metric or a span attribute.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::VersionNotFound { .. } => "version_not_found",
            Self::LabelNotFound { .. } => "label_not_found",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden { .. } => "forbidden",
            Self::RateLimited => "rate_limited",
            Self::Transport { .. } => "transport",
            Self::Malformed { .. } => "malformed",
            Self::Unsupported { .. } => "unsupported",
            Self::InconsistentReference { .. } => "inconsistent_reference",
        }
    }

    /// Returns `true` when the same request could plausibly succeed later
    /// without anybody changing anything.
    ///
    /// A transport fault and a rate limit are transient. A missing name, a
    /// pinned version that does not exist, a rejected credential and a
    /// malformed answer are not: they need an edit somewhere before the answer
    /// changes.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::RateLimited | Self::Transport { .. })
    }
}

/// Where prompt text comes from.
///
/// # The obligation
///
/// An implementation owes exactly one thing beyond returning text:
///
/// > **The text it returns must be the text whose hash the reference carries.**
///
/// Everything downstream rests on that. The replay record stores the reference
/// and not the text, so an auditor answering "which prompt produced this turn"
/// is trusting the digest; a source that returns one text and a reference to
/// another turns the whole record into decoration. Returning
/// [`LoadedPrompt::new`] discharges the obligation by construction, and
/// [`LoadedPrompt::from_parts`] checks it when a reference arrives from
/// somewhere else.
///
/// Two smaller expectations follow from it:
///
/// * A source must not rewrite, trim or template-expand the text after hashing
///   it. Normalize first, hash second.
/// * A pinned [`PromptSelector::Version`] that the source cannot honour is
///   [`PromptError::VersionNotFound`], never a quiet substitution.
///
/// # Object safety
///
/// The trait is dyn-compatible through [`async_trait`], because the runtime
/// holds one as `Arc<dyn PromptSource>` and because a cache decorates an
/// arbitrary one. Implementations are `Send + Sync` for the same reason.
///
/// ```
/// use std::sync::Arc;
/// use turnframe_core::prompt::{
///     LoadedPrompt, PromptError, PromptName, PromptSelector, PromptSource,
/// };
///
/// #[derive(Debug)]
/// struct OnePrompt(&'static str);
///
/// #[async_trait::async_trait]
/// impl PromptSource for OnePrompt {
///     async fn load(
///         &self,
///         name: &PromptName,
///         _selector: &PromptSelector,
///     ) -> Result<LoadedPrompt, PromptError> {
///         Ok(LoadedPrompt::new(name.clone(), "v1", self.0))
///     }
///
///     fn describe(&self) -> &'static str {
///         "one-prompt"
///     }
/// }
///
/// // The coercion is the assertion: a source is usable behind a shared
/// // pointer, which is how the runtime holds one.
/// let shared: Arc<dyn PromptSource> = Arc::new(OnePrompt("Say hello."));
/// assert_eq!(shared.describe(), "one-prompt");
///
/// let name = PromptName::from("greeting");
/// let loading = shared.load(&name, &PromptSelector::Latest);
/// // Awaiting it needs an executor; that the future exists at all is what
/// // dyn-compatibility buys.
/// drop(loading);
/// ```
#[async_trait::async_trait]
pub trait PromptSource: Send + Sync + fmt::Debug {
    /// Loads one prompt.
    ///
    /// # Errors
    ///
    /// See [`PromptError`]. A source that reaches a network reports a failure
    /// to reach it as [`PromptError::Transport`], so a caller can tell "this
    /// prompt does not exist" from "this registry is down".
    async fn load(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError>;

    /// A short stable label naming the kind of source, for logs and metrics.
    ///
    /// `"file"`, `"cache"`, `"langfuse"`. Never a URL and never a credential.
    fn describe(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_built_from_text_matches_that_text_and_no_other() {
        let reference = PromptRef::of_text("interpret.system", "v1", "one");
        assert!(reference.matches("one"));
        assert!(!reference.matches("two"));
        assert!(!reference.matches("one "));
    }

    #[test]
    fn the_same_text_under_two_names_hashes_the_same_but_is_a_different_reference() {
        let a = PromptRef::of_text("a", "v1", "shared");
        let b = PromptRef::of_text("b", "v1", "shared");
        assert_eq!(a.hash, b.hash);
        assert_ne!(a, b);
    }

    #[test]
    fn rendering_carries_identifiers_and_never_the_text() {
        let reference = PromptRef::of_text("interpret.system", "v7", "SECRET INSTRUCTIONS");
        let rendered = reference.to_string();
        assert!(rendered.starts_with("interpret.system@v7#"));
        assert!(!rendered.contains("SECRET"));
        assert_eq!(reference.label(), "interpret.system@v7");
        assert!(!format!("{reference:?}").contains("SECRET"));
    }

    #[test]
    fn a_reference_round_trips() {
        let reference = PromptRef::of_text("n", "v", "text");
        let json = serde_json::to_string(&reference).unwrap();
        assert_eq!(serde_json::from_str::<PromptRef>(&json).unwrap(), reference);
    }

    #[test]
    fn a_loaded_prompt_carries_the_hash_of_its_own_text() {
        let loaded = LoadedPrompt::new("interpret.system", "v1", "Answer with the plan only.");
        assert!(loaded.reference().matches(loaded.text()));
        assert_eq!(loaded.name().as_str(), "interpret.system");
        assert_eq!(loaded.version().as_str(), "v1");
        assert_eq!(loaded.clone().into_text(), "Answer with the plan only.");
        let (reference, text) = LoadedPrompt::new("n", "v", "t").into_parts();
        assert!(reference.matches(&text));
    }

    #[test]
    fn a_reference_that_names_another_text_is_refused() {
        let honest = PromptRef::of_text("n", "v", "the real text");
        assert!(LoadedPrompt::from_parts(honest.clone(), "the real text").is_ok());
        let error = LoadedPrompt::from_parts(honest, "a different text").unwrap_err();
        assert_eq!(error.code(), "inconsistent_reference");
    }

    #[test]
    fn selector_keys_are_distinct_stable_and_say_whether_they_pin() {
        assert_eq!(PromptSelector::Latest.as_key(), "latest");
        assert_eq!(
            PromptSelector::label("production").as_key(),
            "label:production"
        );
        assert_eq!(PromptSelector::version("7").as_key(), "version:7");
        assert_ne!(
            PromptSelector::label("7").as_key(),
            PromptSelector::version("7").as_key()
        );
        assert_eq!(PromptSelector::default(), PromptSelector::Latest);
        assert_eq!(PromptSelector::Latest.to_string(), "latest");
        assert!(PromptSelector::version("7").is_pinned());
        assert!(!PromptSelector::label("production").is_pinned());
        assert!(!PromptSelector::Latest.is_pinned());
    }

    #[test]
    fn every_error_variant_renders_identifiers_and_codes_only() {
        let planted = "sk-live-0123456789abcdef";
        let errors = [
            PromptError::NotFound {
                name: PromptName::from("interpret.system"),
            },
            PromptError::VersionNotFound {
                name: PromptName::from("interpret.system"),
                version: PromptVersion::from("7"),
            },
            PromptError::LabelNotFound {
                name: PromptName::from("interpret.system"),
                label: "production".to_owned(),
            },
            PromptError::Unauthorized,
            PromptError::Forbidden {
                name: PromptName::from("interpret.system"),
            },
            PromptError::RateLimited,
            PromptError::Transport { code: "timeout" },
            PromptError::Malformed { code: "not_json" },
            PromptError::Unsupported {
                reason: "chat_prompt",
            },
            PromptError::InconsistentReference {
                name: PromptName::from("interpret.system"),
            },
        ];
        for error in errors {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains(planted), "{rendered}");
            assert!(!error.code().is_empty());
        }
    }

    #[test]
    fn only_a_transport_fault_and_a_rate_limit_are_worth_repeating() {
        assert!(PromptError::RateLimited.is_transient());
        assert!(PromptError::Transport { code: "connect" }.is_transient());
        assert!(!PromptError::Unauthorized.is_transient());
        assert!(
            !PromptError::NotFound {
                name: PromptName::from("x"),
            }
            .is_transient()
        );
    }
}
