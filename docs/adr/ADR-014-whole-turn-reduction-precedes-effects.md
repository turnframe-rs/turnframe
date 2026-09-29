# ADR-014: Whole-turn reduction precedes effects

- Status: Accepted (2026-09-05)
- Amended by ADR-016 (2026-09-26): all-or-nothing applies per unit and per record, not per model answer.

## Context

A natural conversational turn is rarely a single instruction. In one message a user may set two fields, ask a question about a third, and then correct or withdraw something they said a few words earlier. "Change the travel date to Friday and add a note. Actually, leave the date as it is. Is the discount still valid?" is one turn, not four. Turnframe positions itself as a framework for natural conversation with deterministic workflows, so it cannot ask users to split their thoughts into one action per message (spec §0 rule 13).

Most agent runtimes process such a message incrementally: the model emits a tool call, the runtime executes it, the model sees the result, emits the next call, and so on. That loop makes the following failure modes structural rather than accidental.

- **A withdrawn change is applied anyway.** The runtime executes "change the travel date" as soon as the model proposes it; the correction "actually, leave it" arrives after the write has already committed. The case now carries a value the user explicitly rejected, and the transcript shows the assistant confirming it.
- **A negation is scoped wrongly.** "Do not submit" was meant to block every submission in the message, but a submission act earlier in the model's output has already been dispatched before the constraint is read.
- **A single message produces half a change.** Two fields on the same case were meant to move together. The first write succeeds, the second fails validation, and the case is left in a state nobody asked for, with a partial receipt that reads like success.
- **A question disappears behind an action.** The runtime executes the write, narrates it, and never returns to the question that shared the same sentence. The user has to ask again, and often does not notice.
- **A malformed second act executes a valid first act.** When the model's array of proposed acts is parsed and dispatched one element at a time, a broken element half-way through leaves earlier elements already applied (the failure that invariant I18 names).
- **Provider retry repeats a write.** If interpretation and execution are interleaved, a provider failure in the middle of a turn invites a retry of the whole turn, and the retry re-proposes commands that may already have committed (the failure that invariant I17 names).

The turn execution algorithm of spec §23 makes the ordering explicit: one strict plan is obtained (step H), validated all-or-nothing (I), target-resolved (J), reduced as a whole (K), and only then are pre-execution interactions persisted (L) and eligible command batches executed (M). The `TurnPhase` marker of §23.1 records the same ordering durably, so a crash between phases can be recovered without guessing whether effects already happened.

Every one of these breaks the technical promise that models propose meaning, deterministic reducers decide effects, and committed events decide claims. The spec addresses them with a single structural rule: the complete user turn is interpreted and reduced before any dependent effect executes (spec §0 rule 7, invariants I10 and I11, and the whole-turn reducer of §13).

## Decision

1. The runtime MUST obtain one complete `UserTurnPlan` for the whole user turn (text and any structured interaction response together) before any command derived from that turn is executed. Interpretation is a single strict, all-or-nothing step, not a streamed sequence of executable calls.
2. The runtime MUST pass the complete plan through a `TurnReducer` and obtain a `ReductionPlan` before dispatching anything. Reduction MUST be pure: it produces no side effects and performs no I/O beyond the `ReductionContext` it is given.
3. The reducer MUST resolve corrections, negations, cancellations, conditions, and execution constraints across the entire turn before deciding what is eligible to execute, applying the same-turn precedence rules of spec §13.2 as deterministic defaults: an explicit cancellation supersedes the earlier act; a later explicit correction on the same target and field supersedes the earlier value; "do not submit" blocks every submission act in the turn; a hypothetical question does not become an action.
4. Every proposed act MUST receive exactly one `PlannedActResult`. An act may end as `ReadyToExecute`, `AwaitingConfirmation`, `NeedsClarification`, `Rejected`, `SupersededByCorrection`, or `NoChange`; it MUST NOT disappear silently, and a question MUST remain answerable even when a sibling action needs clarification or was superseded.
5. The reducer MUST group `ReadyToExecute` commands by case and `AtomicityScope`. Mutating fields on the same case default to `PerCase`; a domain that allows partial application MUST make the partial outcome typed and visible, never a generic success.
6. The runtime MUST NOT execute any command before the turn phase marker has advanced through `Interpreted` and `Reduced`. Clarification and review interactions required before execution MUST be created and persisted between reduction and execution (spec §23 steps K, L, M).
7. Model-side retry and provider fallback MUST occur only before the first command of the turn executes, or after commit for narration only. The runtime MUST NOT re-run interpretation or reduction for a turn whose commands may have committed; recovery after that point proceeds by idempotency key and from events.
8. A malformed structured response MUST cause the whole `UserTurnPlan` to be rejected. The runtime MUST NOT reduce or execute the well-formed subset.

## Consequences

Positive

