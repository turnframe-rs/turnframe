//! Pinning what a projector emits, so a change to it cannot pass unnoticed.
//!
//! A [`WorkflowDefinition`]'s version must change when its projection means something new:
//! replay records and evaluation baselines cite it, and the invariant checker only asks
//! whether a view is coherent. Build a [`ProjectionFingerprint`] from a definition and the
//! states worth pinning, then snapshot it:
//!
//! ```
//! use turnframe_test::projection::ProjectionFingerprint;
//! use turnframe_test::workflows::traveler::{self, TravelerWorkflow};
//!
//! let workflow = TravelerWorkflow::default();
//! let fingerprint = ProjectionFingerprint::of(&workflow)
//!     .at("empty", None)
//!     .at("collecting", Some(traveler::incomplete_draft()))
//!     .build();
//!
//! assert_eq!(fingerprint.workflow.as_str(), "traveler");
//! assert_eq!(fingerprint.version.as_str(), "1");
//! // In a test: insta::assert_yaml_snapshot!(fingerprint.snapshot_name(), fingerprint);
//! ```
//!
//! [`ProjectionFingerprint::snapshot_name`] carries the version: a projector changed without
//! a bump fails against its snapshot, and one changed with a bump writes a new file to review.
//! Pinned: phase and owner, obligations, the blocking card and what answering it authorizes,
//! notice codes and severity, the outcome. Prose is not: it is localized and changes freely.

use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseRef;
use turnframe_core::flow::{
    InteractionRequirement, PhaseOwnership, WorkflowDefinition, WorkflowView,
};
use turnframe_core::ids::{CaseRevision, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::{InteractionKind, TextResolutionPolicy};
use turnframe_core::response::NoticeSeverity;

/// The case a fingerprint projects at. Fixed, because the case identifier is
/// not part of what a projector decides.
const CASE_ID: &str = "fingerprint";

/// The revision a fingerprint projects at.
///
/// A projector must not read the revision — the state explorer reports one that
/// does — so any value serves, and a fixed one keeps the snapshot stable.
const REVISION: CaseRevision = CaseRevision(1);

/// What one projected state looks like, reduced to the parts that are contract
/// rather than copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectedShape {
    /// The name the caller gave this state, so a diff says which one moved.
    pub state: String,
    /// The phase, as canonical JSON.
    pub phase: serde_json::Value,
    /// Who the phase says must act next.
    pub ownership: PhaseOwnership,
    /// Every open obligation's stable identifier, in the order projected.
    pub obligations: Vec<String>,
    /// The blocking card the phase requires, if it requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<RequiredCard>,
    /// Notice codes and severities, without their prose.
    pub notices: Vec<NoticeShape>,
    /// The outcome, present only when the workflow is finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
}

/// The part of a required card that is contract rather than copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RequiredCard {
    /// Stable key of the requirement within its phase.
    pub key: String,
    /// The shape the card takes.
    pub kind: InteractionKind,
    /// Whether it owns an unqualified answer for the case.
    pub blocking: bool,
    /// Whether a revision change leaves it standing.
    pub revision_independent: bool,
    /// Whether typed text may resolve it.
    pub text_resolution: TextResolutionPolicy,
    /// The highest risk answering it authorizes.
    pub confirms_risk: turnframe_core::command::RiskClass,
}

impl From<&InteractionRequirement> for RequiredCard {
    fn from(requirement: &InteractionRequirement) -> Self {
        Self {
            key: requirement.key.clone(),
            kind: requirement.kind,
            blocking: requirement.blocking,
            revision_independent: requirement.revision_independent,
            text_resolution: requirement.text_resolution.clone(),
            confirms_risk: requirement.confirms_risk,
        }
    }
}

/// A notice reduced to its identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NoticeShape {
    /// The stable code, which is the notice's identity.
    pub code: String,
    /// How loudly it is meant to read.
    pub severity: NoticeSeverity,
}

/// Everything one projector emits over a chosen set of states, at one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectionFingerprint {
    /// The workflow this projector answers for.
    pub workflow: WorkflowKey,
    /// The version it declares, which is what the fingerprint is pinned to.
    pub version: WorkflowVersion,
    /// One entry per state, in the order they were added.
    pub states: Vec<ProjectedShape>,
}

impl ProjectionFingerprint {
    /// Starts a fingerprint for `definition`.
    #[must_use]
    pub fn of<W: WorkflowDefinition>(definition: &W) -> FingerprintBuilder<'_, W> {
        FingerprintBuilder {
            definition,
            states: Vec::new(),
        }
    }

    /// A snapshot name carrying the workflow and the version it pins.
    ///
    /// Naming the snapshot after the version is the whole mechanism: a
    /// projector that changes without a bump fails against the file that
    /// already exists, and one that changes with a bump writes a new file
    /// beside the old, so the previous shape stays on record.
    #[must_use]
    pub fn snapshot_name(&self) -> String {
        format!("{}@{}", self.workflow.as_str(), self.version.as_str())
    }
}

/// Collects the states a fingerprint covers.
#[derive(Debug)]
pub struct FingerprintBuilder<'a, W: WorkflowDefinition> {
    definition: &'a W,
    states: Vec<(String, Option<W::State>)>,
}

impl<'a, W: WorkflowDefinition> FingerprintBuilder<'a, W> {
    /// Adds a state to pin, under a name a diff can refer to.
    ///
    /// Choose states that mean something: the empty case, one obligation
    /// closed, the phase that raises a card, and each terminal outcome. A
    /// fingerprint over states nobody reaches pins nothing worth pinning.
    #[must_use]
    pub fn at(mut self, name: impl Into<String>, state: Option<W::State>) -> Self {
        self.states.push((name.into(), state));
        self
    }

    /// Projects every state and reduces each view to its contract.
    #[must_use]
    pub fn build(self) -> ProjectionFingerprint {
        let case_ref = CaseRef::new(self.definition.key(), CASE_ID, REVISION);
        let states = self
            .states
            .iter()
            .map(|(name, state)| {
                let view = self.definition.project(case_ref.clone(), state.as_ref());
                shape_of(name, self.definition, &view)
            })
            .collect();
        ProjectionFingerprint {
            workflow: self.definition.key(),
            version: self.definition.version(),
            states,
        }
    }
}

/// Reduces one projected view to the parts that are contract.
fn shape_of<W: WorkflowDefinition>(
    name: &str,
    definition: &W,
    view: &WorkflowView<W::Phase, W::Obligation, W::Outcome>,
) -> ProjectedShape {
    ProjectedShape {
        state: name.to_owned(),
        phase: serde_json::to_value(&view.phase).unwrap_or(serde_json::Value::Null),
        ownership: definition.phase_ownership(&view.phase),
        obligations: view
            .obligations
            .iter()
            .map(|obligation| {
                serde_json::to_string(obligation)
                    .unwrap_or_else(|_| String::from("<unserializable>"))
            })
            .collect(),
        blocking: view.blocking_interaction.as_ref().map(RequiredCard::from),
        notices: view
            .notices
            .iter()
            .map(|notice| NoticeShape {
                code: notice.code.clone(),
                severity: notice.severity,
            })
            .collect(),
        outcome: view
            .outcome
            .as_ref()
            .map(|outcome| serde_json::to_value(outcome).unwrap_or(serde_json::Value::Null)),
    }
}
