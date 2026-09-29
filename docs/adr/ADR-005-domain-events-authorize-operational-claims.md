# ADR-005: Domain events authorize operational claims

- Status: Accepted (2026-09-05)
- Amended by ADR-019 (2026-09-26): the reply is written from the turn's outcome and reviewed; the claim guard reads the record, never the words.

## Context

Turnframe's technical promise is "Models propose meaning. Deterministic reducers decide effects. Committed events decide claims." The first two clauses are covered by earlier decisions about interpretation and reduction. This ADR fixes the third clause: what an assistant is allowed to *tell the user happened*, and where that permission comes from.

In a conversational application the most damaging failures are not clumsy sentences but confident sentences that are false about the world. The master spec (§2.2, "Operational claim integrity") lists the concrete cases, and each one corresponds to something that goes wrong in production if the reply text is trusted as a source of truth:

- A user asks to create a record. The command fails on a revision conflict, but the model, having already been told "the user wants a record created", writes "Done, I created it." The user moves on and the record does not exist.
- A user asks to change three fields. Two are applied, one is rejected by validation. The narration says "I updated everything as requested." The user later finds one field unchanged and has no idea which one or why.
- A user asks to delete something. The command was queued but never committed because the transaction rolled back. The chat says "Deleted." The item is still there.
- A rebooking is sent to the airline. The HTTP call times out. The model, prompted with "the user asked to rebook", says "Rebooked successfully." The real state is `OutcomeUnknown` and needs reconciliation, not celebration.
- The runtime meant to show a confirmation card, but persisting the interaction failed. The text still says "Please confirm using the card below." There is no card, and on reload there never will be.
- A regulated status has a fine-grained lifecycle (prepared, validated, submitted, received by intermediary, accepted, rejected, delivered, and so on). Free prose collapses all of them into "done", and the user cannot distinguish "the airline accepted it" from "we handed it to an intermediary".

Every one of these failures has the same shape: the sentence describing the effect was generated from *intent* or from *an in-progress plan*, not from *evidence that the effect committed*. A language model is not able to know whether a database transaction succeeded; it can only be told. If we allow it to phrase operational outcomes freely, the correctness of the reply depends on the model faithfully repeating a fact and never inferring a more satisfying one. That is not a controllable invariant, and §2 of the spec is explicit that reliability must be defined only in terms of controllable invariants.

The spec already gives us the raw material for a stronger design. Every committed mutation produces a `Commit` carrying `CommittedEvent` values with stable `event_id`s (§17.1). The response is not one free-form string but an ordered `Vec<ResponseBlock>` in which `Receipt(OperationalReceipt)` is a distinct block type (§18.1). Authorship is divided (§18.2): the model writes answers, acknowledgments, transitions and explanations; the server writes whether an action occurred, which fields changed, command and submission status, card availability, revisions and protocol identifiers. Invariant I16 (§4) names the rule directly: "Visible operational receipts are generated from committed events or authoritative external receipts."

This ADR turns that material into a single, enforced boundary.

## Decision

1. An **operational claim** is any statement, visible to the user, that an effect happened or has a status: creation, update (including which fields changed), deletion, submission, acceptance, rejection, delivery, completion, command failure, external status, case revision, and the availability of an interaction or artifact. The list in spec §18.2 under "Server-authored" is the authoritative enumeration.

2. Every operational claim MUST be carried by a server-authored `ResponseBlock` (a `Receipt(OperationalReceipt)`, a `Notice(ServerNotice)`, an `Interaction(InteractionView)` or an `Artifact(ArtifactView)`) and MUST NOT exist only inside model-authored text (`Answer`, `Transition`).

3. An `OperationalReceipt` that asserts an effect MUST reference the committed events that authorize it through its `event_ids` field, or the stored external receipt or protocol identifier for effects performed by an external system. A receipt with an empty authorization set MUST NOT be rendered as success.

4. Receipts MUST be rendered by deterministic, application-owned code (domain renderers and localized copy), never by the model. The renderer's input is the set of `CommittedEvent` values and authoritative external receipts produced in the current turn plus the re-projected `WorkflowView`; its output is a function of those inputs only.

5. Receipts MUST be derived only after the atomic persistence step (spec §23, step N) has completed and the changed cases have been re-loaded (step O). The runtime MUST NOT build a receipt from the plan, from the command envelope, or from the intent to execute.

