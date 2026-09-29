//! Bounded exploration of both sample workflows (spec §8.5, §27.3).
//!
//! The first two tests are the point of the kit: every state reachable from the
//! samples satisfies the projection invariants. The last two are the control
//! group — a model that lies produces exactly the violation it should, which is
//! what stops the first two from passing vacuously.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::error::DomainRejection;
use turnframe_core::event::{OperationalReceipt, ReceiptEvent};
use turnframe_core::flow::{
    CommandPolicy, InteractionRequirement, PhaseOwnership, ViewOf, WorkflowDefinition,
    WorkflowNotice, WorkflowRegistry, WorkflowView,
};
use turnframe_core::ids::{CaseRevision, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::InteractionSpec;
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::operation::OperationSpec;
use turnframe_core::response::NoticeSeverity;
use turnframe_core::target::ResolvedAct;
use turnframe_test::assertions::{AssertionFailure, same_phase_in, single_phase};
use turnframe_test::explore::{
    EXPLORATION_REVISIONS, ExplorationLimits, ExplorationViolationKind, SimulatedTransition,
    WorkflowModel, explore, reachable_states,
};
use turnframe_test::workflows::claim::{
    ClaimModel, ClaimOutcome, ClaimPhase, ClaimWorkflow, complete_proposal, partial_proposal,
    under_review,
};
use turnframe_test::workflows::traveler::{
    TravelerCommand, TravelerEvent, TravelerModel, TravelerObligation, TravelerOutcome,
    TravelerPhase, TravelerState, TravelerWorkflow, awaiting_activation,
};
use turnframe_test::workflows::trip::apply::MAX_EXTRAS;
use turnframe_test::workflows::trip::{
    OTHER_TRAVELER_ID, TripCommand, TripEvent, TripExecutor, TripModel, TripOutcome, TripPhase,
    TripState, TripWorkflow, awaiting_rebooking_confirmation, complete_case, incomplete_case,
};

#[test]
fn trip_reachable_states_satisfy_every_invariant() {
    let report = explore(
        &TripWorkflow::default(),
        &TripModel::default(),
        ExplorationLimits::generous(),
    );

    assert!(report.is_clean(), "{}", report.describe());
    assert!(
        !report.truncated,
        "the trip state space should fit in the generous limits: {}",
        report.describe()
    );
    assert!(
        report.states_explored > 400,
        "exploration barely moved: {}",
        report.describe()
    );
    // Printed on failure only; the number is the budget this test guards.
    assert!(
        report.states_explored < 2_500,
        "the trip state space grew past what a fast test can explore: {}",
        report.describe()
    );
    for outcome in [
        TripOutcome::Withdrawn,
        TripOutcome::Notified,
        TripOutcome::NotNotified,
    ] {
        assert!(
            report.reached_outcome(&outcome),
            "{outcome:?} was never reached: {}",
            report.describe()
        );
    }
    // Where it looked, and not only how much. A budget that stops eight steps
    // in reports every invariant clean over half a workflow, so the phases are
    // named here rather than inferred from a state count.
    for phase in [
        TripPhase::Collecting,
        TripPhase::AwaitingRebookingConfirmation,
        TripPhase::Dispatching,
        TripPhase::Refused,
        TripPhase::Ticketed,
        TripPhase::Notified,
        TripPhase::NotNotified,
        TripPhase::Withdrawn,
    ] {
        assert!(
            report.reached_phase(&phase),
            "{phase:?} was never projected: {}",
            report.describe()
        );
    }
}

/// A budget that stops early reports clean over half a workflow.
///
/// This is the argument for `reached_phases`, made executable. The run below
/// finds no violations and is honest that it truncated — and it never projects
/// the phases that come after the rebooking card, so every check the explorer
/// performs was performed nowhere near them. `truncated` said as much and says
/// it for any workflow with a large collection half, which is why it stopped
/// carrying information; the phases say where.
#[test]
fn a_truncated_run_is_clean_about_the_half_it_never_saw() {
    let report = explore(
        &TripWorkflow::default(),
        &TripModel::default(),
        ExplorationLimits::new(24, 2, 8),
    );

    assert!(report.is_clean(), "{}", report.describe());
    assert!(report.truncated, "{}", report.describe());
    assert!(
        report.reached_phase(&TripPhase::Collecting),
        "the search did start: {}",
        report.describe()
    );
    for phase in [
        TripPhase::Dispatching,
        TripPhase::Ticketed,
        TripPhase::Notified,
        TripPhase::NotNotified,
    ] {
        assert!(
            !report.reached_phase(&phase),
            "{phase:?} is out of reach at depth two, and a clean report must not \
             be read as covering it: {}",
            report.describe()
        );
    }
}

#[test]
fn traveler_reachable_states_satisfy_every_invariant() {
    let report = explore(
        &TravelerWorkflow::new().with_cards(),
        &TravelerModel::of(TravelerWorkflow::new().with_cards()),
        ExplorationLimits::generous(),
    );

    assert!(report.is_clean(), "{}", report.describe());
    assert!(!report.truncated, "{}", report.describe());
    assert!(report.states_explored > 8, "{}", report.describe());
    for outcome in [TravelerOutcome::Archived, TravelerOutcome::Deleted] {
        assert!(
            report.reached_outcome(&outcome),
            "{outcome:?} was never reached: {}",
            report.describe()
        );
    }
    for phase in [
        TravelerPhase::AwaitingActivation,
        TravelerPhase::Active,
        TravelerPhase::Archived,
        TravelerPhase::Deleted,
    ] {
        assert!(
            report.reached_phase(&phase),
            "{phase:?} was never projected: {}",
            report.describe()
        );
    }
}

#[test]
fn claim_reachable_states_satisfy_every_invariant() {
    let report = explore(
        &ClaimWorkflow::default(),
        &ClaimModel::default(),
        ExplorationLimits::generous(),
    );

    assert!(report.is_clean(), "{}", report.describe());
    assert!(!report.truncated, "{}", report.describe());
    assert!(report.states_explored > 8, "{}", report.describe());
    for outcome in [ClaimOutcome::Recorded, ClaimOutcome::Discarded] {
        assert!(
            report.reached_outcome(&outcome),
            "{outcome:?} was never reached: {}",
            report.describe()
        );
    }
    for phase in [
        ClaimPhase::AwaitingDocument,
        ClaimPhase::Extracting,
        ClaimPhase::AwaitingReview,
        ClaimPhase::Recorded,
        ClaimPhase::Discarded,
    ] {
        assert!(
            report.reached_phase(&phase),
            "{phase:?} was never projected: {}",
            report.describe()
        );
    }
}

#[test]
fn the_review_phase_is_reached_with_and_without_a_complete_proposal() {
    let states: Vec<turnframe_test::workflows::claim::ClaimState> =
        reachable_states(&ClaimModel::default(), ExplorationLimits::generous())
            .into_iter()
            .flatten()
            .collect();

    let reviewing: Vec<_> = states
        .iter()
        .filter(|state| ClaimWorkflow::phase_of(state) == ClaimPhase::AwaitingReview)
        .collect();
    assert!(
        reviewing
            .iter()
            .any(|state| state.proposal.as_ref().is_some_and(|p| p.is_complete())),
        "a complete proposal must be reachable"
    );
    assert!(
        reviewing
            .iter()
            .any(|state| state.proposal.as_ref().is_some_and(|p| !p.is_complete())),
        "a proposal short of a required field must be reachable"
    );
    assert!(
        reviewing.iter().any(|state| state
            .proposal
            .as_ref()
            .is_some_and(|p| p.edited_count() > 0)),
        "a proposal a human corrected must be reachable"
    );
    assert!(
        states
            .iter()
            .any(|state| state.abandoned_from.is_some() && state.proposal.is_none()),
        "a review the user threw away must be reachable"
    );
    assert!(
        states
            .iter()
            .any(|state| state.recorded.iter().any(|recorded| recorded.corrected)),
        "a recorded value that a human corrected must be reachable: that is what \
         makes provenance worth keeping"
    );
}

#[test]
fn every_sample_projection_carries_one_phase() {
    let trip = TripWorkflow::default();
    for state in [
        None,
        Some(incomplete_case()),
        Some(complete_case()),
        Some(awaiting_rebooking_confirmation()),
    ] {
        let view = trip.project(
            CaseRef::new("trip", "trip-1", CaseRevision(3)),
            state.as_ref(),
        );
        let erased = view
            .erase(trip.phase_ownership(&view.phase))
            .expect("the sample view is serializable");
        single_phase(&erased).expect("one phase per projection");
    }

    let traveler = TravelerWorkflow::new().with_cards();
    for state in [None, Some(awaiting_activation())] {
        let view = traveler.project(
            CaseRef::new("traveler", "c-1", CaseRevision(1)),
            state.as_ref(),
        );
        let erased = view
            .erase(traveler.phase_ownership(&view.phase))
            .expect("the sample view is serializable");
        single_phase(&erased).expect("one phase per projection");
    }

    let claim = ClaimWorkflow::default();
    for state in [
        None,
        Some(under_review(complete_proposal())),
        Some(under_review(partial_proposal())),
    ] {
        let view = claim.project(
            CaseRef::new("claim", "claim-1", CaseRevision(2)),
            state.as_ref(),
        );
        let erased = view
            .erase(claim.phase_ownership(&view.phase))
            .expect("the sample view is serializable");
        single_phase(&erased).expect("one phase per projection");
    }
}

/// A model that never offers `Withdraw`, while still declaring `Withdrawn`
/// reachable.
struct NeverWithdraws(TripModel);

impl WorkflowModel<TripWorkflow> for NeverWithdraws {
    fn initial_states(&self) -> Vec<Option<TripState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TripState>) -> Vec<TripCommand> {
        self.0
            .candidate_commands(state)
            .into_iter()
            .filter(|command| !matches!(command, TripCommand::Withdraw))
            .collect()
    }

    fn simulate(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> SimulatedTransition<TripState, TripEvent> {
        self.0.simulate(state, command)
    }

    fn declared_outcomes(&self) -> Vec<TripOutcome> {
        self.0.declared_outcomes()
    }
}

#[test]
fn an_outcome_no_path_reaches_is_reported() {
    let report = explore(
        &TripWorkflow::default(),
        &NeverWithdraws(TripModel::default()),
        ExplorationLimits::generous(),
    );

    assert!(!report.truncated, "{}", report.describe());
    let unreachable: Vec<_> = report
        .violations
        .iter()
        .filter(|violation| {
            matches!(
                violation.kind,
                ExplorationViolationKind::UnreachableOutcome { .. }
            )
        })
        .collect();
    assert_eq!(unreachable.len(), 1, "{}", report.describe());
    assert!(report.reached_outcome(&TripOutcome::Notified));
}

/// A model that stops offering commands as soon as the case exists.
struct StopsAfterCreation(TravelerModel);

impl WorkflowModel<TravelerWorkflow> for StopsAfterCreation {
    fn initial_states(&self) -> Vec<Option<TravelerState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TravelerState>) -> Vec<TravelerCommand> {
        match state {
            None => self.0.candidate_commands(state),
            Some(_) => Vec::new(),
        }
    }

    fn simulate(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> SimulatedTransition<TravelerState, TravelerEvent> {
        self.0.simulate(state, command)
    }
}

#[test]
fn a_state_with_nowhere_to_go_is_reported_as_a_dead_end() {
    let report = explore(
        &TravelerWorkflow::new().with_cards(),
        &StopsAfterCreation(TravelerModel::of(TravelerWorkflow::new().with_cards())),
        ExplorationLimits::smoke(),
    );

    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.kind == ExplorationViolationKind::DeadEnd),
        "{}",
        report.describe()
    );
    let dead_end = report
        .violations
        .iter()
        .find(|violation| violation.kind == ExplorationViolationKind::DeadEnd)
        .unwrap();
    assert_eq!(
        dead_end.path.len(),
        1,
        "the shortest path to the dead end is one command: {}",
        dead_end.describe()
    );
}

