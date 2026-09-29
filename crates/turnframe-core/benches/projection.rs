//! Projection and invariant checking (spec §28).
//!
//! Spec §28 asks for pure projection time to be measured on its own, and says
//! no public performance claim may be made until it is. These two benchmarks
//! are the projection half.
//!
//! They are deliberately separate because they cost different things.
//! [`WorkflowDefinition::project`] is the pure function a turn runs before
//! anything else: state in, view out, no input/output and no allocation beyond
//! the obligations it lists. [`check_view`] is what the runtime pays to *trust*
//! that view — it erases the view, which canonicalizes and hashes every
//! obligation, so its cost grows with the obligation count while projection's
//! barely does. Measuring them together would hide that.
//!
//! # Why the workflow is defined here
//!
//! A realistic shape matters: a flat two-obligation domain would measure
//! nothing. This is a trip modelled on the sample domain in
//! `turnframe-test` — several phases, simultaneous obligations, a
//! *parameterized* obligation per extra with no payer, a blocking rebooking
//! confirmation with a payload, and a notice on the external phases.
//!
//! It is copied rather than imported because `turnframe-test` depends on
//! `turnframe-core`: a dev-dependency the other way is a cycle Cargo tolerates
//! but the publication order in `docs/release-checklist.md` does not, and it
//! would drag the store and provider crates into every `cargo bench -p
//! turnframe-core`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseRef;
use turnframe_core::command::CommandPolicy;
use turnframe_core::error::DomainRejection;
use turnframe_core::event::{OperationalReceipt, ReceiptEvent};
use turnframe_core::flow::{
    InteractionRequirement, PhaseOwnership, ViewOf, WorkflowDefinition, WorkflowNotice,
    WorkflowView, check_view,
};
use turnframe_core::ids::{CaseRevision, OperationKey, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::{
    InteractionKind, InteractionOption, InteractionPayload, StoredInteractionAction,
};
use turnframe_core::locale::Locale;
use turnframe_core::operation::OperationSpec;
use turnframe_core::response::NoticeSeverity;
use turnframe_core::target::ResolvedAct;

// ---------------------------------------------------------------------------
// The domain.
// ---------------------------------------------------------------------------

/// One extra of the trip. An extra without a payer owes an obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Extra {
    extra_id: String,
    description: String,
    amount_cents: i64,
    payer: Option<String>,
}