6. Each claimable outcome kind is assigned a `ClaimMode`. High-risk outcomes (irreversible mutations, regulated submissions, anything with legal or financial consequence) MUST be `ServerReceiptOnly`: the model may not paraphrase them at all. Other outcomes may be `EventReferencedParaphrase`, in which model text may restate a receipt but each statement must trace to an event referenced by an emitted receipt. `FreeExplanation` is reserved for content with no operational meaning.

7. The narrator MAY introduce, explain or soften a receipt; it MUST NOT replace one, contradict one, or add an operational claim that no receipt makes. The canonical status of any effect is always the receipt block, and the model is instructed with the emitted receipts as its allowed facts (spec §17.4).

8. Failed, rejected, superseded and not-yet-executed acts MUST produce a truthful server-authored block (a `Notice` or a failure-severity `Receipt`) rather than silence or success-looking text. This is the claim-side counterpart of invariant I11 (every act receives a result).

9. External effects with an unknown outcome MUST be claimed as `Unknown` or `ReconciliationRequired` using the explicit regulated status vocabulary of spec §17.4. The runtime MUST NOT collapse distinct external statuses into a generic "done" and MUST NOT report a definite failure when the remote effect may have happened (invariant I15).

10. A block that refers to a card MUST NOT be emitted unless the corresponding interaction has already been persisted in the same turn (invariant I6). Text that says "use the card below" without a persisted `Interaction` block is a claim without authorization and is a defect.

11. Live responses and reloaded transcripts MUST be served from the same persisted `AssistantTurn` blocks, so that a claim cannot appear in the live stream and vanish from history, or vice versa.

## Consequences

### Positive

- The truthfulness of "what happened" no longer depends on model behaviour, prompt quality, provider or temperature. It is guaranteed by construction: the only path from an effect to a sentence about that effect runs through a committed event and a deterministic renderer.
- Replay and audit become mechanical. Because every receipt carries `event_ids`, the audit record of spec §26.4 can answer "which event authorized each receipt?" by a join, not by reading prose.
- Provider fallback after commit is safe (invariant I17). Regenerating narration cannot change what is claimed, because claims live in server-authored blocks that are computed once from the committed events.
- Regulated domains get a fine-grained, localized status vocabulary for free. The distinction between "received by intermediary" and "accepted by authority" is a receipt `status_code`, not a phrasing choice.
- Streaming stays possible: model-authored blocks can be streamed early because they carry no claims; receipts are appended once persistence completes.

### Negative

- Adopters must write and maintain receipt copy per domain and per locale. The framework provides the types and the pipeline position, not the words.
- Replies are less fluid than an unconstrained assistant. A receipt block reads as a receipt. The model can wrap it with a transition, but it cannot merge the fact into a single flowing sentence when the outcome is `ServerReceiptOnly`.
- Ordering and composition of blocks becomes a design task. A turn with two applied acts, one rejected act and one answer produces several blocks; the response planner must arrange them so the conversation still reads naturally (spec §18.3, §18.4).
- The rule pushes a hard boundary into the prompt layer as well: the narrator must be given the emitted receipts as its allowed facts and nothing more about effects. Prompt authors cannot "just tell the model what happened".

### What adopters must do

- Implement a domain receipt renderer that maps `CommittedEvent` payloads and external receipts to `OperationalReceipt` values with localized `title` and `body`, a `status_code`, and populated `event_ids`.
- Assign a `ClaimMode` to every claimable outcome kind, defaulting to `ServerReceiptOnly` for anything irreversible or regulated.
- Model external submissions with the explicit status vocabulary of §17.4 and store the external reference (a ticket number, a booking reference) on the receipt's `artifact_refs` or equivalent domain field.
- Keep operational facts out of the narrator prompt except as emitted receipts, and never ask the model to summarise "what we did" from the plan.
- Write the "forbidden phrase" checks for their domain so that structured-output evaluation can detect a success phrase without a matching receipt (spec §27.4 scenario 20).

## Alternatives considered

### A. Let the model narrate outcomes, feeding it the true results

