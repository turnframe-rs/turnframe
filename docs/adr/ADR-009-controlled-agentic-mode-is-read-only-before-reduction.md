# ADR-009: Controlled agentic mode is read-only before reduction

- Status: Accepted (2026-09-05)
- Amended by ADR-016 (2026-09-26): `ReadAgentic` and `ControlledAgentic` are removed. Reads stay read-only and before reduction; in 0.1 they are declared and not run.

## Context

Turnframe supports both non-agentic and agentic products on the same core. The
`OrchestrationMode` enum offers four modes: `Deterministic` (one structured
interpretation call, no tool loop), `ReadAgentic` (a bounded read-only loop
before the final interpretation), `ControlledAgentic` (read-only planning plus
model-proposed acts), and the experimental `SandboxedAutonomous`, which is
restricted to sandboxed, reversible domains.

`ControlledAgentic` is the mode most adopters will reach for when they want a
conversational application that feels capable: the model can look things up
before it answers, and it can propose that things change. It is also the mode
where the boundary between "looking" and "changing" is most tempting to blur.
Generic agent frameworks blur it by design: the model is handed a flat list of
tools, some of which read and some of which write, and it calls them in a loop
until it decides it is done. This ADR records why Turnframe does not do that,
and what "controlled" means concretely.

The failure modes this decision prevents are the ordinary ways a conversational
application goes wrong when writes can happen inside the model loop:

- A user writes "change the amount on the second line to 120, actually no,
  leave it". A loop that writes as it goes has already updated the line before
  it reads the correction. The final state is wrong, and the transcript makes
  it look like the assistant did what it was told.
- A user has two open cases of the same kind. The model, mid-loop, picks the
  most recent one because that is the plausible reading, and mutates it. There
  was never a point at which the runtime could stop and ask "which one?".
- A model returns a batch where the first two tool calls are valid and the
  third is malformed. A loop that executes as it parses has already committed
  two effects when it discovers the response was unreliable.
- A provider times out mid-loop after a write has happened. The natural
  recovery, retrying the loop from the top, repeats the write.
- The model narrates "I have sent the rebooking" because the tool call returned
  `200 OK` in the loop, before anything was committed to the ledger. If the
  commit later fails, the user has a receipt for something that did not happen.

Each of these is a case where a side effect happened before the whole turn was
understood, before targets were resolved deterministically, or before a policy
check could run. The spec's invariants I8, I9, I10, I12, I17, and I18 exist to
rule these out. `ControlledAgentic` has to honour them with the same rigour as
`Deterministic` does, or the mode split is meaningless.

## Decision

1. In `ControlledAgentic` mode, the bounded loop that runs before the final
   interpretation MUST be a read-only loop. The only tools visible to the model
   in that loop are read tools drawn from the read tool registry (spec §21.1),
   authorized for the current actor and purpose.
2. Read tools MUST NOT mutate application state or external state. A read tool
   may query case state, search authorized records, retrieve knowledge
   documents, calculate pure values, resolve external reference data, or
   inspect attachments. Anything else is not a read tool and MUST NOT be
   registered as one.
3. The runtime MUST NOT expose any write command, act executor, command
   handler, or credential to the model inside the read loop. There is no write
   tool the model can call; writes do not exist at that layer.
4. Within the read loop the model MAY return either a complete `UserTurnPlan`
   or a list of read requests. The runtime MUST validate every read request in
   a response before executing any of them. If one request is malformed, the
   runtime MUST execute none of the requests from that response.
5. The loop MUST be bounded by the configured `max_rounds` and `max_calls`, and
   every read MUST run under the declared per-tool timeout and result size cap.
   Exhausting a budget ends the loop; it never escalates to writing.
6. The model's proposed acts are semantic acts, not tool calls. They MUST enter
   the runtime only as part of the single `UserTurnPlan` that closes the loop,
   and they MUST pass through the `TurnReducer` (spec §13) before anything is
   dispatched: schema validation, evidence validation, target resolution,
   correction and negation handling, policy evaluation, and confirmation
   derivation.
7. Nothing the model does in the read loop MAY constitute a trusted command
   origin. Consequential commands still require a server-issued origin such as
   a confirmed `Interaction`; the fact that the model "decided" to do something
   during the loop grants it nothing.
8. Read results appended to the loop MUST be treated as untrusted context. They
   inform the plan; they never authorize it.
9. The distinction between `ReadAgentic` and `ControlledAgentic` is therefore
   only in whether the closing plan may carry proposed acts. The read loop
   itself MUST be the same code path with the same guarantees in both modes.

