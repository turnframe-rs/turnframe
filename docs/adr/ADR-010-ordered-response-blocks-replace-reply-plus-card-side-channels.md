# ADR-010: Ordered response blocks replace reply-plus-card side channels

- Status: Accepted (2026-09-05)
- Amended by ADR-019 (2026-09-26): the reply streams no text; it is published whole after the commit.

## Context

Most conversational backends return an assistant turn as two unrelated things: a free-form
reply string and a side array of "cards" (buttons, forms, attachments). The text is produced by
a model; the cards are produced by the server; the client is left to guess how the two relate.
Turnframe rejects this shape. The spec (§18.1) is explicit: "Do not return one free-form string
plus an unrelated card array. Persist and return the same ordered blocks."

The forces behind that sentence are concrete failures that show up in real conversational
applications:

- **The text describes a card that does not exist.** The model writes "I have prepared the
  change below, just confirm it", but the interaction failed to persist, or was never derived
  at all. The user reads a promise with nothing to click. With a side channel there is no
  structural link between the sentence and the card, so nothing stops this from shipping.
- **The card arrives out of context.** A reply answers a question first and then proposes an
  action, but the card array is rendered above or below the whole text. The user cannot tell
  which sentence the button belongs to. When two informational cards and one blocking
  interaction are present, the ordering ambiguity becomes an ownership ambiguity for a bare
  "yes".
- **Prose claims an effect the server never committed.** The narrator writes "Rebooking sent"
  while the command is still in the outbox, or has failed. A single string offers no place
  for a server-authored receipt to stand as the canonical status, so the model's phrasing
  silently becomes the record of what happened.
- **Reload rebuilds a different conversation.** The live response was assembled from string
  plus cards, but persistence stored only the string (or a lossy render of it). On reload the
  client tries to reconstruct cards by parsing text, or simply loses them. Spec §22.3 forbids
  exactly this: "Reload must not reconstruct cards from free text."
- **Streaming leaks unsafe claims.** Token streaming of a single reply string starts before
  commands commit. If the text mentions success and the commit then fails, the user has
  already seen the claim. Spec §18.5 requires that operational success is never streamed
  before commit and that interactions and receipts are emitted as atomic typed events, which
  a flat string cannot express.
- **Replay and audit lose the response.** Invariant I20 requires reconstructing why a turn
  produced its commands and its response plan. A reply string plus a detached card array
  cannot be diffed against events, receipts, and interaction state.

Every one of these is a failure of *shape*, not of model quality. A better prompt does not
fix them; a typed, ordered, persisted response does.

## Decision

1. An assistant turn MUST be represented as a single `AssistantTurn` whose content is an
   ordered `Vec<ResponseBlock>` (spec §18.1). There MUST NOT be a separate top-level reply
   string, card array, attachment list, or any other side channel carrying user-visible
   content.
2. `ResponseBlock` variants are the only carriers of user-visible content: `Answer`,
   `Transition`, `Receipt`, `Notice`, `Interaction`, and `Artifact`. New kinds of content
   MUST be added as new variants, never as fields smuggled into an existing block or as an
   out-of-band structure.
3. Block order is semantic and MUST be preserved end to end: composition, persistence, live
   delivery, streaming, and reload all use the same sequence. Clients MUST render blocks in
   the order given and MUST NOT reorder, merge, or split them.
4. Authorship is fixed per block kind (spec §18.2). `Answer` and `Transition` MAY be
   model-authored. `Receipt`, `Notice`, `Interaction`, and `Artifact` MUST be server-authored
   and MUST NOT be produced, edited, or replaced by a model. The narrator "may introduce or
   explain" a receipt, but the receipt block remains the canonical status (spec §17.4).
5. A `Receipt` block MUST reference the committed domain events (its `event_ids`) that
   authorize it (invariant I16). A `Receipt` MUST NOT be composed for a command that has not
   committed.
6. An `Interaction` block MUST refer to an interaction that is already persisted (invariant
   I6). Model-authored text MUST NOT refer to a card until the corresponding `Interaction`
   block exists in the same turn.
7. The runtime MUST persist the exact `AssistantTurn`, including block order and block
   payloads, as produced live (spec §22.3, pipeline steps T and U in §23). Reload MUST return
   that persisted structure. Reconstruction of any block from free text is forbidden.
8. Streaming (spec §18.5) MUST stream only model-authored `Answer` and `Transition` blocks,
   and only after the response plan is safe to publish. `Interaction`, `Receipt`, `Notice`,
   and `Artifact` blocks MUST be emitted as atomic typed events, never as token streams.
9. A button-only turn MAY produce an `AssistantTurn` containing only server-authored blocks,
   with no model call.
10. Every `AssistantTurn` MUST carry a `replay_token` so the persisted blocks can be joined
    back to the turn's plan, target resolutions, command outcomes, and events (invariant I20).

## Consequences

