//! How a run is configured (spec §27.6).
//!
//! The whole point of this module is one distinction the specification insists
//! on, and it is worth stating before any type appears:
//!
//! * **A sample is one execution of the agent under test.** More samples
//!   measure how much the *model* varies. Ten samples of the same item are ten
//!   chances for understanding to read something different, and the spread
//!   between them is the number a release gate cares about.
//! * **A vote is one judge opinion about one sample.** More votes measure how
//!   much the *judge* varies. Three votes on one sample tell you nothing about
//!   the agent; they tell you whether the judge would have said the same thing
//!   twice.
//!
//! Averaging the two together produces a number that moves when either the
//! model or the judge wobbles and cannot say which — which is exactly the
//! failure §26.3 warns about. So they are two settings, they are counted
//! separately, and they are reported separately.
//!
//! The file format is the one printed in the specification:
//!
//! ```toml
//! [execution]
//! samples_per_item = 10
//!
//! [judging]
//! votes_per_sample = 3
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::corpus::{ItemId, Tag};

/// Everything one evaluation run needs to know.
///
/// ```
/// use turnframe_eval::config::EvalConfig;
///
/// let config = EvalConfig::from_toml_str(
///     r#"
///     [execution]
///     samples_per_item = 10
///
///     [judging]
///     votes_per_sample = 3
///     "#,
/// )?;
///
/// assert_eq!(config.execution.samples_per_item, 10);
/// assert_eq!(config.judging.votes_per_sample, 3);
/// config.validate()?;
/// # Ok::<(), turnframe_eval::config::ConfigError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EvalConfig {
    /// How the agent under test is exercised.
    pub execution: ExecutionConfig,
    /// How the judge is polled, when an item asks for one.
    pub judging: JudgingConfig,
    /// Which subset of a suite runs.
    pub selection: SelectionConfig,
}

impl EvalConfig {
    /// A run of one sample and one vote: the cheapest configuration that still
    /// exercises every stage. Use it for a smoke run; a release gate wants the
    /// numbers of spec §27.6 instead.
    #[must_use]
    pub fn single() -> Self {
        Self::default()
    }

    /// Sets the number of executions of the agent under test per item.
    #[must_use]
    pub const fn with_samples_per_item(mut self, samples: u32) -> Self {
        self.execution.samples_per_item = samples;
        self
    }

    /// Keeps the turn's own words on each sample, for curating a corpus.
    ///
    /// See [`ExecutionConfig::record_answers`] for why this is off by default.
    #[must_use]
    pub const fn recording_answers(mut self) -> Self {
        self.execution.record_answers = true;
        self
    }

    /// Sets the number of judge opinions collected per *sample*.
    #[must_use]
    pub const fn with_votes_per_sample(mut self, votes: u32) -> Self {
        self.judging.votes_per_sample = votes;
        self
    }

    /// Sets how many samples may be in flight at once.
    ///
    /// Leave it at one for a reproducible in-memory run; raise it only for a
    /// corpus against a real endpoint. See
    /// [`ExecutionConfig::sample_concurrency`].
    #[must_use]
    pub const fn with_sample_concurrency(mut self, samples: u32) -> Self {
        self.execution.sample_concurrency = samples;
        self
    }

    /// Caps how many of a turn's events one observation records, loudly.
    ///
    /// See [`ExecutionConfig::max_observed_events`]: an observation that hits
    /// the cap fails every assertion about events rather than passing over a
    /// ledger it only half read.
    #[must_use]
    pub const fn with_max_observed_events(mut self, events: usize) -> Self {
        self.execution.max_observed_events = Some(events);
        self
    }

    /// Restricts the run to items carrying `tag`.
    #[must_use]
    pub fn including_tag(mut self, tag: impl Into<Tag>) -> Self {
        self.selection.include_tags.push(tag.into());
        self
    }

    /// Excludes items carrying `tag`, whatever else selects them.
    #[must_use]
    pub fn excluding_tag(mut self, tag: impl Into<Tag>) -> Self {
        self.selection.exclude_tags.push(tag.into());
        self
    }

