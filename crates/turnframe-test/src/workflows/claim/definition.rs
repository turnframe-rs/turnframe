//! The claim [`WorkflowDefinition`], its pure transition function, its review
//! card and its receipts.

use serde::de::DeserializeOwned;
use turnframe_core::case::CaseRef;
use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandPolicy, ConfirmationPolicy, RiskClass,
};
use turnframe_core::error::DomainRejection;
use turnframe_core::event::{OperationalReceipt, ReceiptEvent, ReceiptSeverity, RedactedEvent};
use turnframe_core::flow::{
    InteractionRequirement, PhaseOwnership, ViewOf, WorkflowDefinition, WorkflowNotice,
    WorkflowView,
};
use turnframe_core::hash::canonical_digest;
use turnframe_core::ids::{AttachmentId, OperationKey, ReceiptId, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::{
    FieldValue, InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
    OptionStyle, ReviewDiffEntry, StoredInteractionAction,
};
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::operation::{DateDirection, OperationSpec};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::response::NoticeSeverity;
use turnframe_core::target::{ResolvedAct, ResolvedActKind};

use crate::workflows::claim::command::{
    AttachArgs, ClaimCommand, ClaimEvent, ReferenceArgs, ReviseArgs, operations,
};
use crate::workflows::claim::state::{
    ClaimField, ClaimObligation, ClaimOutcome, ClaimPhase, ClaimState, ClaimStatus, Proposal,
    ProposedField, RecordedField,
};
use crate::workflows::{Applied, PureWorkflow};

/// Stable rejection codes of the claim sample.
pub mod rejection {
    /// The case does not exist yet.
    pub const NOT_FOUND: &str = "claim.not_found";
    /// The case already exists.
    pub const ALREADY_EXISTS: &str = "claim.already_exists";
    /// The case is closed and no longer accepts changes.
    pub const CLOSED: &str = "claim.closed";
    /// The reference is blank.
    pub const REFERENCE_EMPTY: &str = "claim.reference_empty";
    /// A document is already attached.
    pub const RECEIPT_ALREADY_ATTACHED: &str = "claim.receipt_already_attached";
    /// No document has arrived, so nothing can have been read out of one.
    pub const NO_RECEIPT: &str = "claim.no_receipt";
    /// The proposal names a document other than the attached one.
    pub const WRONG_RECEIPT: &str = "claim.wrong_receipt";
    /// A review is already open, and a second reading would silently replace
    /// values the user is looking at.
    pub const REVIEW_ALREADY_OPEN: &str = "claim.review_already_open";
    /// Nothing is under review.
    pub const NO_REVIEW: &str = "claim.no_review";
    /// The reading produced no values at all.
    pub const PROPOSAL_EMPTY: &str = "claim.proposal_empty";
    /// The reading proposed the same field twice.
    pub const PROPOSAL_DUPLICATE_FIELD: &str = "claim.proposal_duplicate_field";
    /// A proposed value is blank.
    pub const PROPOSED_VALUE_EMPTY: &str = "claim.proposed_value_empty";
    /// A required field has no value, so the proposal cannot be accepted.
    pub const PROPOSAL_INCOMPLETE: &str = "claim.proposal_incomplete";
    /// The workflow does not compile this kind of act.
    pub const UNSUPPORTED_ACT: &str = "claim.unsupported_act";
    /// The operation is not in the catalog.
    pub const UNKNOWN_OPERATION: &str = "claim.unknown_operation";
    /// The arguments do not match the operation's schema.
    pub const INVALID_ARGUMENTS: &str = "claim.invalid_arguments";
}

/// Key of the blocking review requirement.
pub const REVIEW_KEY: &str = "claim.proposal_review";

/// Option that accepts the proposal.
pub const ACCEPT_OPTION: &str = "accept";

/// Option that throws the reading away and keeps the document.
pub const ABANDON_OPTION: &str = "abandon";

/// Option that answers nothing and leaves the proposal alone.
pub const NOT_NOW_OPTION: &str = "not_now";

/// Notice code carried while a proposal is open.
pub const PROPOSED_NOTICE: &str = "claim.fields_proposed_from_attachment";

/// Notice code carried when the user changed proposed values.
pub const EDITED_NOTICE: &str = "claim.proposal_edited";

/// Notice code carried after a reading was thrown away.
pub const ABANDONED_NOTICE: &str = "claim.proposal_abandoned";

/// Longest accepted value, in characters.
pub const MAX_VALUE_CHARS: usize = 200;

fn reject(code: &'static str) -> DomainRejection {
    let suffix = code.strip_prefix("claim.").unwrap_or(code);
    DomainRejection::new(code, format!("claim.error.{suffix}"))
}

fn parse<T: DeserializeOwned>(arguments: &serde_json::Value) -> Result<T, DomainRejection> {
    serde_json::from_value(arguments.clone()).map_err(|_| reject(rejection::INVALID_ARGUMENTS))
}

fn open(state: &ClaimState) -> Result<(), DomainRejection> {
    if state.status.is_open() {
        Ok(())
    } else {
        Err(reject(rejection::CLOSED))
    }
}

fn check_value(value: &str) -> Result<String, DomainRejection> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_VALUE_CHARS {
        return Err(reject(rejection::PROPOSED_VALUE_EMPTY)
            .with_details(serde_json::json!({ "max_chars": MAX_VALUE_CHARS })));
    }
    Ok(trimmed.to_owned())
}

