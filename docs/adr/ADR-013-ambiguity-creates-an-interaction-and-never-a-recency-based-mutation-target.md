# ADR-013: Ambiguity creates an interaction and never a recency-based mutation target

- Status: Accepted (2026-09-05)

## Context

Turnframe's promise is that models propose meaning while deterministic reducers decide effects. The
weakest link in that promise is the moment a proposed act has to be attached to a concrete record. A
user says "add an extra for the hotel" while two trips are open, or "delete the traveler"
after a search returned three homonyms, or "send it" in a conversation that has touched two
documents. The `TurnInterpreter` can legitimately produce a `ProposedTarget` that names a workflow
and a mention, but it cannot know which row the user means, and the spec forbids letting it invent a
record ID (§12.1). Something has to decide, and the question this ADR settles is *who* and *how*.

The tempting shortcuts are all forms of guessing:

- **Recency.** "The user is probably talking about the document touched last." In a conversational
  application this fails the moment a person switches subjects mid-chat: they open a second draft to
  compare, then say "change the amount", and the change lands on whichever draft the runtime happened
  to touch most recently. The user sees a correct-sounding confirmation and only discovers the wrong
  trip when the traveler complains.
- **List order.** "Take the first candidate the search returned." Search order is an implementation
  detail of the store; a reindex or a sort change silently moves mutations from one record to another.
- **Model confidence.** "The interpreter said 0.92, so trust it." Confidence scores are not calibrated
  against the store, and a model that is confidently wrong produces exactly the same output as a
  model that is confidently right.
- **Conversational plausibility.** "It makes sense in context." This is the one that produces
  ghost-claim incidents: the reply reads naturally, the receipt reads naturally, and the effect
  happened on the wrong case.

The legacy system this framework replaces did exactly the first of these, and §31.2 lists "choosing
the most recent document when a mutation target is ambiguous" as a behavior that must not be ported.
The known wrong-target incidents in trip handling (§31.3) are the concrete cost of that shortcut.

The second force is that a turn rarely contains only the ambiguous act. The same message may carry
an independent question ("what is the baggage allowance for this?") and an unambiguous act on a different
case. Refusing the whole turn because one target is unclear is the safe-but-hostile answer; it makes
the assistant feel like a form that rejects the entire submission over one field.

The third force is durability. If the runtime asks "which trip?" only in prose, the user's answer
("the second one") has to be interpreted again by the model, which reintroduces the guess one turn
later. The disambiguation must be a persistent `Interaction` whose options the server owns, so that
the eventual choice is a stored option ID and not another inference.

## Decision

1. When `TargetResolution` for a proposed act is `Ambiguous`, the `TurnReducer` MUST NOT emit any
   command that depends on that target. Only `Exact` reaches command compilation (§12.2).
2. The reducer MUST NOT break the tie using recency, list or search order, model confidence, or
   conversational plausibility (§4 I8, §12.3 rule 5). No domain hook may reintroduce such a
   heuristic as a "default target" for mutations.
3. The reducer MUST record the ambiguous act's result as `NeedsClarification` carrying an
   `InteractionSpec` of kind `SelectTarget`, whose options are the server-computed
   `TargetCandidate` list. Option labels and actions are stored server-side; the client never
   supplies them (§4 I7, §15.3).
4. The runtime MUST persist the `SelectTarget` interaction before the response refers to it
   (§4 I6, §15.5), and MUST treat it as the case's single blocking interaction (§4 I5, §15.6).
5. Independent questions and independent unambiguous acts in the same turn MUST still receive their
   own results (§4 I11, §12.3 rule 3, §13.2 rule 5). Only acts that depend on the ambiguous target
   are blocked, unless the user explicitly declared an all-or-nothing scope for the turn.
6. The response MUST make the partial outcome explicit (§12.3 rule 4): what was done, what is
   waiting on the selection, and which candidates are offered. A reply that reads as full success
   while an act is parked is a defect.
7. Resolution of a `SelectTarget` interaction MUST bind the chosen stored option to a target token
   that then resolves `Exact`; the resumed act goes through normal policy and confirmation checks.
   Selecting a target is never itself a confirmation of a high-risk command.
8. The runtime MUST emit `turnframe.target.ambiguous` whenever rule 1 fires, without user or case
   text as labels (§26.2).
9. Read-only acts (questions, previews) MAY resolve over multiple candidates and answer for all of
   them or ask in prose; this ADR constrains mutation targets only.

