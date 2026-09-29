# ADR-002: Flow Map is a pure workflow projector

- Status: Accepted (2026-09-05)

## Context

Turnframe promises that models propose meaning, deterministic reducers decide effects, and
committed events decide claims. The Flow Map is the architecture that sits between persisted
domain state and everything else in a turn: it answers the question "where is this case in its
workflow, what is still owed, and what is the user being asked right now?" Every downstream
component depends on that answer. The `TurnInterpreter` receives its catalog of allowed acts
from it, the `TurnReducer` compiles acts into commands against it, the `Interaction` engine
derives blocking cards from it, and the response composer describes the situation to the user
from it.

Spec §8 states the position bluntly: the Flow Map is not the command executor and not a prompt.
It is a pure projection of domain state into a `WorkflowView`. This ADR records why that purity
is a hard boundary, by naming the failures it prevents.

**The transcript drifts from the record.** In a conversational app it is tempting to let the
model infer "we are in the confirmation step" from the last few messages. The moment two sources
of truth exist, they diverge: a reload shows a different step than the live session did, a second
device sees a different card, and a correction the user typed three messages ago is silently
forgotten because the model no longer sees it. Spec §4 I1 closes this door: persisted state is
authoritative, and the transcript is evidence and history, not workflow state.

**The same case projects to different views on different calls.** If projection performs I/O
(reads a clock, queries a service, calls a model, consults a feature flag), two projections
of the same state snapshot may disagree. The reducer then compiles an act against one view
while the interaction engine persisted a card against another. The user clicks "Confirm" on a
card whose meaning the runtime no longer recognises, or a stale card executes because nobody
can prove which view it belonged to. Spec §4 I2 forbids this: same workflow version plus same
state snapshot must yield the same view with no I/O.

**Phases blur together.** When a projector is allowed to keep its own scratch state or to blend
live lookups with stored state, the result is a case that is simultaneously "collecting" and
"awaiting confirmation", or one that is in no phase at all. The response composer cannot say
one honest sentence about such a case. Spec §4 I3 requires exactly one lifecycle phase.

**Replay becomes impossible.** Spec §4 I20 requires that a turn can be replayed. A projector
with hidden inputs cannot be replayed, so an audit that asks "why was this user shown a
send-confirmation card?" has no answer that can be reproduced.

**Testing collapses to fixtures.** Spec §8.5 observes that fixture coverage alone is insufficient
and asks for bounded state exploration over reachable states. Exploration is only meaningful
when projection is a function of state: an impure projector cannot be enumerated, only sampled.

The forces pulling the other way are real. Domain authors want to read the current date inside
projection to decide whether a travel date is overdue, or to look up a traveler record to decide
whether an obligation is satisfied. Product teams want a model to "just understand" where the
conversation is. These conveniences are the sources of the failures above, so they are moved
outside the projector rather than accommodated inside it.

## Decision

1. The Flow Map MUST be implemented as a pure projection: `WorkflowDefinition::project` takes
a `CaseRef` and an optional state snapshot and returns a `WorkflowView`. It MUST NOT perform
I/O of any kind, including clock reads, random number generation, database or service access,
model calls, or environment lookups.
2. Given the same `WorkflowVersion` and the same state snapshot, projection MUST return an equal
`WorkflowView`. Any input that could change the result MUST be part of the state snapshot, loaded
by the `WorkflowExecutor` before projection, not fetched during it.
3. The `WorkflowView` MUST carry exactly one `phase`, zero or more `obligations`, at most one
`blocking_interaction`, any number of non-blocking `notices`, and an `outcome` that is present
only when the workflow is actually complete. These are separate fields by design; a phase MUST
NOT be encoded as a set of overlapping checkpoints.
4. Projection MUST NOT execute commands, emit events, persist anything, or compose user-facing
prose. Those responsibilities belong to the `TurnReducer`, the `WorkflowExecutor`, the `Interaction`
engine, and the response composer respectively.
5. Projection MUST NOT consume the transcript. The transcript is evidence for the `TurnInterpreter`;
the projector reads only the persisted state snapshot (§4 I1).
6. A phase that requires user action MUST derive an `InteractionRequirement` in the view (§4
I6). The requirement describes what must be asked; the runtime, not the projector, persists
the `Interaction` and hands out its identifier.
7. Terminal phases MUST NOT carry a blocking interaction, and an `outcome` MUST NOT be present
while mutable obligations remain (§8.4).
8. Obligation identities MUST be stable and unique within a projection, and parameterized
obligations MUST embed the stable entity identifier they refer to, such as `AssignPayer {
extra_id }` (§4 I4, §8.4).
9. A change in projection semantics MUST be accompanied by an explicit `WorkflowVersion` change
(§8.4). Two versions of the same workflow key MAY coexist in the `WorkflowRegistry`; a persisted
case is always projected by the version recorded against it.
10. The pure types and the `WorkflowDefinition` trait MUST live in `turnframe-core`, which
MUST NOT depend on an async runtime, HTTP, database, or provider crates (§6.1, §6.2). The type
signature of `project` is synchronous precisely so that the compiler enforces most of this decision.

## Consequences