/// Persisted state of one trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TripState {
    traveler: Option<String>,
    name: Option<String>,
    travel_date: Option<String>,
    extras: Vec<Extra>,
    confirmation_requested: bool,
    rebooking_sent: bool,
    notified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TripPhase {
    Collecting,
    AwaitingRebookingConfirmation,
    Rebooking,
    Notified,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TripObligation {
    SelectTraveler,
    AddAtLeastOneExtra,
    AssignPayer { extra_id: String },
    SetName,
    SetTravelDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TripCommand {
    Rebook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TripEvent {
    RebookingSent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TripOutcome {
    Notified,
}

#[derive(Debug, Default, Clone, Copy)]
struct TripWorkflow;

impl TripState {
    /// Everything still missing, in a fixed order.
    fn open_obligations(&self) -> Vec<TripObligation> {
        if self.rebooking_sent {
            return Vec::new();
        }
        let mut obligations = Vec::new();
        if self.traveler.is_none() {
            obligations.push(TripObligation::SelectTraveler);
        }
        if self.extras.is_empty() {
            obligations.push(TripObligation::AddAtLeastOneExtra);
        }
        for extra in &self.extras {
            if extra.payer.is_none() {
                obligations.push(TripObligation::AssignPayer {
                    extra_id: extra.extra_id.clone(),
                });
            }
        }
        if self.name.is_none() {
            obligations.push(TripObligation::SetName);
        }
        if self.travel_date.is_none() {
            obligations.push(TripObligation::SetTravelDate);
        }
        obligations
    }

    fn phase(&self) -> TripPhase {
        if self.notified {
            TripPhase::Notified
        } else if self.rebooking_sent {
            TripPhase::Rebooking
        } else if self.confirmation_requested && self.open_obligations().is_empty() {
            TripPhase::AwaitingRebookingConfirmation
        } else {
            TripPhase::Collecting
        }
    }
}

impl WorkflowDefinition for TripWorkflow {
    type State = TripState;
    type Phase = TripPhase;
    type Obligation = TripObligation;
    type Command = TripCommand;
    type Event = TripEvent;
    type Outcome = TripOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("trip")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &TripPhase) -> PhaseOwnership {
        match phase {
            TripPhase::AwaitingRebookingConfirmation => PhaseOwnership::User,
            TripPhase::Collecting | TripPhase::Rebooking => PhaseOwnership::System,
            TripPhase::Notified => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&TripState>) -> ViewOf<Self> {
        let version = self.version();
        let Some(state) = state else {
            return WorkflowView::new(case_ref, version, TripPhase::Collecting)
                .with_obligations([TripObligation::SelectTraveler]);
        };
        let phase = state.phase();
        let view =
            WorkflowView::new(case_ref, version, phase).with_obligations(state.open_obligations());
        match phase {
            TripPhase::AwaitingRebookingConfirmation => view.with_blocking_interaction(
                InteractionRequirement::blocking(
                    "rebooking_confirmation",
                    InteractionKind::Boolean,
                )
                .with_payload(
                    InteractionPayload::new("Rebook this flight?")
                        .with_body(state.name.clone().unwrap_or_else(|| "—".to_owned()))
                        .with_option(InteractionOption::new(
                            "confirm",
                            "Rebook",
                            StoredInteractionAction::ApplyOperation {
                                operation: OperationKey::from("trip.rebook"),
                                arguments: serde_json::Value::Null,
                                freeform_argument: None,
                            },
                        ))
                        .with_option(InteractionOption::new(
                            "decline",
                            "Keep my flight",
                            StoredInteractionAction::Dismiss,
                        )),
                ),
            ),
            TripPhase::Rebooking => view.with_notice(WorkflowNotice {
                code: "trip.awaiting_authority".to_owned(),
                severity: NoticeSeverity::Info,
                text: "Waiting for the airline to answer.".into(),
            }),
            TripPhase::Notified => view.with_outcome(TripOutcome::Notified),
            TripPhase::Collecting => view,
        }
    }

    fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
        Vec::new()
    }

    fn compile_act(
        &self,
        _state: Option<&TripState>,
        _view: &ViewOf<Self>,
        _act: &ResolvedAct,
    ) -> Result<Vec<TripCommand>, DomainRejection> {
        Err(DomainRejection::new(
            "trip.not_benchmarked",
            "trip.not_benchmarked",
        ))
    }

    fn command_policy(&self, _state: Option<&TripState>, _command: &TripCommand) -> CommandPolicy {
        CommandPolicy::conservative()
    }

    fn validate_command(
        &self,
        _state: Option<&TripState>,
        _command: &TripCommand,
    ) -> Result<(), DomainRejection> {
        Ok(())
    }

    fn receipts(
        &self,
        _events: &[ReceiptEvent<TripEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Fixtures. Built once, outside every measured closure.
// ---------------------------------------------------------------------------

/// A draft with `extras` extras, of which the first `unassigned` still owe a
/// payer: the parameterized obligation that makes a view grow.
fn collecting(extras: usize, unassigned: usize) -> TripState {
    TripState {
        traveler: Some("trav_00001".to_owned()),
        name: Some("Lisbon offsite".to_owned()),
        travel_date: None,
        extras: (0..extras)
            .map(|index| Extra {
                extra_id: format!("extra_{index:04}"),
                description: format!("Hotel night {index}"),
                amount_cents: 12_500 + index as i64,
                payer: (index >= unassigned).then(|| "company".to_owned()),
            })
            .collect(),
        confirmation_requested: false,
        rebooking_sent: false,
        notified: false,
    }
}

/// A complete draft parked on the blocking rebooking card.
fn awaiting_confirmation() -> TripState {
    let mut state = collecting(8, 0);
    state.travel_date = Some("2026-11-30".to_owned());
    state.confirmation_requested = true;
    state
}

fn case_ref() -> CaseRef {
    CaseRef::new("trip", "trip-000042", CaseRevision(17))
}

// ---------------------------------------------------------------------------
// Benchmarks.
// ---------------------------------------------------------------------------

fn projection(c: &mut Criterion) {
    let workflow = TripWorkflow;
    let case_ref = case_ref();

    let mut group = c.benchmark_group("projection/project");
    for (label, state) in [
        ("empty_case", None),
        ("collecting_3_obligations", Some(collecting(4, 1))),
        ("collecting_33_obligations", Some(collecting(64, 32))),
        ("awaiting_confirmation", Some(awaiting_confirmation())),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(label), &state, |b, state| {
            b.iter(|| {
                black_box(workflow.project(black_box(case_ref.clone()), black_box(state.as_ref())))
            });
        });
    }
    group.finish();
}

fn invariants(c: &mut Criterion) {
    let workflow = TripWorkflow;
    let case_ref = case_ref();

    // Projecting is not part of what this measures, so the views are built up
    // front. The cost here is erasure: canonical JSON plus a BLAKE3 digest for
    // every obligation, which is why the row count is the parameter.
    let views: Vec<(&str, ViewOf<TripWorkflow>)> = vec![
        (
            "collecting_3_obligations",
            workflow.project(case_ref.clone(), Some(&collecting(4, 1))),
        ),
        (
            "collecting_33_obligations",
            workflow.project(case_ref.clone(), Some(&collecting(64, 32))),
        ),
        (
            "awaiting_confirmation",
            workflow.project(case_ref, Some(&awaiting_confirmation())),
        ),
    ];

    let mut group = c.benchmark_group("projection/check_view");
    for (label, view) in &views {
        assert!(
            check_view(&workflow, view).is_ok(),
            "{label} must project a legal view"
        );
        group.bench_with_input(BenchmarkId::from_parameter(label), view, |b, view| {
            b.iter(|| black_box(check_view(black_box(&workflow), black_box(view))).is_ok());
        });
    }
    group.finish();
}

criterion_group!(benches, projection, invariants);
criterion_main!(benches);
