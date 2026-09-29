# The hostile refund demo: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A home-page section of turnframe.rs where a visitor picks one of nine attacks on a
refund and replays the recorded turn of the real runtime, which never moves the money.

**Architecture:** A new example crate, `examples/refund-desk`, holds a small `order` workflow, the
desk's world (stores, directory, payment provider) and ten scripted runs; it records each run's
frames from the runtime's replay record and response blocks into
`website/src/data/refund-runs.json`. A drift test keeps the file equal to what the runtime does.
The site renders the file with a new `BreakIt` component.

**Tech Stack:** Rust 2024 (workspace crates via the `turnframe` facade with `test-kit`), tokio,
serde_json; Astro 7 + React 19 for the site, `node --test` for its tests.

**Spec:** `docs/superpowers/specs/2026-09-30-hostile-refund-demo-design.md`

## Global Constraints

- The example is a workspace member with `publish = false`; it never enters the test kit, so the
  travel sample, the live corpus and the benchmarks do not change.
- Comments: `//` at most 4 lines, `///` at most 10, `//!` at most 15 (AGENTS.md).
- Copy on the site and in docs: no em or en dash, no stacked negatives, no «rather than»
  (`website/tests/rules.mjs`).
- The core names no domain; nothing here touches core crates.
- Tests: one behaviour per file under `examples/refund-desk/tests/`, named as a sentence.
- Every sentence inside a run on the page comes from the recording; only the attack line and a
  world event's line are authored.
- Commit locally on `main`, no co-author trailers, never push.

## Review Focus

- A minted identifier that differs between two runs of the recorder: the drift test would flap.
  Pinned by recording twice in the drift test and comparing.
- A run whose recorded frames skip a station the page expects: the page must still render the
  station, marked «not reached». Pinned in the site test (every run names its stations).
- A visitor landing on `/#break-<unknown>`: the section falls back to «No attack». Pinned in the
  component (hash lookup with a default).
- Reduced motion or no JavaScript: the first run's frames must be readable in the static HTML.
  Pinned by the site test reading the built page.
- The recording written from a checkout where the site directory is absent: `--record` takes an
  explicit path and fails with the path named.

---

### Task 1: The crate and the `order` workflow

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `examples/refund-desk/Cargo.toml`, `examples/refund-desk/src/lib.rs`,
  `examples/refund-desk/src/order.rs`, `examples/refund-desk/src/main.rs` (placeholder `fn main`
  printing nothing until Task 4)

**Interfaces:**
- Produces: `refund_desk::order::{OrderWorkflow, OrderState, OrderCommand, OrderEvent, OrderPhase,
  operations::{REQUEST_REFUND, REFUND, DECLINE_REFUND}, REFUND_OPTION, KEEP_OPTION,
  REFUND_CARD_KEY, euros(cents: i64) -> String}`.
- `OrderState { number: u32, customer: String, paid_cents: i64, refunded_cents: i64,
  delivered: NaiveDate, pending_cents: Option<i64>, external: Option<ExternalStatus>,
  reference: Option<String> }`, with `refundable_cents()` and `window_open(today)`.
- `OrderCommand::{RequestRefund { cents }, Refund, DeclineRefund, RefundOutside { cents,
  by: String }, RecordProviderOutcome { status: ExternalStatus, reference: Option<String> }}`.
- `OrderEvent::{RefundRequested, RefundSent, RefundDeclined, RefundedOutside,
  ProviderOutcomeRecorded}` with `event_type()` `order.refund_requested` etc.

Rules in `apply` and `validate`: amount over `paid - refunded` refused
(`order.refund_exceeds_paid`, with explanation naming what is left); more than 30 days after
delivery refused (`order.window_closed`); a second request while one is pending refused
(`order.refund_pending`); `Refund` sends `min(pending, refundable)`. Policy: `Refund` is
`ExternalRegulated`, `ExplicitClick`, `ExternalSaga { saga: "order.refund" }`,
`ServerReceiptOnly`; everything else `low_risk`. The view in phase `AwaitingConfirmation` carries
the blocking card: title «Refund this order?», body «€X.XX to <customer>, order <n>. Once sent,
the payment provider decides.», options Refund (primary, `ApplyOperation order.refund`) and «Don't
refund» (`DeclineAndRecord order.decline_refund`); when the pending amount is over what is left,
a review entry «Amount» before `€129.00` after `€99.00`. `narratable_state` gives customer,
paid, refunded, delivered and «refund window: open until <date>» or «closed».

- [ ] Step 1: unit tests at the foot of `order.rs`: a request over what is left is refused with
      `order.refund_exceeds_paid`; a request after 30 days is refused; a second pending request is
      refused; the card shows the before/after entry only when the cap applies; `Refund` sends the
      capped amount.