    /// Parses a configuration from TOML.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Parse`] when the document is malformed or carries a key
    /// this crate does not understand.
    pub fn from_toml_str(source: &str) -> Result<Self, ConfigError> {
        toml::from_str(source).map_err(|error| ConfigError::Parse {
            message: error.to_string(),
        })
    }

    /// Reads a configuration from a TOML file.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::Read`] when the file cannot be read;
    /// * [`ConfigError::Parse`] when its contents are not a configuration.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|error| ConfigError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        Self::from_toml_str(&source)
    }

    /// Checks the configuration is one this crate will run.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::ZeroSamples`] — an item that never executes has no
    ///   result, not an empty one;
    /// * [`ConfigError::ZeroVotes`] — a judge with no votes has no verdict.
    pub const fn validate(&self) -> Result<(), ConfigError> {
        if self.execution.samples_per_item == 0 {
            return Err(ConfigError::ZeroSamples);
        }
        if self.judging.votes_per_sample == 0 {
            return Err(ConfigError::ZeroVotes);
        }
        Ok(())
    }
}

/// How the agent under test is exercised.
///
/// `#[non_exhaustive]`: this 0.1 grows a field whenever a run needs to say
/// something new, and each one would be source-breaking for anybody constructing
/// this with a struct literal — `#[serde(default)]` keeps a FILE readable and
/// does nothing for Rust. Build it from [`Default`] and the `with_*` methods,
/// which is what the next added field will not break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ExecutionConfig {
    /// How many times each item runs end to end.
    ///
    /// This is the only knob that measures **model** variance. Raising it makes
    /// a flaky item visible; raising [`JudgingConfig::votes_per_sample`] never
    /// will.
    pub samples_per_item: u32,
    /// Stop the whole run after this many samples have failed their
    /// deterministic assertions. `None` runs the corpus to the end, which is
    /// what a nightly job wants; a pull-request gate may prefer to stop early.
    pub stop_after_failures: Option<u32>,
    /// How many samples may be in flight at once.
    ///
    /// One — the default — runs the corpus strictly one sample at a time, which
    /// is what a reproducible in-memory run wants: the harness sees the samples
    /// in index order, and a scripted provider keyed on call order sees exactly
    /// the sequence it was written for.
    ///
    /// Raising it is for a corpus against a **real endpoint**, where a hundred
    /// samples in series is hours of waiting on a network. What it changes is
    /// the *execution* order: samples start and finish interleaved, so a
    /// harness that shares anything between them — a counter, a queue of
    /// scripted answers, a rate limit — will see a different order every run.
    /// The report does not move: samples are still reported in index order,
    /// and the same set of results comes back whatever this is set to. Zero is
    /// read as one.
    pub sample_concurrency: u32,
    /// How many of a turn's events one observation may record before it stops
    /// and says so.
    ///
    /// `None` — the default — reads the ledger to the end, paging the journal
    /// by its sequence cursor. A `Some(limit)` is a deliberate ceiling for a
    /// run where one item could commit an unbounded number of events, and it is
    /// never silent: an observation that hit it is marked truncated, and every
    /// assertion that reads the event list then fails loudly instead of passing
    /// over the half of the ledger nobody read.
    pub max_observed_events: Option<usize>,
    /// Keep the turn's own words on each sample of the report.
    ///
    /// Off by default, and the default is the careful one: the reply is the
    /// only model-authored prose a run produces, it is the one field that can
    /// carry whatever a person typed, and a report is a file that gets attached
    /// to things. A measurement does not need it — every assertion reads
    /// storage, and the judge is handed the text directly whether this is on or
    /// off.
    ///
    /// Turn it on to CURATE. Writing the expectations of an item means deciding
    /// what the right reply would have been, and that cannot be done from a
    /// signature: two runs whose effects are identical can differ entirely in
    /// whether the assistant asked the question the turn needed. Reading them
    /// is how a corpus of scenes carried over from another engine gets its
    /// assertions.
    pub record_answers: bool,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            samples_per_item: 1,
            stop_after_failures: None,
            sample_concurrency: 1,
            max_observed_events: None,
            record_answers: false,
        }
    }
}