## Consequences

Positive:

- Every invariant that protects the `Deterministic` mode applies unchanged in
  `ControlledAgentic`. Adopters get context acquisition without a second,
  weaker safety model.
- The reducer sees the whole message, so "actually, leave it" and "do not
  submit" are resolved before any effect. Ambiguous targets become a selection
  `Interaction` instead of a guess.
- Provider fallback and retry inside the read loop are always safe, because
  nothing has been committed yet. Only post-commit narration needs the
  separate "regenerate without re-executing" path.
- Receipts remain event-authorized. The model cannot observe a write inside
  the loop and claim it, because no write is observable there.

Negative:

- Multi-step workflows that genuinely need "write, then observe, then write
  again" inside one user message are not expressible in this mode. They must
  be modelled as several turns, or as a domain saga driven by the reducer and
  the outbox, not by the model.
- Adopters coming from tool-calling frameworks must reframe "tools" into two
  registries (read tools and acts) and resist the reflex to register a write
  as a tool because it is convenient.

What adopters must do:

- Register mutating operations as acts in the act registry with an explicit
  `ActMutability`, never as read tools.
- Keep read tool implementations genuinely side-effect free, including no
  logging into domain tables and no cache writes that alter later reads of
  authoritative state.
- Import MCP tools into the read registry only when they are read-only; keep
  write authorization and confirmation in the command bus (spec §21.4).
- Reach for `SandboxedAutonomous` only for sandboxed, reversible domains, and
  expect the library to make that configuration loud.

## Alternatives considered

Mixed read/write tool loop (the conventional agent design). The model receives
one tool list containing both reads and writes and calls them until it stops.
Rejected because it makes I10 (whole-turn planning precedes effects) and I18
(model arrays are all-or-nothing) impossible to enforce: by the time the
runtime sees the whole turn, effects have already happened. It also makes I17
unenforceable, since a provider failure mid-loop leaves the runtime unable to
tell whether retrying repeats a committed write.

Write tools guarded by per-call policy checks inside the loop. Each write tool
call is intercepted, policy-checked, and executed immediately if allowed.
Rejected because per-call checks cannot see later corrections in the same
message, cannot resolve ambiguity across acts, and turn every confirmation into
an interruption of the model loop rather than a persisted `Interaction`. The
model also ends up observing write results and narrating them, which conflicts
with event-authorized claims (I16).

## Enforcement

Invariants from spec §4 this decision implements or directly supports: I8
(ambiguous target means no mutation), I9 (model output is a proposal), I10
(whole-turn planning precedes effects), I12 (critical command origins are
trusted), I16 (events authorize claims), I17 (provider failure cannot repeat
effects), I18 (model arrays are all-or-nothing), and I19 (critical state reads
fail closed, applied to the read loop's authorization of read tools).

Tests that prove it (spec §27):

- Runtime integration scenarios 1 ("change X, actually leave it" causes no
  mutation), 5 (two same-kind cases produce a selection interaction), 6 (a
  malformed second act executes zero acts), 12 (provider fallback before
  commit is safe), 16 (a read-only question does not lose the active case),
  and 20 (no critical success phrase without a matching receipt or event). Each
  MUST run under `ControlledAgentic` as well as `Deterministic`.
- Property tests that no high-risk command exists without a trusted origin,
  extended so that arbitrary read-loop transcripts never yield a command.
- A registry test that every registered read tool, including MCP-imported
  ones, is rejected if it declares or exhibits mutability.
- Chaos tests that inject provider failure during the read loop and assert no
  command journal entry and no event exists afterwards.

Release gates from spec §33 this decision serves: "No consequential command can
originate from raw model output", "No ambiguous target can execute a
mutation", "No malformed multi-act response executes a subset", "Corrections
and negations are resolved before effects", and "Fallback never repeats a
possibly committed command".

Responsible crates: `turnframe-runtime` owns the read-only context loop (step
G of the turn execution algorithm, spec §23), the strict acquisition of one
`UserTurnPlan` (step H), and the reduction step (step K); it is where the
budget, the all-or-nothing batch validation, and the absence of any write path
are enforced. `turnframe-core` owns the `OrchestrationMode`, `ReadToolDefinition`,
and `ActDefinition` types that make the read/act split visible at compile time.
An MCP adapter, which spec §21.4 describes as optional and which does not ship
in 0.1, is responsible for admitting only read-only MCP tools into the read
registry when it exists. `turnframe-test` supplies the fake providers and
model-script doubles that let the tests above drive the loop deterministically.
