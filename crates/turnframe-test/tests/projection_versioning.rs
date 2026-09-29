//! A projector cannot change what it emits without its version changing.
//!
//! The state explorer already asks whether a projection is internally coherent:
//! one phase, obligations that are unique and stable, a card exactly where the
//! user owns the step. It does not ask whether the projection is the *same one*
//! the declared version promised, and nothing else did either. So a projector
//! could start reporting a different phase, drop an obligation or stop raising
//! a card, keep its version, and the whole suite would stay green.
//!
//! That is not a tidiness problem. A replay record cites the workflow version
//! to explain why a past turn decided what it decided, and an evaluation
//! baseline compares two runs on the assumption that a version pins a
//! behaviour. A projector that moves under a fixed version makes both quietly
//! untrue.
//!
//! Each test below snapshots a fingerprint named after the version it pins. A
//! projector changed without a bump fails against the file that already exists.
//! A projector changed *with* a bump writes a new file beside the old one, so
//! the previous shape stays on record and the change is reviewed rather than
//! absorbed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_test::projection::ProjectionFingerprint;
use turnframe_test::workflows::{claim, traveler, trip};

#[test]
fn the_trip_projector_emits_what_its_version_promised() {
    let workflow = trip::TripWorkflow::default();
    let fingerprint = ProjectionFingerprint::of(&workflow)
        .at("absent", None)
        .at("incomplete_case", Some(trip::incomplete_case()))
        .at("complete_case", Some(trip::complete_case()))
        .at(
            "awaiting_rebooking_confirmation",
            Some(trip::awaiting_rebooking_confirmation()),
        )
        .build();

    insta::assert_yaml_snapshot!(fingerprint.snapshot_name(), fingerprint);
}

#[test]
fn the_traveler_projector_emits_what_its_version_promised() {
    for workflow in [
        traveler::TravelerWorkflow::default(),
        traveler::TravelerWorkflow::new().with_cards(),
    ] {
        pin_traveler(&workflow);
    }
}

fn pin_traveler(workflow: &traveler::TravelerWorkflow) {
    let fingerprint = ProjectionFingerprint::of(workflow)
        .at("absent", None)
        .at("incomplete_draft", Some(traveler::incomplete_draft()))
        .at(
            "declined_because_it_does_not_apply",
            Some(traveler::declined_loyalty_number(
                traveler::DeclineReason::NotApplicable,
            )),
        )
        .at(
            "declined_because_it_is_not_known",
            Some(traveler::declined_loyalty_number(
                traveler::DeclineReason::Unknown,
            )),
        )
        .build();

    insta::assert_yaml_snapshot!(fingerprint.snapshot_name(), fingerprint);
}

#[test]
fn the_claim_projector_emits_what_its_version_promised() {
    let workflow = claim::ClaimWorkflow::default();
    let fingerprint = ProjectionFingerprint::of(&workflow)
        .at("absent", None)
        .at("awaiting_extraction", Some(claim::awaiting_extraction()))
        .at(
            "under_review",
            Some(claim::under_review(claim::complete_proposal())),
        )
        .at(
            "under_review_incomplete",
            Some(claim::under_review(claim::partial_proposal())),
        )
        .at("abandoned", Some(claim::abandoned()))
        .build();

    insta::assert_yaml_snapshot!(fingerprint.snapshot_name(), fingerprint);
}

/// The mechanism itself, rather than the projectors it guards.
///
/// A fingerprint is worth nothing unless a real change moves it and a cosmetic
/// one does not. Both halves are asserted here, because a pin that never moves
/// is indistinguishable from a pin that is not attached.
mod the_pin_itself {
    use super::ProjectionFingerprint;
    use turnframe_core::case::CaseRef;
    use turnframe_core::flow::{PhaseOwnership, WorkflowDefinition, WorkflowNotice, WorkflowView};
    use turnframe_core::ids::{WorkflowKey, WorkflowVersion};
    use turnframe_core::locale::LocalizedText;
    use turnframe_core::response::NoticeSeverity;
    use turnframe_test::workflows::{traveler, trip};

    /// A projector whose behaviour is switched by a flag, standing in for an
    /// edit somebody makes to a real one.
    #[derive(Debug)]
    struct Switchable {
        /// When true, the projector reports a second obligation it did not
        /// report before: a behaviour change.
        extra_obligation: bool,
        /// When true, the notice says something else in the same code: a
        /// wording change and nothing more.
        reworded: bool,
        /// The version it claims, which an honest edit would bump.
        version: &'static str,
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct State;

    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    enum Phase {
        Collecting,
    }

    #[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
    enum Obligation {
        First,
        Second,
    }

    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    enum Outcome {}

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    enum Command {}

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    enum Event {}

    impl WorkflowDefinition for Switchable {
        type State = State;
        type Phase = Phase;
        type Obligation = Obligation;
        type Command = Command;
        type Event = Event;
        type Outcome = Outcome;