/// How the judge is polled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JudgingConfig {
    /// How many judge opinions are collected about **one sample**.
    ///
    /// Votes reduce the judge's own variance. They say nothing about the agent
    /// under test, so they never enter a deterministic pass rate. An odd number
    /// avoids ties; on a tie the lower score wins, because a judge harness that
    /// rounds up in its own favour is not a measurement.
    pub votes_per_sample: u32,
    /// Judge every sample, or only the first one of each item. Judging one
    /// sample per item is the usual choice: the judge is there to grade
    /// language, and language costs money to grade.
    pub judge_every_sample: bool,
}

impl Default for JudgingConfig {
    fn default() -> Self {
        Self {
            votes_per_sample: 1,
            judge_every_sample: false,
        }
    }
}

/// Which items of a suite a run selects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SelectionConfig {
    /// Run only items carrying at least one of these tags. Empty means every
    /// item is a candidate.
    pub include_tags: Vec<Tag>,
    /// Skip items carrying any of these tags, even when `include_tags` selected
    /// them.
    pub exclude_tags: Vec<Tag>,
    /// Run only these items, by identifier. Applied after the tag filters.
    pub items: Vec<ItemId>,
}

impl SelectionConfig {
    /// Returns `true` when nothing is filtered and every item runs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.include_tags.is_empty() && self.exclude_tags.is_empty() && self.items.is_empty()
    }

    /// Returns `true` when an item with these tags and identifier runs.
    #[must_use]
    pub fn selects(&self, id: &ItemId, tags: &[Tag]) -> bool {
        if !self.include_tags.is_empty() && !self.include_tags.iter().any(|t| tags.contains(t)) {
            return false;
        }
        if self.exclude_tags.iter().any(|t| tags.contains(t)) {
            return false;
        }
        self.items.is_empty() || self.items.contains(id)
    }
}

/// Why a configuration could not be loaded or run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file could not be read.
    #[error("evaluation configuration at {path} could not be read: {message}")]
    Read {
        /// The path that was tried.
        path: PathBuf,
        /// What the filesystem said.
        message: String,
    },
    /// The document is not a configuration this crate understands.
    #[error("evaluation configuration could not be parsed: {message}")]
    Parse {
        /// What the parser said, including the unknown key when there was one.
        message: String,
    },
    /// `samples_per_item` is zero.
    #[error("samples_per_item must be at least 1: an item that never runs has no result")]
    ZeroSamples,
    /// `votes_per_sample` is zero.
    #[error("votes_per_sample must be at least 1: a judge with no votes has no verdict")]
    ZeroVotes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specification_snippet_parses() {
        let config = EvalConfig::from_toml_str(
            "[execution]\nsamples_per_item = 10\n\n[judging]\nvotes_per_sample = 3\n",
        )
        .unwrap();
        assert_eq!(config.execution.samples_per_item, 10);
        assert_eq!(config.judging.votes_per_sample, 3);
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let error =
            EvalConfig::from_toml_str("[execution]\nsamples = 10\n").expect_err("unknown key");
        assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
    }

    #[test]
    fn votes_cannot_stand_in_for_samples() {
        // The type system cannot stop someone setting one and meaning the
        // other, but the validation can at least refuse the degenerate values.
        assert!(matches!(
            EvalConfig::default().with_samples_per_item(0).validate(),
            Err(ConfigError::ZeroSamples)
        ));
        assert!(matches!(
            EvalConfig::default().with_votes_per_sample(0).validate(),
            Err(ConfigError::ZeroVotes)
        ));
    }

    #[test]
    fn selection_filters_by_tag_then_by_identifier() {
        let selection = SelectionConfig {
            include_tags: vec![Tag::new("trip")],
            exclude_tags: vec![Tag::new("slow")],
            items: Vec::new(),
        };
        assert!(selection.selects(&ItemId::new("a"), &[Tag::new("trip")]));
        assert!(!selection.selects(&ItemId::new("a"), &[Tag::new("traveler")]));
        assert!(!selection.selects(&ItemId::new("a"), &[Tag::new("trip"), Tag::new("slow")]));
    }
}
