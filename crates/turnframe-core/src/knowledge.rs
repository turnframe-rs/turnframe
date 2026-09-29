//! Knowledge retrieval contract (spec §19.2).
//!
//! Retrieved content is evidence for answers, never authorization for
//! commands (spec §19.3).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::case::CaseRef;
use crate::ids::AccountId;
use crate::locale::Locale;
use crate::read::{DataSensitivity, TrustLevel};
use crate::reduce::SourcePolicy;

/// Citation metadata of a knowledge chunk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Citation {
    /// Source identifier.
    pub source_id: String,
    /// Human label of the source.
    pub label: String,
    /// Where to read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Locator within the source (section, page, article).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    /// Source version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A retrieval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeRequest {
    /// Tenant.
    pub account_id: AccountId,
    /// The query.
    pub query: String,
    /// Locale of the user.
    pub locale: Locale,
    /// Cases the question is about.
    pub case_refs: Vec<CaseRef>,
    /// Source requirements.
    pub source_policy: SourcePolicy,
    /// Maximum chunks to return, or `None` to let the provider decide.
    ///
    /// Unset by default: how much retrieved material belongs in an answer is a
    /// property of the corpus and the model, and a provider that owns the
    /// corpus is better placed to bound it than a library that has not seen it.
    pub max_chunks: Option<usize>,
    /// Only sources effective on this date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<NaiveDate>,
}

/// One retrieved chunk with provenance (spec §19.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeChunk {
    /// Chunk identifier.
    pub chunk_id: String,
    /// Source identifier.
    pub source_id: String,
    /// Source version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    /// First day the content is effective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<NaiveDate>,
    /// Last day the content is effective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<NaiveDate>,
    /// Permission tags required to read it.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Citation metadata.
    pub citation: Citation,
    /// The text.
    pub text: String,
    /// Trust level.
    pub trust: TrustLevel,
    /// Sensitivity.
    pub sensitivity: DataSensitivity,
}

impl KnowledgeChunk {
    /// Returns `true` when the chunk is effective on `date`.
    #[must_use]
    pub fn is_effective_on(&self, date: NaiveDate) -> bool {
        self.effective_from.is_none_or(|from| from <= date)
            && self.effective_to.is_none_or(|to| date <= to)
    }
}

/// Retrieval failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum KnowledgeError {
    /// A source is unavailable.
    #[error("knowledge source {source_id} unavailable")]
    Unavailable {
        /// The source.
        source_id: String,
    },
    /// The actor may not read the requested sources.
    #[error("knowledge access unauthorized")]
    Unauthorized,
    /// Retrieval timed out.
    #[error("knowledge retrieval timed out")]
    Timeout,
    /// The provider returned malformed data.
    #[error("knowledge provider returned malformed data")]
    Malformed,
    /// Provider-specific failure.
    #[error("knowledge failure {code}")]
    Other {
        /// Stable code.
        code: String,
    },
}

/// A knowledge retrieval adapter (spec §19.2).
#[async_trait::async_trait]
pub trait KnowledgeProvider: Send + Sync {
    /// Retrieves chunks for a request. Must enforce the actor's permissions.
    async fn retrieve(
        &self,
        request: KnowledgeRequest,
    ) -> Result<Vec<KnowledgeChunk>, KnowledgeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_window() {
        let chunk = KnowledgeChunk {
            chunk_id: "c".into(),
            source_id: "s".into(),
            source_version: None,
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1),
            effective_to: NaiveDate::from_ymd_opt(2026, 12, 31),
            permissions: vec![],
            citation: Citation {
                source_id: "s".into(),
                label: "Source".into(),
                uri: None,
                locator: None,
                version: None,
            },
            text: "t".into(),
            trust: TrustLevel::Retrieved,
            sensitivity: DataSensitivity::Public,
        };
        assert!(chunk.is_effective_on(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()));
        assert!(!chunk.is_effective_on(NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()));
    }
}
