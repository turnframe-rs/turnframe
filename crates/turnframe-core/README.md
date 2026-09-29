# turnframe-core

Pure types, pure functions and traits: the contract every other Turnframe crate
is written against. No async runtime, no HTTP, no database.

Part of the [Turnframe](https://github.com/turnframe-rs/turnframe) workspace. See the
workspace [README](../../README.md) and [architecture guide](../../docs/architecture.md).

## Scope

The crate implements the **Flow Map** architecture as data and pure logic:

| Module | What it holds |
| --- | --- |
| `ids`, `case`, `locale`, `hash`, `schema` | Identity and version newtypes, `CaseRef`, localized copy, canonical JSON hashing, schema fingerprints |
| `flow` | `WorkflowDefinition` / `WorkflowExecutor`, `WorkflowView`, phase ownership, §8.4 invariants, the type-erased `WorkflowRegistry` |
| `turn` | `TurnInput`: text, interaction response, attachments and origin may coexist |
| `plan` | What an operation declares about where it may run: target policy, mutability, availability; the limits a turn is held to |
| `operation` | `OperationSpec`: an operation's arguments, labels, value shapes and examples; `DateExpr` and `Money` |
| `understanding` | `Understanding`: what a turn was understood to ask, with the words behind every value; the reducer's input |
| `target` | Opaque target tokens, account-scoped resolution, `ResolvedAct` |
| `reduce`, `policy`, `command` | Whole-turn reduction contract, policy decisions, command envelopes, origins (what was confirmed, by which option class, on which channel), risk, derived command and batch ids, idempotency keys |
| `interaction` | Durable server-owned cards, stored options, the status state machine and `validate_response` |
| `event`, `response` | Commits, events, receipts, external statuses, ordered response blocks and the claim guard |
| `read`, `knowledge` | Read-only tool and knowledge retrieval contracts |
| `replay`, `observe`, `error` | Replay records (decisions, command outcomes, provider attempts and the outbox and reconciliation handles of a turn's external effects), metrics signals, the typed error family |

The rule the types enforce: **the model proposes meaning, deterministic code
decides effects, committed events decide claims.**

## Minimal example

```rust
use turnframe_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json;

#[derive(Clone, Serialize, Deserialize)]
struct Greeting { name: Option<String> }

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Phase { Collecting, Done }

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Obligation { ProvideName }

#[derive(Clone, Serialize, Deserialize)]
enum Command { SetName { value: String } }

#[derive(Clone, Serialize, Deserialize)]
enum Event { NameSet { value: String } }

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Outcome { Greeted }

struct GreetingWorkflow;

impl WorkflowDefinition for GreetingWorkflow {
    type State = Greeting;
    type Phase = Phase;
    type Obligation = Obligation;
    type Command = Command;
    type Event = Event;
    type Outcome = Outcome;

    fn key(&self) -> WorkflowKey { "greeting".into() }
    fn version(&self) -> WorkflowVersion { "1".into() }

    fn phase_ownership(&self, phase: &Phase) -> PhaseOwnership {
        match phase {
            Phase::Collecting => PhaseOwnership::User,
            Phase::Done => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&Greeting>) -> ViewOf<Self> {
        match state.and_then(|s| s.name.as_ref()) {
            Some(_) => WorkflowView::new(case_ref, self.version(), Phase::Done)
                .with_outcome(Outcome::Greeted),
            None => WorkflowView::new(case_ref, self.version(), Phase::Collecting)
                .with_obligations([Obligation::ProvideName])
                .with_blocking_interaction(
                    // A user-owned phase must offer a card the user can answer.
                    InteractionRequirement::blocking("ask_name", InteractionKind::Freeform)
                        .with_payload(
                            InteractionPayload::new("What is your name?")
                                .with_freeform_prompt("Your name")
                                .with_option(
                                    InteractionOption::new(
                                        "save",
                                        "Save",
                                        StoredInteractionAction::Custom {
                                            key: "greeting.set_name".into(),
                                            payload: serde_json::Value::Null,
                                        },
                                    )
                                    .with_freeform(FreeformPolicy::Required { max_len: 80 }),
                                ),
                        )
                        .with_confirms_risk(RiskClass::ReversibleLowRisk),
                ),
        }
    }

    fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> { vec![] }

    fn compile_act(&self, _s: Option<&Greeting>, _v: &ViewOf<Self>, act: &ResolvedAct)
        -> Result<Vec<Command>, DomainRejection>
    {
        let value = act.arguments["value"].as_str()
            .ok_or_else(|| DomainRejection::new("greeting.missing_value", "greeting.error.value"))?;
        Ok(vec![Command::SetName { value: value.to_owned() }])
    }

    fn command_policy(&self, _s: Option<&Greeting>, _c: &Command) -> CommandPolicy {
        CommandPolicy::low_risk()
    }

    fn validate_command(&self, _s: Option<&Greeting>, _c: &Command) -> Result<(), DomainRejection> {
        Ok(())
    }

    fn receipts(&self, events: &[ReceiptEvent<Event>], _locale: &Locale)
        -> Vec<OperationalReceipt>
    {
        // A receipt cites the events that authorize its claim. An event whose
        // payload was erased still authorizes it, and still gets a receipt --
        // one that does not pretend to know what changed.
        events
            .iter()
            .map(|e| match e {
                ReceiptEvent::Committed(e) => OperationalReceipt {
                    receipt_id: ReceiptId::derive(&[e.event_id], "greeting.name_set"),
                    event_ids: vec![e.event_id],
                    severity: ReceiptSeverity::Success,
                    title: "Greeting".into(),
                    body: "Name saved".into(),
                    status_code: "greeting.name_set".into(),
                    artifact_refs: vec![],
                },
                ReceiptEvent::Redacted(e) => OperationalReceipt {
                    receipt_id: ReceiptId::derive(&[e.event_id], "greeting.detail_erased"),
                    event_ids: vec![e.event_id],
                    severity: ReceiptSeverity::Info,
                    title: "Greeting".into(),
                    body: "This step is on record; its detail was erased.".into(),
                    status_code: "greeting.detail_erased".into(),
                    artifact_refs: vec![],
                },
            })
            .collect()
    }
}

let workflow = GreetingWorkflow;
let case_ref = CaseRef::new("greeting", "g1", CaseRevision::ZERO);
let view = workflow.project(case_ref, None);
assert!(check_view(&workflow, &view).is_ok());
assert!(view.blocking_interaction.is_some());
```

Executors implement `WorkflowExecutor<W>`; `WorkflowRegistry::builder().register(def, exec)`
erases both behind `ErasedWorkflow` / `ErasedExecutor` so the runtime never
needs the concrete types.

## Auditing one turn

`ReplayRecord` is what §26.1 of the specification asks for: everything needed to say why a turn
produced the commands and the answer it did. Besides the understanding, every model task, the
resolutions and the decisions, it names the turn's external effects: `outbox_ids` for the rows it
enqueued and `reconciliation_attempt_ids` for the attempts whose outcome never came back, so an
auditor holding only the record can ask the outbox what became of them. `pending_reconciliations()`
unions those with the attempts recorded against individual commands, once each.

Two shapes of "attempt" meet in that record, on purpose. `ProviderAttemptRecord::attempt` is a plain
ordinal because a model call that failed left nothing outside the process, and its stage is already
identified. `AttemptId` is an identifier because an external effect may have happened even though the
answer never arrived, and settling it means naming that exact attempt to a remote system. Both types
document the asymmetry.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