/// A workflow whose projection reads the case revision: at an odd revision it
/// adds a notice, at an even one it does not. Nothing else changes.
///
/// This is the defect the depth-derived revision could not see. When every
/// state was projected at exactly one revision — its own depth — a projector
/// that varied with the revision produced one consistent view per state and
/// looked perfectly deterministic.
struct RevisionSensitive(TripWorkflow);

impl WorkflowDefinition for RevisionSensitive {
    type State = TripState;
    type Phase = turnframe_test::workflows::trip::TripPhase;
    type Obligation = turnframe_test::workflows::trip::TripObligation;
    type Command = TripCommand;
    type Event = TripEvent;
    type Outcome = TripOutcome;

    fn key(&self) -> WorkflowKey {
        self.0.key()
    }

    fn version(&self) -> WorkflowVersion {
        self.0.version()
    }

    fn phase_ownership(&self, phase: &Self::Phase) -> PhaseOwnership {
        self.0.phase_ownership(phase)
    }

    fn project(&self, case_ref: CaseRef, state: Option<&Self::State>) -> ViewOf<Self> {
        let odd = case_ref.expected_revision.value() % 2 == 1;
        let mut view = self.0.project(case_ref, state);
        if odd {
            view.notices.push(WorkflowNotice {
                code: "trip.odd_revision".to_owned(),
                severity: NoticeSeverity::Info,
                text: LocalizedText::new("An odd revision."),
            });
        }
        view
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        self.0.operations(view)
    }

    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Self::Command>, DomainRejection> {
        self.0.compile_act(state, view, act)
    }

