//! Reusable checks over the artefacts a turn produces.
//!
//! Every helper returns a typed [`AssertionFailure`] instead of panicking, so
//! it composes inside a proptest, inside an exploration model, or inside a
//! production self-check. Call sites in `#[test]` code usually end in
//! `.expect(...)`; call sites in library code can branch on the failure.
//!
//! The checks cover the properties spec §27.2 asks for by name:
//!
//! * no high-risk command executes without a trusted origin (I12);
//! * no receipt claims an outcome the ledger does not back (I16);
//! * a projection carries exactly one phase (§8.4);
//! * one case resolves to the same phase in two projections, which is what "one
//!   phase" means for a case rather than for a view (I3);
//! * two renderings of the same turn produce the same ordered blocks, which is
//!   what "reload returns the same blocks" means (§27.4, scenario 18).

use turnframe_core::command::{
    CommandBatch, CommandEnvelope, CommandPolicy, ConfirmationPolicy, RiskClass, origin_satisfies,
};
use turnframe_core::event::{OperationalReceipt, ReceiptEvent};
use turnframe_core::flow::{ErasedWorkflowView, PhaseOwnership, WorkflowDefinition};
use turnframe_core::ids::{CaseId, CommandId, EventId, ReceiptId, WorkflowKey};
use turnframe_core::response::AssistantTurn;

/// A check did not hold.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AssertionFailure {
    /// A command whose policy demands a trusted origin carried an untrusted one.
    #[error(
        "command {command_id} needs a trusted origin (risk {risk:?}, confirmation {confirmation:?}) but its origin is untrusted"
    )]
    UntrustedOriginForHighRisk {
        /// The offending command.
        command_id: CommandId,
        /// Its risk class.
        risk: RiskClass,
        /// Its confirmation policy.
        confirmation: ConfirmationPolicy,
    },
    /// A receipt states an outcome with nothing in the ledger behind it.
    #[error("receipt {receipt_id} ({status_code}) is not backed by any committed event")]
    ReceiptWithoutEvents {
        /// The offending receipt.
        receipt_id: ReceiptId,
        /// Its status code.
        status_code: String,
    },
    /// A receipt cites an event the commit did not produce.
    #[error("receipt {receipt_id} ({status_code}) cites event {event_id}, which was not committed")]
    ReceiptCitesUnknownEvent {
        /// The offending receipt.
        receipt_id: ReceiptId,
        /// Its status code.
        status_code: String,
        /// The event it invented.
        event_id: EventId,
    },
    /// A projection does not carry exactly one phase.
    #[error("the view of {workflow}/{case_id} does not carry exactly one phase")]
    NotASinglePhase {
        /// The workflow.
        workflow: WorkflowKey,
        /// The case.
        case_id: CaseId,
    },
    /// Two views that should be of the same case are not.
    #[error("the views are of {left_workflow}/{left_case} and {right_workflow}/{right_case}")]
    DifferentCases {
        /// Workflow of the first view.
        left_workflow: WorkflowKey,
        /// Case of the first view.
        left_case: CaseId,
        /// Workflow of the second view.
        right_workflow: WorkflowKey,
        /// Case of the second view.
        right_case: CaseId,
    },
    /// One case resolved to two different phases.
    #[error("{workflow}/{case_id} is in phase {left} in one projection and {right} in the other")]
    PhaseDiffers {
        /// The workflow.
        workflow: WorkflowKey,
        /// The case.
        case_id: CaseId,
        /// Phase of the first view, as canonical JSON.
        left: String,
        /// Phase of the second view, as canonical JSON.
        right: String,
    },
    /// One case resolved to two different phase ownerships.
    #[error(
        "{workflow}/{case_id} is owned by {left:?} in one projection and {right:?} in the other"
    )]
    PhaseOwnershipDiffers {
        /// The workflow.
        workflow: WorkflowKey,
        /// The case.
        case_id: CaseId,
        /// Ownership in the first view.
        left: PhaseOwnership,
        /// Ownership in the second view.
        right: PhaseOwnership,
    },
    /// Two renderings of the same turn have different numbers of blocks.
    #[error("the turns have {left} and {right} blocks")]
    BlockCountDiffers {
        /// Blocks in the first turn.
        left: usize,
        /// Blocks in the second turn.
        right: usize,
    },
    /// Two renderings of the same turn differ at one position.
    #[error("the turns differ at block {index}")]
    BlocksDiffer {
        /// Position of the first difference.
        index: usize,
    },
}

