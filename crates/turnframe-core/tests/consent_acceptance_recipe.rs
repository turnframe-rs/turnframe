//! Consent and legal acceptance, modelled with the types that already exist.
//!
//! The first adopter asked for a new `InteractionKind` for "the user accepted a
//! legal document". The answer is no, and `docs/consent-and-acceptance.md`
//! gives the reasoning: every interaction binds to a case and a revision,
//! because that binding is what makes a card safe, and an acceptance that is
//! deliberately *not* about the case would either carry a meaningless case
//! reference or force an account-scoped variant on every adopter.
//!
//! This file is the recipe the guide points at, executed. Consent is its own
//! small workflow whose case is the person; the acceptance is an ordinary
//! command behind an ordinary `ConfirmCommand` card; the versioned artifact
//! identity travels in the card, in the command and in the event; and another
//! workflow is gated by a projection that reads committed consent rather than
//! by a flag somebody set.
//!
//! The part the adopter rightly insisted on — that the artifact *version* stays
//! bound to the resolution — is checked three times over, because it is bound
//! in three places: the payload hash the origin cites, the arguments of the
//! stored option the server derives the command from, and the committed event
//! the audit will read years later.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, TimeZone, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use turnframe_core::prelude::*;
use turnframe_core::response::NoticeSeverity;

/// The document this workflow is about. One consent workflow can carry several;
/// one is enough to make the point.
const TERMS: &str = "terms.electronic-invoicing";

/// The tenant everything below belongs to.
const ACCOUNT: &str = "acct-7";

/// The person whose consent is being recorded. It is also the case identifier,
/// which is the first move of the whole recipe.
const PERSON: &str = "user-42";

// ---------------------------------------------------------------------------
// The artifact
// ---------------------------------------------------------------------------

/// A versioned legal artifact: which document, which version of it, and the
/// digest of the bytes the person was actually shown.
///
/// The digest is what makes the version more than a label. A document quietly
/// re-published under the same version number produces a different digest, so
/// the acceptance on file stops matching what is on the wire and the obligation
/// reopens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactRef {
    document: String,
    version: String,
    digest: String,
}

impl ArtifactRef {
    fn published(version: &str, body: &str) -> Self {
        Self {
            document: TERMS.to_owned(),
            version: version.to_owned(),
            digest: Digest::of_bytes(body.as_bytes()).as_str().to_owned(),
        }
    }
}

/// Arguments of the acceptance operation, as the interpreter would see them.
#[derive(JsonSchema, serde::Deserialize)]
#[allow(dead_code)]
struct AcceptTermsArgs {
    artifact: ArtifactRefSchema,
}

/// Schema twin of [`ArtifactRef`] for the act catalog.
#[derive(JsonSchema, serde::Deserialize)]
#[allow(dead_code)]
struct ArtifactRefSchema {
    document: String,
    version: String,
    digest: String,
}

// ---------------------------------------------------------------------------
// The consent workflow
// ---------------------------------------------------------------------------

/// One acceptance, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Acceptance {
    artifact: ArtifactRef,
    accepted_by: String,
    accepted_at: DateTime<Utc>,
}

/// State of one person's consent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ConsentState {
    accepted: Vec<Acceptance>,
}