**Positive.** Every consumer of a `WorkflowView` sees the same case the same way within a turn,
so the act catalog, the compiled commands, the persisted card, and the spoken response cannot
disagree about where the case is. Replay (§4 I20) and audit reconstruction become mechanical:
store the state snapshot and the workflow version, re-run projection, obtain the identical
view. State exploration (§8.5) can enumerate reachable states breadth-first and assert the
§8.4 invariants on each, which is how workflow defects are found before a user finds them.
Unit tests of projection need no fixtures, mocks, or async runtime. Adding a new provider or
a new store never changes how a case is projected.

**Negative.** Domain authors lose the convenience of "just look it up" inside projection. Anything
the view depends on must be materialised into the state snapshot by the executor's `load`, which
makes the state type larger and pushes freshness questions (how old is this snapshot?) onto
the loading step. Time-dependent phases such as "overdue" require the reference instant to be
captured into the snapshot at load time, which is slightly more ceremony than reading a clock.
Projection semantics changes force an explicit version bump even when the change looks cosmetic,
because there is no other way to know which version produced a persisted view.

**What adopters must do.** Model every projection input as part of `WorkflowDefinition::State`,
including reference time and any derived flags that would otherwise require a lookup. Keep
`project` free of side effects and of dependencies on anything but its arguments; treat a compile
error from a missing `async` as the design working as intended. Implement a `WorkflowModel` in
the test kit for each workflow so exploration can run. Bump `WorkflowVersion` whenever phases,
obligations, interaction requirements, or outcomes change meaning, and keep the previous version
registered for cases persisted against it. Put user-facing wording in receipts and response
composition, never in the view.

## Alternatives considered

**A. Model-inferred workflow position.** Let the `TurnInterpreter` read the transcript and the
current record and state which step the case is in, with the reducer trusting that statement.
Rejected because it violates §4 I1 and I2 at once: the transcript becomes the state of record,
and the answer varies between calls to the same model on the same inputs. It also collapses
the technical promise, since the model would be deciding effects (which commands are allowed)
rather than proposing meaning. Spec §4 I9 is explicit that model output is a proposal.

**B. Projection with controlled I/O.** Keep a projector but allow it to read a clock and query
read-only services through an injected context, on the argument that reads are harmless. Rejected
because determinism, not mutation, is the property at stake. Two reads at different instants
yield different views for the same snapshot, which breaks replay (§4 I20), makes the view
unreproducible in audit, and prevents exhaustive state exploration (§8.5). The same information
is available by capturing it into the snapshot during `WorkflowExecutor::load`, where freshness
is an explicit, testable decision.

**C. Fused projector and executor.** Merge projection and command execution into one stateful
workflow object that both reports the current step and mutates the case, as many state-machine
libraries do. Rejected because it makes the view a by-product of execution rather than a function
of state: the reducer could not obtain a view without being willing to mutate, exploration would
have to run real side effects, and the §6.2 dependency rule (core depends on nothing) could
not hold. Spec §8.2 deliberately splits `WorkflowDefinition` (pure) from `WorkflowExecutor`
(async, effectful).

## Enforcement

**Invariants implemented.** This decision is the implementation of §4 I1 (persisted state
is authoritative), I2 (projection is pure), I3 (one lifecycle phase), I4 (parameterized
obligations), and I6 (user-owned phase implies a real interaction requirement), together with
the seven projection invariants listed in §8.4. It also supplies the precondition for I20
(replay is possible), since a replayable turn needs a reproducible view.

**Compile-time enforcement.** `WorkflowDefinition::project` is a synchronous method on a trait
defined in `turnframe-core`, and that crate carries no async runtime, HTTP, database, or provider
dependency (§6.1, §6.2). Dependency rules are checked in CI via the workspace's dependency policy
so that I/O capability cannot be smuggled into the core crate.

**Tests that prove it.**

- Pure unit tests (§27.1): workflow projection, phase exclusivity, obligation generation, and
interaction requirements are unit-tested without fixtures or a runtime.
- Property tests (§27.2): projection determinism (project the same snapshot twice, assert
equal views), serialization round trips of `WorkflowView`, and "no terminal state with
mutable obligations".
- State exploration tests (§27.3): for each `WorkflowModel`, generate bounded reachable
states breadth-first and assert exactly one phase, unique obligation identities, blocking
interaction present for user phases and absent for terminal phases, and reachable terminal
outcomes where expected.

**Release gates that depend on it.** The workflow gates of §33 are the acceptance criteria for
this ADR: exactly one phase for every generated reachable state; parameterized obligations cover
repeated entities; user phases derive blocking interactions; terminal outcomes are explicit
and domain-correct; projection behavior is versioned; and state exploration finds no dead end
lacking an explicit user, system, or external trigger. The operational gate "audit records
reconstruct command authorization and claims" also depends on projection being reproducible
from stored inputs.

**Responsible crates.** `turnframe-core` owns the `WorkflowDefinition` trait, `WorkflowView`,
`InteractionRequirement`, and the version and identifier types. The runtime crate (`turnframe-runtime`)
owns the `WorkflowRegistry`, the type-erased adapter, and the rule that a case is projected by
the version recorded against it. The test kit crate (`turnframe-test`) owns the `WorkflowModel`
trait, the bounded breadth-first explorer, and the property-test helpers that assert the §8.4
invariants; every example workflow under `examples/` is expected to ship a model and be covered
by exploration in CI.