        fn key(&self) -> WorkflowKey {
            WorkflowKey::from("switchable")
        }

        fn version(&self) -> WorkflowVersion {
            WorkflowVersion::from(self.version)
        }

        fn phase_ownership(&self, _phase: &Self::Phase) -> PhaseOwnership {
            PhaseOwnership::System
        }

        fn project(
            &self,
            case_ref: CaseRef,
            _state: Option<&Self::State>,
        ) -> WorkflowView<Self::Phase, Self::Obligation, Self::Outcome> {
            let mut obligations = vec![Obligation::First];
            if self.extra_obligation {
                obligations.push(Obligation::Second);
            }
            let text = if self.reworded {
                "Rephrased, meaning unchanged."
            } else {
                "Original wording."
            };
            WorkflowView {
                case_ref,
                workflow_version: self.version(),
                phase: Phase::Collecting,
                obligations,
                blocking_interaction: None,
                notices: vec![WorkflowNotice {
                    code: String::from("switchable.note"),
                    severity: NoticeSeverity::Info,
                    text: LocalizedText::new(text),
                }],
                outcome: None,
            }
        }

        // The five below exist so the type is a definition at all. This
        // projector is only ever projected, never interpreted or executed, and
        // its command and event types are uninhabited, so each body is the only
        // one that can be written rather than a stub standing in for something.

        fn operations(
            &self,
            _view: &WorkflowView<Self::Phase, Self::Obligation, Self::Outcome>,
        ) -> Vec<turnframe_core::operation::OperationSpec> {
            Vec::new()
        }

        fn compile_act(
            &self,
            _state: Option<&Self::State>,
            _view: &WorkflowView<Self::Phase, Self::Obligation, Self::Outcome>,
            _act: &turnframe_core::target::ResolvedAct,
        ) -> Result<Vec<Self::Command>, turnframe_core::error::DomainRejection> {
            Ok(Vec::new())
        }

        fn command_policy(
            &self,
            _state: Option<&Self::State>,
            command: &Self::Command,
        ) -> turnframe_core::command::CommandPolicy {
            match *command {}
        }

        fn validate_command(
            &self,
            _state: Option<&Self::State>,
            command: &Self::Command,
        ) -> Result<(), turnframe_core::error::DomainRejection> {
            match *command {}
        }

        fn receipts(
            &self,
            _events: &[turnframe_core::event::ReceiptEvent<Self::Event>],
            _locale: &turnframe_core::locale::Locale,
        ) -> Vec<turnframe_core::event::OperationalReceipt> {
            Vec::new()
        }
    }

    fn fingerprint(definition: &Switchable) -> ProjectionFingerprint {
        ProjectionFingerprint::of(definition)
            .at("only", Some(State))
            .build()
    }

    #[test]
    fn a_behaviour_change_under_a_fixed_version_moves_the_pin() {
        let before = fingerprint(&Switchable {
            extra_obligation: false,
            reworded: false,
            version: "1",
        });
        let after = fingerprint(&Switchable {
            extra_obligation: true,
            reworded: false,
            version: "1",
        });

        assert_eq!(
            before.snapshot_name(),
            after.snapshot_name(),
            "the version did not move, so both would be compared against one snapshot"
        );
        assert_ne!(
            before, after,
            "an obligation appeared: the recorded snapshot must fail"
        );
    }

    #[test]
    fn a_rewording_does_not_move_the_pin() {
        let before = fingerprint(&Switchable {
            extra_obligation: false,
            reworded: false,
            version: "1",
        });
        let after = fingerprint(&Switchable {
            extra_obligation: false,
            reworded: true,
            version: "1",
        });

        assert_eq!(
            before, after,
            "copy is not contract: a pin that failed on wording would be silenced"
        );
    }

    #[test]
    fn bumping_the_version_writes_a_new_snapshot_rather_than_overwriting() {
        let before = fingerprint(&Switchable {
            extra_obligation: false,
            reworded: false,
            version: "1",
        });
        let after = fingerprint(&Switchable {
            extra_obligation: true,
            reworded: false,
            version: "2",
        });

        assert_ne!(
            before.snapshot_name(),
            after.snapshot_name(),
            "a bumped version names a different snapshot file, so the old shape stays on record"
        );
    }

    #[test]
    fn every_sample_projector_pins_under_its_own_name() {
        let trip_workflow = trip::TripWorkflow::default();
        let traveler_workflow = traveler::TravelerWorkflow::default();
        let trip = ProjectionFingerprint::of(&trip_workflow)
            .at("absent", None)
            .build();
        let traveler = ProjectionFingerprint::of(&traveler_workflow)
            .at("absent", None)
            .build();

        assert_eq!(trip.snapshot_name(), "trip@1");
        assert_eq!(traveler.snapshot_name(), "traveler@1");
        assert_ne!(
            trip.snapshot_name(),
            traveler.snapshot_name(),
            "two projectors must never share a snapshot"
        );
    }
}
