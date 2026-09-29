# ADR-004: Persistent interactions own CTA semantics

- Status: Accepted (2026-09-05)
- Related: ADR-001 (the LLM is an untrusted interpreter), ADR-010 (ordered response blocks), ADR-013 (ambiguity creates an interaction)

## Context

A conversational application constantly asks its user to decide something: "Rebook this flight?", "Which of these two travelers did you mean?", "Delete the draft?". Every such question is a call to action (CTA), and in Turnframe every CTA is an `Interaction`: a durable record that a user can answer with a click, with typed text, or with both in the same turn (spec §9). The question this ADR settles is *who decides what an answer means*.

In the systems that Turnframe replaces, meaning lived in the wrong places, and each misplacement produced a concrete failure that an end user could see:

- **The client defined the meaning.** A card was rendered with buttons whose payload carried the action ("action=rebook_flight", "value=confirm"). Whoever could edit the request body could turn a "Cancel" button into a "Send" one, or replay a stale confirmation against a different draft. The server executed whatever value arrived because it had nothing else to compare it against. Spec §31.2 lists "client-supplied option values controlling CTA semantics" among the behaviors that must not be ported.
- **The card was ephemeral.** Suggestion cards were generated on the fly, shown once, and never stored. On reload the card was gone or re-rendered differently; a click after a reload referenced a card the server no longer knew, so either it was rejected as noise or, worse, re-interpreted by the model from the button label. Spec §31.2 also forbids "ephemeral suggestion cards as the interaction source of truth".
- **The model owned the confirmation.** Destructive operations were confirmed by prompt instructions ("ask before deleting"). A model that skipped the question, or that read an ambiguous "ok" as consent, executed the operation with no server-side proof that consent was ever given. Spec §31.2: "prompt-only confirmation of destructive operations" is out.
- **Two cards competed for one word.** With a send card and a discard card both open on the same case, a plain "yes" typed in chat had two candidate meanings, and the recency heuristic chose one. The user confirmed a send while believing they were confirming a discard.
- **The card was described before it existed.** The assistant text said "I have prepared the confirmation below", the interaction insert failed, and the user saw a promise pointing at nothing.
- **A double click sent twice.** Without a durable interaction with a status, a second click on the same button was indistinguishable from a first one.

The forces are therefore: the client must stay a thin renderer; the model must stay a proposer (ADR-001); a user's decision must survive reloads, crashes and time; the same word must never have two live meanings on one case; and every claim about a card must be backed by a persisted row.

## Decision

Persistent interactions own CTA semantics. Concretely:

1. Every CTA presented to a user MUST be a persisted `Interaction` (spec §15.1) with a stored `kind`, an immutable `payload`, a `payload_hash`, a `status`, and a set of `InteractionOption`s. The persistence write MUST complete before any response block refers to the card (invariant I6; spec §15.5).
2. Each `InteractionOption` MUST carry its `StoredInteractionAction` server-side. The client MUST NOT be able to supply, alter or replace the action; the client sends only an `InteractionResponse` made of `interaction_id`, `option_id`, `expected_case_revision` and an optional `freeform_input` whose admissibility is governed by Decision 4 (spec §9; invariant I7).
3. On receipt of an `InteractionResponse` the runtime MUST load the interaction, verify that it belongs to the calling account and conversation, verify that the `option_id` is one of the stored options, verify that the `expected_case_revision` matches the interaction's binding, and only then derive the command from the stored action. Any check that cannot be performed MUST fail closed (invariant I19).
4. Free-form text MUST be accepted as part of an interaction response only when the stored option's `freeform_policy` allows it, and only through an interaction whose kind is explicitly free-form (`Freeform`) or whose option declares it. Free-form text MUST NOT change which action is executed; it may only parameterize the action the stored option already names.
5. At most one `Active` blocking interaction MAY exist per case (invariant I5; spec §15.6). Creating a new blocking interaction MUST invalidate the previous one and MUST issue a new `InteractionId`; the old ID MUST NOT become resolvable again.
6. An unqualified textual answer ("yes", "no", "continue", "ok") MAY resolve an interaction only through its `TextResolutionPolicy` (spec §15.7). Interactions that confirm high-risk commands (`Destructive`, `Irreversible`, `ExternalRegulated` in the sense of spec §14.3) MUST default to `Never` and MUST require the structured click.
7. Interaction payloads MUST be immutable after first display. A change in the case revision MUST move revision-bound interactions to `Invalidated` unless the interaction explicitly declares revision independence (spec §15.5).
8. A second response to an interaction that is already `Resolved` MUST return the original resolution result and MUST NOT execute anything again (spec §15.5; invariant I14).
9. The interaction MUST reach `Resolved` only after the derived command has committed. If the command fails, the runtime MUST persist `Failed` or restore `Active` according to policy and MUST NOT render a successful receipt (spec §15.5; invariant I16).
10. The meaning of an interaction MUST be derivable by the `FlowProjector` from persisted case state (the `InteractionRequirement` on the `WorkflowView`, spec §8.1), so that the card a user sees is the card the workflow demands, not one the model invented.

## Consequences

### Positive

- The client becomes a pure renderer: it can only pick among options the server already stored. Tampering with a button changes nothing the server does.
- Consent to consequential operations has a server-issued origin (a resolved interaction), which is exactly what invariant I12 requires for critical commands. The model never confirms anything on the user's behalf.
- Reload, crash recovery and replay all see the same card, because the card is a row, not a rendering.
- A plain "yes" has at most one live meaning per case, so unqualified answers become safe to interpret.
- Double clicks, stale confirmations after an edit, and clicks from another tenant are rejected mechanically rather than by prompt discipline.
- Button-only turns need no model call at all (spec §9), which removes an entire class of interpretation failures from the confirmation path.

