//! What a simulated user wants, and the state that proves it got it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use turnframe_core::locale::Locale;

use crate::corpus::{
    CaseCountExpectation, CorpusError, EvalItem, Expectations, ItemId, Setup, StateExpectation,
    TurnSpec, WorkflowStateExpectation,
};

/// The most turns a goal may allow.
pub const MAX_TURNS: u32 = 40;

const fn ten() -> u32 {
    10
}

/// One goal: what the person wants, how they talk, where they start and what proves it done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    /// Stable identifier; reports join on it.
    pub id: String,
    /// One sentence a human reads in a report.
    pub name: String,
    /// What the person wants, in words for the simulator, with every value it needs.
    pub want: String,
    /// How the person talks, one conversation each: «terse», «changes their mind».
    pub manners: Vec<String>,
    /// The turns the person takes at most before giving up.
    #[serde(default = "ten")]
    pub max_turns: u32,
    /// The person's locale, English when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<Locale>,
    /// The world before the first turn.
    #[serde(default)]
    pub setup: Setup,
    /// The state that proves the goal reached, checked without a model.
    pub reached: Reached,
}

/// The state a goal reached leaves behind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reached {
    /// What a seeded case holds.
    pub case_state: Vec<StateExpectation>,
    /// What some case of a workflow holds, seeded or created.
    pub workflow_state: Vec<WorkflowStateExpectation>,
    /// How many cases of a workflow exist.
    pub case_count: Vec<CaseCountExpectation>,
}

impl Reached {
    /// Whether it says nothing, which would score every conversation reached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.case_state.is_empty() && self.workflow_state.is_empty() && self.case_count.is_empty()
    }

    /// As the expectations the deterministic checks read.
    #[must_use]
    pub fn expectations(&self) -> Expectations {
        Expectations {
            case_state: self.case_state.clone(),
            workflow_state: self.workflow_state.clone(),
            case_count: self.case_count.clone(),
            ..Expectations::default()
        }
    }
}

impl Goal {
    /// Checks what `serde` cannot.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] naming the field and the reason.
    pub fn validate(&self) -> Result<(), CorpusError> {
        let blank = |text: &str| text.trim().is_empty();
        if blank(&self.id) || blank(&self.name) || blank(&self.want) {
            return Err(CorpusError::invalid(
                "goal",
                "a goal needs an id, a name and a want",
            ));
        }
        if self.manners.is_empty() || self.manners.iter().any(|manner| blank(manner)) {
            return Err(CorpusError::invalid(
                "manners",
                "a goal needs at least one manner",
            ));
        }
        if !(1..=MAX_TURNS).contains(&self.max_turns) {
            return Err(CorpusError::invalid("max_turns", "between 1 and 40"));
        }
        if self.reached.is_empty() {
            return Err(CorpusError::invalid(
                "reached",
                "a goal with no state to reach scores every conversation reached",
            ));
        }
        self.reached.expectations().validate()?;
        for seed in &self.setup.cases {
            seed.validate()?;
        }
        Ok(())
    }

    /// The item a harness prepares the goal's world from; its turn is never taken.
    #[must_use]
    pub fn item(&self) -> EvalItem {
        EvalItem {
            id: ItemId(self.id.clone()),
            name: self.name.clone(),
            description: None,
            tags: Vec::new(),
            setup: self.setup.clone(),
            before: Vec::new(),
            turn: TurnSpec {
                text: Some(self.want.clone()),
                locale: self.locale.clone(),
                ..TurnSpec::default()
            },
            expect: self.reached.expectations(),
            judge: Vec::new(),
            provenance: crate::corpus::Provenance::default(),
        }
    }

    /// Every `.toml` goal in `dir`, sorted by id and validated.
    ///
    /// # Errors
    ///
    /// [`CorpusError`] for a file that cannot be read, parsed or validated, or two goals
    /// sharing an id.
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<Vec<Self>, CorpusError> {
        let dir = dir.as_ref();
        let read = |path: &Path, error: std::io::Error| CorpusError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        };
        let mut goals = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|error| read(dir, error))? {
            let path = entry.map_err(|error| read(dir, error))?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| read(&path, error))?;
            let goal: Self = toml::from_str(&text).map_err(|error| CorpusError::Parse {
                path: path.clone(),
                message: error.to_string(),
            })?;
            goal.validate()?;
            goals.push(goal);
        }
        goals.sort_by(|one, other| one.id.cmp(&other.id));
        if let Some(pair) = goals.windows(2).find(|pair| pair[0].id == pair[1].id) {
            return Err(CorpusError::DuplicateId {
                id: ItemId(pair[0].id.clone()),
            });
        }
        Ok(goals)
    }
}
