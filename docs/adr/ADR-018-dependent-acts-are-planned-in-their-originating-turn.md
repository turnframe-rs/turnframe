# ADR-018: Dependent acts are planned in their originating turn

- Status: Accepted (2026-09-26)
- Replaces: `continues_turn`

## Context

«Register Marta Bianchi and open a trip for her» asks for two acts, the second depending on a record the
first creates. When creating the traveler needed a confirmation click, the runtime handled the rest
with `continues_turn`: the click committed, the cases were read again, a model was asked to plan what
was left, and the turn committed a second time. That path interpreted after a commit, needed a hint
listing what was already done so the model would not plan it twice, and let the click's authority
reach the acts of the second plan (ADR-017).

## Decision

1. Every act of a message is planned in the turn that carries it, dependent acts included.
2. An argument that refers to a record another act of the same turn creates takes that act as its
   value; the plan records the dependency, and the prerequisite's case id is minted at planning.
3. The reducer orders the turn:
   - a ready prerequisite commits before its dependents;
   - a prerequisite waiting on a card carries its dependents on that card as deferred acts, which
     run after the confirmed commands, under their own origin and policy;
   - a refused or declined prerequisite refuses its dependents with a notice.
4. No model runs after a commit except the narration tasks.

## Consequences

- `continues_turn`, the early settlement of a card, the second commit and the catalog's
  `already_done` are removed.
- A click turn that releases deferred acts makes no model call.
- A deferred act is validated again when it runs; a stale one is refused with a notice.

## Alternatives considered

1. **Keep re-planning after the click.** Interpretation after a commit is what ADR-014 forbids, and
   it needed patches to avoid repeating work.
2. **Ask the user to repeat the second request.** Loses what the user already said, which ADR-014
   exists to avoid.

## Enforcement

- Reducer scenarios for each prerequisite outcome; a runtime scenario for a click that releases a
  deferred act with no model call.