## Consequences

**Positive.** Wrong-target mutations stop being a class of incident: an ambiguous case produces a
card, not a write, and the card outlives the turn. The user's eventual choice is a stored option ID,
so the decision is auditable and replayable (§4 I20) and never re-interpreted by a model. The rest of
the turn keeps working, which preserves the conversational feel the framework is built around.
Because the candidates are computed by the server, the assistant can show real record identity
(number, counterparty, date) rather than a paraphrase.

**Negative.** Some turns that a recency heuristic would have handled "correctly" now cost the user
one extra tap. Domains that frequently have several same-kind open cases will see more `SelectTarget`
interactions and must invest in good candidate labels, otherwise the card is unhelpful. The
one-blocking-interaction rule means a pending `SelectTarget` displaces any other blocking card on the
case, so the reducer must order interaction creation deliberately when a turn produces several.

**What adopters must do.** A `WorkflowDefinition` must supply a candidate description mapping so the
`TargetCandidate` list renders as distinguishable options. UI surfaces that already know the exact
record must pass an origin reference (§12.4) rather than describing the row in prose; that removes
the ambiguity at the source instead of asking the user to resolve it. Domain reducers refining
same-turn precedence must not add a target-selection default. Teams migrating from a system that
picked the newest document must expect a behavior change and communicate it as a fix, not a
regression.

## Alternatives considered

**Pick the most recent case and let the user undo.** Rejected. It is the legacy behavior §31.2
explicitly excludes. Undo is not always possible for external effects (a submitted document cannot
be un-submitted), the wrong-target receipt reads as truthful so the user has no cue to undo, and it
violates §4 I8 directly.

**Ask the interpreter for a confidence threshold and act above it.** Rejected. Confidence is model
output and therefore a proposal (§4 I9); using it to authorize a mutation makes the model the command
authority, which ADR-001 forbids. It also makes behavior vary across providers and model versions,
so provider fallback (§20.7) would change which record gets mutated.

**Reject the entire turn on any ambiguity.** Rejected. It is safe but it contradicts §12.3 rule 3
and §13.2 rules 5–6: independent questions and unambiguous acts must survive. It would also make
multi-action turns unreliable, failing the conversation gates in §33.

**Ask "which one?" in prose and resume from the next free-text message.** Rejected. The answer would
be re-interpreted by the model, reintroducing the guess one turn later and leaving no durable record
of what was offered. A persistent `SelectTarget` interaction with stored options is the only form
that satisfies §4 I6 and I7 and remains replayable.

## Enforcement

**Invariants implemented.** Primary: §4 I8 (ambiguous target means no mutation). Supporting: I6
(the `SelectTarget` interaction is persisted before it is mentioned), I7 (options are server-owned),
I9 (model output including confidence is a proposal), I11 (the blocked act still receives a
`NeedsClarification` result), I5 (one blocking interaction per case).

**Tests that prove it.**

- §27.4 scenario 5: two same-kind open cases create a selection interaction, not a recency guess.
- §27.4 scenario 3 and §33 conversation gates: the independent question in the same turn is still
  answered, and questions cannot disappear behind actions.
- §27.4 scenario 11: if persisting the `SelectTarget` interaction fails, no text may refer to it.
- §27.2 property tests over arbitrary command sequences and random field order: no command is ever
  compiled from a `TargetResolution` other than `Exact`.
- §27.6 deterministic evaluation asserts target resolution, commands, and interaction status directly
  rather than through a judge, so a model that "sounds right" cannot pass while writing to the wrong
  case.
- §33 safety gate "No ambiguous target can execute a mutation" is the release gate that blocks
  shipping if any of the above regress.

**Responsible crates.** `turnframe-core` owns the `ProposedTarget`, `TargetResolution`,
`TargetCandidate`, `PlannedActResult`, and `InteractionKind::SelectTarget` types and the pure
reduction rule that maps `Ambiguous` to `NeedsClarification`. `turnframe-runtime` owns target
resolution against the store, the reducer's whole-turn pass, interaction persistence ordering, and
the `turnframe.target.ambiguous` metric. `turnframe-store` defines the interaction persistence
contract the runtime relies on. `turnframe-test` carries the scenario-5 fixture and the property
generators used to prove the rule against every workflow model.

Related: ADR-001 (untrusted interpreter), ADR-004 (persistent interactions own CTA semantics),
ADR-014 (whole-turn reduction precedes effects).
