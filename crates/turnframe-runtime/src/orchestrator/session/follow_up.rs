//! Steps O and P: every case as it now stands, the cards those cases require, and
//! which cases and workflows the turn was about.

use std::collections::{BTreeMap, BTreeSet};

use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::flow::ErasedWorkflowView;
use turnframe_core::ids::WorkflowKey;
use turnframe_core::interaction::InteractionSpec;
use turnframe_core::reduce::{PlannedActResult, ReductionPlan};
use turnframe_core::target::TargetResolution;
use turnframe_core::understanding::{ActAction, ActTarget};

use super::Session;
use crate::execute::ExecutionReport;
use crate::interactions::PersistedInteractions;

type Standing = (CaseRef, Option<serde_json::Value>, ErasedWorkflowView);

/// The state of each case the answer stands on, after the turn's commit.
pub(super) type States = std::collections::BTreeMap<CaseKey, Option<serde_json::Value>>;

impl Session<'_> {
    /// The views the answer is written from, the cards the standing cases require,
    /// and whether a case the turn touched could not be read back.
    ///
    /// Every case in view is walked, not only the changed ones: a turn that wrote
    /// nothing may still have a question with a card. I5 and the domain's
    /// `build_interaction` keep that from raising a card nobody wanted, and a card
    /// already answered at this revision is not put back up.
    pub(super) async fn views_and_follow_up(
        &mut self,
        execution: &ExecutionReport,
        plan: &ReductionPlan,
    ) -> (Vec<ErasedWorkflowView>, States, PersistedInteractions, bool) {
        let attempted: BTreeSet<CaseKey> = execution
            .changed
            .keys()
            .cloned()
            .chain(
                execution
                    .outcomes
                    .iter()
                    .map(|record| record.case_ref.key()),
            )
            .collect();
        let (standing, refresh_unavailable) = self.standing_cases(&attempted).await;
        let mut specs: Vec<InteractionSpec> = Vec::new();
        for (case_ref, state, view) in &standing {
            let Some(requirement) = view.blocking_interaction.as_ref() else {
                continue;
            };
            let Ok(registered) = self.runtime.workflows.require(&case_ref.workflow) else {
                continue;
            };
            // A card drawn at an older revision shows facts that no longer hold: the new
            // one replaces it, and a click on the old one is stale.
            let occupied = self
                .runtime
                .interactions
                .open_for_case(self.account(), &case_ref.key())
                .await
                .map(|open| {
                    open.iter().any(|interaction| {
                        interaction.blocking
                            && interaction.case_ref.expected_revision == case_ref.expected_revision
                    })
                })
                .unwrap_or(true);
            if occupied {
                continue;
            }
            // «Not now» means «ask me when the record changes».
            if self
                .runtime
                .interactions
                .blocking_answered_at(self.account(), &case_ref.key(), case_ref.expected_revision)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            if let Ok(spec) = registered.definition.build_interaction(
                case_ref.clone(),
                state.as_ref(),
                requirement,
            ) {
                specs.push(spec);
            }
        }
        let persisted = self
            .runtime
            .interactions
            .persist(
                &specs,
                self.account(),
                self.input.conversation_id,
                self.input.turn_id,
                self.now,
            )
            .await;
        let states: States = standing
            .iter()
            .map(|(case_ref, state, _)| (case_ref.key(), state.clone()))
            .collect();
        let mut views: Vec<ErasedWorkflowView> =
            standing.into_iter().map(|(_, _, view)| view).collect();
        views.extend(self.projected_but_absent(plan, &attempted, &views));
        (views, states, persisted, refresh_unavailable)
    }

    /// The projection of a case an act reached that does not exist yet: a start the
    /// domain compiled no commands for still has a phase, obligations and a briefing.
    /// A refused act's case never will, so it is not projected; nor does a case that
    /// does not exist get a card.
    fn projected_but_absent(
        &self,
        plan: &ReductionPlan,
        attempted: &BTreeSet<CaseKey>,
        standing: &[ErasedWorkflowView],
    ) -> Vec<ErasedWorkflowView> {
        let mut projected: Vec<ErasedWorkflowView> = Vec::new();
        for planned in &plan.acts {
            if matches!(
                planned.result,
                PlannedActResult::Rejected { .. } | PlannedActResult::SupersededByCorrection
            ) {
                continue;
            }
            let Some(case_ref) = planned.target.as_ref().and_then(TargetResolution::exact) else {
                continue;
            };
            let key = case_ref.key();
            if self.cases.contains_key(&key)
                || attempted.contains(&key)
                || standing.iter().any(|view| view.case_ref.key() == key)
                || projected.iter().any(|view| view.case_ref.key() == key)
            {
                continue;
            }
            let Ok(registered) = self.runtime.workflows.require(&key.workflow) else {
                continue;
            };
            if let Ok(view) = registered.definition.project(case_ref.clone(), None) {
                projected.push(view);
            }
        }
        projected
    }

    /// Which cases this turn was about, as opposed to which it loaded: those an act
    /// reached, a question rests on, or an open card sits on. A turn with none («ok»,
    /// «sì») is about what the last turn was about.
    pub(super) fn subjects(&self, plan: &ReductionPlan) -> Vec<CaseKey> {
        let mut subjects: BTreeSet<CaseKey> = plan
            .acts
            .iter()
            .filter_map(|act| Some(act.target.as_ref()?.exact()?.key()))
            .collect();
        subjects.extend(
            plan.answer_tasks
                .iter()
                .flat_map(|task| task.case_refs.iter())
                .map(CaseRef::key),
        );
        subjects.extend(
            self.open_interactions
                .iter()
                .map(|interaction| interaction.case_ref.key()),
        );
        if subjects.is_empty() {
            subjects.extend(self.carried_subjects.iter().map(CaseRef::key));
        }
        subjects.into_iter().collect()
    }

    /// The workflows this turn tried to open a record of: started, or aimed at with a
    /// new or unlisted record. A refused start still names its workflow; a record that
    /// exists does not, since why a workflow cannot start is nothing to its records.
    pub(super) fn named_workflows(&self, plan: &ReductionPlan) -> Vec<WorkflowKey> {
        let mut named: BTreeSet<WorkflowKey> = BTreeSet::new();
        for planned in &plan.acts {
            if let ActAction::Start { workflow } = &planned.act.action {
                named.insert(workflow.clone());
            }
            if let ActTarget::New { workflow } | ActTarget::NotListed { workflow, .. } =
                &planned.act.target
            {
                named.insert(workflow.clone());
            }
        }
        named.into_iter().collect()
    }

    /// Every case in view with its state and projection as they now stand.
    ///
    /// Every attempted case is reloaded, refusals included: a domain may project
    /// facts a refusal was the first to observe. A case that cannot be refreshed is
    /// left out rather than shown stale (I19); untouched cases keep their view.
    async fn standing_cases(&mut self, attempted: &BTreeSet<CaseKey>) -> (Vec<Standing>, bool) {
        let mut fresh: BTreeMap<CaseKey, Standing> = BTreeMap::new();
        let mut refresh_unavailable = false;
        for case_key in attempted {
            match self.refreshed(case_key).await {
                Some(entry) => {
                    fresh.insert(case_key.clone(), entry);
                }
                None => refresh_unavailable = true,
            }
        }
        let mut standing = Vec::with_capacity(self.cases.len().max(fresh.len()));
        for (key, case) in &self.cases {
            match fresh.remove(key) {
                Some(entry) => standing.push(entry),
                None if !attempted.contains(key) => standing.push((
                    case.view.case_ref.clone(),
                    case.state.clone(),
                    case.view.clone(),
                )),
                None => {}
            }
        }
        standing.extend(fresh.into_values());
        (standing, refresh_unavailable)
    }

    async fn refreshed(&self, case_key: &CaseKey) -> Option<Standing> {
        let fields = |error: &dyn std::fmt::Display, what: &str| {
            tracing::warn!(
                target: "turnframe.orchestrator",
                workflow = case_key.workflow.as_str(),
                case_id = %case_key.case_id,
                error = %error,
                "{what}"
            );
        };
        let registered = self
            .runtime
            .workflows
            .require(&case_key.workflow)
            .inspect_err(|error| fields(error, "attempted case has no registered workflow"))
            .ok()?;
        let loaded = registered
            .executor
            .load(self.account(), &case_key.case_id)
            .await
            .inspect_err(|error| fields(error, "attempted case could not be reloaded"))
            .ok()?;
        let case_ref = case_key.clone().at(loaded.revision);
        let view = registered
            .definition
            .project(case_ref.clone(), loaded.value.as_ref())
            .inspect_err(|error| fields(error, "attempted case could not be projected"))
            .ok()?;
        Some((case_ref, loaded.value, view))
    }
}
