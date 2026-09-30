//! A toy "note" workflow proving that a typed `WorkflowDefinition` plus
//! `WorkflowExecutor` compile, register through `TypedWorkflowAdapter`, and
//! project/compile/execute through the erased path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use turnframe_core::error::{ErasedCallError, ErasureError, InvariantViolationKind};
use turnframe_core::prelude::*;
use turnframe_core::response::{
    AssistantTurn, ReceiptBlock, ReplayToken, ResponseBlock, claim_guard,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NoteState {
    text: Option<String>,
    published: bool,
    archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NotePhase {
    Empty,
    AwaitingPublish,
    Published,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NoteObligation {
    WriteText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NoteCommand {
    SetText { value: String },
    Publish,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NoteEvent {
    TextSet { value: String },
    Published,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NoteOutcome {
    Archived,
}

#[derive(JsonSchema, serde::Deserialize)]
#[allow(dead_code)]
struct SetTextArgs {
    value: String,
}

#[derive(Debug, Default)]
struct NoteWorkflow {
    /// When set, the definition lies about ownership to exercise invariants.
    broken_ownership: bool,
}

impl WorkflowDefinition for NoteWorkflow {
    type State = NoteState;
    type Phase = NotePhase;
    type Obligation = NoteObligation;
    type Command = NoteCommand;
    type Event = NoteEvent;
    type Outcome = NoteOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("note")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &NotePhase) -> PhaseOwnership {
        if self.broken_ownership {
            return PhaseOwnership::System;
        }
        match phase {
            NotePhase::Empty | NotePhase::AwaitingPublish => PhaseOwnership::User,
            NotePhase::Published => PhaseOwnership::System,
            NotePhase::Archived => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&NoteState>) -> ViewOf<Self> {
        let version = self.version();
        match state {
            Some(s) if s.archived => WorkflowView::new(case_ref, version, NotePhase::Archived)
                .with_outcome(NoteOutcome::Archived),
            Some(s) if s.published => WorkflowView::new(case_ref, version, NotePhase::Published),
            Some(s) if s.text.is_some() => {
                let payload = InteractionPayload::new("Publish this note?")
                    .with_option(InteractionOption::new(
                        "publish",
                        "Publish",
                        StoredInteractionAction::ApplyOperation {
                            operation: OperationKey::from("note.publish"),
                            arguments: serde_json::Value::Null,
                            freeform_argument: None,
                        },
                    ))
                    .with_option(InteractionOption::new(
                        "keep_draft",
                        "Keep it a draft",
                        StoredInteractionAction::DeclineCommands,
                    ));
                WorkflowView::new(case_ref, version, NotePhase::AwaitingPublish)
                    .with_blocking_interaction(
                        InteractionRequirement::blocking(
                            "publish_confirmation",
                            InteractionKind::ConfirmCommand,
                        )
                        .with_payload(payload),
                    )
            }
            _ => WorkflowView::new(case_ref, version, NotePhase::Empty)
                .with_obligations([NoteObligation::WriteText])
                .with_blocking_interaction(
                    InteractionRequirement::blocking("write_text", InteractionKind::Freeform)
                        .with_payload(
                            InteractionPayload::new("What should the note say?")
                                .with_freeform_prompt("Write the note")
                                .with_option(
                                    InteractionOption::new(
                                        "set_text",
                                        "Save",
                                        StoredInteractionAction::Custom {
                                            key: "note.set_text".into(),
                                            payload: serde_json::Value::Null,
                                        },
                                    )
                                    .with_freeform(FreeformPolicy::Required { max_len: 280 }),
                                ),
                        )
                        // Writing a draft note is reversible, which is what
                        // lets the card accept typed text at all.
                        .with_confirms_risk(RiskClass::ReversibleLowRisk)
                        .with_text_resolution(TextResolutionPolicy::ModelInterpretedLowRisk),
                ),
        }
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        let mut acts = vec![
            OperationSpec::new(OperationKey::from("note.set_text"))
                .summary("Set the note text")
                .arguments::<SetTextArgs>()
                .target(TargetPolicy::AllowsNewCase)
                .mutating(),
        ];
        if view.phase == NotePhase::AwaitingPublish {
            acts.push(
                OperationSpec::new(OperationKey::from("note.publish"))
                    .summary("Publish the note")
                    .target(TargetPolicy::RequiresExistingCase)
                    .mutating(),
            );
        }
        acts
    }

    fn compile_act(
        &self,
        _state: Option<&NoteState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<NoteCommand>, DomainRejection> {
        match act.operation().map(OperationKey::as_str) {
            Some("note.set_text") => {
                let value = act.arguments["value"].as_str().ok_or_else(|| {
                    DomainRejection::new("note.missing_value", "note.error.missing_value")
                })?;
                Ok(vec![NoteCommand::SetText {
                    value: value.to_owned(),
                }])
            }
            Some("note.publish") => Ok(vec![NoteCommand::Publish]),
            Some("note.archive") => Ok(vec![NoteCommand::Archive]),
            _ => Err(DomainRejection::new(
                "note.unknown_operation",
                "note.error.unknown_operation",
            )),
        }
    }

    fn command_policy(&self, _state: Option<&NoteState>, command: &NoteCommand) -> CommandPolicy {
        match command {
            NoteCommand::SetText { .. } => CommandPolicy::low_risk(),
            NoteCommand::Publish => CommandPolicy::conservative(),
            NoteCommand::Archive => CommandPolicy {
                risk: RiskClass::Destructive,
                ..CommandPolicy::conservative()
            },
        }
    }

    fn validate_command(
        &self,
        state: Option<&NoteState>,
        command: &NoteCommand,
    ) -> Result<(), DomainRejection> {
        match command {
            NoteCommand::Publish if state.and_then(|s| s.text.as_ref()).is_none() => {
                Err(DomainRejection::new("note.no_text", "note.error.no_text"))
            }
            _ => Ok(()),
        }
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<NoteEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        events
            .iter()
            .map(|event| match event {
                ReceiptEvent::Committed(e) => {
                    let status_code: String = match e.payload {
                        NoteEvent::TextSet { .. } => "note.text_set".into(),
                        NoteEvent::Published => "note.published".into(),
                        NoteEvent::Archived => "note.archived".into(),
                    };
                    OperationalReceipt {
                        receipt_id: ReceiptId::derive(&[e.event_id], &status_code),
                        event_ids: vec![e.event_id],
                        severity: ReceiptSeverity::Success,
                        title: LocalizedText::new("Note"),
                        body: LocalizedText::new(match e.payload {
                            NoteEvent::TextSet { .. } => "Text set",
                            NoteEvent::Published => "Published",
                            NoteEvent::Archived => "Archived",
                        }),
                        status_code,
                        artifact_refs: vec![],
                    }
                }
                // The event is still in the ledger and still backs the claim;
                // what it recorded is gone, and the copy says so.
                ReceiptEvent::Redacted(e) => {
                    let status_code = "note.detail_erased".to_owned();
                    OperationalReceipt {
                        receipt_id: ReceiptId::derive(&[e.event_id], &status_code),
                        event_ids: vec![e.event_id],
                        severity: ReceiptSeverity::Info,
                        title: LocalizedText::new("Note"),
                        body: LocalizedText::new("This step is on record; its detail was erased."),
                        status_code,
                        artifact_refs: vec![],
                    }
                }
            })
            .collect()
    }
}

/// One journalled execution: the command as written, and what it produced.
type JournalEntry = (serde_json::Value, Commit<NoteState, NoteEvent>);

#[derive(Debug, Default)]
struct MemoryExecutor {
    states: Mutex<HashMap<(AccountId, CaseId), Versioned<NoteState>>>,
    /// The idempotency journal of I14: a key seen again returns the original
    /// commit instead of repeating the effect.
    journal: Mutex<HashMap<IdempotencyKey, JournalEntry>>,
}

#[async_trait::async_trait]
impl WorkflowExecutor<NoteWorkflow> for MemoryExecutor {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<NoteState>>, StoreError> {
        let states = self.states.lock().unwrap();
        Ok(match states.get(&(account.clone(), case_id.clone())) {
            Some(v) => Versioned::new(Some(v.value.clone()), v.revision),
            None => Versioned::new(None, CaseRevision::ZERO),
        })
    }

    async fn execute(
        &self,
        batch: CommandBatch<NoteCommand>,
    ) -> Result<Commit<NoteState, NoteEvent>, ExecutionError> {
        let mut states = self.states.lock().unwrap();
        let mut journal = self.journal.lock().unwrap();
        let first = batch
            .envelopes
            .first()
            .ok_or(ExecutionError::ScopeViolation)?;
        // I14: the same key with the same command replays, the same key with a
        // different command is a defect.
        if let Some((command, commit)) = journal.get(&first.idempotency_key) {
            let requested =
                serde_json::to_value(&first.command).map_err(|_| ExecutionError::Other {
                    code: "serialize".into(),
                })?;
            if command != &requested {
                return Err(ExecutionError::IdempotencyMismatch {
                    command_id: first.command_id,
                });
            }
            return Ok(Commit {
                idempotency_replay: true,
                ..commit.clone()
            });
        }
        let key = (first.account_id().clone(), first.case_ref.case_id.clone());
        let current = states
            .get(&key)
            .cloned()
            .unwrap_or(Versioned::new(NoteState::default(), CaseRevision::ZERO));
        if current.revision != first.case_ref.expected_revision {
            return Err(ExecutionError::RevisionConflict(RevisionConflict {
                expected: first.case_ref.clone(),
                current_revision: current.revision,
            }));
        }
        let mut state = current.value;
        let mut events = Vec::new();
        for envelope in &batch.envelopes {
            let event = match &envelope.command {
                NoteCommand::SetText { value } => {
                    state.text = Some(value.clone());
                    NoteEvent::TextSet {
                        value: value.clone(),
                    }
                }
                NoteCommand::Publish => {
                    state.published = true;
                    NoteEvent::Published
                }
                NoteCommand::Archive => {
                    state.archived = true;
                    NoteEvent::Archived
                }
            };
            events.push(CommittedEvent {
                event_id: EventId::new(),
                event_type: format!(
                    "note.{}",
                    serde_json::to_value(&event)
                        .unwrap()
                        .as_object()
                        .and_then(|o| o.keys().next().cloned())
                        .unwrap_or_else(|| "event".into())
                ),
                occurred_at: Utc::now(),
                payload: event,
            });
        }
        let new_revision = current.revision.next();
        states.insert(key, Versioned::new(state.clone(), new_revision));
        let commit = Commit {
            state: Some(state),
            new_revision,
            events,
            idempotency_replay: false,
        };
        let command = serde_json::to_value(&first.command).map_err(|_| ExecutionError::Other {
            code: "serialize".into(),
        })?;
        journal.insert(first.idempotency_key.clone(), (command, commit.clone()));
        Ok(commit)
    }
}

fn registry() -> WorkflowRegistry {
    WorkflowRegistry::builder()
        .register(NoteWorkflow::default(), MemoryExecutor::default())
        .build()
        .unwrap()
}

fn case(revision: u64) -> CaseRef {
    CaseRef::new("note", "n1", CaseRevision(revision))
}

#[test]
fn registry_lookup_without_knowing_concrete_types() {
    let registry = registry();
    assert_eq!(registry.len(), 1);
    let entry = registry.require(&WorkflowKey::from("note")).unwrap();
    assert_eq!(entry.version, WorkflowVersion::from("1"));
    assert!(
        registry
            .check_version(&WorkflowKey::from("note"), &WorkflowVersion::from("1"))
            .is_ok()
    );
    assert!(matches!(
        registry.check_version(&WorkflowKey::from("note"), &WorkflowVersion::from("2")),
        Err(ErasureError::VersionMismatch { .. })
    ));
    assert!(matches!(
        registry.require(&WorkflowKey::from("nope")),
        Err(ErasureError::UnknownWorkflow { .. })
    ));
    let duplicate = WorkflowRegistry::builder()
        .register(NoteWorkflow::default(), MemoryExecutor::default())
        .register(NoteWorkflow::default(), MemoryExecutor::default())
        .build();
    assert!(matches!(
        duplicate,
        Err(ErasureError::DuplicateWorkflow { .. })
    ));
}

#[test]
fn erased_projection_matches_typed_projection_and_invariants() {
    let registry = registry();
    let erased = registry.require(&WorkflowKey::from("note")).unwrap();
    let view = erased.definition.project(case(0), None).unwrap();
    assert_eq!(view.phase, json!("empty"));
    assert!(view.is_user_owned());
    assert_eq!(view.obligations.len(), 1);
    assert_eq!(view.obligations[0].id.as_str(), "\"write_text\"");
    assert_eq!(
        view.blocking_interaction.as_ref().unwrap().kind,
        InteractionKind::Freeform
    );
    assert!(check_erased_view(&view).is_ok());

    let drafted = json!({"text": "hello", "published": false, "archived": false});
    let view = erased.definition.project(case(1), Some(&drafted)).unwrap();
    assert_eq!(view.phase, json!("awaiting_publish"));
    let spec = erased
        .definition
        .build_interaction(
            case(1),
            Some(&drafted),
            view.blocking_interaction.as_ref().unwrap(),
        )
        .unwrap();
    assert_eq!(spec.kind, InteractionKind::ConfirmCommand);
    assert_eq!(spec.payload.options[0].id, OptionId::from("publish"));
    assert_eq!(spec.case_ref, case(1));

    let archived = json!({"text": "hello", "published": true, "archived": true});
    let view = erased.definition.project(case(3), Some(&archived)).unwrap();
    assert!(view.is_complete());
    assert_eq!(view.phase_ownership, PhaseOwnership::Terminal);
    assert!(check_erased_view(&view).is_ok());

    let typed = NoteWorkflow::default();
    for state in [
        None,
        Some(NoteState::default()),
        Some(NoteState {
            text: Some("x".into()),
            ..NoteState::default()
        }),
    ] {
        let view = typed.project(case(0), state.as_ref());
        assert!(check_view(&typed, &view).is_ok());
    }
    let broken = NoteWorkflow {
        broken_ownership: true,
    };
    let view = broken.project(case(0), None);
    let violations = check_view(&broken, &view).unwrap_err();
    assert_eq!(violations.len(), 1);
    assert_eq!(
        violations[0].kind,
        InvariantViolationKind::BlockingInteractionOnNonUserPhase
    );

    let bad_state = json!({"text": 5});
    assert!(matches!(
        erased.definition.project(case(0), Some(&bad_state)),
        Err(ErasureError::StateDeserialization { .. })
    ));
}

#[test]
fn erased_catalog_compile_policy_and_validation() {
    let registry = registry();
    let erased = registry.require(&WorkflowKey::from("note")).unwrap();
    let catalog = erased.definition.operations(case(0), None).unwrap();
    assert_eq!(catalog.len(), 1);
    assert!(catalog[0].check_arguments(&json!({"value": "hi"})).is_ok());
    assert!(catalog[0].check_arguments(&json!({"value": 1})).is_err());

    let act = ResolvedAct {
        act: ActId::new(UnitId(1), 1),
        kind: ResolvedActKind::ApplyOperation {
            operation: OperationKey::from("note.set_text"),
        },
        case_ref: case(0),
        arguments: json!({"value": "hi"}),
        evidence_digest: Digest::of_bytes(b"e"),
    };
    let commands = erased.definition.compile_act(case(0), None, &act).unwrap();
    assert_eq!(commands, vec![json!({"set_text": {"value": "hi"}})]);
    assert_eq!(
        erased
            .definition
            .command_policy(None, &commands[0])
            .unwrap(),
        CommandPolicy::low_risk()
    );
    assert!(
        erased
            .definition
            .validate_command(None, &commands[0])
            .is_ok()
    );
    assert!(matches!(
        erased.definition.validate_command(None, &json!("publish")),
        Err(ErasedCallError::Rejected(r)) if r.code == RejectionCode::from("note.no_text")
    ));
    assert!(matches!(
        erased.definition.command_policy(None, &json!({"nope": 1})),
        Err(ErasureError::CommandDeserialization { .. })
    ));
    let unknown = ResolvedAct {
        kind: ResolvedActKind::ApplyOperation {
            operation: OperationKey::from("note.explode"),
        },
        ..act.clone()
    };
    assert!(matches!(
        erased.definition.compile_act(case(0), None, &unknown),
        Err(ErasedCallError::Rejected(_))
    ));

    // An act resolved to one case may not be compiled against another case, or
    // against another revision of the same one (I13, §12.2).
    for projected in [CaseRef::new("note", "n2", CaseRevision(0)), case(1)] {
        assert!(
            matches!(
                erased.definition.compile_act(projected, None, &act),
                Err(ErasedCallError::Erasure(ErasureError::CaseMismatch { .. }))
            ),
            "compiling against a different case must be refused"
        );
    }
}

#[test]
fn an_unanswerable_or_misbound_card_never_leaves_the_boundary() {
    /// A definition whose `build_interaction` misbehaves in one chosen way.
    struct RogueWorkflow(&'static str);

    impl WorkflowDefinition for RogueWorkflow {
        type State = ();
        type Phase = ();
        type Obligation = NoteObligation;
        type Command = NoteCommand;
        type Event = NoteEvent;
        type Outcome = NoteOutcome;

        fn key(&self) -> WorkflowKey {
            WorkflowKey::from("rogue")
        }
        fn version(&self) -> WorkflowVersion {
            WorkflowVersion::from("1")
        }
        fn phase_ownership(&self, _phase: &()) -> PhaseOwnership {
            PhaseOwnership::User
        }
        fn project(&self, case_ref: CaseRef, _state: Option<&()>) -> ViewOf<Self> {
            WorkflowView::new(case_ref, self.version(), ()).with_blocking_interaction(
                InteractionRequirement::blocking("ask", InteractionKind::Boolean),
            )
        }
        fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
            vec![]
        }
        fn compile_act(
            &self,
            _state: Option<&()>,
            _view: &ViewOf<Self>,
            _act: &ResolvedAct,
        ) -> Result<Vec<NoteCommand>, DomainRejection> {
            Ok(vec![])
        }
        fn command_policy(&self, _state: Option<&()>, _command: &NoteCommand) -> CommandPolicy {
            CommandPolicy::conservative()
        }
        fn validate_command(
            &self,
            _state: Option<&()>,
            _command: &NoteCommand,
        ) -> Result<(), DomainRejection> {
            Ok(())
        }
        fn receipts(
            &self,
            _events: &[ReceiptEvent<NoteEvent>],
            _locale: &Locale,
        ) -> Vec<OperationalReceipt> {
            vec![]
        }
        fn build_interaction(
            &self,
            _state: Option<&()>,
            view: &ViewOf<Self>,
            requirement: &InteractionRequirement,
        ) -> Result<InteractionSpec, DomainRejection> {
            let mut spec = requirement.to_spec(view.case_ref.clone());
            match self.0 {
                // A Boolean card needs exactly two options; the default payload
                // has none.
                "unanswerable" => {}
                _ => {
                    spec.payload = InteractionPayload::new("Ready?")
                        .with_option(InteractionOption::new(
                            "yes",
                            "Yes",
                            StoredInteractionAction::ConfirmCommands {
                                command_refs: vec![],
                            },
                        ))
                        .with_option(InteractionOption::new(
                            "no",
                            "No",
                            StoredInteractionAction::DeclineCommands,
                        ));
                    spec.case_ref = CaseRef::new("rogue", "somewhere_else", CaseRevision(9));
                }
            }
            Ok(spec)
        }
    }

    let case_ref = CaseRef::new("rogue", "r1", CaseRevision(0));
    let requirement = InteractionRequirement::blocking("ask", InteractionKind::Boolean);
    let unanswerable = TypedWorkflowAdapter::new(RogueWorkflow("unanswerable"), ());
    let err =
        ErasedWorkflow::build_interaction(&unanswerable, case_ref.clone(), None, &requirement)
            .unwrap_err();
    assert!(matches!(err, ErasedCallError::InvalidSpec(_)), "{err:?}");

    let misbound = TypedWorkflowAdapter::new(RogueWorkflow("misbound"), ());
    let err =
        ErasedWorkflow::build_interaction(&misbound, case_ref, None, &requirement).unwrap_err();
    assert!(
        matches!(
            err,
            ErasedCallError::Erasure(ErasureError::CaseMismatch { .. })
        ),
        "{err:?}"
    );
}

#[test]
fn an_obligation_that_cannot_be_serialized_is_a_map_defect() {
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Opaque;

    impl Serialize for Opaque {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("obligations must be data"))
        }
    }

    impl<'de> Deserialize<'de> for Opaque {
        fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
            Err(serde::de::Error::custom("obligations must be data"))
        }
    }

    struct OpaqueWorkflow;

    impl WorkflowDefinition for OpaqueWorkflow {
        type State = ();
        type Phase = ();
        type Obligation = Opaque;
        type Command = NoteCommand;
        type Event = NoteEvent;
        type Outcome = NoteOutcome;

        fn key(&self) -> WorkflowKey {
            WorkflowKey::from("opaque")
        }
        fn version(&self) -> WorkflowVersion {
            WorkflowVersion::from("1")
        }
        fn phase_ownership(&self, _phase: &()) -> PhaseOwnership {
            PhaseOwnership::System
        }
        fn project(&self, case_ref: CaseRef, _state: Option<&()>) -> ViewOf<Self> {
            WorkflowView::new(case_ref, self.version(), ()).with_obligations([Opaque])
        }
        fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
            vec![]
        }
        fn compile_act(
            &self,
            _state: Option<&()>,
            _view: &ViewOf<Self>,
            _act: &ResolvedAct,
        ) -> Result<Vec<NoteCommand>, DomainRejection> {
            Ok(vec![])
        }
        fn command_policy(&self, _state: Option<&()>, _command: &NoteCommand) -> CommandPolicy {
            CommandPolicy::conservative()
        }
        fn validate_command(
            &self,
            _state: Option<&()>,
            _command: &NoteCommand,
        ) -> Result<(), DomainRejection> {
            Ok(())
        }
        fn receipts(
            &self,
            _events: &[ReceiptEvent<NoteEvent>],
            _locale: &Locale,
        ) -> Vec<OperationalReceipt> {
            vec![]
        }
    }

    let workflow = OpaqueWorkflow;
    let view = workflow.project(CaseRef::new("opaque", "o1", CaseRevision(0)), None);
    let violations = check_view(&workflow, &view).unwrap_err();
    assert_eq!(
        violations.into_iter().map(|v| v.kind).collect::<Vec<_>>(),
        vec![InvariantViolationKind::UnserializableObligation]
    );
}

#[tokio::test]
async fn erased_execute_round_trips_state_and_events() {
    let registry = registry();
    let erased = registry.require(&WorkflowKey::from("note")).unwrap();
    let account = AccountId::from("acct");
    let loaded = erased
        .executor
        .load(&account, &CaseId::from("n1"))
        .await
        .unwrap();
    assert_eq!(loaded.revision, CaseRevision::ZERO);
    assert!(loaded.value.is_none());

    let actor = ActorContext::new("acct", "u1");
    let origin = CommandOrigin::DirectSafeUserAct {
        evidence_digest: Digest::of_bytes(b"e"),
    };
    let command = json!({"set_text": {"value": "hello"}});
    let turn_id = TurnId::new();
    let envelope = CommandEnvelope {
        command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
        turn_id,
        actor,
        case_ref: case(0),
        idempotency_key: IdempotencyKey::derive(&account, &turn_id, &case(0), &origin, &command)
            .unwrap(),
        origin,
        command,
    };
    let batch = CommandBatch {
        batch_id: BatchId::new(),
        scope: AtomicityScope::PerCase,
        envelopes: vec![envelope.clone()],
    };
    let commit = erased.executor.execute(batch.clone()).await.unwrap();
    assert_eq!(commit.new_revision, CaseRevision(1));
    assert_eq!(commit.events.len(), 1);
    assert_eq!(
        commit.events[0].payload,
        json!({"text_set": {"value": "hello"}})
    );
    assert_eq!(
        commit.state,
        Some(json!({"text": "hello", "published": false, "archived": false}))
    );

    let receipts = erased
        .definition
        .receipts(&commit.receipt_events(), &Locale::from("en"))
        .unwrap();
    assert_eq!(receipts[0].status_code, "note.text_set");
    assert_eq!(
        receipts[0].event_ids,
        vec![commit.events[0].event_id],
        "a receipt cites the events that authorize it (I16)"
    );

    // I14: the same batch again is a replay, not a second effect.
    let replay = erased.executor.execute(batch.clone()).await.unwrap();
    assert!(replay.idempotency_replay);
    assert_eq!(replay.new_revision, commit.new_revision);
    assert_eq!(replay.events.len(), commit.events.len());
    let reloaded_after_replay = erased
        .executor
        .load(&account, &CaseId::from("n1"))
        .await
        .unwrap();
    assert_eq!(reloaded_after_replay.revision, CaseRevision(1));

    // The same key with a different command is a defect, not a replay.
    let mut tampered = batch.clone();
    tampered.envelopes[0].command = json!({"set_text": {"value": "goodbye"}});
    assert!(matches!(
        erased.executor.execute(tampered).await,
        Err(ExecutionError::IdempotencyMismatch { .. })
    ));

    // A fresh key against a stale revision still conflicts.
    let mut stale_batch = batch;
    stale_batch.envelopes[0].idempotency_key = IdempotencyKey::new("fresh");
    let stale = erased.executor.execute(stale_batch).await;
    assert!(matches!(stale, Err(ExecutionError::RevisionConflict(_))));

    let reloaded = erased
        .executor
        .load(&account, &CaseId::from("n1"))
        .await
        .unwrap();
    assert_eq!(reloaded.revision, CaseRevision(1));
    let view = erased
        .definition
        .project(case(1), reloaded.value.as_ref())
        .unwrap();
    assert_eq!(view.phase, json!("awaiting_publish"));

    let garbage = CommandBatch {
        batch_id: BatchId::new(),
        scope: AtomicityScope::PerCase,
        envelopes: vec![CommandEnvelope {
            command: json!({"launch_missiles": {}}),
            ..envelope
        }],
    };
    assert!(matches!(
        erased.executor.execute(garbage).await,
        Err(ExecutionError::Erasure(
            ErasureError::CommandDeserialization { .. }
        ))
    ));
}

#[test]
fn interaction_from_spec_and_response_validation_through_toy() {
    let workflow = NoteWorkflow::default();
    let state = NoteState {
        text: Some("hello".into()),
        ..NoteState::default()
    };
    let view = workflow.project(case(1), Some(&state));
    let requirement = view.blocking_interaction.clone().unwrap();
    let spec = workflow
        .build_interaction(Some(&state), &view, &requirement)
        .unwrap();
    let now = Utc::now();
    let interaction = Interaction::from_spec(
        spec,
        InteractionId::new(),
        AccountId::from("acct"),
        ConversationId::nil(),
        TurnId::nil(),
        now,
    )
    .unwrap();
    assert!(interaction.verify_payload_hash().unwrap());
    assert_eq!(interaction.bound_revision(), Some(CaseRevision(1)));
    let response = InteractionResponse {
        interaction_id: interaction.id,
        option_id: OptionId::from("publish"),
        expected_case_revision: CaseRevision(1),
        freeform_input: None,
    };
    let accepted = validate_response(
        &interaction,
        &response,
        ResolutionChannel::Click,
        &ActorContext::new("acct", "u1"),
        &ConversationId::nil(),
        CaseRevision(1),
        now,
    )
    .unwrap();
    assert!(matches!(
        accepted.action,
        StoredInteractionAction::ApplyOperation { ref operation, .. } if operation.as_str() == "note.publish"
    ));
    let origin = accepted.origin().expect("publishing authorizes commands");
    assert!(origin_satisfies(&origin, &CommandPolicy::conservative()));

    // Declining the same card resolves it without authorizing anything.
    let declined = validate_response(
        &interaction,
        &InteractionResponse {
            interaction_id: interaction.id,
            option_id: OptionId::from("keep_draft"),
            expected_case_revision: CaseRevision(1),
            freeform_input: None,
        },
        ResolutionChannel::Click,
        &ActorContext::new("acct", "u1"),
        &ConversationId::nil(),
        CaseRevision(1),
        now,
    )
    .unwrap();
    assert_eq!(declined.origin(), None);
    let _ = FreeformPolicy::Forbidden;
}

#[tokio::test]
async fn receipts_from_the_definition_satisfy_the_claim_guard() {
    let registry = registry();
    let erased = registry.require(&WorkflowKey::from("note")).unwrap();
    let account = AccountId::from("acct");
    let origin = CommandOrigin::DirectSafeUserAct {
        evidence_digest: Digest::of_bytes(b"e"),
    };
    let command = json!({"set_text": {"value": "hello"}});
    let turn_id = TurnId::new();
    let commit = erased
        .executor
        .execute(CommandBatch {
            batch_id: BatchId::derive(&turn_id, &case(0).key(), &AtomicityScope::PerCase),
            scope: AtomicityScope::PerCase,
            envelopes: vec![CommandEnvelope {
                command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
                turn_id,
                actor: ActorContext::new("acct", "u1"),
                case_ref: case(0),
                idempotency_key: IdempotencyKey::derive(
                    &account,
                    &turn_id,
                    &case(0),
                    &origin,
                    &command,
                )
                .unwrap(),
                origin,
                command,
            }],
        })
        .await
        .unwrap();

    let receipts = erased
        .definition
        .receipts(&commit.receipt_events(), &Locale::from("en"))
        .unwrap();
    let blocks: Vec<ResponseBlock> = receipts
        .iter()
        .enumerate()
        .map(|(i, receipt)| {
            ResponseBlock::Receipt(ReceiptBlock {
                block_id: BlockId::from(format!("r{i}")),
                receipt: receipt.clone(),
            })
        })
        .collect();
    let turn = AssistantTurn {
        turn_id,
        conversation_id: ConversationId::nil(),
        blocks,
        subjects: Vec::new(),
        expectations: Vec::new(),
        replay_token: ReplayToken::from("rt"),
        done: Vec::new(),
        offers: Vec::new(),
    };
    // The whole point of the signature change: a Success receipt rendered by a
    // definition can name the events that back it.
    assert_eq!(claim_guard::verify(&turn), Ok(()));
    assert!(turn.receipts().all(OperationalReceipt::is_event_backed));
}

#[test]
fn typed_projections_are_checked_against_the_invariants() {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum Broken {
        TerminalWithoutOutcome,
        UserWithoutRequirement,
        DuplicateObligations,
        UnanswerableCard,
    }

    struct BrokenWorkflow(Broken);

    impl WorkflowDefinition for BrokenWorkflow {
        type State = ();
        type Phase = Broken;
        type Obligation = NoteObligation;
        type Command = NoteCommand;
        type Event = NoteEvent;
        type Outcome = NoteOutcome;

        fn key(&self) -> WorkflowKey {
            WorkflowKey::from("broken")
        }
        fn version(&self) -> WorkflowVersion {
            WorkflowVersion::from("1")
        }
        fn phase_ownership(&self, phase: &Broken) -> PhaseOwnership {
            match phase {
                Broken::TerminalWithoutOutcome => PhaseOwnership::Terminal,
                _ => PhaseOwnership::User,
            }
        }
        fn project(&self, case_ref: CaseRef, _state: Option<&()>) -> ViewOf<Self> {
            let view = WorkflowView::new(case_ref, self.version(), self.0.clone());
            let answerable = InteractionRequirement::blocking("k", InteractionKind::Boolean)
                .with_payload(
                    InteractionPayload::new("Ready?")
                        .with_option(InteractionOption::new(
                            "yes",
                            "Yes",
                            StoredInteractionAction::ConfirmCommands {
                                command_refs: vec![],
                            },
                        ))
                        .with_option(InteractionOption::new(
                            "no",
                            "No",
                            StoredInteractionAction::DeclineCommands,
                        )),
                );
            match self.0 {
                Broken::TerminalWithoutOutcome | Broken::UserWithoutRequirement => view,
                Broken::DuplicateObligations => view
                    .with_obligations([NoteObligation::WriteText, NoteObligation::WriteText])
                    .with_blocking_interaction(answerable),
                Broken::UnanswerableCard => view.with_blocking_interaction(
                    InteractionRequirement::blocking("k", InteractionKind::Boolean)
                        .with_payload(InteractionPayload::new("Ready?")),
                ),
            }
        }
        fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
            vec![]
        }
        fn compile_act(
            &self,
            _state: Option<&()>,
            _view: &ViewOf<Self>,
            _act: &ResolvedAct,
        ) -> Result<Vec<NoteCommand>, DomainRejection> {
            Ok(vec![])
        }
        fn command_policy(&self, _state: Option<&()>, _command: &NoteCommand) -> CommandPolicy {
            CommandPolicy::conservative()
        }
        fn validate_command(
            &self,
            _state: Option<&()>,
            _command: &NoteCommand,
        ) -> Result<(), DomainRejection> {
            Ok(())
        }
        fn receipts(
            &self,
            _events: &[ReceiptEvent<NoteEvent>],
            _locale: &Locale,
        ) -> Vec<OperationalReceipt> {
            vec![]
        }
    }

    let expected = [
        (
            Broken::TerminalWithoutOutcome,
            InvariantViolationKind::TerminalPhaseWithoutOutcome,
        ),
        (
            Broken::UserWithoutRequirement,
            InvariantViolationKind::MissingBlockingInteraction,
        ),
        (
            Broken::DuplicateObligations,
            InvariantViolationKind::DuplicateObligation {
                obligation_id: "\"write_text\"".into(),
            },
        ),
        (
            Broken::UnanswerableCard,
            InvariantViolationKind::UnanswerableBlockingInteraction {
                error: turnframe_core::error::InteractionSpecError::NotEnoughOptions {
                    interaction_kind: InteractionKind::Boolean,
                    required: 2,
                    found: 0,
                },
            },
        ),
    ];
    for (phase, violation) in expected {
        let workflow = BrokenWorkflow(phase.clone());
        let view = workflow.project(case(0), None);
        let violations = check_view(&workflow, &view).unwrap_err();
        assert!(
            violations.iter().any(|v| v.kind == violation),
            "{phase:?}: {violations:?}"
        );
    }
}