- A correction that arrives later in the same message wins over the value it corrects, and the write that the user withdrew never happens. "Change X, actually leave it" is a no-op by construction.
- Multi-field changes on one case commit together or not at all, so the case never sits in a shape the user did not describe.
- Questions and actions in the same message both receive a result, which keeps the conversation natural without giving up determinism.
- Because reduction is pure and happens before execution, the `ReductionPlan` can be persisted as part of the replay record. Auditors can reconstruct why a turn did or did not execute a command without re-running a model (invariant I20).
- Provider retry before execution is trivially safe, and post-commit narration can fall back to another provider without any risk of duplicating effects.

Negative

- Latency to first effect is the latency of the whole interpretation plus reduction. Nothing consequential can be streamed while the model is still speaking; only safe model-authored blocks may stream, and only after the plan is closed.
- The model cannot observe the outcome of one command before proposing the next within the same turn. Sequential dependencies that genuinely need an intermediate result must be expressed as separate turns, or as read-only context acquisition before interpretation (see ADR-009), not as write-then-read loops.
- Precedence rules are a design surface. A domain with unusual correction semantics must refine them deliberately rather than relying on conversational order.

What adopters must do

- Implement domain `TurnReducer` refinements only as extensions of the default precedence rules, never as bypasses that dispatch a command directly from a proposed act.
- Declare `AtomicityScope` explicitly wherever partial application is acceptable, and surface the typed partial outcome in the response.
- Treat the `PlannedActResult` slot as a contract: response composition must render something for every act, including the superseded and rejected ones, so the user can see what did not happen and why.
- Do not build "agentic" write loops on top of the runtime. Reads may loop (ADR-009); writes are decided once per turn.

Related decisions

- ADR-001 establishes that the model is an untrusted interpreter; this ADR fixes the point at which its proposal becomes decidable.
- ADR-009 confines the agentic loop to read-only context acquisition before reduction, which is what makes a single closed plan per turn feasible.
- ADR-013 handles ambiguous targets; here an ambiguous target blocks only the acts that depend on it, while the rest of the reduced turn proceeds.

## Alternatives considered

1. **Incremental tool-call execution (the conventional agent loop).** Execute each model-proposed call as it arrives, feed the result back, and let the model decide the next step. Rejected because it makes late corrections, negations, and constraints structurally unenforceable: by the time the runtime reads "actually, do not change it", the change has been written. It also couples provider retry to effect repetition and turns a malformed later element into a partial execution of earlier ones.
2. **Interpret the whole turn, but execute acts in proposal order with per-act rollback.** Obtain the full plan first, then dispatch acts one by one and compensate on failure. Rejected because compensation is not available for many consequential effects (an external submission cannot be un-sent), because rollback produces receipts that briefly read as success, and because it still lets a correction appearing later in the plan race the act it corrects unless a reducer pass happens first anyway. Once that pass exists, ordering execution by proposal position adds risk without adding capability.
3. **Ask the model to self-resolve corrections before proposing acts.** Instruct the interpreter to emit only the net acts after applying its own corrections. Rejected as the sole mechanism because model output is a proposal (invariant I9) and cannot be trusted to enforce a safety property; it remains a useful prompt-quality improvement, but the deterministic reducer must still resolve precedence and produce the per-act results.

## Enforcement

Invariants from spec §4 implemented by this decision

- I10 (whole-turn planning precedes effects) is the decision itself.
- I11 (every act receives a result) is enforced by the mandatory `PlannedActResult` slot per act.
- I17 (provider failure cannot repeat effects) follows from confining retry and fallback to before the first execution or to post-commit narration.
- I18 (model arrays are all-or-nothing) is enforced by rejecting the whole `UserTurnPlan` on any malformed element before reduction begins.
- I2 and I9 are prerequisites: reduction is pure, and the plan it reduces is untrusted input.

Tests that prove it (spec §27)

- Pure unit tests for correction/negation reduction (§27.1).
- Property tests for same-turn corrections and random field order (§27.2): a correction placed anywhere in the turn yields the same reduced plan as the corrected act alone.
- Runtime integration scenarios §27.4 no. 1 ("change X, actually leave it unchanged" causes no mutation), no. 2 (several independent fields set atomically), no. 3 (action plus question both receive results), no. 6 (a malformed second act executes zero acts), no. 12 (provider fallback before commit is safe), no. 13 (provider failure after commit regenerates narration without repeating commands), and no. 14 (a database timeout during an atomic batch produces no hidden partial state).
- Chaos tests (§27.7) that inject failure before the command journal insert and during provider streaming, verifying that no command executes before the `Reduced` phase and that recovery never re-reduces a possibly committed turn.

Release gates (spec §33)

- Safety gates: no malformed multi-act response executes a subset.
- Conversation gates: multi-action turns are supported; action plus question works in either order; questions cannot disappear behind actions; corrections and negations are resolved before effects.
- Provider gates: fallback never repeats a possibly committed command.

Responsible crates and modules

- `turnframe-core` owns the pure types: `UserTurnPlan`, `ReductionPlan`, `PlannedActResult`, `AtomicityScope`, and the `TurnPhase` marker.
- `turnframe-runtime` owns the `TurnReducer` implementation with the default precedence rules, the ordering of pipeline steps H through M, and the crash-recovery coordination that forbids re-reduction after commit.
- `turnframe-test` provides the model-script doubles and replay assertions used by the scenarios above.