/// Checks a command against the current state without changing anything.
pub fn validate(state: Option<&ClaimState>, command: &ClaimCommand) -> Result<(), DomainRejection> {
    let Some(state) = state else {
        return match command {
            ClaimCommand::CreateDraft => Ok(()),
            _ => Err(reject(rejection::NOT_FOUND)),
        };
    };
    match command {
        ClaimCommand::CreateDraft => Err(reject(rejection::ALREADY_EXISTS)),
        ClaimCommand::SetReference { value } => {
            open(state)?;
            if value.trim().is_empty() {
                return Err(reject(rejection::REFERENCE_EMPTY));
            }
            Ok(())
        }
        ClaimCommand::AttachReceipt { .. } => {
            open(state)?;
            if state.attachment.is_some() {
                return Err(reject(rejection::RECEIPT_ALREADY_ATTACHED));
            }
            Ok(())
        }
        ClaimCommand::ProposeFields {
            attachment_id,
            fields,
        } => {
            open(state)?;
            let Some(attached) = state.attachment.as_ref() else {
                return Err(reject(rejection::NO_RECEIPT));
            };
            if attached != attachment_id {
                return Err(reject(rejection::WRONG_RECEIPT));
            }
            // A second reading may not overwrite a proposal the user is looking
            // at: the open review has to be answered or abandoned first.
            if state.proposal.is_some() {
                return Err(reject(rejection::REVIEW_ALREADY_OPEN));
            }
            if fields.is_empty() {
                return Err(reject(rejection::PROPOSAL_EMPTY));
            }
            let mut seen = std::collections::BTreeSet::new();
            for proposed in fields {
                if !seen.insert(proposed.field) {
                    return Err(reject(rejection::PROPOSAL_DUPLICATE_FIELD));
                }
                check_value(&proposed.value)?;
            }
            Ok(())
        }
        ClaimCommand::ReviseProposedField { value, .. } => {
            open(state)?;
            if state.proposal.is_none() {
                return Err(reject(rejection::NO_REVIEW));
            }
            check_value(value)?;
            Ok(())
        }
        ClaimCommand::AcceptProposal => {
            open(state)?;
            let Some(proposal) = state.proposal.as_ref() else {
                return Err(reject(rejection::NO_REVIEW));
            };
            if proposal.is_complete() {
                Ok(())
            } else {
                Err(
                    reject(rejection::PROPOSAL_INCOMPLETE).with_details(serde_json::json!({
                        "missing": proposal
                            .missing()
                            .into_iter()
                            .map(ClaimField::key)
                            .collect::<Vec<_>>(),
                    })),
                )
            }
        }
        ClaimCommand::AbandonReview => {
            open(state)?;
            if state.proposal.is_some() {
                Ok(())
            } else {
                Err(reject(rejection::NO_REVIEW))
            }
        }
        ClaimCommand::DiscardReceipt => open(state),
    }
}