### Positive

- Text and cards can no longer disagree about position: a sentence that introduces a card is
  followed by the card, and the client renders exactly that.
- The canonical status of any operation is a typed `Receipt` block bound to event IDs, so a
  "success" phrase without a matching receipt is detectable by a deterministic test rather
  than by a human reader.
- Live and reload views are the same bytes. Chat history is not a lossy render of what the
  user once saw.
- Streaming becomes safe by construction: the streamable set is the model-authored set, and
  the model-authored set carries no operational claims.
- Evaluation and replay can assert on block types and block order (spec §27.6) without
  parsing prose.

### Negative

- Clients written for a "message plus cards" API need a new renderer that walks a block list.
  There is no compatibility shim; a flattening adapter would reintroduce the side channel.
- Composition is a real pipeline stage (steps S and T in §23), not a string concatenation.
  Every server-authored block kind needs a deterministic renderer with localized copy.
- The persisted representation is larger than a text column, and its schema is versioned
  content that migrations must carry forward.
- Narration is constrained: a model cannot decide to "just mention" a card or a status inline
  as prose, because the surrounding blocks own those facts.

### What adopters must do

- Render `AssistantTurn.blocks` in order and treat each variant as its own UI element.
- Store the typed blocks verbatim in chat persistence; never store a rendered string as the
  source of truth.
- Provide domain renderers for `Receipt` and `Notice` copy in the application's locales.
- Consume streaming as a mix of token streams (answers, transitions) and atomic block events
  (everything else), and never assume a turn is complete until the final `AssistantTurn`.

## Alternatives considered

1. **Reply string plus card array (the incumbent shape).** Rejected because it is the direct
   source of every failure listed in Context: it has no place for position, no place for a
   canonical receipt, and no persisted form that survives reload. The spec names this shape
   only to forbid it.
2. **Rich text with inline placeholders** (for example, markup tokens inside the reply that
   the client expands into cards). Rejected because it makes the model the author of card
   placement and, in practice, of card existence: a placeholder for a card that failed to
   persist is indistinguishable from a valid one until the client tries to resolve it. It also
   forces reload to re-parse text, violating §22.3.
3. **Server-authored everything, no model text at all.** Rejected because the spec (§18)
   requires that determinism coexist with a natural conversational surface. Answers to user
   questions, acknowledgments, and transitions are legitimately model-authored; removing them
   yields a form, not a conversation. The block model keeps that authorship while fencing it
   to the two variants that cannot carry operational claims.
4. **Two parallel ordered lists** (a text list and a card list, each ordered). Rejected
   because two lists still need a merge rule, and the merge rule is where the ordering
   ambiguity lives. One list removes the rule.

## Enforcement

### Invariants implemented (spec §4)

- **I6** (user-owned phase implies a persisted interaction): an `Interaction` block exists only
  for a persisted interaction, and no `Answer` text may reference a card before that block.
- **I16** (events authorize claims): `Receipt` blocks carry committed event IDs and are the
  only carrier of operational status.
- **I20** (replay is possible): the persisted `AssistantTurn` with its `replay_token` is the
  response half of the replay record.
- Indirectly **I10** and **I17**: the composition stage runs after reduction and commit, so
  post-commit narration regeneration produces new `Answer`/`Transition` blocks around the same
  server-authored blocks without re-executing commands.

### Tests and release gates that prove it

- Runtime integration scenarios (spec §27.4): #10 (a failed command cannot produce a
  resolved-looking receipt), #11 (a failed interaction persistence cannot produce text
  referring to a visible card), #18 (reload returns the same ordered blocks and interaction
  state), #20 (no critical success phrase without a matching receipt/event in structured
  output).
- Pure unit tests (§27.1) for receipt rendering; model evaluation (§27.6) asserting response
  block types deterministically, with judges confined to linguistic quality.
- Chaos tests (§27.7) at the "before response persistence" and "during provider streaming"
  boundaries, verifying that a crash yields either the full persisted turn or none of it, and
  that no success claim was streamed pre-commit.
- Release gates (§33): Safety: "No critical success receipt lacks committed event IDs" and
  "No required interaction can be referenced before persistence"; Operational: "Live response
  and reload use the same persisted blocks"; Conversation: "Questions cannot disappear behind
  actions" and "The response remains natural and localized".

### Responsible crates

- `turnframe-core` owns the `AssistantTurn` and `ResponseBlock` types and the authorship rule
  they encode; it has no I/O and no provider dependency.
- `turnframe-runtime` owns event-to-response composition (pipeline steps S, T, U, V) and the
  streaming split between model-authored and server-authored blocks.
- `turnframe-store` defines the chat persistence interface that stores and reloads the exact
  typed blocks and ships the deterministic in-memory implementation; `turnframe-store-postgres`
  implements it over the `tf_*` tables.
- `turnframe-test` hosts the integration scenarios and chaos harness listed above.
