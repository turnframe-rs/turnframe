# turnframe-runtime

The turn runtime of [Turnframe](https://github.com/turnframe-rs/turnframe): the code
that decides what a user's turn actually does, and then does it.

[`turnframe-core`](../turnframe-core) defines the contract: what an understanding is,
what a command needs before it may run, what a card means. This crate makes the
decisions and carries them out. The shape of it is one sentence: **the model
proposes meaning, deterministic code decides effects, committed events decide
claims.**

## One turn, in stages

`Orchestrator::handle_turn` runs the pipeline of spec §23. Each stage is its own
module, so a trace, a phase marker and a replay record can all point at the same
place.

| Module | What it decides |
| --- | --- |
| `config` | how much autonomy the model gets (§11.1), which risk classes a sandbox refuses outright (§11.4), and the conservative defaults of Appendix A |
| `budget` | what a sandboxed turn may spend (model calls, prompt tokens, wall clock) and which bound stopped it (§11.1) |
| `understand` | the cases in view as the understanding tasks see them (opaque tokens, labels, what each still needs) and the domain's own dry-run check of every act |
| `resolve` | which case "the Ferri trip" is, or that it is a question. Recency, list order and plausibility never break a tie (I8) |
| `policy` | whether a command may run now, and if not, which card would authorize it (§14.3, I12) |
| `reduce` | the whole-turn algorithm of §13, giving every act an explicit result (I11) |
| `interactions` | durable cards: written before any sentence refers to them, one blocking card per case, resolution by compare-and-set, `Resolved` only once the command commits (§15) |
| `resume` | what a card carries, so answering it runs the acts that waited for it (§13.3) |
| `execute` | admission to the journal before any effect, optimistic concurrency, typed outcomes, the outbox, and one all-or-nothing commit (§16) |
| `compose` | receipts from committed events, answers with an explicit state basis, and the outcome the reply is written from (§17, §18, §19) |
| `narrate` | the reply's model tasks: the acknowledgement and its review, one answer per question, and step prose when asked for |
| `stream` | nothing that states an outcome goes on the wire before the commit; the reply is published whole (§18.5) |
| `trace` | a turn from message to reply, for local debugging |
| `recover` | after a crash: restart, resume by idempotency key, regenerate the answer, or reconcile (§23.1) |
| `dispatch` | the other half of the external-effect saga: claim a due outbox row, send it, settle it, driven by the application's own task and never a thread this library starts (§16.4) |
| `orchestrator` | the facade that runs them in order, writes the phase marker at each step and records a replay record for every turn |

## The properties worth stating out loud

- **A guess is never a target.** When two authorized cases match a mention, the
  resolver returns `Ambiguous` and the reducer raises a selection card. There is
  no code path in `resolve` that reads a timestamp, a list position or a model
  confidence.
- **A click is not a blanket consent.** Answering "which trip did you mean?"
  authorizes nothing. `policy` maps each `ConfirmationPolicy` to the specific
  card whose answer satisfies it, and `HumanProfessionalReview` maps to no card
  at all, because a card the user can click would let them approve themselves.
- **A click costs nothing.** A turn carrying only a card answer reaches
  execution with no model call at all: the meaning of the click is the stored
  option, and the runtime reads it from there.
- **A clarification interrupts; it does not cancel.** The card carries the act
  it was guarding, so picking one of two Ferri trips applies the change that
  was waiting to *that* trip, and answering "no" to a conditional
  instruction records that it was declined. Nobody restates a request because
  the server asked a question about it.
- **Silence is not permission.** A command the domain never classified is
  treated as `CommandPolicy::conservative`: irreversible, explicit click.
- **A correction inside the turn lands before any effect.** "Change it, actually
  leave it" compiles nothing, rather than writing and then undoing.
- **Model output is all or nothing.** A task's answer that breaks its schema or
  a check is sent back whole with the exact error, within its repairs. A unit
  still not understood becomes a notice, and every act on its record is held.
- **Admission precedes effect.** Every command is journaled under its
  idempotency key before the domain hears about it, so a crash is resumed rather
  than repeated, and resuming means handing the executor the *journaled*
  entry, not re-planning the turn against a state that has since moved.
- **Uncertainty is a state.** A timeout after transmission becomes an unknown
  outcome carrying an attempt identifier. Nothing in this crate retries it.
- **Events authorize claims.** A receipt is rendered from committed events by
  the workflow itself; a command that failed contributes none, so there is
  nothing to render a success from. The reply's writer is handed only the turn's
  outcome and reviewed against it, and the assembled turn goes through a
  structural check that reads the record rather than the prose. No word matching runs on what a model
  wrote: a substring match cannot see a negation, and would refuse the true
  sentence on exactly the turns whose only true sentence is a denial.
- **A budget is enforced, not advertised.** A sandboxed autonomous turn that
  spends its model calls, its prompt tokens or its wall clock stops, and the
  error names the bound that stopped it. Before the commit that means the turn
  fails with nothing written; after it, the effects stand and only the wording
  is cut short.
- **Every question gets its own answer.** Each is a small task run beside the
  others, and a question no model answers still gets a block, reported as not
  written.
- **Effort buys judgment, never authority.** A turn runs at `low`, `medium` or
  `high` (`OrchestratorConfig::effort`, or `TurnInput::effort` for one turn).
  `high` votes, reasons and checks the whole reading; `low` skips the reply
  review. Policy, cards and the claim guard read no level (ADR-020).

## Example

Configure the runtime and check that a sandbox refuses what §11.4 says it must.

```rust
use turnframe_core::prelude::*;
use turnframe_runtime::config::{
    OrchestrationMode, OrchestratorConfig, ResourceBudget, SandboxAcknowledgement,
};

let config = OrchestratorConfig::conservative();
config.validate()?;
assert_eq!(config.mode, OrchestrationMode::Deterministic);

let sandbox = OrchestrationMode::sandboxed_autonomous(
    ResourceBudget::conservative(),
    SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
);
assert!(sandbox.allows_risk(RiskClass::ReversibleLowRisk));
assert!(!sandbox.allows_risk(RiskClass::ExternalRegulated));
# Ok::<(), turnframe_runtime::config::ConfigError>(())
```

Building the target catalog is the step that decides the model will never see a
record identifier: it gets opaque tokens and server-authored labels, and the
mapping back stays here.

```rust
use turnframe_core::prelude::*;
use turnframe_runtime::resolve::{AuthorizedCase, TargetResolver};

// One resolver per turn, over the two trips this actor may address.
let resolver = TargetResolver::builder(AccountId::from("acct"), TurnId::nil())
    .candidate(AuthorizedCase::new(
        CaseRef::new("trip", "trip-12", CaseRevision(4)),
        "Trip 12",
    ))
    .candidate(AuthorizedCase::new(
        CaseRef::new("trip", "trip-13", CaseRevision(1)),
        "Trip 13",
    ))
    .build();

let catalog = resolver.catalog(); // what understanding sees: tokens and labels, no row ids
assert_eq!(catalog.len(), 2);
assert!(catalog[0].token.as_str().starts_with("t_"));
assert!(!catalog[0].token.as_str().contains("trip-12"));
```

## Wiring a whole orchestrator

```rust,ignore
let orchestrator = Orchestrator::builder()
    .workflows(registry)              // Arc<WorkflowRegistry>
    .providers(pool)                  // Arc<ProviderPool>
    .stores(stores)                   // turnframe_store::stores::Stores
    .case_directory(directory)        // which cases this actor may address
    .knowledge(knowledge_provider)    // optional, for answers (§19.2)
    .policy(PolicySnapshot::conservative())
    .observer(observer)
    .build()?;

let answer = orchestrator.handle_turn(input).await?;
```

The runnable version lives in the integration tests: [`tests/support/mod.rs`]
assembles the sample trip and traveler domains from
[`turnframe-test`](../turnframe-test) against its in-memory stores and a scripted
provider, and the twenty scenarios of spec §27.4 are one named test each in
[`tests/runtime_scenarios.rs`]. [`tests/chaos.rs`] kills a turn at each of the
seven crash boundaries of §27.7 and asserts idempotent recovery and truthful
status. [`tests/continuation.rs`] answers a clarification and checks the work it
interrupted actually happens, [`tests/budget.rs`] spends each bound of a
sandboxed budget in turn, and [`tests/answers.rs`] puts three questions in one
turn and checks each gets its own answer, in the order asked.

[`tests/support/mod.rs`]: tests/support/mod.rs
[`tests/runtime_scenarios.rs`]: tests/runtime_scenarios.rs
[`tests/chaos.rs`]: tests/chaos.rs
[`tests/continuation.rs`]: tests/continuation.rs
[`tests/budget.rs`]: tests/budget.rs
[`tests/answers.rs`]: tests/answers.rs
[`tests/signals.rs`]: tests/signals.rs

## What a turn reports

The `Observer` handed to `.observer(...)` receives every signal of spec §26.2
and §28, so each panel of the reliability dashboard has a series behind it. The
runtime pushes that same observer into the understanding, composition and
interaction stages when it builds them, which is why an application that
supplies its own `Composer` still reports on the same series.

How often each one fires matters when you write the alert:

| Signal | Once per |
| --- | --- |
| `turn.received`, `turn.completed`, `turn.failed`, `turn.duration_ms` | turn; the duration whether it succeeded or not |
| `task.completed`, `task.repaired`, `task.escalated`, `task.vote_disagreement` | model task, repair round, escalation and split vote, for understanding and the reply |
| `budget.exhausted` | bound a turn's model calls reached |
| `case.not_authorized` | candidate the case directory refused |
| `projection.duration_us` | case projected, so several times in one turn |
| `reduction.duration_us`, `persistence.duration_ms` | turn |
| `interaction.resolved`, `interaction.failed` | card settled, on whichever path settled it |
| `provider.latency_ms`, `provider.fallback` | provider attempt, the abandoned ones included |
| `narration.latency_ms` | acknowledge, answer or review call |
| `provider.capability_mismatch` | candidate routing refused for a capability it lacks, visible only when routing then found nobody, because that is when the router hands its rejection list back |
| `external.latency_ms`, `external.reconciled` | outbox row sent, and unknown outcome settled |

[`tests/signals.rs`](tests/signals.rs) drives each of them through the real
pipeline and asserts the labels, and its last test fails if a declared signal
has no driver at all.

## Streaming a turn

`stream_turn` runs the turn on a task and yields `TurnEvent`s. Phases go out as the
turn progresses, and each understanding step as it is decided, so a surface can show
what the message was read to say while the rest runs. Blocks follow once the commit
has landed; the reply is reviewed before it is shown, so it arrives whole.

```rust,ignore
let mut events = Arc::new(orchestrator).stream_turn(input);
while let Some(event) = events.next().await {
    match event {
        TurnEvent::Phase(phase) => ui.progress(phase),
        TurnEvent::Step(step) => ui.note(&step.describe()),
        TurnEvent::Block(block) => ui.render(*block),
        TurnEvent::Completed(turn) => ui.finish(*turn),
        TurnEvent::Failed { code } => ui.failed(&code),
        _ => {}
    }
}
```

## Dispatching the outbox

A command with an `ExternalSaga` scope leaves an outbox row inside the turn's
one atomic write. `OutboxDispatcher` is the reference worker that picks those
rows up, and it is driven by *your* task, because a library that starts a
thread of its own would keep calling external systems out of a process that was
only supposed to answer a turn.

```rust,ignore
let dispatcher = OutboxDispatcher::new(
    stores.outbox().clone(),
    Arc::new(MySender::new(http_client)),   // implements `OutboxSender`
    DispatchConfig::new("dispatcher-1"),
)
// It runs on your task, not the orchestrator's, so it is given its own
// observer: without one, `external.latency_ms` and `external.reconciled` stay
// empty and the remote half of the saga is invisible.
.with_observer(observer);

// Your loop, your shutdown, your schedule.
loop {
    let report = dispatcher.run_once(Utc::now()).await?;
    tracing::debug!(sent = report.completed.len(), unknown = report.unknown.len());
    ticker.tick().await;
}
```

A sender classifies its own outcome, and the classification is the contract: a
send that timed out is `Dispatched::Unknown`, never a retry, and the row waits
for `OutboxDispatcher::reconcile` to settle it against the remote system.

## Running beside the path you are migrating from

An application moving off a free tool-calling agent cannot cut over on faith: it
has to run both paths on the same turns and compare them while the old path
stays authoritative. `Orchestrator::plan_turn` runs the whole decision pipeline
(cases loaded and projected, the message understood, targets resolved, turn
reduced, policy applied) and returns a `PlannedTurn` before the first side
effect of any kind. No card is persisted, no command journaled, no event
appended, no outbox row written, no conversation block stored.

That is enforced by the types rather than by care: the planner holds the read
half of the stores (`ReadOnlyStores`) and the loading half of each executor
(`ErasedCaseLoader`), so `commit`, `insert` and `execute` cannot be named from
it at all.

```rust,ignore
let planned = orchestrator.plan_turn(input).await?;

planned.would_persist;  // the cards it would have written, not written
planned.would_execute;  // the commands it would have run, not journaled
planned.would_claim;    // what the answer could have said

// Same turn, state handed in: a recorded corpus becomes a shadow corpus.
let planned = orchestrator
    .plan_turn_from(input, vec![SeededCase::new(case_ref, state)])
    .await?;

// And a shared name for every way the two paths disagreed.
let report = divergence::compare(&planned.summary(), &what_the_old_path_did);
report.any_against_shadow();   // a refusal on an ambiguous target is not one
```

`divergence` carries one asymmetry deliberately: when this library
refuses a mutation because it could not tell which record was meant and the old
path performed it anyway, the finding is against the old path. There is no
traffic router, cohort selection or kill switch here: those depend on how an
application identifies conversations and belong in the application.

## Links

- Workspace [README](../../README.md)
- [Architecture guide](../../docs/architecture.md)
- [Reliability model](../../docs/reliability-model.md)
- [Interactions](../../docs/interactions.md)

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