### Negative

- Every CTA costs a write before the response can mention it, and a read on every click. Latency figures are not claimed here; they must be measured against the goals in spec §28 once the runtime exists.
- Workflows must declare their interactions in the projection rather than letting the model improvise a card. Domains with many ad-hoc prompts must model them explicitly.
- Invalidating the blocking interaction whenever the case revision moves means users occasionally see a card disappear and be replaced; the response composer must explain this rather than silently swapping IDs.
- Text-resolution aliases are a new surface to localize and to keep in sync with option labels.

### What adopters must do

- Implement `WorkflowDefinition` so that any user-owned phase derives an `InteractionRequirement`, and never emit a card from prompt text alone.
- Store an action per option; never encode an action in a label, a URL or a client value.
- Bind revision-dependent interactions (send, delete, review) to the case revision and, where relevant, to a preview or payload hash, as the rebooking card does in spec §31.3.
- Choose `TextResolutionPolicy::Never` for anything that confirms a high-risk command; use aliases or model-interpreted resolution only for low-risk, reversible interactions.
- Render only from persisted ordered blocks (ADR-010), so live response and reload cannot diverge on which cards exist.

## Alternatives considered

1. **Signed client payloads.** Keep the action in the button payload but sign it server-side so the client cannot forge it. Rejected: a signature proves the payload was once issued, not that it is still valid. It cannot express invalidation after a case edit, single-blocking-interaction replacement, or resolution status, and it still makes the client the carrier of meaning. It also leaves ephemeral cards ephemeral, so reload and replay remain broken.
2. **Model-mediated confirmation.** Let the interpreter read "yes"/"confirm" from text and from button labels and decide whether consent was given. Rejected: this makes a proposal (ADR-001, invariant I9) the origin of a critical command, contradicting invariant I12, and it cannot disambiguate two open cards on one case. It is precisely the prompt-only confirmation that spec §31.2 refuses to port.
3. **Stateless option enumeration.** Recompute the valid options from the current case state on every click and accept any option that is valid *now*, without storing the interaction. Rejected: it accepts a click that was made against a different view of the case (the stale-confirmation failure), it cannot return the original result on a duplicate click, and it gives the response composer nothing persisted to refer to, so invariant I6 cannot hold.
4. **Allow several blocking interactions and require qualified answers.** Keep more than one active card and force the user to name which one they mean. Rejected: it pushes disambiguation onto the user for the common case and re-opens the "unqualified yes" ambiguity whenever they do not comply; a single blocking interaction is simpler to enforce and to reason about.

## Enforcement

### Invariants implemented (spec §4)

- **I5** At most one blocking interaction per case (Decision 5).
- **I6** User-owned phase implies a real, persisted interaction (Decisions 1 and 10).
- **I7** Client input cannot define CTA semantics (Decisions 2, 3 and 4).
- Supporting: **I12** (a resolved interaction is the trusted origin for critical commands), **I13** (expected case revision on every response), **I14** (duplicate click returns the original outcome), **I16** (no resolved-looking receipt without a committed command), **I19** (unverifiable ownership or revision fails closed).

### Tests that prove it (spec §27)

- Pure unit tests (§27.1): interaction requirements derived from projection; policy evaluation for `TextResolutionPolicy` defaults.
- Property tests (§27.2): duplicate interaction clicks; no high-risk command without trusted origin; revision conflicts.
- State exploration (§27.3): user phases derive interactions; stale interactions cannot execute.
- Runtime integration scenarios (§27.4): 4 (CTA response and text coexist), 7 (stale confirmation after an edit is rejected), 8 (double click executes at most once), 9 (client cannot change CTA semantics by changing a value field), 10 (failed command cannot produce a resolved-looking receipt), 11 (failed interaction persistence cannot produce text referring to a visible card), 18 (reload returns the same interaction state), 19 (cross-tenant interaction IDs rejected without leakage).
- Chaos tests (§27.7): failure injected after interaction persistence and between command journal insert and domain commit must leave a truthful interaction status.

### Release gates (spec §33)

- Safety: "No stale interaction can execute", "No CTA meaning comes from a client-supplied value", "No required interaction can be referenced before persistence", "No consequential command can originate from raw model output".
- Workflow: "User phases derive blocking interactions".
- Conversation: "Text and an interaction response can coexist".
- Operational: "Live response and reload use the same persisted blocks".

### Responsible crates

- `turnframe-core` owns the types: `Interaction`, `InteractionKind`, `InteractionOption`, `StoredInteractionAction`, `InteractionStatus`, `TextResolutionPolicy`, `InteractionResponse`, and the `InteractionRequirement` carried by `WorkflowView`. It stays free of I/O.
- `turnframe-runtime` owns the interaction lifecycle: persistence before response, ownership and revision verification on response, single-blocking-interaction replacement, resolution only after command commit, and the failure paths to `Failed`/`Active`.
- `turnframe-store` defines the interaction store trait; `turnframe-store-postgres` implements it over the `tf_*` interaction tables with the immutability and status constraints described above.
- `turnframe-test` supplies the fixtures and exploration helpers used by the tests listed here.

## Amendment (2026-09-26)

Commands journaled for a confirmation card are stored as `AwaitingConfirmation`, not `Pending`.
Crash recovery resumes unfinished commands, and a command waiting for its card is not unfinished:
it runs only when the card is confirmed. Before this, a turn that raised a confirmation card and
then failed before delivery executed the unconfirmed command on recovery.