/// Applies a command, producing the next state and the events to commit.
pub fn apply(
    state: Option<&ClaimState>,
    command: &ClaimCommand,
) -> Result<Applied<ClaimState, ClaimEvent>, DomainRejection> {
    validate(state, command)?;
    let Some(state) = state else {
        return Ok(Applied::new(
            ClaimState::default(),
            vec![ClaimEvent::DraftCreated],
        ));
    };
    let mut next = state.clone();
    let event = match command {
        ClaimCommand::CreateDraft => return Err(reject(rejection::ALREADY_EXISTS)),
        ClaimCommand::SetReference { value } => {
            let value = value.trim().to_owned();
            next.reference = Some(value.clone());
            ClaimEvent::ReferenceSet { value }
        }
        ClaimCommand::AttachReceipt { attachment_id } => {
            next.attachment = Some(attachment_id.clone());
            ClaimEvent::ReceiptAttached {
                attachment_id: attachment_id.clone(),
            }
        }
        ClaimCommand::ProposeFields {
            attachment_id,
            fields,
        } => {
            let normalized: Vec<ProposedField> = fields
                .iter()
                .map(|proposed| ProposedField {
                    field: proposed.field,
                    value: proposed.value.trim().to_owned(),
                    edited: false,
                })
                .collect();
            let proposal = Proposal::new(attachment_id.clone(), normalized);
            let fields = proposal.fields.clone();
            next.proposal = Some(proposal);
            // A fresh reading supersedes the memory of the abandoned one.
            next.abandoned_from = None;
            ClaimEvent::FieldsProposed {
                attachment_id: attachment_id.clone(),
                fields,
            }
        }
        ClaimCommand::ReviseProposedField { field, value } => {
            let value = value.trim().to_owned();
            let Some(proposal) = next.proposal.as_mut() else {
                return Err(reject(rejection::NO_REVIEW));
            };
            let previous = proposal
                .field(*field)
                .map(|proposed| proposed.value.clone());
            proposal.fields.retain(|proposed| proposed.field != *field);
            proposal.fields.push(ProposedField {
                field: *field,
                value: value.clone(),
                // The mark that outlives the review: a human put this here.
                edited: true,
            });
            proposal.fields.sort_by_key(|proposed| proposed.field);
            ClaimEvent::ProposedFieldRevised {
                field: *field,
                previous,
                value,
            }
        }
        ClaimCommand::AcceptProposal => {
            let Some(proposal) = next.proposal.take() else {
                return Err(reject(rejection::NO_REVIEW));
            };
            let recorded: Vec<RecordedField> = proposal
                .fields
                .iter()
                .map(|proposed| RecordedField {
                    field: proposed.field,
                    value: proposed.value.clone(),
                    from_attachment: proposal.attachment_id.clone(),
                    corrected: proposed.edited,
                })
                .collect();
            next.recorded = recorded.clone();
            next.status = ClaimStatus::Recorded;
            ClaimEvent::ProposalAccepted {
                attachment_id: proposal.attachment_id,
                fields: recorded,
            }
        }
        ClaimCommand::AbandonReview => {
            let Some(proposal) = next.proposal.take() else {
                return Err(reject(rejection::NO_REVIEW));
            };
            // This domain's explicit choice: the reading goes, the document
            // stays, and the case says so.
            next.abandoned_from = Some(proposal.attachment_id.clone());
            ClaimEvent::ProposalAbandoned {
                attachment_id: proposal.attachment_id,
            }
        }
        ClaimCommand::DiscardReceipt => {
            next.proposal = None;
            next.status = ClaimStatus::Discarded;
            ClaimEvent::ReceiptDiscarded
        }
    };
    Ok(Applied::new(next, vec![event]))
}

/// The receipt-claim workflow.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct ClaimWorkflow;

impl ClaimWorkflow {
    /// Builds the workflow.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The phase a state projects to.
    #[must_use]
    pub fn phase_of(state: &ClaimState) -> ClaimPhase {
        match state.status {
            ClaimStatus::Recorded => ClaimPhase::Recorded,
            ClaimStatus::Discarded => ClaimPhase::Discarded,
            ClaimStatus::Draft if state.proposal.is_some() => ClaimPhase::AwaitingReview,
            ClaimStatus::Draft if state.attachment.is_some() => ClaimPhase::Extracting,
            ClaimStatus::Draft => ClaimPhase::AwaitingDocument,
        }
    }

    fn compile_operation(
        operation: &OperationKey,
        arguments: &serde_json::Value,
    ) -> Result<Vec<ClaimCommand>, DomainRejection> {
        let command = match operation.as_str() {
            operations::CREATE_DRAFT => ClaimCommand::CreateDraft,
            operations::SET_REFERENCE => ClaimCommand::SetReference {
                value: parse::<ReferenceArgs>(arguments)?.value,
            },
            operations::ATTACH_RECEIPT => ClaimCommand::AttachReceipt {
                attachment_id: AttachmentId::from(parse::<AttachArgs>(arguments)?.attachment_id),
            },
            operations::REVISE_PROPOSED_FIELD => {
                let args = parse::<ReviseArgs>(arguments)?;
                // Each field is read in its own shape: a total in cents, a date as a date.
                let value = match args.field {
                    ClaimField::Merchant => args.merchant,
                    ClaimField::Total => args.total.map(|total| total.minor.to_string()),
                    ClaimField::ReceiptDate => args.date.map(|date| date.to_string()),
                };
                ClaimCommand::ReviseProposedField {
                    field: args.field,
                    value: value.ok_or_else(|| reject(rejection::INVALID_ARGUMENTS))?,
                }
            }
            operations::ACCEPT_PROPOSAL => ClaimCommand::AcceptProposal,
            operations::ABANDON_REVIEW => ClaimCommand::AbandonReview,
            operations::DISCARD_RECEIPT => ClaimCommand::DiscardReceipt,
            // The reserved code, not this workflow's own: it says which of the
            // two mistakes this is, so the state explorer can report a
            // catalogue that offers what the compiler does not know.
            _ => return Err(reject(turnframe_core::error::UNKNOWN_OPERATION)),
        };
        Ok(vec![command])
    }
}

