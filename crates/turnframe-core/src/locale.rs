//! Locales and localized copy.
//!
//! Receipts, notices and interaction labels are server-authored copy. They are
//! stored as [`LocalizedText`]: a required default plus optional translations,
//! resolved against the turn's [`Locale`] at render time.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A BCP-47 language tag such as `"it-IT"` or `"en"`.
///
/// The library does not validate the tag beyond being non-empty; it only
/// splits it into language and region for fallback resolution.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Locale(pub String);

impl Locale {
    /// Wraps a BCP-47 tag.
    #[must_use]
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    /// Borrows the full tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The primary language subtag (`"it"` for `"it-IT"`), lowercased view is
    /// not applied: the tag is returned as written.
    #[must_use]
    pub fn language(&self) -> &str {
        self.0.split(['-', '_']).next().unwrap_or(self.0.as_str())
    }

    /// The region subtag when present (`Some("IT")` for `"it-IT"`).
    #[must_use]
    pub fn region(&self) -> Option<&str> {
        let mut parts = self.0.split(['-', '_']);
        parts.next()?;
        parts.next().filter(|s| !s.is_empty())
    }

    /// Returns `true` when both locales share the primary language.
    #[must_use]
    pub fn same_language(&self, other: &Locale) -> bool {
        self.language().eq_ignore_ascii_case(other.language())
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::new("en")
    }
}

impl From<&str> for Locale {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for Locale {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Server-authored copy with a mandatory fallback and optional translations.
///
/// Resolution order for a requested locale: exact tag, then any translation
/// whose primary language matches, then the default text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct LocalizedText {
    /// Text used when no translation matches.
    pub default: String,
    /// Translations keyed by locale tag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<Locale, String>,
}

impl LocalizedText {
    /// Creates copy with only a default text.
    #[must_use]
    pub fn new(default: impl Into<String>) -> Self {
        Self {
            default: default.into(),
            translations: BTreeMap::new(),
        }
    }

    /// Adds or replaces a translation.
    #[must_use]
    pub fn with(mut self, locale: impl Into<Locale>, text: impl Into<String>) -> Self {
        self.translations.insert(locale.into(), text.into());
        self
    }

    /// Resolves the best text for `locale`.
    #[must_use]
    pub fn resolve(&self, locale: &Locale) -> &str {
        if let Some(exact) = self.translations.get(locale) {
            return exact;
        }
        self.translations
            .iter()
            .find(|(candidate, _)| candidate.same_language(locale))
            .map_or(self.default.as_str(), |(_, text)| text.as_str())
    }
}

impl From<&str> for LocalizedText {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for LocalizedText {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_parts() {
        let l = Locale::from("it-IT");
        assert_eq!(l.language(), "it");
        assert_eq!(l.region(), Some("IT"));
        assert_eq!(Locale::from("en").region(), None);
        assert!(Locale::from("it-CH").same_language(&l));
    }

    #[test]
    fn localized_text_resolution_order() {
        let text = LocalizedText::new("Confirm")
            .with("it-IT", "Conferma")
            .with("de", "Bestätigen");
        assert_eq!(text.resolve(&Locale::from("it-IT")), "Conferma");
        assert_eq!(text.resolve(&Locale::from("it-CH")), "Conferma");
        assert_eq!(text.resolve(&Locale::from("de-AT")), "Bestätigen");
        assert_eq!(text.resolve(&Locale::from("fr")), "Confirm");
    }

    #[test]
    fn localized_text_round_trip() {
        let text = LocalizedText::new("x").with("it", "y");
        let json = serde_json::to_string(&text).unwrap();
        let back: LocalizedText = serde_json::from_str(&json).unwrap();
        assert_eq!(back, text);
        assert_eq!(
            serde_json::to_string(&LocalizedText::new("x")).unwrap(),
            r#"{"default":"x"}"#
        );
    }
}
