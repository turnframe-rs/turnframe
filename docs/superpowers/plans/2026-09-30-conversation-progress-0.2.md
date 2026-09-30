# Turnframe 0.2.0, a conversation always moves forward: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement ADR-021: progress guarantees in code, the assistant's offers as data, the
correction that keeps what it does not restate, and a simulated-user evaluation that finds classes
of failure.

**Architecture:** Next steps become typed operations the runtime dry-runs before offering (I22);
the offers a reply made are recorded on the turn and read first by the next turn's understanding
through one small closed-schema task; the reply's composition gains the remaining guarantees; the
evaluation crate gains goal-driven conversations with a model playing the user and code scoring
them.

**Tech Stack:** Rust 2024, the workspace crates; OpenAI `gpt-5.4-mini` for the paid runs.

**Spec:** `docs/adr/ADR-021-a-conversation-always-moves-forward.md` (Accepted 2026-09-30).

## Global Constraints

- The core names no domain (`no_core_prompt_names_a_sample_domain`); sample words live in
  `turnframe-test` and the examples.
- Comments: `//` at most 4 lines, `///` at most 10, `//!` at most 15 (AGENTS.md).
- One behaviour per test file under `crates/<crate>/tests/`, named as a sentence; unit tests at the
  foot of their module.
- Copy: no em or en dash, no stacked negatives, no «rather than».
- A new public field or changed trait method is breaking: 0.2.0, named in `CHANGELOG.md` with
  migration notes. Every crate moves to 0.2.0, since all depend on `turnframe-core`.
- Paid runs are announced with their cost before they start. No subagents.
- Rewording a prompt for one phrasing is not a fix; a fix changes structure or a guarantee.

## Review Focus

- A next step whose arguments are incomplete (it needs values the user gives): it is offered when
  the view offers the operation, and asks for its values when taken up.
- A message that takes up no offer («add a bag at 40 euros» after «add another extra, or rebook»):
  routing runs as before, the take-up changes nothing.
- A turn with offers from two records: the taken-up offer names its record; nothing is located again.
- The same question asked twice because the user answered something else: the reply says why and
  offers the other ways forward; asked twice because the answer was refused, the refusal is the
  reason already given.
- A simulated user that never reaches its goal within the turn limit: scored as not reached, its
  transcript kept, the run does not fail.

---

### Task 1: Typed next steps, dry-run before offered (I22)

**Files:** `crates/turnframe-core/src/flow/{mod.rs,registry.rs}`, `crates/turnframe-runtime/src/orchestrator/session/answer.rs`, `crates/turnframe-runtime/src/narrate/outcome.rs`, `crates/turnframe-test/src/workflows/trip/definition.rs`, tests.

- `pub struct NextStep { operation: OperationKey, words: LocalizedText, arguments: serde_json::Value }`
  with `NextStep::new(operation, words)` and `with_arguments`.
- `WorkflowDefinition::next_steps(&self, state: Option<&Self::State>, view: &ViewOf<Self>) -> Vec<NextStep>`;
  the erased form takes the case and the state.
- The runtime keeps a step only when the view offers its operation and, when its arguments are
  complete, the act compiles and every command validates against the state.
- The trip sample offers another extra always, and a rebooking of the quoted leg when a quote is in.
- Tests: a step the domain would refuse is not offered; the trip offers rebooking only with a quote.

### Task 2: The offers a reply made are recorded on the turn

**Files:** `crates/turnframe-core/src/response.rs`, the runtime's composition and answer, tests.

- `AssistantTurn::offers: Vec<Offer>`, `Offer { case_ref, operation, words, arguments }`, in the
  order the reply offered them. Only offers the reply carries are recorded.
- Test: a turn ending on next steps records them as offers with their records.

### Task 3: A message is read first against the offers (take-up)

**Files:** `crates/turnframe-provider/src/purpose.rs` (`TakeUp`), `crates/turnframe-understand/src/{input.rs,tasks/take_up.rs,pipeline/mod.rs}`, `crates/turnframe-understand/prompts/understand/take_up.md`, `crates/turnframe-runtime/src/understand.rs`, tests.

- `UnderstandingInput::offers: Vec<OfferBrief>` built from the previous turn's offers.
- For each request or answer unit, when offers exist: `take_up` chooses one offer handle or none.
  A chosen offer is the unit's route: its operation on its record with its arguments; nothing is
  routed or located again for it.
- Tests: «yes» after one offer takes it up; «add a bag at 40 euros» takes none and routes as before;
  an offer on another record keeps that record.

### Task 4: The same question is not asked twice without a reason

**Files:** the runtime's outcome, narration notes, fallback, tests.

- The ask is marked repeated when the previous turn asked the same thing of the same record and it is
  still open; the next steps are offered beside it; the writer is told to say why it asks again;
  code's reply says it (`AskCopy::again`).
- Test: an ask repeated from the last turn carries the next steps and the reason.

### Task 5: Every question gets an answer, or where things stand

**Files:** the runtime's outcome and composition, tests.

- When a question goes unanswered, the outcome carries where its record stands (its narratable state
  and open obligations); the writer gives it; code's reply gives it.
- Test: an unanswered question's reply says where the record stands.

### Task 6: A correction keeps what it does not restate

**Files:** `crates/turnframe-understand` (date evaluation for a correction), tests.

- A corrected date given without its year takes the year of the date it corrects, in the same
  message or from the last turn.
- Test: «10 December 2026, no wait, 12 December» gives 2026-12-12 whatever today is.

### Task 7: Conversations evaluated by simulated users

**Files:** `crates/turnframe-eval/src/simulate/*`, `crates/turnframe-eval/tests/simulated_users.rs`, `crates/turnframe-eval/tests/simulated_users/*.toml`, `docs/evaluation.md`.

- A goal: what the user wants (in words for the simulator), a manner, the state that proves it
  reached, a turn limit. A simulated user: a model answering with the next message, a card option
  to press, or done. A runner over the travel desk. A scorer from the stores and the turns: reached,
  turns, dead ends, loops, parts not understood, offers refused, guarantee violations.
- Offline test with a scripted user; the live run gated like the live corpus, announced and paid.

### Task 8: Release 0.2.0

- CHANGELOG with migration notes; `docs/evaluation.md`; all crates at 0.2.0 on the workspace
  version; the refund recording; the site; the gate; a targeted live corpus run and one simulated
  user run, each announced; one squashed commit, tags, then the owner publishes.
