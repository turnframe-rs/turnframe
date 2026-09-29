# ADR-003: Lifecycle phase and obligations are separate

- Status: Accepted (2026-09-05)
- Related: master spec §4 (I3, I4), §8.1, §8.4, §31.2; ADR-001 (the LLM is an untrusted interpreter), ADR-002 (projection is pure and persisted state is authoritative)

## Context

A conversational application that drives a real workflow (a trip, an onboarding file, a support case) has to answer two different questions on every turn. The first is "where is this case in its life?": is it still being collected, waiting for the user to confirm, in transit to an external system, rejected but correctable, issued, cancelled. The second is "what still has to happen before it can move on?": pick a traveler, add at least one line, classify the third line, set a travel date. The first question has exactly one answer. The second has zero, one, or many answers at the same time, and several of those answers point at specific entities inside the case.

The architecture this project replaces modelled both questions with a single flat list of static checkpoints, each with a state such as pending, satisfied, or blocked. That representation looked simple and produced concrete failures in a conversational app:

- Two checkpoints could both be "current" after an unusual sequence of edits, so the assistant told the user the rebooking was ready to send while, in the same reply, asking for a traveler. The user could not tell which statement to trust, and the model that composed the reply had no way to know either.
- Repeated obligations did not fit a fixed checkpoint list. A trip with four extras where two have no payer needs two distinct open obligations, each tied to an extra ID. One "assign payers" checkpoint hid which extras were affected, so the assistant asked "who pays for the extra?" without saying which one, and a later answer for one extra marked the whole checkpoint satisfied while another had none.
- Because a checkpoint carried both "who acts next" and "what is being watched" in one owner field, the projection could not say cleanly that the case was in a system-owned dispatching phase while a user-owned obligation (a missing note) was still open and harmless. The result was either a spurious blocking card or a silently dropped requirement.
- Terminal states leaked. A case could be reported as delivered while the checkpoint list still contained an unsatisfied entry, because nothing prevented the map from having a completed lifecycle and open work at the same time.
- Every new small requirement meant adding a new checkpoint and re-deriving the exclusivity rules by hand, and every such change altered what "current step" meant for existing cases without a version bump.

The spec makes the separation an invariant rather than a modelling preference: I3 says every represented case resolves to exactly one lifecycle phase, and zero or multiple phases are map defects; I4 says the workflow may have several simultaneous obligations, which may carry identifiers such as `AssignPayer { extra_id }`. §8.1 gives the shape of the projection output, `WorkflowView`, in which phase, obligations, blocking interaction, notices, and outcome are distinct fields with distinct cardinalities. §31.2 explicitly lists "static checkpoints for repeated obligations" and "one owner field carrying both next-actor and claim-monitoring semantics" among the behaviors that must not be ported unchanged.

In the separated shape, the trip example from §8.1 reads as follows for a trip with a traveler selected and two extras, one of which has no payer yet:

```text
phase:                Collecting                      (exactly one)
obligations:          [AssignPayer { extra_id: <extra-2> }, SetTravelDate]
blocking_interaction: None                            (nothing needs a decision yet)
notices:              []
outcome:              None                            (not complete)
```

After the user confirms the rebooking, the same case reads `phase: Dispatching`, `obligations: []`, `blocking_interaction: None`, and still `outcome: None`, because the airline has not answered. Only when the traveler is notified does the view become `phase: Notified` with `outcome: Some(Notified)`. At no point do two of these fields have to be read together to work out what is true.

## Decision

1. A `WorkflowView` MUST expose the lifecycle phase as exactly one value of a domain-defined phase type. There is no "no phase" and no "several phases" representation; a projector that cannot decide on one phase has a defect, not a valid edge case.
2. A `WorkflowView` MUST expose open obligations as a collection of a domain-defined obligation type, with zero or more entries. Obligations MUST NOT be encoded as phases, and the absence of obligations MUST NOT by itself imply that the workflow is complete.
3. An obligation that refers to a repeated entity (an extra, an attachment, a leg) MUST carry the stable identifier of that entity as a parameter. A projector MUST NOT collapse several instances of the same requirement on different entities into one unparameterized obligation.
4. Obligation identities MUST be stable across projections of equivalent state and unique within a single projection, so that a card, a metric, or a test can refer to "this obligation" and get the same answer on reload.
5. The terminal outcome MUST be a separate optional value. It MUST be present only when the represented workflow is actually complete, and it MUST be absent whenever a mutable obligation remains open. A phase whose name sounds final does not by itself constitute an outcome.
6. The blocking interaction requirement MUST be a separate optional value, derived from the phase and the obligations, never from a field that also encodes who acts next. A user-owned phase MUST derive one blocking interaction requirement; a terminal phase MUST derive none.
7. Domain crates MUST NOT reintroduce a single per-case status or owner field that combines lifecycle position, open work, and claim monitoring. Where a legacy representation exists, the projector MUST translate it into the separated shape at the boundary.
8. Any change to how phases or obligations are derived from the same state MUST be accompanied by an explicit workflow version change.

## Consequences

Positive:

