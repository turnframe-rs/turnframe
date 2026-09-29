# turnframe-test

The test kit for [Turnframe](https://github.com/turnframe-rs/turnframe): bounded workflow
exploration, `proptest` strategies over the core types, three complete sample domains, scripted
model providers, fake stores, an executor conformance suite, provider conformance macros, replay
assertions, and the checks that keep coming up when you test a deterministic assistant.

Turnframe's promise is that the model proposes meaning while deterministic code decides effects.
Testing that promise is harder than it looks: every value must point at words the user wrote, a
workflow's invariants must hold in *every* reachable state, a model double that keeps answering
hides the extra call that should have failed the test, and a receipt may not claim one word more
than the event ledger says. This crate writes that machinery once, so every
other crate, example and integration test can borrow it instead of re-deriving it slightly wrong.

## What is in it

- **`explore`**: breadth-first exploration of a workflow's reachable states (spec §8.5). You supply
  a `WorkflowModel`: the initial states, the commands worth trying, and a pure simulation of each.
  The explorer projects every reachable state and checks the Flow Map invariants of §8.4 on it
  (one phase, unique and stable obligation identifiers, a blocking card exactly where the phase is
  user-owned, an outcome exactly where it is terminal), plus the rules only a search can see: a
  refused command changes nothing, a user-owned phase really does derive an answerable card, the
  projection does not change when the same state is read at another case revision, no state is a
  dead end, no case ends by disappearing, and every declared outcome is actually reachable. Because
  the search is breadth-first, a violation is reported with the *shortest* command path that
  reaches it. `reachable_states` hands you the states themselves when an assertion is
  domain-specific.

- **`strategies`**: `proptest` strategies for identifiers, case references, actor contexts, turn
  inputs, interactions, command origins and policies. The one that earns its keep is
  `grounded_understanding(text)`: it generates understandings whose every unit and argument
  excerpt lands on word boundaries of `text`, so a property test asserts that reduction *accepts*
  what it should, not only that it rejects what it should.

- **`executors`**: the conformance suite for the `WorkflowExecutor` an adopter writes. Stores have
  one and provider adapters have one; the executor is the third thing every adopter writes and the
  one where optimistic concurrency and idempotency actually live. See below.

- **`workflows`**: the three sample domains below, each with a pure transition function, an
  in-memory executor (revision checks, idempotency replay, resumption of a batch that only
  half-executed, derived event identifiers) and an exploration model, all sharing one description
  of what a command does.

- **`providers`**: a `ScriptedProvider` whose behaviour is a script the test declares, an
  `UnderstandingBuilder` that turns quotes into word ranges, a `ScriptedUnderstanding` that hands a
  runtime the understandings a test wrote, and the provider conformance harness behind the
  `provider_conformance_suite!` macro.

- **`stores`**: the in-memory persistence layer wrapped for turn tests: fail at a named crash
  boundary, count calls per store method, freeze the clock, and read back what was persisted
  without disturbing the tally. The store conformance suite is re-exported here, so an adopter
  building a store over their own database reaches everything through this one crate.

- **`replay`**: compare two executions of the same turn artefact by artefact (understanding,
  commands, events, ordered blocks), and ask a replay record to account for its own turn.

- **`assertions`**: no high-risk command without a trusted origin, no receipt without events
  behind it, one phase per projection, one case with the same phase in two projections, and two
  renderings of a turn that agree block by block.

## Scripted model providers

`turnframe-provider` already ships a `StaticProvider` that answers from a queue and then repeats a
default for ever. That is the right shape for testing retry, fallback and routing. It is the wrong
shape for testing a **turn**, where the interesting bugs are the extra model call nobody expected
and the call that carried the wrong catalog. `ScriptedProvider` consumes its steps in order and
fails loudly when the script runs out: the call fails with a distinctive code, the violation is
recorded, and `verify()` reports it at the end of the test, so a runtime that swallowed the error
cannot hide the extra call from the test that wrote the script.

```rust
use turnframe_provider::prelude::*;
use turnframe_test::providers::{ScriptedProvider, ScriptedReply};

# futures::executor::block_on(async {
let text = "Cambia il nome in Lisbona";
let provider = ScriptedProvider::builder("scripted", "m")
    .reply_to(ModelPurpose::Acknowledge, ScriptedReply::text("Fatto."))
    .build();

let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user(text));
let response = provider.generate(request).await.unwrap();
assert_eq!(response.text(), "Fatto.");

// What was sent, not only what came back.
assert_eq!(provider.calls()[0].user_text(), text);
# });
```

A step can return arbitrary JSON, prose, a body that is not JSON, a refusal, a timeout, a rate
limit with its `Retry-After`, any normalized failure, or a stream delivered in the chunks you name.

A runtime's understanding is scripted apart from its provider calls: `UnderstandingBuilder` writes
what a turn was understood to ask, from quotes of the message, and `ScriptedUnderstanding` returns
those understandings in order.

```rust
use turnframe_test::providers::{ScriptedUnderstanding, UnderstandingBuilder};

let understanding = UnderstandingBuilder::of("Chiama il viaggio Lisbona")
    .apply("trip.set_name", "tok_1", serde_json::json!({"value": "Lisbona"}), "Chiama il viaggio Lisbona")
    .build()
    .unwrap();
let scripted = ScriptedUnderstanding::new().then(understanding);
assert_eq!(scripted.remaining(), 1);
```

## Fake stores

```rust
use turnframe_store::error::StoreError;
use turnframe_test::stores::FakeStores;

let fake = FakeStores::new();
fake.fail_at_boundary("before_response_persistence", StoreError::Unavailable).unwrap();
// Hand `fake.stores()` to the code under test, then assert on
// `fake.call_count("conversations.append_assistant_turn")` and on what survived.
```

The clock is frozen at the Unix epoch until the test moves it, the seven crash boundaries of
spec §27.7 are addressable by their specification names, and the accessors that read back what was
persisted bypass the counting layer so an assertion never inflates the tally it is asserting on.

## Provider conformance macros

An adapter crate declares its suite in one block. The workspace ships no proc macros, so these are
declarative; they expand to an ordinary `#[test]` that runs the harness on its own runtime.

```rust,ignore
turnframe_test::provider_conformance_suite! {
    name: gpt_4o_conforms,
    factory: OpenAiFactory::default(),
    fixtures: OpenAiFixtures,
    // Every row must pass. A row the profile puts out of scope is listed here,
    // and nowhere else, so "we never tested it" is visible in review.
    allow_skipped: [StreamingReconstruction],
}
```

The default is that **no** skip is acceptable: the harness itself treats a skipped row as neither
pass nor failure, which is right for a diagnosis and wrong for a gate, because an unexercised row
is an unproven one.

Some rows a deployment genuinely cannot put on the wire: a daemon on a laptop authenticates
nobody and meters nothing, a gateway may answer 504 and never 408, a profile may have no streaming
endpoint at all. Declining such a row is not free: the deployment has to say so in words, and the
report then calls the row **unproven** with that reason attached rather than passing it. Write the
row and its reason once, and the macro tells both the harness and the gate:

```rust,ignore
turnframe_test::provider_conformance_suite! {
    name: the_bare_daemon_conforms,
    factory: OllamaFactory::default(),
    fixtures: OllamaFixtures,
    not_producible: [
        AuthenticationFailure =>
            "`ollama serve` authenticates nothing: every request that reaches /api/chat is \
             served, so no credential is ever rejected",
        StatusMapping(TooManyRequests) =>
            "the daemon queues requests behind the runner instead of rejecting them, so \
             nothing in front of /api/chat ever answers 429",
    ],
}
```

Behind it is `DeclaredRows`, which answers both of the harness's declaration hooks
(`WireFixtures::feature_support` for a feature row, `WireFixtures::status_support` for a per-status
one) from that single table, and hands the same rows to the gate as the skips it may tolerate.
Fixtures that already implement either hook keep what they say; the wrapper only adds. What a
declaration is *worth* stays the harness's decision: a reason-less one fails its row, and so does
one on a row that describes the adapter rather than the deployment around it.

## Replay assertions

```rust,ignore
use turnframe_test::replay::{ReplayEvidence, TurnExecution, same_turn};

// Determinism: the same turn twice, artefact by artefact.
same_turn(&first, &second)?;

// Self-explanation: every command has a policy decision and an origin, no
// refused command committed, and every receipt cites events the record lists.
ReplayEvidence::new(&record)
    .with_batch(&batch)
    .with_receipts(&receipts)
    .explains_its_turn()?;
```

## Executor conformance

```rust
use turnframe_test::executors;
use turnframe_test::workflows::trip::conformance_case;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# tokio::runtime::Runtime::new()?.block_on(async {
let report = executors::run_all(&conformance_case()).await;
assert!(report.passed(), "{report}");
// A caller asserting coverage cannot drift as the suite grows.
assert_eq!(report.outcomes.len(), executors::CHECK_COUNT);
# });
# Ok(())
# }
```

Point it at your own executor by implementing `ExecutorFactory`: build a fresh executor, seed one
case, and name three commands: one that appends, one that follows it, and one the domain refuses.
Seven checks run against a fresh executor each, and the runner reports all of them rather than
stopping at the first:

| Check | Rule |
|---|---|
| `check_stale_expected_revision_is_a_conflict` | a batch planned against a superseded (or unreached) revision is refused, and nothing is overwritten (I13) |
| `check_commit_reports_the_revision_it_reached` | the revision in the commit is the one `load` reports and the one the next batch is planned against |
| `check_repeated_key_replays_the_outcome` | a key seen before returns the original outcome and repeats no effect (I14) |
| `check_repeated_key_with_another_command_is_refused` | the same key carrying a different command is a mismatch, never a replay (I14) |
| `check_per_case_batch_is_all_or_nothing` | one refused envelope discards the whole batch, and a `PerCase` batch may not span two cases |
| `check_interrupted_batch_resumes_to_the_same_state` | a batch that half-committed resumes to exactly the state an uninterrupted one reaches |
| `check_refused_command_leaves_the_case_byte_identical` | a refusal writes nothing at all and stays a refusal when it is retried |

Nothing panics: every check returns a `CheckOutcome`, so the suite runs outside a test harness: in
a migration tool, or as a boot-time gate on a freshly written adapter. Two states are compared
through their canonical digest rather than rendered, so a failure message never carries an
adopter's data.

The fourth-from-last row is the reason the suite exists. An implementer who writes `load` and
`execute` over their own tables will test the happy path, will test a stale revision, will probably
test a repeated key, and will still get partial-batch recovery wrong, because producing a
half-committed batch takes deliberate effort and the bug it hides only appears when a process dies
between two commands. The suite cannot half-commit a batch through the `WorkflowExecutor` trait, so
`ExecutorFactory::interrupt_after` asks the implementation to do it. If a partial batch is
impossible in your executor because every envelope commits inside one transaction, implement that
hook by executing the whole batch: the checks then prove the stronger property instead of a weaker
one.

The suite is falsified, not merely run. `tests/executor_conformance.rs` keeps two executors that are
broken on purpose (one that misreports its revision, one whose idempotency memory is keyed on the
batch instead of the envelope) and asserts that each makes exactly one named check fail. Every
other check was falsified the same way by mutating the in-memory executor itself.

## The three sample domains

Together they make a travel-disruption desk.

**`workflows::trip`** is the disruption case of one booking, and it is the awkward workflow on
purpose. Several obligations are open at once; one of them is per extra, so two extras without a
payer are two distinct obligations rather than one checkpoint that flickers. The rebooking card is
a persistent card bound to the case revision and to a hash of what it shows, so when the airline
quotes a new fare the card is drawn again at the new revision and a click on the old one is stale;
editing the trip while the card is open takes it down instead of confirming what the user no
longer sees. The rebooking goes to an airline that may answer, refuse, or never answer
(`AirlineMode`), and the states after it (sent, ticketed, refused, the traveler told or not) never
collapse into "done". A leg the traveler asked to keep is protected: the domain refuses any
command that would change it, in the turn that asked and in every later one. Rebooking is
externally regulated and is the only trip step a card confirms; `TripWorkflow::with_cards` also
puts a click in front of withdrawing and a review card in front of changing the traveler, for
exercising the card machinery.

**`workflows::traveler`** is the flat onboarding slice: three fields, no parameterized obligation,
all given in text, and an activation card derived from the projection once every field is settled.
`TravelerWorkflow::with_cards` also puts a click in front of deletion and a review card in front of
a new contact address. One of its fields, the loyalty number, is **three-valued**: untouched,
answered, or *declined*. A collection workflow with two states per field has a defect every
individual turn passes: either an obligation that never closes, so the assistant asks the same
question for ever, or an obligation silently dropped by a projector that can no longer tell a
decline from an answer. The sample closes the obligation on a decline and keeps the reason in the
view, in a notice whose *code* names it, because "there is no such number", "I would have to look
it up" and "I have it and I am not giving it to you" are three different facts and only one of
them is worth raising again. The pattern is documented on the module and proved by
`tests/three_valued_field.rs`.

**`workflows::claim`** is the worked recipe for **proposed values awaiting review**: a receipt
arrives, values are read out of it and held as a proposal, a review card asks the user to confirm
them, and only then do they become state. A proposal is domain state, and modelling it as such gets
every property it needs out of the vocabulary already there. The projector says "these N fields are
proposed from attachment A" as one parameterized obligation per proposed value plus a notice naming
the receipt; the card's payload hash covers the proposal rather than the whole state, because the
proposal is what the card shows; editing a proposed value is a different operation from answering
the review, with a different policy; and abandoning the review has an effect the domain chose out
loud (the reading is discarded, the receipt is kept, and the case says so), while declining the card
commits nothing and leaves the proposal alone. The recipe is written on the module, each step
pointing at the test in `tests/receipt_claim.rs` that proves it.

### What all three do with an erased payload

`WorkflowDefinition::receipts` is handed `ReceiptEvent`s, so each sample has a second arm for an
event whose payload was erased from the ledger, which is how personal data leaves an append-only
journal (`EventJournalWriter::redact_payload`). All three render a receipt for it rather than
skipping it, because a turn that quietly drops a receipt reads as a turn in which nothing happened.
The copy is the least any of them could honestly say: the step is on record, its detail was erased.
It quotes no value, there being none left, and it prints no event type either, that being an
internal label. The receipt still cites the event, so it is still backed and still passes the claim
guard, which is what `tests/claim_guard.rs` pins, for one erased event in a turn and for a turn
whose events were all erased.

## Example

```rust
use turnframe_test::explore::{ExplorationLimits, explore};
use turnframe_test::workflows::trip::{TripModel, TripOutcome, TripWorkflow};

let report = explore(
    &TripWorkflow::default(),
    &TripModel::default(),
    ExplorationLimits::standard(),
);

assert!(report.is_clean(), "{}", report.describe());
assert!(report.reached_outcome(&TripOutcome::Notified));
```

## Scope

This crate depends on `turnframe-core`, `turnframe-provider` (with its `conformance` feature, which
is why `wiremock` comes with it) and `turnframe-store`. It never depends on `turnframe-runtime`, so
a runtime test can use it without a dependency cycle.

Between them, the three conformance suites reachable from here cover everything a deployment writes:
`stores::conformance` for a store over their database, `providers::conformance` for a model adapter
over their vendor, and `executors` for the workflow executor that sits between the two.

Part of the [Turnframe](https://github.com/turnframe-rs/turnframe) workspace. See the workspace
[README](../../README.md) and [architecture guide](../../docs/architecture.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