    fn command_policy(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> CommandPolicy {
        self.0.command_policy(state, command)
    }

    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), DomainRejection> {
        self.0.validate_command(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        self.0.receipts(events, locale)
    }

    fn build_interaction(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        self.0.build_interaction(state, view, requirement)
    }
}

/// The trip model, retargeted at the wrapper. The associated types are the
/// same, so every method is a plain delegation.
struct SensitiveModel(TripModel);

impl WorkflowModel<RevisionSensitive> for SensitiveModel {
    fn initial_states(&self) -> Vec<Option<TripState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TripState>) -> Vec<TripCommand> {
        self.0.candidate_commands(state)
    }

    fn simulate(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> SimulatedTransition<TripState, TripEvent> {
        self.0.simulate(state, command)
    }
}

struct OffersWhatItCannotCompile(TripWorkflow);

impl WorkflowDefinition for OffersWhatItCannotCompile {
    type State = TripState;
    type Phase = turnframe_test::workflows::trip::TripPhase;
    type Obligation = turnframe_test::workflows::trip::TripObligation;
    type Command = TripCommand;
    type Event = TripEvent;
    type Outcome = TripOutcome;

    fn key(&self) -> WorkflowKey {
        self.0.key()
    }

    fn version(&self) -> WorkflowVersion {
        self.0.version()
    }