fn operation(key: &str, summary: &str) -> OperationSpec {
    OperationSpec::new(key)
        .summary(summary)
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
}

/// The diff the review card shows: one line per proposed value.
fn review_entries(state: &ClaimState, proposal: &Proposal) -> Vec<ReviewDiffEntry> {
    proposal
        .fields
        .iter()
        .map(|proposed| {
            let before = state
                .recorded_value(proposed.field)
                .map_or(FieldValue::Absent, FieldValue::present);
            ReviewDiffEntry::new(
                proposed.field.key(),
                LocalizedText::new(proposed.field.label()).with("it", proposed.field.label_it()),
            )
            .with_before(before)
            .with_after(FieldValue::present(proposed.value.clone()))
        })
        .collect()
}

/// The blocking review card of the `AwaitingReview` phase.
///
/// Its payload is the proposal and nothing else: the diff entries are the
/// proposed values, the copy counts them and names the document, and no part of
/// the case outside the proposal appears. That is what makes the payload hash
/// cover the proposal rather than the whole state — not a feature, a
/// consequence of putting the proposal in the payload.
fn review_requirement(state: &ClaimState, proposal: &Proposal) -> InteractionRequirement {
    let count = proposal.fields.len();
    let mut payload = InteractionPayload::new(
        LocalizedText::new(format!("Review {count} value(s) read from the receipt"))
            .with("it", format!("Controlla {count} valore/i letti dalla ricevuta")),
    )
    .with_body(
        LocalizedText::new(
            "These values were read from the attached receipt. Nothing has been recorded yet.",
        )
        .with(
            "it",
            "Questi valori sono stati letti dalla ricevuta allegata. Non è stato ancora registrato nulla.",
        ),
    );
    for entry in review_entries(state, proposal) {
        payload = payload.with_review_entry(entry);
    }
    // A proposal that is missing a required value cannot be accepted, so the
    // card does not offer to accept it: an option nobody can act on is a worse
    // answer than an option that is not there.
    if proposal.is_complete() {
        payload = payload.with_option(
            InteractionOption::new(
                ACCEPT_OPTION,
                LocalizedText::new("Record these values").with("it", "Registra questi valori"),
                StoredInteractionAction::ApplyOperation {
                    operation: OperationKey::from(operations::ACCEPT_PROPOSAL),
                    arguments: serde_json::Value::Null,
                    freeform_argument: None,
                },
            )
            .with_style(OptionStyle::Primary),
        );
    }
    payload = payload
        .with_option(
            InteractionOption::new(
                ABANDON_OPTION,
                LocalizedText::new("Discard this reading").with("it", "Scarta questa lettura"),
                StoredInteractionAction::ApplyOperation {
                    operation: OperationKey::from(operations::ABANDON_REVIEW),
                    arguments: serde_json::Value::Null,
                    freeform_argument: None,
                },
            )
            .with_style(OptionStyle::Danger),
        )
        .with_option(InteractionOption::new(
            NOT_NOW_OPTION,
            LocalizedText::new("Not now").with("it", "Non ora"),
            // Declining is *not* abandoning: nothing is committed, so the
            // proposal survives and the card is derived again next turn.
            StoredInteractionAction::DeclineCommands,
        ));
    InteractionRequirement::blocking(REVIEW_KEY, InteractionKind::ReviewChanges)
        .with_confirms_risk(RiskClass::SensitiveDataChange)
        .with_payload(payload)
}

fn proposed_notice(proposal: &Proposal) -> WorkflowNotice {
    let count = proposal.fields.len();
    let document = proposal.attachment_id.as_str();
    WorkflowNotice {
        code: PROPOSED_NOTICE.to_owned(),
        severity: NoticeSeverity::Info,
        text: LocalizedText::new(format!(
            "{count} value(s) are proposed from receipt {document} and are not recorded yet."
        ))
        .with(
            "it",
            format!(
                "{count} valore/i sono proposti dalla ricevuta {document} e non sono ancora registrati."
            ),
        ),
    }
}