- The composed reply can be truthful about two orthogonal facts at once: "this trip is being collected" and "these three things are still missing". The runtime can present the missing items as a list and the phase as context, and the two never contradict each other because they come from different fields.
- Parameterized obligations let the runtime ask about a specific extra, let an answer for extra 2 close only the obligation for extra 2, and let tests assert that every extra with no payer produces its own obligation.
- Exactly-one-phase is a property that a bounded state exploration can check mechanically over every reachable state, which turns a class of conversational contradictions into a build-time failure.
- Terminal outcomes become explicit and domain-correct (ticketed, withdrawn, refunded) instead of being inferred from an empty checkpoint list.

Negative:

- Every workflow needs three domain types (phase, obligation, outcome) plus a derivation for the interaction requirement, which is more up-front modelling than a checkpoint list.
- The projector has to decide phase precedence deliberately. If two conditions each suggest a phase, the domain author has to write the rule that picks one; the library rejects the ambiguity rather than resolving it silently.
- Adopters migrating from a checkpoint-style model cannot map checkpoints one-to-one; repeated requirements have to be re-expressed with entity identifiers, and the "current step" notion has to be split into phase and obligations.

What adopters must do:

- Define the phase enum so that variants are mutually exclusive by construction and cover every reachable state, including error and external-wait states.
- Define obligations as a separate enum, adding identifier fields wherever the requirement can apply to more than one entity.
- Define the outcome enum as the small set of ways the workflow can actually finish, and make the projector return it only when no mutable obligation remains.
- Register a workflow model for the test kit's (`turnframe-test`) bounded exploration so the exclusivity, uniqueness, and terminal properties are checked over generated states, not only over hand-written fixtures.
- Bump the workflow version whenever the derivation of phase or obligations changes.

## Alternatives considered

1. Keep one flat list of static checkpoints with a per-checkpoint status, as in the pre-Turnframe implementation. Rejected because it is the source of the contradictions described above: nothing enforces that a single checkpoint is current, repeated entities cannot be represented without inventing checkpoints per index, and completion has to be inferred rather than stated. §31.2 names this pattern explicitly as one not to port.

2. Model everything as phases, with a rich phase enum whose variants carry the open work as payload (for example, a "collecting" variant holding the set of missing fields). Rejected because it forces every combination of open work into the phase type, so the exactly-one-phase property becomes vacuous (there is always one value) while the useful checks (unique obligation IDs, parameterized entity coverage, no terminal outcome with open work) become hard to express. It also couples changes in obligations to changes in the phase type, so every new requirement changes the lifecycle vocabulary.

3. Model everything as obligations, with the lifecycle reconstructed by the caller from which obligations are open. Rejected because the lifecycle is not a function of open work alone: a trip in transit to an external system and a trip that was rejected but is correctable can have identical open obligations and require completely different handling, interactions, and language. Callers would each rebuild an implicit phase, and they would rebuild it differently.

4. A single owner or status field per case that encodes both next actor and what is being monitored. Rejected because a system-owned dispatching phase can coexist with harmless user-owned open work, and because the same field would then decide whether a blocking card is required; §31.2 lists this coupling among the behaviors not to preserve, and I5 and I6 need the blocking interaction to be derived separately.

## Enforcement

Invariants from spec §4 implemented by this decision:

- I3 (one lifecycle phase) and I4 (zero or more parameterized obligations) directly.
- I5 (at most one blocking interaction per case) and I6 (user-owned phase implies a real interaction) rely on the blocking interaction being a distinct derived field rather than an owner flag on a checkpoint.
- The §8.4 projection invariants that depend on this separation: one phase for every generated reachable state; no terminal outcome while mutable obligations remain; a blocking interaction for every user-action phase and none for terminal phases; stable, unique obligation IDs; parameterized obligations carrying stable entity IDs; explicit workflow version change when projection semantics change.

Tests and release gates that prove it:

- §27.1 pure unit tests for phase exclusivity and obligation generation.
- §27.2 property tests for projection determinism and for "no terminal state with mutable obligations".
- §27.3 state exploration tests: for each workflow model, generated reachable states must have exactly one phase, unique obligation IDs, and derived interactions for user phases.
- §33 workflow gates: exactly one phase for every generated reachable state; parameterized obligations cover repeated entities; user phases derive blocking interactions; terminal outcomes are explicit and domain-correct; projection behavior is versioned.
- Milestone 1 exit gate (§32): the example trip projector passes exclusivity and reachability properties.

Responsible crates:

- `turnframe-core` owns the projection types (`WorkflowView`, the `WorkflowDefinition` trait, the interaction requirement type) and the pure checks that a produced view is well-formed. It has no async runtime and no I/O, so these checks can run in unit and property tests without infrastructure.
- `turnframe-test` owns the bounded state exploration (the workflow model trait and breadth-first exploration with configurable limits) that asserts the phase, obligation, and outcome properties over generated reachable states.
- `turnframe-runtime` consumes the separated view and must not re-merge the fields; it derives cards from the blocking interaction requirement and persists interactions before any response refers to them, which is the subject of a separate ADR.