/// Checks one envelope against one policy (I12).
pub fn origin_satisfies_policy<C>(
    envelope: &CommandEnvelope<C>,
    policy: &CommandPolicy,
) -> Result<(), AssertionFailure> {
    if origin_satisfies(&envelope.origin, policy) {
        return Ok(());
    }
    Err(AssertionFailure::UntrustedOriginForHighRisk {
        command_id: envelope.command_id,
        risk: policy.risk,
        confirmation: policy.confirmation,
    })
}

/// Checks that no command in `batch` executes above
/// [`RiskClass::ReversibleLowRisk`], or under any confirmation policy, without
/// a trusted origin.
///
/// The policy comes from the workflow itself, so the check follows the domain's
/// own classification rather than a duplicate table in the test.
pub fn no_high_risk_without_trusted_origin<W: WorkflowDefinition>(
    definition: &W,
    state: Option<&W::State>,
    batch: &CommandBatch<W::Command>,
) -> Result<(), AssertionFailure> {
    for envelope in &batch.envelopes {
        let policy = definition.command_policy(state, &envelope.command);
        origin_satisfies_policy(envelope, &policy)?;
    }
    Ok(())
}

/// Checks that every receipt cites at least one event and only events the
/// commit really produced (I16).
///
/// It takes [`ReceiptEvent`]s — what [`WorkflowDefinition::receipts`] itself is
/// given — so a receipt rendered over an event whose payload was erased is
/// checked by exactly the same rule as any other. It has to be: an erased event
/// is still in the ledger and still authorizes the claim, and a helper that
/// quietly accepted an unbacked receipt for it would hide the one case where
/// the domain had least to go on. Build the list from a commit with
/// [`Commit::receipt_events`], or from the ledger with
/// `StoredEvent::to_receipt_event`.
///
/// [`Commit::receipt_events`]: turnframe_core::event::Commit::receipt_events
pub fn receipts_backed_by_events<E>(
    receipts: &[OperationalReceipt],
    events: &[ReceiptEvent<E>],
) -> Result<(), AssertionFailure> {
    for receipt in receipts {
        if receipt.event_ids.is_empty() {
            return Err(AssertionFailure::ReceiptWithoutEvents {
                receipt_id: receipt.receipt_id,
                status_code: receipt.status_code.clone(),
            });
        }
        for event_id in &receipt.event_ids {
            if !events.iter().any(|event| event.event_id() == *event_id) {
                return Err(AssertionFailure::ReceiptCitesUnknownEvent {
                    receipt_id: receipt.receipt_id,
                    status_code: receipt.status_code.clone(),
                    event_id: *event_id,
                });
            }
        }
    }
    Ok(())
}

/// Checks that a projection carries exactly one phase (§8.4).
///
/// "Exactly one" is structural in the typed view — `phase` is a single value by
/// type — so what is left to check on the erased form is that the phase is a
/// real value and not a null or a list of concurrent phases smuggled through
/// the serializer.
pub fn single_phase(view: &ErasedWorkflowView) -> Result<(), AssertionFailure> {
    if view.phase.is_null() || view.phase.is_array() {
        return Err(AssertionFailure::NotASinglePhase {
            workflow: view.case_ref.workflow.clone(),
            case_id: view.case_ref.case_id.clone(),
        });
    }
    Ok(())
}

/// Checks that one case resolves to the same phase in two projections (I3).
///
/// "Exactly one phase" is a statement about a *case*, not about a view: a case
/// that is `Collecting` when the typed workflow projects it and `Dispatching`
/// when the erased registry projects the same state has two phases, however
/// well each view checks out on its own. That happens for real — an erased
/// projection deserializes the state through a second serde path, a second
/// projector is written for a read model, a cached view survives a version
/// bump — and none of the single-view invariants can see it.
///
/// The two views must be of the same case; a phase comparison across cases
/// would be meaningless, so it is refused rather than answered.
pub fn same_phase_in(
    left: &ErasedWorkflowView,
    right: &ErasedWorkflowView,
) -> Result<(), AssertionFailure> {
    if left.case_ref.key() != right.case_ref.key() {
        return Err(AssertionFailure::DifferentCases {
            left_workflow: left.case_ref.workflow.clone(),
            left_case: left.case_ref.case_id.clone(),
            right_workflow: right.case_ref.workflow.clone(),
            right_case: right.case_ref.case_id.clone(),
        });
    }
    single_phase(left)?;
    single_phase(right)?;
    if left.phase != right.phase {
        return Err(AssertionFailure::PhaseDiffers {
            workflow: left.case_ref.workflow.clone(),
            case_id: left.case_ref.case_id.clone(),
            left: left.phase.to_string(),
            right: right.phase.to_string(),
        });
    }
    if left.phase_ownership != right.phase_ownership {
        return Err(AssertionFailure::PhaseOwnershipDiffers {
            workflow: left.case_ref.workflow.clone(),
            case_id: left.case_ref.case_id.clone(),
            left: left.phase_ownership,
            right: right.phase_ownership,
        });
    }
    Ok(())
}

