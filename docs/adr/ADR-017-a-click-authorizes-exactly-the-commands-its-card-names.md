# ADR-017: A click authorizes exactly the commands its card names

- Status: Accepted (2026-09-26)
- Tightens: I12, ADR-004

## Context

A card answer mints a `ConfirmedInteraction` origin. The reducer used to give that origin to any act
whose evidence cited the click, and a confirmation policy was satisfied by the kind of card alone.
On a turn carrying a click, a model-proposed act could therefore cite the click and pass an
`ExplicitClick` or `ReviewCard` gate it had no click for: confirming the cancellation of one trip
also cancelled another one the model proposed beside it. A reducer scenario asserted the same shape
as intended behaviour.

## Decision

1. A card's journaled commands run on its confirmation with the click's origin, as before.
2. In reduction, the click's origin goes to the one act the card itself put into the plan: its own
   operation, or the act it resumes. No other act receives it, whatever its evidence cites.
3. Every other act compiles with its own origin and raises its own card when its policy asks.

## Consequences

- A click is never blanket consent; a second consequential command in the same turn needs its own
  card.
- `DefaultTurnReducer::with_confirmed_origin` takes the index of the card's act.

## Alternatives considered

1. **Match evidence more strictly.** The model writes the evidence, so any evidence rule can be
   satisfied by the model; authority has to come from what the server put in the plan.
2. **Check the command against the card's `command_refs` only.** Covers confirmation cards and not
   cards whose option applies an operation, which have no command list before compilation.

## Enforcement

- `tests/a_click_authorizes_only_its_own_card.rs` and the reducer scenarios
  `a_card_click_binds_harder_than_the_text_next_to_it` and `the_cards_own_act_carries_the_click`.