fn edited_notice(proposal: &Proposal) -> WorkflowNotice {
    let edited = proposal.edited_count();
    WorkflowNotice {
        code: EDITED_NOTICE.to_owned(),
        severity: NoticeSeverity::Info,
        text: LocalizedText::new(format!(
            "{edited} of the proposed value(s) were changed by hand."
        ))
        .with(
            "it",
            format!("{edited} dei valori proposti sono stati modificati a mano."),
        ),
    }
}

fn abandoned_notice(attachment: &AttachmentId) -> WorkflowNotice {
    let document = attachment.as_str();
    WorkflowNotice {
        code: ABANDONED_NOTICE.to_owned(),
        severity: NoticeSeverity::Info,
        text: LocalizedText::new(format!(
            "The reading of receipt {document} was discarded. The receipt is still attached and can be read again."
        ))
        .with(
            "it",
            format!(
                "La lettura della ricevuta {document} è stata scartata. La ricevuta resta allegata e si può rileggere."
            ),
        ),
    }
}

impl WorkflowDefinition for ClaimWorkflow {
    type State = ClaimState;
    type Phase = ClaimPhase;
    type Obligation = ClaimObligation;
    type Command = ClaimCommand;
    type Event = ClaimEvent;
    type Outcome = ClaimOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("claim")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &ClaimPhase) -> PhaseOwnership {
        match phase {
            ClaimPhase::PreDraft | ClaimPhase::AwaitingDocument | ClaimPhase::Extracting => {
                PhaseOwnership::System
            }
            ClaimPhase::AwaitingReview => PhaseOwnership::User,
            ClaimPhase::Recorded | ClaimPhase::Discarded => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&ClaimState>) -> ViewOf<Self> {
        let version = self.version();
        let Some(state) = state else {
            return WorkflowView::new(case_ref, version, ClaimPhase::PreDraft);
        };
        let phase = Self::phase_of(state);
        let mut view =
            WorkflowView::new(case_ref, version, phase).with_obligations(state.open_obligations());
        if let Some(proposal) = state.proposal.as_ref() {
            // "These N fields are proposed from attachment A", said in this
            // domain's own words rather than in a vocabulary the framework had
            // to grow for it.
            view = view.with_notice(proposed_notice(proposal));
            if proposal.edited_count() > 0 {
                view = view.with_notice(edited_notice(proposal));
            }
        } else if let (Some(attachment), true) =
            (state.abandoned_from.as_ref(), state.status.is_open())
        {
            view = view.with_notice(abandoned_notice(attachment));
        }
        match (phase, state.proposal.as_ref()) {
            (ClaimPhase::AwaitingReview, Some(proposal)) => {
                view.with_blocking_interaction(review_requirement(state, proposal))
            }
            (ClaimPhase::Recorded, _) => view.with_outcome(ClaimOutcome::Recorded),
            (ClaimPhase::Discarded, _) => view.with_outcome(ClaimOutcome::Discarded),
            _ => view,
        }
    }

    fn summary(&self) -> Option<String> {
        Some(String::from(
            "Expense claims: read a receipt the user sends and record what it says.",
        ))
    }

    fn noun(&self) -> Option<LocalizedText> {
        Some(LocalizedText::new("expense claim").with("it", "nota spese"))
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        super::super::in_italian(Self::offered(view), ITALIAN)
    }

    fn compile_act(
        &self,
        _state: Option<&ClaimState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<ClaimCommand>, DomainRejection> {
        match &act.kind {
            ResolvedActKind::StartWorkflow => Ok(vec![ClaimCommand::CreateDraft]),
            ResolvedActKind::ApplyOperation { operation } => {
                Self::compile_operation(operation, &act.arguments)
            }
            _ => Err(reject(rejection::UNSUPPORTED_ACT)),
        }
    }

    fn command_policy(&self, _state: Option<&ClaimState>, command: &ClaimCommand) -> CommandPolicy {
        match command {
            // Answering the review writes derived values into the record: a
            // review card, not a sentence, is what authorizes it.
            ClaimCommand::AcceptProposal => CommandPolicy {
                risk: RiskClass::SensitiveDataChange,
                confirmation: ConfirmationPolicy::ReviewCard,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::EventReferencedParaphrase,
            },
            ClaimCommand::DiscardReceipt => CommandPolicy {
                risk: RiskClass::Destructive,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            ClaimCommand::AbandonReview => CommandPolicy {
                risk: RiskClass::ReversibleLowRisk,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::EventReferencedParaphrase,
            },
            // Server-issued: the values come from an extractor, so the receipt
            // is the only thing allowed to state what was read.
            ClaimCommand::ProposeFields { .. } => CommandPolicy {
                risk: RiskClass::ReversibleLowRisk,
                confirmation: ConfirmationPolicy::None,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            // Editing a proposal changes nothing outside the proposal, which is
            // exactly why it is not the same act as answering the review.
            ClaimCommand::CreateDraft
            | ClaimCommand::SetReference { .. }
            | ClaimCommand::AttachReceipt { .. }
            | ClaimCommand::ReviseProposedField { .. } => CommandPolicy::low_risk(),
        }
    }

    fn validate_command(
        &self,
        state: Option<&ClaimState>,
        command: &ClaimCommand,
    ) -> Result<(), DomainRejection> {
        validate(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<ClaimEvent>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        let _ = locale;
        events
            .iter()
            .map(|event| match event {
                ReceiptEvent::Committed(committed) => {
                    let event_ids = vec![committed.event_id];
                    let status_code = committed.payload.event_type().to_owned();
                    let (severity, title, body) = receipt_copy(&committed.payload);
                    OperationalReceipt {
                        receipt_id: ReceiptId::derive(&event_ids, &status_code),
                        event_ids,
                        severity,
                        title,
                        body,
                        status_code,
                        artifact_refs: Vec::new(),
                    }
                }
                ReceiptEvent::Redacted(redacted) => redacted_receipt(redacted),
            })
            .collect()
    }

    /// Binds the card to the **proposal**, not to the whole state.
    ///
    /// The trip sample puts a digest of the entire case in its rebooking card's
    /// metadata, because the entire case is what that card is about. This card is
    /// about the proposal, so the proposal is what it hashes — and
    /// the case revision is deliberately *not* in the metadata, because
    /// revision binding is already the interaction's own mechanism and putting
    /// it in the payload would make the payload hash change for every unrelated
    /// edit.
    fn build_interaction(
        &self,
        state: Option<&ClaimState>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        let mut spec = requirement.to_spec(view.case_ref.clone());
        if let Some(proposal) = state.and_then(|state| state.proposal.as_ref()) {
            let digest =
                canonical_digest(proposal).map_err(|_| reject(rejection::INVALID_ARGUMENTS))?;
            spec.payload = spec.payload.with_metadata(serde_json::json!({
                "proposal_hash": digest.as_str(),
                "attachment_id": proposal.attachment_id.as_str(),
            }));
        }
        Ok(spec)
    }
}

impl ClaimWorkflow {
    /// The operations a view offers, with their summaries in English.
    fn offered(view: &ViewOf<Self>) -> Vec<OperationSpec> {
        let reference = || {
            operation(
                operations::SET_REFERENCE,
                "Set the reference the user types by hand. It is not part of any proposal.",
            )
            .arguments::<ReferenceArgs>()
            .argument("value", |a| {
                a.label("reference")
                    .label_in("it-IT", "riferimento")
                    .required()
            })
        };
        let discard = || {
            operation(
                operations::DISCARD_RECEIPT,
                "Throw the receipt away without recording anything.",
            )
        };
        match view.phase {
            ClaimPhase::PreDraft => vec![
                operation(operations::CREATE_DRAFT, "Start an expense claim.")
                    .target(TargetPolicy::AllowsNewCase),
            ],
            ClaimPhase::AwaitingDocument => vec![
                reference(),
                operation(
                    operations::ATTACH_RECEIPT,
                    "Attach a receipt that arrived with this turn.",
                )
                .arguments::<AttachArgs>()
                .argument("attachment_id", |a| a.written().required()),
                discard(),
            ],
            ClaimPhase::Extracting => vec![reference(), discard()],
            ClaimPhase::AwaitingReview => vec![
                // Editing a proposed value and answering the review are two operations.
                operation(
                    operations::REVISE_PROPOSED_FIELD,
                    "Change one value proposed from the receipt. The review stays open.",
                )
                .arguments::<ReviseArgs>()
                .argument("field", |a| {
                    a.label("field")
                        .label_in("it-IT", "campo")
                        .describe("Which proposed value the user names: the merchant, the total or the receipt's date.")
                        .required()
                })
                .argument("merchant", |a| {
                    a.label("merchant").label_in("it-IT", "esercente")
                })
                .argument("total", |a| {
                    a.label("total").label_in("it-IT", "totale").money()
                })
                .argument("date", |a| {
                    a.label("receipt date")
                        .label_in("it-IT", "data della ricevuta")
                        .date_direction(DateDirection::Past)
                })
                .example(
                    "the total is 180 euros",
                    serde_json::json!({
                        "field": "total",
                        "total": { "minor": 18_000, "currency": "EUR" }
                    }),
                )
                .example(
                    "the merchant is Café Aurora",
                    serde_json::json!({ "field": "merchant", "merchant": "Café Aurora" }),
                ),
                operation(
                    operations::ACCEPT_PROPOSAL,
                    "Accept the proposed values: they become the record.",
                ),
                operation(
                    operations::ABANDON_REVIEW,
                    "Throw the reading away and keep the receipt, so it can be read again.",
                ),
                reference(),
                discard(),
            ],
            ClaimPhase::Recorded | ClaimPhase::Discarded => Vec::new(),
        }
    }
}

/// What each operation does, in Italian.
const ITALIAN: &[(&str, &str)] = &[
    (operations::CREATE_DRAFT, "Apre una nota spese."),
    (
        operations::SET_REFERENCE,
        "Imposta il riferimento che l'utente scrive a mano. Non fa parte di alcuna proposta.",
    ),
    (
        operations::ATTACH_RECEIPT,
        "Allega una ricevuta arrivata con questo turno.",
    ),
    (
        operations::REVISE_PROPOSED_FIELD,
        "Cambia un valore proposto dalla ricevuta. La revisione resta aperta.",
    ),
    (
        operations::ACCEPT_PROPOSAL,
        "Accetta i valori proposti: diventano il record.",
    ),
    (
        operations::ABANDON_REVIEW,
        "Scarta la lettura e tiene la ricevuta, così si può leggere di nuovo.",
    ),
    (
        operations::DISCARD_RECEIPT,
        "Scarta la ricevuta senza registrare nulla.",
    ),
];

/// Renders an event whose payload was erased.
///
/// The values this domain records were read out of a document a person sent in,
/// so an erasure request reaches them, and the proposal that quoted them is
/// exactly the kind of receipt that cannot be re-rendered afterwards. The
/// erased receipt keeps the one thing that is still true: a step happened, and
/// it is still in the ledger.
fn redacted_receipt(redacted: &RedactedEvent) -> OperationalReceipt {
    let event_ids = vec![redacted.event_id];
    let status_code = "claim.detail_erased".to_owned();
    OperationalReceipt {
        receipt_id: ReceiptId::derive(&event_ids, &status_code),
        event_ids,
        severity: ReceiptSeverity::Info,
        title: LocalizedText::new("Detail erased").with("it", "Dettaglio cancellato"),
        body: LocalizedText::new(
            "This step is still on record; the detail of what it recorded was erased.",
        )
        .with(
            "it",
            "Questo passaggio resta a registro; il dettaglio di cosa ha registrato è stato cancellato.",
        ),
        status_code,
        artifact_refs: Vec::new(),
    }
}

/// Server-authored copy, in English with an Italian translation.
fn receipt_copy(event: &ClaimEvent) -> (ReceiptSeverity, LocalizedText, LocalizedText) {
    match event {
        ClaimEvent::DraftCreated => (
            ReceiptSeverity::Success,
            LocalizedText::new("Claim opened").with("it", "Nota spese aperta"),
            LocalizedText::new("A new expense claim is open.")
                .with("it", "È aperta una nuova nota spese."),
        ),
        ClaimEvent::ReferenceSet { value } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Reference set").with("it", "Riferimento impostato"),
            LocalizedText::new(format!("The reference is now \"{value}\"."))
                .with("it", format!("Il riferimento ora è \"{value}\".")),
        ),
        ClaimEvent::ReceiptAttached { attachment_id } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Receipt attached").with("it", "Ricevuta allegata"),
            LocalizedText::new(format!("Receipt {attachment_id} is attached."))
                .with("it", format!("La ricevuta {attachment_id} è allegata.")),
        ),
        ClaimEvent::FieldsProposed { fields, .. } => {
            let count = fields.len();
            (
                ReceiptSeverity::Success,
                LocalizedText::new("Values proposed").with("it", "Valori proposti"),
                LocalizedText::new(format!(
                    "{count} value(s) were read from the receipt and are waiting for review. \
                     Nothing has been recorded."
                ))
                .with(
                    "it",
                    format!(
                        "{count} valore/i sono stati letti dalla ricevuta e attendono conferma. \
                         Non e stato registrato nulla."
                    ),
                ),
            )
        }
        ClaimEvent::ProposedFieldRevised { field, .. } => {
            let label = field.label();
            (
                ReceiptSeverity::Success,
                LocalizedText::new("Proposed value changed").with("it", "Valore proposto corretto"),
                LocalizedText::new(format!(
                    "\"{label}\" now holds what you typed. The review is still open."
                ))
                .with(
                    "it",
                    format!("\"{label}\" ora contiene quanto hai scritto. La conferma resta aperta."),
                ),
            )
        }
        ClaimEvent::ProposalAccepted { fields, .. } => {
            let count = fields.len();
            (
                ReceiptSeverity::Success,
                LocalizedText::new("Values recorded").with("it", "Valori registrati"),
                LocalizedText::new(format!("{count} value(s) are now on the record."))
                    .with("it", format!("{count} valore/i ora sono registrati.")),
            )
        }
        ClaimEvent::ProposalAbandoned { attachment_id } => (
            ReceiptSeverity::Warning,
            LocalizedText::new("Reading discarded").with("it", "Lettura scartata"),
            LocalizedText::new(format!(
                "The reading of receipt {attachment_id} was discarded. The receipt is still attached."
            ))
            .with(
                "it",
                format!(
                    "La lettura della ricevuta {attachment_id} è stata scartata. La ricevuta resta allegata."
                ),
            ),
        ),
        ClaimEvent::ReceiptDiscarded => (
            ReceiptSeverity::Warning,
            LocalizedText::new("Receipt discarded").with("it", "Ricevuta scartata"),
            LocalizedText::new("The receipt was thrown away without being recorded.")
                .with("it", "La ricevuta è stata scartata senza registrare nulla."),
        ),
    }
}

impl PureWorkflow for ClaimWorkflow {
    fn apply(
        &self,
        state: Option<&ClaimState>,
        command: &ClaimCommand,
    ) -> Result<Applied<ClaimState, ClaimEvent>, DomainRejection> {
        apply(state, command)
    }

    fn event_type(&self, event: &ClaimEvent) -> String {
        event.event_type().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::claim::model::{SAMPLE_ATTACHMENT, complete_proposal, under_review};

    #[test]
    fn a_second_reading_may_not_replace_an_open_proposal() {
        let state = under_review(complete_proposal());
        let rejection = validate(
            Some(&state),
            &ClaimCommand::ProposeFields {
                attachment_id: AttachmentId::from(SAMPLE_ATTACHMENT),
                fields: complete_proposal().fields,
            },
        )
        .unwrap_err();
        assert_eq!(rejection.code.as_str(), rejection::REVIEW_ALREADY_OPEN);
    }

    #[test]
    fn an_incomplete_proposal_cannot_be_accepted_and_offers_no_accept_option() {
        let partial = Proposal::new(
            AttachmentId::from(SAMPLE_ATTACHMENT),
            vec![ProposedField {
                field: ClaimField::Merchant,
                value: "Hotel Tejo".into(),
                edited: false,
            }],
        );
        let state = under_review(partial.clone());
        assert_eq!(
            validate(Some(&state), &ClaimCommand::AcceptProposal)
                .unwrap_err()
                .code
                .as_str(),
            rejection::PROPOSAL_INCOMPLETE
        );
        let requirement = review_requirement(&state, &partial);
        let payload = requirement.payload.expect("the card carries its payload");
        assert!(payload.option(&ACCEPT_OPTION.into()).is_none());
        assert!(payload.option(&ABANDON_OPTION.into()).is_some());
        payload
            .validate_for(InteractionKind::ReviewChanges)
            .expect("a card without an accept option is still answerable");
    }

    #[test]
    fn the_proposal_is_normalized_so_two_readings_of_the_same_values_hash_alike() {
        let forwards = complete_proposal();
        let mut shuffled = forwards.fields.clone();
        shuffled.reverse();
        let backwards = Proposal::new(AttachmentId::from(SAMPLE_ATTACHMENT), shuffled);
        assert_eq!(forwards, backwards);
        assert_eq!(
            canonical_digest(&forwards).unwrap(),
            canonical_digest(&backwards).unwrap()
        );
    }

    #[test]
    fn a_revised_value_is_read_in_the_shape_its_field_takes() {
        let revise = OperationKey::from(operations::REVISE_PROPOSED_FIELD);
        let compiled =
            |arguments: serde_json::Value| ClaimWorkflow::compile_operation(&revise, &arguments);
        assert_eq!(
            compiled(serde_json::json!({
                "field": "total",
                "total": { "minor": 130_000, "currency": "EUR" }
            }))
            .unwrap(),
            vec![ClaimCommand::ReviseProposedField {
                field: ClaimField::Total,
                value: "130000".to_owned(),
            }]
        );
        assert_eq!(
            compiled(serde_json::json!({ "field": "receipt_date", "date": "2026-09-01" })).unwrap(),
            vec![ClaimCommand::ReviseProposedField {
                field: ClaimField::ReceiptDate,
                value: "2026-09-01".to_owned(),
            }]
        );
        assert_eq!(
            compiled(serde_json::json!({ "field": "merchant", "merchant": "Café Aurora" }))
                .unwrap(),
            vec![ClaimCommand::ReviseProposedField {
                field: ClaimField::Merchant,
                value: "Café Aurora".to_owned(),
            }]
        );
        // A total given as words, or no value for the field named, is not a revision.
        assert!(compiled(serde_json::json!({ "field": "total", "merchant": "1,300" })).is_err());
    }
}