    fn phase_ownership(&self, phase: &Self::Phase) -> PhaseOwnership {
        self.0.phase_ownership(phase)
    }

    fn project(&self, case_ref: CaseRef, state: Option<&Self::State>) -> ViewOf<Self> {
        self.0.project(case_ref, state)
    }

    /// One operation more than the compiler knows: the drift this reports.
    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        let mut offered = self.0.operations(view);
        if !offered.is_empty() {
            offered.push(
                OperationSpec::new("trip.forgotten")
                    .summary("Offered, and never wired to a command."),
            );
        }
        offered
    }

    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Self::Command>, DomainRejection> {
        self.0.compile_act(state, view, act)
    }

    fn command_policy(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> CommandPolicy {
        self.0.command_policy(state, command)
    }

    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), DomainRejection> {
        self.0.validate_command(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        self.0.receipts(events, locale)
    }

    fn build_interaction(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        self.0.build_interaction(state, view, requirement)
    }
}

/// The trip model, retargeted at the wrapper. The associated types are the
/// same, so every method is a plain delegation.
struct ForgottenModel(TripModel);

impl WorkflowModel<OffersWhatItCannotCompile> for ForgottenModel {
    fn initial_states(&self) -> Vec<Option<TripState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TripState>) -> Vec<TripCommand> {
        self.0.candidate_commands(state)
    }

