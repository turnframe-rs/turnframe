//! How much judgment a turn buys: more model calls behind each step, never more authority.

use serde::{Deserialize, Serialize};

/// How hard a turn works to be read correctly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effort {
    /// Fewer calls: no reply review, no step prose, a shorter transcript.
    Low,
    /// Today's behaviour, and the default.
    #[default]
    Medium,
    /// More calls: votes, some reasoning, and a check of the whole turn.
    High,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// Stable label, for records, metrics and configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A label that names no level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an effort level: low, medium or high")]
pub struct UnknownEffort(pub String);

impl std::str::FromStr for Effort {
    type Err = UnknownEffort;

    fn from_str(label: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == label)
            .ok_or_else(|| UnknownEffort(label.to_owned()))
    }
}
