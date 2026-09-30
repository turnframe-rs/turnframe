//! API probe: a runtime that knows only a `WorkflowKey` (no concrete types)
//! drives a workflow end to end through the erased registry — load, project,
//! invariants, interaction spec, catalog, compile, policy, validate, execute,
//! receipts — and the registry crosses a task boundary (`Send + Sync + 'static`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_core::prelude::*;
use turnframe_core::response::{
    AssistantTurn, ReceiptBlock, ReplayToken, ResponseBlock, claim_guard,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Counter {
    n: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Counting,
    Done,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Obligation {
    ReachThree,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Cmd {
    Bump,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Ev {
    Bumped { n: u32 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Outcome {
    Reached,
}

struct CounterWorkflow;

impl WorkflowDefinition for CounterWorkflow {
    type State = Counter;
    type Phase = Phase;
    type Obligation = Obligation;
    type Command = Cmd;
    type Event = Ev;
    type Outcome = Outcome;

    fn key(&self) -> WorkflowKey {
        "counter".into()
    }
    fn version(&self) -> WorkflowVersion {
        "1".into()
    }
    fn phase_ownership(&self, phase: &Phase) -> PhaseOwnership {
        match phase {
            Phase::Counting => PhaseOwnership::User,
            Phase::Done => PhaseOwnership::Terminal,
        }
    }
    fn project(&self, case_ref: CaseRef, state: Option<&Counter>) -> ViewOf<Self> {
        if state.is_some_and(|s| s.n >= 3) {
            WorkflowView::new(case_ref, self.version(), Phase::Done).with_outcome(Outcome::Reached)
        } else {
            WorkflowView::new(case_ref, self.version(), Phase::Counting)
                .with_obligations([Obligation::ReachThree])
                .with_blocking_interaction(
                    InteractionRequirement::blocking("bump", InteractionKind::Boolean)
                        .with_payload(
                            InteractionPayload::new("Bump the counter?")
                                .with_option(InteractionOption::new(
                                    "yes",
                                    "Bump",
                                    StoredInteractionAction::ApplyOperation {
                                        operation: OperationKey::from("counter.bump"),
                                        arguments: Value::Null,
                                        freeform_argument: None,
                                    },
                                ))
                                .with_option(InteractionOption::new(
                                    "no",
                                    "Leave it",
                                    StoredInteractionAction::DeclineCommands,
                                )),
                        ),
                )
        }
    }
    fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
        vec![
            OperationSpec::new("counter.bump")
                .summary("Increment the counter")
                .target(TargetPolicy::AllowsNewCase)
                .mutating(),
        ]
    }
    fn compile_act(
        &self,
        _state: Option<&Counter>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Cmd>, DomainRejection> {
        match act.operation().map(OperationKey::as_str) {
            Some("counter.bump") => Ok(vec![Cmd::Bump]),
            _ => Err(DomainRejection::new(
                "counter.unknown",
                "counter.error.unknown",
            )),
        }
    }
    fn command_policy(&self, _state: Option<&Counter>, _command: &Cmd) -> CommandPolicy {
        CommandPolicy::low_risk()
    }
    fn validate_command(
        &self,
        _state: Option<&Counter>,
        _command: &Cmd,
    ) -> Result<(), DomainRejection> {
        Ok(())
    }
    fn receipts(&self, events: &[ReceiptEvent<Ev>], _locale: &Locale) -> Vec<OperationalReceipt> {
        events
            .iter()
            .map(|event| match event {
                ReceiptEvent::Committed(committed) => {
                    let Ev::Bumped { n } = committed.payload;
                    OperationalReceipt {
                        receipt_id: ReceiptId::derive(&[committed.event_id], "counter.bumped"),
                        event_ids: vec![committed.event_id],
                        severity: ReceiptSeverity::Success,
                        title: "Counter".into(),
                        body: format!("now {n}").into(),
                        status_code: "counter.bumped".into(),
                        artifact_refs: vec![],
                    }
                }
                ReceiptEvent::Redacted(redacted) => OperationalReceipt {
                    receipt_id: ReceiptId::derive(&[redacted.event_id], "counter.detail_erased"),
                    event_ids: vec![redacted.event_id],
                    severity: ReceiptSeverity::Info,
                    title: "Counter".into(),
                    body: "This step is on record; its detail was erased.".into(),
                    status_code: "counter.detail_erased".into(),
                    artifact_refs: vec![],
                },
            })
            .collect()
    }
}

/// Account-scoped in-memory executor.
#[derive(Default)]
struct MemExec(Mutex<HashMap<(AccountId, CaseId), Versioned<Counter>>>);

#[async_trait::async_trait]
impl WorkflowExecutor<CounterWorkflow> for MemExec {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<Counter>>, StoreError> {
        let store = self.0.lock().unwrap();
        Ok(match store.get(&(account.clone(), case_id.clone())) {
            Some(v) => v.clone().map(Some),
            None => Versioned::new(None, CaseRevision::ZERO),
        })
    }
    async fn execute(
        &self,
        batch: CommandBatch<Cmd>,
    ) -> Result<Commit<Counter, Ev>, ExecutionError> {
        let first = batch
            .envelopes
            .first()
            .ok_or(ExecutionError::ScopeViolation)?;
        let key = (first.account_id().clone(), first.case_ref.case_id.clone());
        let mut store = self.0.lock().unwrap();
        let current = store
            .get(&key)
            .cloned()
            .unwrap_or(Versioned::new(Counter::default(), CaseRevision::ZERO));
        if current.revision != first.case_ref.expected_revision {
            return Err(ExecutionError::RevisionConflict(RevisionConflict {
                expected: first.case_ref.clone(),
                current_revision: current.revision,
            }));
        }
        let mut state = current.value;
        let events = batch
            .envelopes
            .iter()
            .map(|_| {
                state.n += 1;
                CommittedEvent {
                    event_id: EventId::new(),
                    event_type: "counter.bumped".into(),
                    occurred_at: chrono::Utc::now(),
                    payload: Ev::Bumped { n: state.n },
                }
            })
            .collect();
        let new_revision = current.revision.next();
        store.insert(key, Versioned::new(state.clone(), new_revision));
        Ok(Commit {
            state: Some(state),
            new_revision,
            events,
            idempotency_replay: false,
        })
    }
}

/// The "runtime": generic-free, it only knows a registry and a key.
async fn drive(
    registry: Arc<WorkflowRegistry>,
    key: WorkflowKey,
) -> Result<(CaseRevision, bool), BoxError> {
    let wf = registry.require(&key)?.clone();
    let account = AccountId::from("acct");
    let loaded = wf.executor.load(&account, &CaseId::from("c1")).await?;
    let state = loaded.value.as_ref();
    let case_ref = CaseKey::new(key, "c1").at(loaded.revision);
    let view = wf.definition.project(case_ref.clone(), state)?;
    check_erased_view(&view).map_err(|v| format!("{v:?}"))?;
    if let Some(requirement) = &view.blocking_interaction {
        let spec = wf
            .definition
            .build_interaction(case_ref.clone(), state, requirement)?;
        assert_eq!(
            (spec.kind, &spec.case_ref),
            (InteractionKind::Boolean, &case_ref)
        );
    }
    let catalog = wf.definition.operations(case_ref.clone(), state)?;
    let act = ResolvedAct {
        act: ActId::new(UnitId(1), 1),
        kind: ResolvedActKind::ApplyOperation {
            operation: catalog.first().ok_or("empty catalog")?.key.clone(),
        },
        case_ref: case_ref.clone(),
        arguments: Value::Null,
        evidence_digest: Digest::of_bytes(b"evidence"),
    };
    let commands = wf.definition.compile_act(case_ref.clone(), state, &act)?;
    let policy = wf.definition.command_policy(state, &commands[0])?;
    assert!(origin_satisfies(&act.direct_origin(), &policy));
    wf.definition.validate_command(state, &commands[0])?;
    let turn_id = TurnId::new();
    let origin = act.direct_origin();
    let envelopes = commands
        .into_iter()
        .enumerate()
        .map(|(position, command)| {
            Ok(CommandEnvelope {
                command_id: CommandId::derive(&turn_id, act.act, position),
                turn_id,
                actor: ActorContext::new("acct", "u1"),
                case_ref: case_ref.clone(),
                idempotency_key: IdempotencyKey::derive(
                    &account, &turn_id, &case_ref, &origin, &command,
                )?,
                origin: origin.clone(),
                command,
            })
        })
        .collect::<Result<Vec<_>, turnframe_core::hash::HashError>>()?;
    let commit = wf
        .executor
        .execute(CommandBatch {
            batch_id: BatchId::new(),
            scope: policy.atomicity,
            envelopes,
        })
        .await?;
    let receipts = wf
        .definition
        .receipts(&commit.receipt_events(), &Locale::default())?;
    assert_eq!(receipts.len(), 1);
    // A receipt rendered through the erased boundary still names its events.
    assert_eq!(receipts[0].event_ids, vec![commit.events[0].event_id]);
    let turn = AssistantTurn {
        turn_id,
        conversation_id: ConversationId::nil(),
        blocks: vec![ResponseBlock::Receipt(ReceiptBlock {
            block_id: BlockId::from("r0"),
            receipt: receipts[0].clone(),
        })],
        replay_token: ReplayToken::from("rt"),
        subjects: Vec::new(),
        expectations: Vec::new(),
        done: Vec::new(),
        offers: Vec::new(),
    };
    claim_guard::verify(&turn).map_err(|v| v.to_string())?;
    let after = wf.definition.project(
        case_ref.with_revision(commit.new_revision),
        commit.state.as_ref(),
    )?;
    Ok((commit.new_revision, after.is_complete()))
}

fn assert_send_sync_static<T: Send + Sync + 'static>() {}

#[tokio::test]
async fn erased_registry_is_drivable_by_a_key_only_runtime() {
    assert_send_sync_static::<Arc<dyn ErasedWorkflow>>();
    assert_send_sync_static::<Arc<dyn ErasedExecutor>>();
    assert_send_sync_static::<WorkflowRegistry>();
    let registry = Arc::new(
        WorkflowRegistry::builder()
            .register(CounterWorkflow, MemExec::default())
            .build()
            .unwrap(),
    );
    let key = WorkflowKey::from("counter");
    // The registry crosses a task boundary; each drive reloads state through the erased executor.
    let first = tokio::spawn(drive(Arc::clone(&registry), key.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, (CaseRevision(1), false));
    drive(Arc::clone(&registry), key.clone()).await.unwrap();
    let third = drive(registry, key).await.unwrap();
    assert_eq!(
        third,
        (CaseRevision(3), true),
        "n reaches 3: terminal phase with outcome"
    );
}