- [ ] Step 2: `cargo test -p refund-desk --lib` fails (nothing defined).
- [ ] Step 3: write `order.rs` (state, commands, events, `apply`, `validate`, the
      `WorkflowDefinition` and `PureWorkflow` impls), add the crate to the workspace.
- [ ] Step 4: `cargo test -p refund-desk --lib` passes; `cargo clippy -p refund-desk --all-targets
      -- -D warnings` is clean.
- [ ] Step 5: commit «The refund desk example: its order workflow».

### Task 2: The desk: stores, directory, provider, turns

**Files:**
- Create: `examples/refund-desk/src/desk.rs`
- Test: `examples/refund-desk/tests/the_clean_run_refunds_once.rs`

**Interfaces:**
- Produces: `Desk::new(provider: Provider) -> Desk` with fields `stores: FakeStores`,
  `orders: Arc<InMemoryExecutor<OrderWorkflow>>`, `account: AccountId`; methods
  `orchestrator(&self, understood: Vec<Understanding>, answers: Vec<&str>) -> Orchestrator`,
  `orchestrator_reading(&self, tasks: ScriptedTasks) -> Orchestrator` (the real pipeline),
  `send(&self, turn: u128, text: &str) -> TurnInput`, `click(&self, turn: u128, card:
  &Interaction, option: &str) -> TurnInput`, `card(&self, order: &str) -> Option<Interaction>`,
  `events(&self, order: &str) -> Vec<String>`, `outside(&self, order: &str, key: &str, command:
  OrderCommand)`, `dispatch(&self) -> OutboxRecord` and `reconcile(&self, row)`, `token(turn,
  order) -> TargetToken`.
- `Provider::{Accepts, Silent}`: the `OutboxSender` behind the outbox.
- Orders seeded: 381 (Giulia Neri, 12900, delivered 12 days before the fixed today), 318 (Luca
  Moretti, 18900, 20 days); the directory lists them for account `desk`; 402 exists in the
  executor under account `other-shop` only. Fixed clock, fixed turn ids, narration off.

- [ ] Step 1: the test drives «Refund order 381 for €129» with a scripted act, clicks Refund,
      dispatches with `Provider::Accepts`, and asserts events
      `["order.refund_requested", "order.refund_sent"]` then, after the provider's answer, one
      `order.provider_outcome_recorded`.
- [ ] Step 2: run it: fails to compile.
- [ ] Step 3: write `desk.rs`, following `examples/travel-desk/src/main.rs` for the builder,
      `outside`, `click`, the outbox dispatcher and the reconciler.
- [ ] Step 4: `cargo test -p refund-desk --test the_clean_run_refunds_once` passes.
- [ ] Step 5: commit «The refund desk: its stores, directory and payment provider».

### Task 3: The recording

**Files:**
- Create: `examples/refund-desk/src/record.rs`
- Test: unit tests at its foot

**Interfaces:**
- Produces: `Recording { recorded_with, version, orders: Vec<OrderLine>, runs: Vec<Run> }`,
  `Run { id, group, label, attack, message, stopped_at: Option<Station>, verdict: Verdict, frames:
  Vec<Frame> }`, `Frame { station: Station, kind: Kind, state: Option<&'static str>, text:
  String, scripted: bool, card: Option<CardFrame>, option: Option<usize>, rev: Option<u64>, code:
  Option<String>, id: Option<String> }`, all `Serialize` with snake_case names.
- `Station::{Reading, Proposal, Reducer, Decision, Ledger}`; `Verdict::{NothingMoved,
  WaitingOnAClick, OneRefund}` with the page's sentences.
- `Recorder` accumulating frames: `reading(&Understanding)`, `turn(&ReplayRecord,
  &AssistantTurn)`, `world(station, text)`, `click(&Interaction, option)`, `refused(station,
  text)`, `outbox(&OutboxRecord)`, `finish(events, open_card) -> (Vec<Frame>, Verdict)`.
- `Names` renames event, card and outbox ids to `evt_1`, `card_1`, `ob_1` in order of
  appearance.

- [ ] Step 1: tests: frames keep the station order they were recorded in; a verdict with no
      refund event and no open card is «Nothing moved»; with an open card «Waiting on a click»;
      with one `order.refund_sent` and its outcome «One refund, recorded once»; `Names` gives the
      same short name to the same id twice.
- [ ] Step 2: run: fails.
- [ ] Step 3: write `record.rs`. Frame text is built from structures: an act as
      `<operation> on <record label>, <arg> <value>`; a result from `act_outcomes`; a policy
      decision as its risk and confirmation; a command outcome as `committed at rev N` or its
      kind; blocks as their receipt, notice, answer, card or transition text.
- [ ] Step 4: tests pass.
- [ ] Step 5: commit «The refund desk records a turn as frames».

### Task 4: The ten runs, their guarantees, the CLI and the drift test