    fn simulate(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> SimulatedTransition<TripState, TripEvent> {
        self.0.simulate(state, command)
    }
}

/// The control group for the compile check: a catalogue that offers one
/// operation more than the compiler knows.
///
/// This is the mistake as an adopter meets it — the declaration in one
/// function, the translation in another, and nothing relating them until now.
/// It fails as far downstream as a mistake can: the catalogue offers it, the
/// interpreter proposes it correctly with the right arguments, and the user is
/// told his request could not be carried out on a sentence that was understood
/// perfectly.
#[test]
fn a_catalogue_that_offers_what_it_cannot_compile_is_reported() {
    let report = explore(
        &OffersWhatItCannotCompile(TripWorkflow::default()),
        &ForgottenModel(TripModel::default()),
        ExplorationLimits::smoke(),
    );

    let offered: Vec<String> = report
        .violations
        .iter()
        .filter_map(|violation| match &violation.kind {
            ExplorationViolationKind::CatalogedOperationDoesNotCompile { operation } => {
                Some(operation.as_str().to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        offered
            .iter()
            .any(|operation| operation == "trip.forgotten"),
        "the operation nobody wired is named: {offered:?}"
    );
    assert!(
        !offered
            .iter()
            .any(|operation| operation.contains("set_name")),
        "and an operation that refuses because the explorer could not invent \
         its arguments is left alone: {offered:?}"
    );
}

#[test]
fn a_projection_that_reads_the_case_revision_is_reported() {
    let report = explore(
        &RevisionSensitive(TripWorkflow::default()),
        &SensitiveModel(TripModel::default()),
        ExplorationLimits::smoke(),
    );

    let varying: Vec<_> = report
        .violations
        .iter()
        .filter_map(|violation| match &violation.kind {
            ExplorationViolationKind::ProjectionVariesWithRevision {
                left,
                right,
                detail,
            } => Some((*left, *right, detail.clone())),
            _ => None,
        })
        .collect();

    assert!(
        !varying.is_empty(),
        "a projector that reads the revision must be caught: {}",
        report.describe()
    );
    let (left, right, detail) = &varying[0];
    assert_ne!(left, right, "the two revisions must be different");
    assert_ne!(
        left % 2,
        right % 2,
        "the table must pair revisions of different parity for this defect to show"
    );
    assert_eq!(detail, "notices");
    // The revisions come from the fixed table, not from the depth.
    assert!(EXPLORATION_REVISIONS.contains(left));
    assert!(EXPLORATION_REVISIONS.contains(right));
}

#[test]
fn the_sample_workflows_do_not_read_the_case_revision() {
    for report in [
        explore(
            &TripWorkflow::default(),
            &TripModel::default(),
            ExplorationLimits::generous(),
        ),
        explore(
            &TravelerWorkflow::new().with_cards(),
            &TravelerModel::of(TravelerWorkflow::new().with_cards()),
            ExplorationLimits::generous(),
        ),
        explore(
            &ClaimWorkflow::default(),
            &ClaimModel::default(),
            ExplorationLimits::generous(),
        ),
    ] {
        assert!(
            !report.violations.iter().any(|violation| matches!(
                violation.kind,
                ExplorationViolationKind::ProjectionVariesWithRevision { .. }
            )),
            "{}",
            report.describe()
        );
    }
}

#[test]
fn the_trip_model_fills_the_extra_budget_and_revisits_the_second_traveler() {
    let states: Vec<TripState> =
        reachable_states(&TripModel::default(), ExplorationLimits::generous())
            .into_iter()
            .flatten()
            .collect();

    let longest = states
        .iter()
        .map(|state| state.extras.len())
        .max()
        .unwrap_or(0);
    assert_eq!(
        longest, MAX_EXTRAS,
        "the model must reach the domain's own extra cap"
    );
    assert!(
        states.iter().any(|state| state.extras.len() == MAX_EXTRAS
            && state.extras.iter().all(|extra| extra.payer.is_some())),
        "every extra of a full trip must get a payer"
    );

    let with_second_traveler = states
        .iter()
        .filter(|state| {
            state
                .traveler
                .as_ref()
                .is_some_and(|traveler| traveler.traveler_id == OTHER_TRAVELER_ID)
        })
        .count();
    assert!(
        with_second_traveler > 1,
        "the second traveler must be exercised more than once, saw {with_second_traveler} state(s)"
    );
    // Bounded: three traveler values, four extras, a paid prefix, a quote and a kept leg.
    assert!(
        states.len() < 2_500,
        "the state space must stay small enough to explore fast, saw {}",
        states.len()
    );
}

#[test]
fn one_case_resolves_to_the_same_phase_in_two_projections() {
    let workflow = TripWorkflow::default();
    let registry = WorkflowRegistry::builder()
        .register(TripWorkflow::default(), TripExecutor::default())
        .build()
        .expect("one workflow, one key");
    let erased = &registry
        .require(&WorkflowKey::from("trip"))
        .expect("the trip workflow is registered")
        .definition;

    for state in [complete_case(), awaiting_rebooking_confirmation()] {
        let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(4));
        let typed = workflow.project(case_ref.clone(), Some(&state));
        let typed = typed
            .erase(workflow.phase_ownership(&typed.phase))
            .expect("the sample view is serializable");
        let json = serde_json::to_value(&state).expect("the sample state is serializable");
        let through_registry = erased
            .project(case_ref, Some(&json))
            .expect("the erased projection round-trips the state");

        same_phase_in(&typed, &through_registry)
            .expect("the typed and erased projections agree on the phase");
    }
}

#[test]
fn two_projections_that_disagree_about_the_phase_are_reported() {
    let workflow = TripWorkflow::default();
    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(4));
    let view = |state: &TripState| {
        let view = workflow.project(case_ref.clone(), Some(state));
        view.erase(workflow.phase_ownership(&view.phase))
            .expect("the sample view is serializable")
    };

    let collecting = view(&complete_case());
    let awaiting = view(&awaiting_rebooking_confirmation());
    assert!(matches!(
        same_phase_in(&collecting, &awaiting).unwrap_err(),
        AssertionFailure::PhaseDiffers { .. }
    ));

    let other_case = {
        let view = workflow.project(
            CaseRef::new("trip", "trip-2", CaseRevision(4)),
            Some(&complete_case()),
        );
        view.erase(workflow.phase_ownership(&view.phase))
            .expect("the sample view is serializable")
    };
    assert!(matches!(
        same_phase_in(&collecting, &other_case).unwrap_err(),
        AssertionFailure::DifferentCases { .. }
    ));
}

/// A projector that takes the opposite position: absence *is* the terminal
/// state, so a case whose row was deleted on completion projects as finished.
///
/// This is the shape an adopter arriving from a delete-on-completion design
/// writes, and nothing else in the kit notices it. The executor answers `None`
/// both for a case nobody has created and for a case that was consumed on
/// success, so one view has to serve both, and the assistant congratulates the
/// user and then offers them a brand-new empty draft.
struct EndsByDisappearing(TravelerWorkflow);

impl WorkflowDefinition for EndsByDisappearing {
    type State = TravelerState;
    type Phase = TravelerPhase;
    type Obligation = TravelerObligation;
    type Command = TravelerCommand;
    type Event = TravelerEvent;
    type Outcome = TravelerOutcome;

    fn key(&self) -> WorkflowKey {
        self.0.key()
    }

    fn version(&self) -> WorkflowVersion {
        self.0.version()
    }

    fn phase_ownership(&self, phase: &Self::Phase) -> PhaseOwnership {
        self.0.phase_ownership(phase)
    }

    fn project(&self, case_ref: CaseRef, state: Option<&Self::State>) -> ViewOf<Self> {
        if state.is_none() {
            return WorkflowView::new(case_ref, self.version(), TravelerPhase::Deleted)
                .with_outcome(TravelerOutcome::Deleted);
        }
        self.0.project(case_ref, state)
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        self.0.operations(view)
    }

    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Self::Command>, DomainRejection> {
        self.0.compile_act(state, view, act)
    }

    fn command_policy(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> CommandPolicy {
        self.0.command_policy(state, command)
    }

    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), DomainRejection> {
        self.0.validate_command(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        self.0.receipts(events, locale)
    }

    fn build_interaction(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        self.0.build_interaction(state, view, requirement)
    }
}

/// The traveler model, retargeted at the wrapper. The associated types are the
/// same, so every method is a plain delegation.
struct DisappearingModel(TravelerModel);

impl WorkflowModel<EndsByDisappearing> for DisappearingModel {
    fn initial_states(&self) -> Vec<Option<TravelerState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TravelerState>) -> Vec<TravelerCommand> {
        self.0.candidate_commands(state)
    }

    fn simulate(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> SimulatedTransition<TravelerState, TravelerEvent> {
        self.0.simulate(state, command)
    }

    fn declared_outcomes(&self) -> Vec<TravelerOutcome> {
        self.0.declared_outcomes()
    }
}

#[test]
fn a_projector_that_ends_a_case_by_disappearing_is_reported() {
    let report = explore(
        &EndsByDisappearing(TravelerWorkflow::new().with_cards()),
        &DisappearingModel(TravelerModel::of(TravelerWorkflow::new().with_cards())),
        ExplorationLimits::generous(),
    );

    let disappearing: Vec<_> = report
        .violations
        .iter()
        .filter(|violation| {
            matches!(
                violation.kind,
                ExplorationViolationKind::CaseEndsByDisappearing { .. }
            )
        })
        .collect();
    assert_eq!(
        disappearing.len(),
        1,
        "the absent state is visited once, and projecting it as finished must be reported: {}",
        report.describe()
    );
    let violation = disappearing[0];
    assert!(
        violation.path.is_empty() && violation.state.is_null(),
        "the offending state is the absent one, reached without a single command: {}",
        violation.describe()
    );
    let ExplorationViolationKind::CaseEndsByDisappearing { phase, outcome } = &violation.kind
    else {
        unreachable!("filtered above");
    };
    assert_eq!(phase, &serde_json::json!("deleted"));
    assert_eq!(outcome.as_ref(), Some(&serde_json::json!("deleted")));
    // The rule is about the absent state alone: every state that exists still
    // projects exactly as the sample does.
    assert!(
        !report.violations.iter().any(|other| !matches!(
            other.kind,
            ExplorationViolationKind::CaseEndsByDisappearing { .. }
        )),
        "only the absent state should be at fault: {}",
        report.describe()
    );
}

/// The traveler model with one change: deleting drops the case instead of
/// moving it to the `Deleted` status.
struct DeletesTheRow(TravelerModel);

impl WorkflowModel<TravelerWorkflow> for DeletesTheRow {
    fn initial_states(&self) -> Vec<Option<TravelerState>> {
        self.0.initial_states()
    }

    fn candidate_commands(&self, state: Option<&TravelerState>) -> Vec<TravelerCommand> {
        self.0.candidate_commands(state)
    }

    fn simulate(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> SimulatedTransition<TravelerState, TravelerEvent> {
        let transition = self.0.simulate(state, command);
        match (command, transition.is_applied()) {
            (TravelerCommand::Delete, true) => {
                SimulatedTransition::removed(transition.events().to_vec())
            }
            _ => transition,
        }
    }

    fn declared_outcomes(&self) -> Vec<TravelerOutcome> {
        self.0.declared_outcomes()
    }
}

#[test]
fn a_transition_that_drops_the_case_is_reported() {
    let report = explore(
        &TravelerWorkflow::new().with_cards(),
        &DeletesTheRow(TravelerModel::of(TravelerWorkflow::new().with_cards())),
        ExplorationLimits::generous(),
    );

    assert!(!report.truncated, "{}", report.describe());
    let removals: Vec<_> = report
        .violations
        .iter()
        .filter_map(|violation| match &violation.kind {
            ExplorationViolationKind::TransitionRemovesCase { command_type, .. } => {
                Some(command_type.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        !removals.is_empty(),
        "a model that deletes the row must be reported: {}",
        report.describe()
    );
    assert!(
        removals.iter().all(|command_type| command_type == "delete"),
        "only deleting drops the case, saw {removals:?}"
    );
    // And the second cost of deleting the row, which the first check cannot
    // show: the terminal outcome the domain declares is no longer reachable,
    // because no state is left to project it.
    assert!(!report.reached_outcome(&TravelerOutcome::Deleted));
    assert!(
        report.violations.iter().any(|violation| matches!(
            &violation.kind,
            ExplorationViolationKind::UnreachableOutcome { outcome }
                if outcome == &serde_json::json!("deleted")
        )),
        "{}",
        report.describe()
    );
    assert!(report.reached_outcome(&TravelerOutcome::Archived));
}

#[test]
fn no_sample_workflow_ends_a_case_by_disappearing() {
    for report in [
        explore(
            &TripWorkflow::default(),
            &TripModel::default(),
            ExplorationLimits::generous(),
        ),
        explore(
            &TravelerWorkflow::new().with_cards(),
            &TravelerModel::of(TravelerWorkflow::new().with_cards()),
            ExplorationLimits::generous(),
        ),
        explore(
            &ClaimWorkflow::default(),
            &ClaimModel::default(),
            ExplorationLimits::generous(),
        ),
    ] {
        assert!(
            !report.violations.iter().any(|violation| matches!(
                violation.kind,
                ExplorationViolationKind::CaseEndsByDisappearing { .. }
                    | ExplorationViolationKind::TransitionRemovesCase { .. }
            )),
            "{}",
            report.describe()
        );
    }
}