The runtime executes commands, then hands the model a structured summary of results and asks it to explain them. This is the most natural-sounding option and the one most agent frameworks adopt. It was rejected because it makes correctness contingent on faithful repetition: the model is given "update rejected: revision conflict" and may still write "I updated it" under pressure from a user who clearly wanted the update. Any post-hoc verifier that checks the text against the results is itself a heuristic and reintroduces a probabilistic failure mode into the one place where the spec demands "effectively 100% by construction". It also fails the streaming and fallback cases: regenerating the text after a provider failure could change the claims.

### B. Post-generation validation of free text against the event log

Generate free text, then run a deterministic or model-based checker that scans the reply for operational claims and blocks or rewrites the ones without matching events. Rejected because claim detection in natural language is not decidable by regex and not reliable by model, especially across locales and paraphrases; because a rejected reply forces a regeneration loop that costs latency and can still fail; and because the output would still be one opaque string, defeating the ordered-block contract of §18.1 that reload and audit depend on. The forbidden-phrase check survives only as a *test* (scenario 20) and a guardrail metric, not as the mechanism.

### C. Claim from the command journal instead of from committed events

Render receipts as soon as a command is accepted into the command journal, before the domain commit. Rejected because the journal records intent to execute, not the effect: a command can be journaled and then fail on revision conflict, policy, validation or a database timeout during an atomic batch (spec §27.4 scenario 14). Anchoring on `CommittedEvent` values produced by the commit, read back after step N, is the only point at which the effect is known to exist.

### D. Claim only from re-projected state, without events

Skip events and derive receipts by diffing the `WorkflowView` before and after the turn. Rejected as insufficient on its own: a state diff cannot tell an update from a delete-and-recreate, cannot carry external protocol identifiers, and cannot express "this command failed" or "outcome unknown", which are claims about what did *not* happen to the state. Events remain the authorization; the re-projected view is used in addition, to render the current phase and any new interaction requirement.

## Enforcement

### Invariants implemented

- **I16: Events authorize claims** is the invariant this ADR implements directly.
- **I6: User-owned phase implies a real interaction** and **I11: Every act receives a result** are enforced on the claim side by decisions 8 and 10.
- **I15: External uncertainty is represented explicitly** and **I17: Provider failure cannot repeat effects** are relied upon by decisions 9 and by the "positive consequences" on fallback; this ADR does not redefine them.
- **I20: Replay is possible** is served by the `event_ids` link on every receipt.

### Tests and release gates that prove it

- Spec §27.1 pure unit tests: *receipt rendering* is deterministic for a fixed set of events and locale.
- Spec §27.4 runtime integration scenarios: 10 (a failed command cannot produce a resolved-looking receipt), 11 (a failed interaction persistence cannot produce text referring to a visible card), 13 (provider failure after commit regenerates narration without repeating commands or changing receipts), 15 (an external timeout becomes `OutcomeUnknown`), 18 (reload returns the same ordered blocks) and 20 (no critical success phrase exists without a matching receipt or event in structured output).
- Spec §27.6 model evaluation: deterministic assertions on *events*, *response block types* and *forbidden effects* are asserted without an LLM judge; only tone and completeness go to judges.
- Spec §27.7 chaos tests at the boundaries "after domain commit but before event readback" and "before response persistence" must show truthful status, never a success receipt for an effect that did not commit.
- Spec §33 release gates: "No critical success receipt lacks committed event IDs", "No required interaction can be referenced before persistence", "No external timeout is represented as a definite failure when the outcome may be unknown", "Live response and reload use the same persisted blocks", "Audit records reconstruct command authorization and claims", and "Fallback never repeats a possibly committed command".

### Responsible crates

- `turnframe-core` owns the types: `CommittedEvent`, `ClaimMode`, `OperationalReceipt`, `ReceiptSeverity`, `ResponseBlock` and `AssistantTurn`. It is pure and contains no rendering logic beyond the type contracts.
- `turnframe-runtime` owns the pipeline position: it performs the atomic persistence and event read-back, invokes the application's receipt renderer with committed events and external receipts only, composes the ordered blocks, and persists the exact `AssistantTurn` before returning or streaming it. It is also responsible for passing the narrator nothing about effects beyond the emitted receipts.
- `turnframe-test` owns the replay assertions, the fake stores and command doubles that make scenarios 10, 11, 13, 14, 15 and 18 reproducible, and the helpers for the forbidden-phrase check of scenario 20.
- Domain applications own the receipt renderer, the localized copy and the `ClaimMode` assignment per outcome kind.