**Files:**
- Create: `examples/refund-desk/src/runs.rs`; replace `src/main.rs`
- Test, one file each: `the_model_reading_the_wrong_order_meets_a_card_that_names_it.rs`,
  `a_refund_over_what_was_paid_is_refused_before_any_card.rs`,
  `an_order_this_desk_cannot_see_is_asked_about.rs`,
  `a_refund_taken_back_in_the_same_message_moves_nothing.rs`, `a_double_click_refunds_once.rs`,
  `a_turn_sent_twice_draws_one_card.rs`, `a_click_on_a_stale_card_authorizes_nothing.rs`,
  `a_provider_that_never_answers_is_never_sent_again.rs`,
  `a_model_provider_failing_halfway_proposes_nothing.rs`,
  `the_recording_on_the_site_is_what_the_runtime_does.rs`

**Interfaces:**
- Produces: `runs::all() -> impl Future<Output = anyhow::Result<Recording>>` and one `pub async fn`
  per run returning `(Run, Desk)`, so a test asserts on the desk's own state.
- `main`: no argument prints each run (station, state, text) in the travel desk's style;
  `--record <path>` writes `serde_json::to_string_pretty(&recording)` plus a newline.

Per run (message «Refund order 381 for €129» unless noted):
- `no-attack`: act `request_refund` on 381, 12900; click Refund; dispatch Accepts; reconcile.
- `wrong-order`: act on 318; the card names Luca Moretti; click «Don't refund».
- `wrong-amount`: act on 381 with 129000; the domain refuses; no card.
- `unseen-order`: act `apply_to_unlisted(order, "order 402")`; the lookup finds none.
- `take-back`: «Refund order 381 for €129. Actually don't refund it yet, just tell me whether it's
  eligible.»: the refund act `superseded_by_next`, then `ask` about 381; the scripted provider
  answers from the facts.
- `double-click`: as `no-attack`, the Refund click sent twice.
- `sent-twice`: the same turn input handled twice; record what the runtime returns the second
  time; assert the open cards number one. If the runtime runs it again with new keys, correct the
  run and the spec.
- `stale-card`: the card open, `outside(RefundOutside { cents: 3000, by: "a colleague" })`, the
  old card clicked (the refusal recorded), then the redrawn card shown with its entry.
- `timeout`: click Refund, dispatch with `Provider::Silent` (20 ms timeout), row
  `OutcomeUnknown`; dispatch again sends nothing; reconcile Completed; the provider's answer
  delivered twice through `outside` with one key, recorded once.
- `model-down`: `orchestrator_reading` over `ScriptedTasks` answering segment, coverage and route,
  failing `u1/extract` with a timeout.

- [ ] Step 1: write the nine guarantee tests on the desk's state (events, open cards, outbox
      rows), and the drift test: record twice, assert equal, and assert equal to
      `../../website/src/data/refund-runs.json`.
- [ ] Step 2: run them: fail.
- [ ] Step 3: write `runs.rs` and `main.rs`.
- [ ] Step 4: `cargo run -p refund-desk -- --record website/src/data/refund-runs.json`; all tests
      pass; clippy clean; `cargo run -p refund-desk` reads well.
- [ ] Step 5: commit «The refund desk: ten runs, recorded for the site».

### Task 5: The site section

**Files:**
- Modify: `website/src/ds/turnframe.js` (a `BreakIt` component), `website/src/styles/ds.css`,
  `website/src/pages/index.astro` (the section after the manifesto, numbers 02 to 08 after it),
  `website/README.md` (the data file's row)
- Create: `website/tests/refund.test.mjs`

**Interfaces:**
- Consumes: `website/src/data/refund-runs.json` as produced by Task 4.
- Produces: `BreakIt({ recording })`, hydrated `client:visible`; markup with
  `data-break-run="<id>"` per control and `data-station="<station>"` per station.

- [ ] Step 1: `refund.test.mjs`: the recording parses, has ten runs with unique ids, every run's
      frames use only the five stations, every verdict is one of the three, `stopped_at` is a
      station or null; the built `dist/index.html` holds `id="break-it"`, ten controls, and the
      first run's frames as text.
- [ ] Step 2: `npm run build && npm test` fails on the new test.
- [ ] Step 3: write the component (controls grouped, stations column, playback with
      `setTimeout` chain, reduced-motion shows all, hash sync with default), its styles (reusing
      the replay's ledger, card and tag classes), the section and the README row.
- [ ] Step 4: `npm run build && npm test` passes; screenshots at 1280 and 390 wide look right.
- [ ] Step 5: commit «The site: break a refund, frame by frame».

### Task 6: Docs and changelog

**Files:**
- Modify: `website/src/content/examples.md` (the refund desk beside the travel desk),
  `CHANGELOG.md` (Unreleased: the example), `README.md` if it lists examples.

- [ ] Step 1: write the entries; `npm run build && npm test` in `website/`.
- [ ] Step 2: commit «The refund desk in the examples and the changelog».