/// Checks that two assistant turns carry identical blocks in identical order.
///
/// This is the reload property: persisting a turn and reading it back, or
/// regenerating a narration after a provider failure, must not reshuffle,
/// duplicate or drop a block (spec §27.4, scenario 18).
pub fn identical_blocks(
    left: &AssistantTurn,
    right: &AssistantTurn,
) -> Result<(), AssertionFailure> {
    if left.blocks.len() != right.blocks.len() {
        return Err(AssertionFailure::BlockCountDiffers {
            left: left.blocks.len(),
            right: right.blocks.len(),
        });
    }
    for (index, (a, b)) in left.blocks.iter().zip(&right.blocks).enumerate() {
        if a != b {
            return Err(AssertionFailure::BlocksDiffer { index });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use turnframe_core::event::{OperationalReceipt, ReceiptSeverity};
    use turnframe_core::ids::{BlockId, ConversationId, EventId, ReceiptId, TurnId};
    use turnframe_core::locale::LocalizedText;
    use turnframe_core::response::{
        AssistantTurn, GeneratedTransition, ReplayToken, ResponseBlock,
    };

    use super::*;

    fn receipt(event_ids: Vec<EventId>) -> OperationalReceipt {
        OperationalReceipt {
            receipt_id: ReceiptId::derive(&event_ids, "trip.rebooking_sent"),
            event_ids,
            severity: ReceiptSeverity::Success,
            title: LocalizedText::new("Sent"),
            body: LocalizedText::new("The rebooking was sent."),
            status_code: "trip.rebooking_sent".to_owned(),
            artifact_refs: Vec::new(),
        }
    }

    fn turn(text: &str) -> AssistantTurn {
        AssistantTurn {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            blocks: vec![ResponseBlock::Transition(GeneratedTransition {
                block_id: BlockId::from("t1"),
                text: text.to_owned(),
                facts_used: Vec::new(),
            })],
            replay_token: ReplayToken::from("rt"),
            subjects: Vec::new(),
            expectations: Vec::new(),
            done: Vec::new(),
        }
    }

    #[test]
    fn a_receipt_with_no_events_is_refused() {
        let failure = receipts_backed_by_events::<()>(&[receipt(Vec::new())], &[]).unwrap_err();
        assert!(matches!(
            failure,
            AssertionFailure::ReceiptWithoutEvents { .. }
        ));
    }

    #[test]
    fn a_receipt_citing_an_uncommitted_event_is_refused() {
        let failure =
            receipts_backed_by_events::<()>(&[receipt(vec![EventId::nil()])], &[]).unwrap_err();
        assert!(matches!(
            failure,
            AssertionFailure::ReceiptCitesUnknownEvent { .. }
        ));
    }

    #[test]
    fn a_phase_that_is_not_one_value_is_refused() {
        use turnframe_core::case::CaseRef;
        use turnframe_core::flow::{ErasedWorkflowView, PhaseOwnership};
        use turnframe_core::ids::{CaseRevision, WorkflowVersion};

        let mut view = ErasedWorkflowView {
            case_ref: CaseRef::new("trip", "trip-1", CaseRevision(1)),
            workflow_version: WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: PhaseOwnership::System,
            obligations: Vec::new(),
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
            state: Vec::new(),
        };
        assert!(single_phase(&view).is_ok());
        view.phase = serde_json::json!(["collecting", "dispatching"]);
        assert!(matches!(
            single_phase(&view).unwrap_err(),
            AssertionFailure::NotASinglePhase { .. }
        ));
        view.phase = serde_json::Value::Null;
        assert!(single_phase(&view).is_err());
    }

    #[test]
    fn turns_are_compared_block_by_block() {
        let a = turn("Fatto.");
        assert!(identical_blocks(&a, &a.clone()).is_ok());
        assert_eq!(
            identical_blocks(&a, &turn("Done.")).unwrap_err(),
            AssertionFailure::BlocksDiffer { index: 0 }
        );
        let mut shorter = a.clone();
        shorter.blocks.clear();
        assert_eq!(
            identical_blocks(&a, &shorter).unwrap_err(),
            AssertionFailure::BlockCountDiffers { left: 1, right: 0 }
        );
    }
}