impl ConsentState {
    /// The most recent acceptance for a document, whatever version it was.
    fn latest(&self, document: &str) -> Option<&Acceptance> {
        self.accepted
            .iter()
            .rev()
            .find(|a| a.artifact.document == document)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentPhase {
    /// Nothing on file for this document.
    NothingAccepted,
    /// An older version is on file and the published one is not.
    SupersededByNewVersion,
    /// The published version is on file.
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentObligation {
    /// Parameterized by document, so a workflow carrying several of them lists
    /// one obligation per outstanding document rather than one flag.
    AcceptDocument { document: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentCommand {
    AcceptTerms { artifact: ArtifactRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentEvent {
    TermsAccepted {
        artifact: ArtifactRef,
        accepted_by: String,
    },
}

/// Consent is never finished, so it has no terminal outcome. The associated
/// type still has to exist; nothing ever constructs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentOutcome {}

/// The consent workflow, holding the version that is currently published.
#[derive(Debug, Clone)]
struct ConsentWorkflow {
    published: ArtifactRef,
}

/// The card the person answers.
///
/// The artifact identity appears twice on purpose. In `metadata` it is part of
/// what the person saw, and therefore part of the payload hash that the
/// resolution's origin cites. In the option's `arguments` it is the server-side
/// meaning of the click, which is where the command is compiled from — the
/// traveler sends an option identifier and never an artifact.
fn acceptance_card(published: &ArtifactRef) -> InteractionPayload {
    InteractionPayload::new("Accept the electronic invoicing terms")
        .with_body("Read the terms, then accept them to continue.")
        .with_metadata(json!({ "artifact": published }))
        .with_option(InteractionOption::new(
            "accept",
            "I accept",
            StoredInteractionAction::ApplyOperation {
                operation: OperationKey::from("consent.accept"),
                arguments: json!({ "artifact": published }),
                freeform_argument: None,
            },
        ))
        .with_option(InteractionOption::new(
            "decline",
            "Not now",
            StoredInteractionAction::DeclineCommands,
        ))
}

impl WorkflowDefinition for ConsentWorkflow {
    type State = ConsentState;
    type Phase = ConsentPhase;
    type Obligation = ConsentObligation;
    type Command = ConsentCommand;
    type Event = ConsentEvent;
    type Outcome = ConsentOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("consent")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &ConsentPhase) -> PhaseOwnership {
        match phase {
            // Both open phases are the person's move, and both raise the same
            // card. Consent is never terminal: a new version reopens it.
            ConsentPhase::NothingAccepted | ConsentPhase::SupersededByNewVersion => {
                PhaseOwnership::User
            }
            ConsentPhase::Accepted => PhaseOwnership::System,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&ConsentState>) -> ViewOf<Self> {
        let version = self.version();
        let on_file = state.and_then(|s| s.latest(&self.published.document));
        match on_file {
            Some(acceptance) if acceptance.artifact == self.published => {
                WorkflowView::new(case_ref, version, ConsentPhase::Accepted)
            }
            other => {
                let phase = if other.is_some() {
                    ConsentPhase::SupersededByNewVersion
                } else {
                    ConsentPhase::NothingAccepted
                };
                WorkflowView::new(case_ref, version, phase)
                    .with_obligations([ConsentObligation::AcceptDocument {
                        document: self.published.document.clone(),
                    }])
                    .with_blocking_interaction(
                        InteractionRequirement::blocking(
                            "accept_terms",
                            InteractionKind::ConfirmCommand,
                        )
                        .with_payload(acceptance_card(&self.published))
                        // An acceptance cannot be inferred from prose, so the
                        // requirement keeps the conservative risk class and the
                        // default `Never` text resolution.
                        .with_confirms_risk(RiskClass::Irreversible),
                    )
            }
        }
    }

    fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
        vec![
            OperationSpec::new(OperationKey::from("consent.accept"))
                .summary("Record the person's acceptance of a published document")
                .arguments::<AcceptTermsArgs>()
                .target(TargetPolicy::RequiresExistingCase)
                .mutating(),
        ]
    }

    fn compile_act(
        &self,
        _state: Option<&ConsentState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<ConsentCommand>, DomainRejection> {
        match act.operation().map(OperationKey::as_str) {
            Some("consent.accept") => {
                let artifact: ArtifactRef =
                    serde_json::from_value(act.arguments["artifact"].clone()).map_err(|_| {
                        DomainRejection::new(
                            "consent.unreadable_artifact",
                            "consent.error.unreadable_artifact",
                        )
                    })?;
                Ok(vec![ConsentCommand::AcceptTerms { artifact }])
            }
            _ => Err(DomainRejection::new(
                "consent.unknown_operation",
                "consent.error.unknown_operation",
            )),
        }
    }

    fn command_policy(
        &self,
        _state: Option<&ConsentState>,
        command: &ConsentCommand,
    ) -> CommandPolicy {
        match command {
            // An explicit click, and nothing weaker. `conservative()` is
            // already exactly that.
            ConsentCommand::AcceptTerms { .. } => CommandPolicy::conservative(),
        }
    }

    fn validate_command(
        &self,
        state: Option<&ConsentState>,
        command: &ConsentCommand,
    ) -> Result<(), DomainRejection> {
        let ConsentCommand::AcceptTerms { artifact } = command;
        // The version in the command is checked against the version on the
        // wire. A command carrying an artifact nobody publishes any more — from
        // a card raised before a re-publication, or from a replayed request —
        // records nothing.
        if *artifact != self.published {
            return Err(DomainRejection::new(
                "consent.artifact_not_published",
                "consent.error.artifact_not_published",
            ));
        }
        if state.is_some_and(|s| {
            s.latest(&artifact.document)
                .is_some_and(|a| a.artifact == *artifact)
        }) {
            return Err(DomainRejection::new(
                "consent.already_accepted",
                "consent.error.already_accepted",
            ));
        }
        Ok(())
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<ConsentEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        events
            .iter()
            .filter_map(|event| match event {
                // An acceptance is the one thing this domain must never lose,
                // so the redacted arm is a hard error rather than softer copy.
                ReceiptEvent::Redacted(_) => None,
                ReceiptEvent::Committed(event) => Some(event),
            })
            .map(|event| {
                let ConsentEvent::TermsAccepted { artifact, .. } = &event.payload;
                let status_code = "consent.terms_accepted".to_owned();
                OperationalReceipt {
                    receipt_id: ReceiptId::derive(&[event.event_id], &status_code),
                    event_ids: vec![event.event_id],
                    severity: ReceiptSeverity::Success,
                    title: LocalizedText::new("Terms accepted"),
                    // The version is in the receipt because it is in the event;
                    // the narrator is not asked to remember it.
                    body: LocalizedText::new(format!(
                        "{} version {} accepted",
                        artifact.document, artifact.version
                    )),
                    status_code,
                    artifact_refs: vec![],
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/// An in-memory event ledger: committed events, and the state folded from them.
///
/// Nothing here is a Turnframe requirement; it is the smallest thing that can
/// stand in for the adopter's store while still making the retention point,
/// which is that the acceptance is a committed event and not a column somebody
/// can overwrite.
#[derive(Debug, Default)]
struct ConsentLedger {
    cases: Mutex<HashMap<(AccountId, CaseId), Versioned<ConsentState>>>,
    events: Mutex<Vec<CommittedEvent<ConsentEvent>>>,
}

impl ConsentLedger {
    /// Rebuilds a person's consent from committed events alone.
    fn fold(&self, person: &str) -> ConsentState {
        let events = self.events.lock().unwrap();
        let mut state = ConsentState::default();
        for event in events.iter() {
            let ConsentEvent::TermsAccepted {
                artifact,
                accepted_by,
            } = &event.payload;
            if accepted_by == person {
                state.accepted.push(Acceptance {
                    artifact: artifact.clone(),
                    accepted_by: accepted_by.clone(),
                    accepted_at: event.occurred_at,
                });
            }
        }
        state
    }
}

#[async_trait::async_trait]
impl WorkflowExecutor<ConsentWorkflow> for ConsentLedger {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<ConsentState>>, StoreError> {
        let cases = self.cases.lock().unwrap();
        Ok(match cases.get(&(account.clone(), case_id.clone())) {
            Some(v) => Versioned::new(Some(v.value.clone()), v.revision),
            None => Versioned::new(None, CaseRevision::ZERO),
        })
    }

    async fn execute(
        &self,
        batch: CommandBatch<ConsentCommand>,
    ) -> Result<Commit<ConsentState, ConsentEvent>, ExecutionError> {
        let mut cases = self.cases.lock().unwrap();
        let mut ledger = self.events.lock().unwrap();
        let envelope = batch
            .envelopes
            .first()
            .ok_or(ExecutionError::ScopeViolation)?;
        let key = (
            envelope.account_id().clone(),
            envelope.case_ref.case_id.clone(),
        );
        let current = cases
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Versioned::new(ConsentState::default(), CaseRevision::ZERO));
        if current.revision != envelope.case_ref.expected_revision {
            return Err(ExecutionError::RevisionConflict(RevisionConflict {
                expected: envelope.case_ref.clone(),
                current_revision: current.revision,
            }));
        }
        let mut state = current.value;
        let mut events = Vec::new();
        for envelope in &batch.envelopes {
            let ConsentCommand::AcceptTerms { artifact } = &envelope.command;
            let accepted_by = envelope.actor.user_id.to_string();
            state.accepted.push(Acceptance {
                artifact: artifact.clone(),
                accepted_by: accepted_by.clone(),
                accepted_at: at(10),
            });
            events.push(CommittedEvent {
                event_id: EventId::new(),
                event_type: "consent.terms_accepted".to_owned(),
                occurred_at: at(10),
                payload: ConsentEvent::TermsAccepted {
                    artifact: artifact.clone(),
                    accepted_by,
                },
            });
        }
        let new_revision = current.revision.next();
        cases.insert(key, Versioned::new(state.clone(), new_revision));
        ledger.extend(events.iter().cloned());
        Ok(Commit {
            state: Some(state),
            new_revision,
            events,
            idempotency_replay: false,
        })
    }
}

// ---------------------------------------------------------------------------
// The gated workflow
// ---------------------------------------------------------------------------

/// State of an trip that may not be sent until the terms are accepted.
///
/// `consent` is not stored on the trip. It is filled in by the executor when
/// the case is loaded, from the consent ledger, which is what makes the gate a
/// projection over committed state rather than a flag with its own lifetime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SendState {
    consent: Option<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SendPhase {
    BlockedOnConsent,
    ReadyToSend,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SendObligation {
    AcceptTermsElsewhere { document: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SendCommand {
    Send,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SendEvent {
    Sent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SendOutcome {}

/// The gated workflow, holding the version it requires.
#[derive(Debug, Clone)]
struct TripSendWorkflow {
    required: ArtifactRef,
}

impl WorkflowDefinition for TripSendWorkflow {
    type State = SendState;
    type Phase = SendPhase;
    type Obligation = SendObligation;
    type Command = SendCommand;
    type Event = SendEvent;
    type Outcome = SendOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("trip_send")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &SendPhase) -> PhaseOwnership {
        match phase {
            // Deliberately not `User`. A user-owned phase must raise a blocking
            // card, and the only card that would unblock this one belongs to
            // the consent case — binding it here would tie a legal acceptance
            // to an trip revision, which is the shape this recipe rejects.
            SendPhase::BlockedOnConsent => PhaseOwnership::External,
            SendPhase::ReadyToSend => PhaseOwnership::User,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&SendState>) -> ViewOf<Self> {
        let version = self.version();
        let current = state
            .and_then(|s| s.consent.as_ref())
            .is_some_and(|accepted| *accepted == self.required);
        if current {
            WorkflowView::new(case_ref, version, SendPhase::ReadyToSend).with_blocking_interaction(
                InteractionRequirement::blocking(
                    "send_confirmation",
                    InteractionKind::ConfirmCommand,
                )
                .with_payload(
                    InteractionPayload::new("Send this trip?")
                        .with_option(InteractionOption::new(
                            "send",
                            "Send",
                            StoredInteractionAction::ApplyOperation {
                                operation: OperationKey::from("trip.request_rebooking"),
                                arguments: serde_json::Value::Null,
                                freeform_argument: None,
                            },
                        ))
                        .with_option(InteractionOption::new(
                            "keep",
                            "Not yet",
                            StoredInteractionAction::DeclineCommands,
                        )),
                ),
            )
        } else {
            WorkflowView::new(case_ref, version, SendPhase::BlockedOnConsent)
                .with_obligations([SendObligation::AcceptTermsElsewhere {
                    document: self.required.document.clone(),
                }])
                .with_notice(WorkflowNotice {
                    code: "trip.terms_not_accepted".to_owned(),
                    severity: NoticeSeverity::Warning,
                    text: LocalizedText::new(
                        "The electronic invoicing terms have not been accepted.",
                    ),
                })
        }
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        // The gate is visible to the interpreter as an absent operation, not as
        // a refusal after the fact: while consent is missing, sending is not
        // part of the vocabulary at all.
        if view.phase != SendPhase::ReadyToSend {
            return Vec::new();
        }
        vec![
            OperationSpec::new(OperationKey::from("trip.request_rebooking"))
                .summary("Send the trip")
                .target(TargetPolicy::RequiresExistingCase)
                .mutating(),
        ]
    }

    fn compile_act(
        &self,
        _state: Option<&SendState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<SendCommand>, DomainRejection> {
        match act.operation().map(OperationKey::as_str) {
            Some("trip.request_rebooking") => Ok(vec![SendCommand::Send]),
            _ => Err(DomainRejection::new(
                "trip.unknown_operation",
                "trip.error.unknown_operation",
            )),
        }
    }

    fn command_policy(&self, _state: Option<&SendState>, _command: &SendCommand) -> CommandPolicy {
        CommandPolicy::conservative()
    }

    fn validate_command(
        &self,
        state: Option<&SendState>,
        _command: &SendCommand,
    ) -> Result<(), DomainRejection> {
        // Belt and braces: the projection already hides the operation, and the
        // validator refuses it anyway, because a gate that exists only in the
        // vocabulary is a gate that a replayed command walks straight through.
        let current = state
            .and_then(|s| s.consent.as_ref())
            .is_some_and(|accepted| *accepted == self.required);
        if current {
            Ok(())
        } else {
            Err(DomainRejection::new(
                "trip.terms_not_accepted",
                "trip.error.terms_not_accepted",
            ))
        }
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<SendEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        events
            .iter()
            .map(|event| {
                // The copy names no value out of the payload, so it stays true
                // whether or not the payload is still there.
                let status_code = "trip.sent".to_owned();
                OperationalReceipt {
                    receipt_id: ReceiptId::derive(&[event.event_id()], &status_code),
                    event_ids: vec![event.event_id()],
                    severity: ReceiptSeverity::Success,
                    title: LocalizedText::new("Trip sent"),
                    body: LocalizedText::new("The trip was sent."),
                    status_code,
                    artifact_refs: vec![],
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .expect("a valid time")
}

fn actor() -> ActorContext {
    ActorContext::new(ACCOUNT, PERSON)
}

/// The consent case: the workflow is `consent` and the case identifier is the
/// person. No trip appears anywhere in it.
fn consent_case(revision: u64) -> CaseRef {
    CaseRef::new("consent", PERSON, CaseRevision(revision))
}

/// Raises the card the projection asks for, as the runtime would.
fn raise_card(
    workflow: &ConsentWorkflow,
    state: Option<&ConsentState>,
    revision: u64,
) -> Interaction {
    let view = workflow.project(consent_case(revision), state);
    check_view(workflow, &view).expect("the consent projection satisfies the invariants");
    let requirement = view
        .blocking_interaction
        .as_ref()
        .expect("an open consent phase raises a card");
    let spec = workflow
        .build_interaction(state, &view, requirement)
        .expect("the requirement completes into a spec");
    Interaction::from_spec(
        spec,
        InteractionId::new(),
        AccountId::from(ACCOUNT),
        ConversationId::new(),
        TurnId::new(),
        at(0),
    )
    .expect("the acceptance card is answerable")
}

/// Answers a card with one of its options, through the same validation the
/// runtime uses.
fn click(card: &Interaction, option: &str, revision: u64) -> AcceptedResponse {
    let response = InteractionResponse {
        interaction_id: card.id,
        option_id: OptionId::from(option),
        expected_case_revision: CaseRevision(revision),
        freeform_input: None,
    };
    validate_response(
        card,
        &response,
        ResolutionChannel::Click,
        &actor(),
        &card.conversation_id,
        CaseRevision(revision),
        at(1),
    )
    .expect("the click is accepted")
}

/// Builds the resolved act from the *stored* option, which is where the
/// server-side meaning of a click lives.
fn act_from(accepted: &AcceptedResponse) -> ResolvedAct {
    let StoredInteractionAction::ApplyOperation {
        freeform_argument: _,
        operation,
        arguments,
    } = &accepted.action
    else {
        panic!("the accepting option applies an operation");
    };
    ResolvedAct {
        act: ActId::new(UnitId(1), 1),
        kind: ResolvedActKind::ApplyOperation {
            operation: operation.clone(),
        },
        case_ref: accepted.case_ref.clone(),
        arguments: arguments.clone(),
        evidence_digest: Digest::of_bytes(b"card-click"),
    }
}

/// Wraps commands into the batch an executor receives.
fn batch(
    commands: Vec<ConsentCommand>,
    origin: CommandOrigin,
    revision: u64,
) -> CommandBatch<ConsentCommand> {
    let turn_id = TurnId::new();
    CommandBatch {
        batch_id: BatchId::new(),
        scope: AtomicityScope::PerCase,
        envelopes: commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| CommandEnvelope {
                command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), index),
                turn_id,
                actor: actor(),
                case_ref: consent_case(revision),
                idempotency_key: IdempotencyKey::new(format!("consent-accept-{index}")),
                origin: origin.clone(),
                command,
            })
            .collect(),
    }
}

fn v1() -> ArtifactRef {
    ArtifactRef::published("2026-01", "the terms as published in January")
}

fn v2() -> ArtifactRef {
    ArtifactRef::published("2026-07", "the terms as published in July")
}

// ---------------------------------------------------------------------------
// The recipe
// ---------------------------------------------------------------------------

/// The case of a consent workflow is the person, not the thing being gated.
///
/// This is the move that makes the rest work. The acceptance is a fact about
/// the account, so it lives on a case whose identifier is the account's, and
/// nothing about it references an trip.
#[test]
fn the_consent_case_is_the_person_not_the_thing_being_gated() {
    let workflow = ConsentWorkflow { published: v1() };
    let view = workflow.project(consent_case(0), None);
    check_view(&workflow, &view).expect("the projection satisfies the invariants");

    assert_eq!(view.case_ref.workflow.as_str(), "consent");
    assert_eq!(view.case_ref.case_id.as_str(), PERSON);
    assert_eq!(view.phase, ConsentPhase::NothingAccepted);
    assert_eq!(
        view.obligations,
        vec![ConsentObligation::AcceptDocument {
            document: TERMS.to_owned()
        }]
    );

    let card = raise_card(&workflow, None, 0);
    assert_eq!(card.kind, InteractionKind::ConfirmCommand);
    assert_eq!(card.case_ref.case_id.as_str(), PERSON);
    // It binds to a revision like every other card. The revision it binds to is
    // the consent case's, which moves when the person's consent changes and at
    // no other time.
    assert_eq!(card.bound_revision(), Some(CaseRevision(0)));
}

/// The artifact version is inside the hash the resolution cites.
///
/// Two cards for two versions of the same document are different cards, and the
/// origin minted by answering one carries a hash that the other cannot produce.
/// That is what "the artifact version stays bound to the resolution" means
/// mechanically, rather than as an intention.
#[test]
fn the_card_binds_the_artifact_version_into_its_hash() {
    let january = ConsentWorkflow { published: v1() };
    let july = ConsentWorkflow { published: v2() };

    let for_v1 = raise_card(&january, None, 0);
    let for_v2 = raise_card(&july, None, 0);
    assert_ne!(
        for_v1.payload_hash, for_v2.payload_hash,
        "a different version must be a different card"
    );

    // Same version, re-published under the same number with different bytes:
    // the digest differs, so the hash differs too.
    let tampered = ConsentWorkflow {
        published: ArtifactRef::published("2026-01", "the terms, quietly edited"),
    };
    assert_ne!(
        for_v1.payload_hash,
        raise_card(&tampered, None, 0).payload_hash,
        "a re-publication under the same version number must not reuse the card"
    );

    let accepted = click(&for_v1, "accept", 0);
    let origin = accepted.origin().expect("accepting authorizes the command");
    let CommandOrigin::ConfirmedInteraction { payload_hash, .. } = &origin else {
        panic!("a click on a stored option mints a confirmed-interaction origin");
    };
    assert_eq!(
        *payload_hash, for_v1.payload_hash,
        "the origin cites the hash of the card the person actually saw"
    );
    assert!(
        origin_satisfies(
            &origin,
            &january.command_policy(None, &ConsentCommand::AcceptTerms { artifact: v1() })
        ),
        "an explicit click satisfies the acceptance policy"
    );
}

/// The command is compiled from the stored option, and the event records the
/// artifact identity the command carried.
///
/// The retention rule is then the event ledger's: rebuilding from committed
/// events alone reproduces which version was accepted and by whom, with no
/// column anybody could overwrite.
#[tokio::test]
async fn accepting_commits_the_artifact_identity_into_the_ledger() {
    let workflow = ConsentWorkflow { published: v1() };
    let ledger = ConsentLedger::default();

    let card = raise_card(&workflow, None, 0);
    let accepted = click(&card, "accept", 0);
    let act = act_from(&accepted);
    let view = workflow.project(consent_case(0), None);
    let commands = workflow
        .compile_act(None, &view, &act)
        .expect("the stored option compiles into a command");
    assert_eq!(
        commands,
        vec![ConsentCommand::AcceptTerms { artifact: v1() }],
        "the command carries the version, and it came from the server's option"
    );
    workflow
        .validate_command(None, &commands[0])
        .expect("the published version validates");

    let origin = accepted.origin().expect("accepting authorizes the command");
    let commit = ledger
        .execute(batch(commands, origin, 0))
        .await
        .expect("the acceptance commits");

    assert_eq!(commit.new_revision, CaseRevision(1));
    assert_eq!(commit.events.len(), 1);
    let ConsentEvent::TermsAccepted {
        artifact,
        accepted_by,
    } = &commit.events[0].payload;
    assert_eq!(
        *artifact,
        v1(),
        "the event names the version, not just the fact"
    );
    assert_eq!(accepted_by, PERSON);

    // The receipt is derived from the event, so the version reaches the user
    // without the narrator being trusted to remember it.
    let receipts = workflow.receipts(&commit.receipt_events(), &Locale::from("en"));
    assert_eq!(receipts.len(), 1);
    assert!(
        receipts[0]
            .body
            .resolve(&Locale::from("en"))
            .contains("2026-01")
    );
    assert_eq!(receipts[0].event_ids, vec![commit.events[0].event_id]);

    // Retention: the ledger alone reproduces the acceptance.
    let rebuilt = ledger.fold(PERSON);
    assert_eq!(
        rebuilt.latest(TERMS).map(|a| a.artifact.clone()),
        Some(v1())
    );
}

/// A card raised for one version cannot be turned into an acceptance of
/// another.
///
/// The click's stored option names the version, and the validator checks it
/// against what is published. A card that outlived a re-publication compiles
/// into a command that records nothing.
#[test]
fn a_card_raised_for_the_old_version_cannot_accept_the_new_one() {
    let january = ConsentWorkflow { published: v1() };
    let july = ConsentWorkflow { published: v2() };

    let stale_card = raise_card(&january, None, 0);
    let accepted = click(&stale_card, "accept", 0);
    let act = act_from(&accepted);
    let view = july.project(consent_case(0), None);
    let commands = july
        .compile_act(None, &view, &act)
        .expect("the stale card still compiles");
    assert_eq!(
        commands,
        vec![ConsentCommand::AcceptTerms { artifact: v1() }],
        "the command carries the version the person saw, not the one now published"
    );
    let rejection = july
        .validate_command(None, &commands[0])
        .expect_err("accepting a version that is no longer published is refused");
    assert_eq!(rejection.code.as_str(), "consent.artifact_not_published");
}

/// A new version reopens the obligation, even though consent is on file.
#[test]
fn a_new_version_reopens_the_obligation() {
    let january = ConsentWorkflow { published: v1() };
    let state = ConsentState {
        accepted: vec![Acceptance {
            artifact: v1(),
            accepted_by: PERSON.to_owned(),
            accepted_at: at(10),
        }],
    };
    let settled = january.project(consent_case(1), Some(&state));
    check_view(&january, &settled).expect("the projection satisfies the invariants");
    assert_eq!(settled.phase, ConsentPhase::Accepted);
    assert!(settled.obligations.is_empty());
    assert!(settled.blocking_interaction.is_none());

    let july = ConsentWorkflow { published: v2() };
    let reopened = july.project(consent_case(1), Some(&state));
    check_view(&july, &reopened).expect("the projection satisfies the invariants");
    assert_eq!(
        reopened.phase,
        ConsentPhase::SupersededByNewVersion,
        "an acceptance of the old version is not an acceptance of the new one"
    );
    assert_eq!(
        reopened.obligations,
        vec![ConsentObligation::AcceptDocument {
            document: TERMS.to_owned()
        }]
    );
    let card = raise_card(&july, Some(&state), 1);
    let metadata: ArtifactRef = serde_json::from_value(card.payload.metadata["artifact"].clone())
        .expect("the card names an artifact");
    assert_eq!(metadata, v2(), "the reopened card is about the new version");
}

/// The gate on another workflow is a projection over committed consent.
///
/// It is not a boolean on the trip, and the trip never raises a consent
/// card: the blocked phase carries an obligation and a notice, and the send
/// operation is not in the vocabulary at all until the required version is on
/// file.
#[tokio::test]
async fn the_gate_is_a_projection_over_committed_consent() {
    let consent = ConsentWorkflow { published: v2() };
    let ledger = ConsentLedger::default();
    let send = TripSendWorkflow { required: v2() };
    let trip_case = CaseRef::new("trip_send", "trip-1", CaseRevision(3));

    // Nothing accepted: blocked, with the obligation pointing elsewhere.
    let blocked = send.project(trip_case.clone(), Some(&SendState::default()));
    check_view(&send, &blocked).expect("the projection satisfies the invariants");
    assert_eq!(blocked.phase, SendPhase::BlockedOnConsent);
    assert!(
        blocked.blocking_interaction.is_none(),
        "the trip must not raise the consent card; that card belongs to the person's case"
    );
    assert_eq!(
        blocked.obligations,
        vec![SendObligation::AcceptTermsElsewhere {
            document: TERMS.to_owned()
        }]
    );
    assert!(send.operations(&blocked).is_empty());
    assert!(
        send.validate_command(Some(&SendState::default()), &SendCommand::Send)
            .is_err()
    );

    // An acceptance of the *previous* version does not open the gate.
    let stale = SendState {
        consent: Some(v1()),
    };
    assert_eq!(
        send.project(trip_case.clone(), Some(&stale)).phase,
        SendPhase::BlockedOnConsent,
        "the gate is on the version, not on the existence of some acceptance"
    );

    // Accept the published version on the consent case, then read the gate off
    // the committed events.
    let card = raise_card(&consent, None, 0);
    let accepted = click(&card, "accept", 0);
    let act = act_from(&accepted);
    let view = consent.project(consent_case(0), None);
    let commands = consent
        .compile_act(None, &view, &act)
        .expect("the option compiles");
    let origin = accepted.origin().expect("accepting authorizes the command");
    ledger
        .execute(batch(commands, origin, 0))
        .await
        .expect("the acceptance commits");

    let loaded = SendState {
        consent: ledger
            .fold(PERSON)
            .latest(TERMS)
            .map(|a| a.artifact.clone()),
    };
    let open = send.project(trip_case, Some(&loaded));
    check_view(&send, &open).expect("the projection satisfies the invariants");
    assert_eq!(open.phase, SendPhase::ReadyToSend);
    assert!(open.obligations.is_empty());
    assert_eq!(send.operations(&open).len(), 1);
    send.validate_command(Some(&loaded), &SendCommand::Send)
        .expect("with consent on file the send validates");
}

/// Accepting authorizes nothing about the gated case.
///
/// The origin minted by the acceptance is bound to the consent card, and the
/// send has its own confirmation. This is the property that makes a shared
/// acceptance card impossible to abuse, and it is also why the acceptance did
/// not need a case reference to an trip in the first place.
#[test]
fn an_acceptance_authorizes_nothing_about_the_gated_case() {
    let consent = ConsentWorkflow { published: v1() };
    let card = raise_card(&consent, None, 0);
    let accepted = click(&card, "accept", 0);

    assert_eq!(
        accepted.case_ref.workflow.as_str(),
        "consent",
        "the resolution is about the consent case and says so"
    );
    let origin = accepted.origin().expect("accepting authorizes the command");
    let CommandOrigin::ConfirmedInteraction { interaction_id, .. } = &origin else {
        panic!("a click mints a confirmed-interaction origin");
    };
    assert_eq!(*interaction_id, card.id);

    // Declining is a resolution too, and it authorizes nothing at all.
    let declined = click(&raise_card(&consent, None, 0), "decline", 0);
    assert!(
        declined.origin().is_none(),
        "declining must not mint an origin"
    );
}
