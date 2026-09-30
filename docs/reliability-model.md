# Reliability model

Turnframe does not define reliability as "the model understands every user perfectly". That is not a property anyone can control, so it cannot be a design target. Instead, reliability is split into five properties that are measured separately, have different targets, and are protected by different mechanisms. Three of them are enforced by construction. Two of them are probabilistic and are allowed to degrade, but only in directions that never produce an incorrect side effect.

This document explains the five properties, what "by construction" means for the first three, how the probabilistic layer is required to fail, which `turnframe.*` metrics measure each property, why a single accuracy percentage is rejected, and the release gates a build has to pass before it can be called production-ready.

## The five properties

| # | Property | Target | Enforced by |
|---|----------|--------|-------------|
| 1 | Side-effect integrity | Effectively 100%, by construction | TurnReducer, command policy, EventLedger commit path |
| 2 | Operational claim integrity | Effectively 100%, by construction | EventLedger, receipt composition |
| 3 | Workflow-state consistency | Effectively 100% for represented states, by construction | FlowProjector / WorkflowDefinition purity, state exploration |
| 4 | Semantic turn completion | High, but probabilistic | The understanding tasks and their checks, plus mandatory safe degradation |
| 5 | Conversational quality | Model- and product-dependent, with progress guaranteed | The reply's tasks, reviewed before they are shown, judged offline; the progress guarantees of ADR-021 in code; simulated users |

### 1. Side-effect integrity

Nothing consequential happens for the wrong reason. Concretely:

- no mutation on the wrong case;
- no stale write overwriting a newer revision;
- no duplicate execution for the same idempotency key;
- no consequential action without the confirmation policy it requires;
- no client-supplied button value changing the meaning of a server-defined call to action;
- no mutation from an ambiguous target;
- no irreversible or regulated action taken directly from model output.

### 2. Operational claim integrity

The assistant never says something happened unless the ledger proves it:

- no claim of creation without a `Created` event;
- no claim of update without matching field-change events;
- no claim of deletion without a `Deleted` event;
- no claim of submission, acceptance, delivery, or completion without the corresponding authoritative event or external receipt;
- no claim that a card is visible unless the Interaction was persisted and included in the response plan.

### 3. Workflow-state consistency

The Flow Map projection is a pure function of persisted state and workflow version:

- the same state and workflow version always produce the same phase, obligations, interaction requirement, and outcome;
- exactly one lifecycle phase applies at any time;
- completion is a predicate over persisted state or authoritative external state, never over prose;
- a user-owned step always has a real, persisted Interaction behind it.

The qualifier "for represented states" matters. The guarantee covers every state the WorkflowDefinition can express and that state exploration can reach; it says nothing about states the definition forgot to model. That gap is closed by the workflow gates below, not by the projector itself.

### 4. Semantic turn completion

The understanding tasks are asked to find every requested action, question, correction, cancellation, and constraint in a turn; bind each one to the right case and field; and tell a request apart from a hypothetical question. Small language models do this, one narrow question at a time, so they are wrong some of the time. The target is "high", and the number is only known once evaluations run against a specific model and prompt version.

A turn's effort level (ADR-020) moves this property and no other. `high` spends about twice the calls of `medium` on votes, some reasoning and a check of the whole reading, meant to read a long message with many acts right more often; its first measurement did not show that yet, and why is in [benchmarks](benchmarks.md). `low` spends fewer. The four properties above do not read the level: a `low` turn that misreads a message still cannot run a command its policy forbids, claim what no event backs, or leave a record in a state its workflow cannot express. The measured difference between the levels is in [benchmarks](benchmarks.md).

### 5. Conversational quality

Natural language, appropriate tone, clean transitions, no robotic repetition, answers integrated with action receipts without misrepresenting operational state. This is the most model- and product-dependent property, and the architecture explicitly allows it to vary without weakening any of the properties above it.

Its floor is not left to the model (ADR-021). Every reply ends on a way forward: the ask, the card on screen, the next steps, or a question to go on (I21). A reply offers only what the domain accepts now, dry-run before it is offered (I22). A question asked again says why, and one no fact answers is told where its record stands. Conversations as a whole are measured by simulated users, which score dead ends and offers the domain refused as violations: see [evaluation](evaluation.md).

## What "by construction" means

For properties 1 to 3, "by construction" means the failure is not made unlikely; it is made unreachable through the public API, and the remaining paths are guarded by release-gate tests. The reasoning is different for each property.

**Side effects.** Model output is a proposal, never a command. An understood act has to pass verification against the user's words, deterministic target resolution, whole-turn reduction, typed command construction, and the risk and confirmation policy before anything runs. Record identifiers are not accepted from the model at all; they are resolved by deterministic code into target tokens, and an ambiguous resolution creates a `SelectTarget` Interaction instead of a mutation. Revision checks and idempotency keys are mandatory on the command envelope, so a stale write or a duplicate command is rejected at the store, not by convention. Every task's answer is all-or-nothing: a malformed answer is sent back or refused whole, and no part of it runs.

**Claims.** Receipts are built from committed event IDs returned by the commit. The reply's writer may introduce or explain a receipt, but it cannot replace one, and the canonical status remains the server-authored receipt block. When an external call times out after transmission, the outcome is recorded as `OutcomeUnknown` and the user sees a deterministic "verification in progress" status, never a definite success or a definite failure.

**Workflow state.** The FlowProjector performs no I/O, holds no clock, and never reads the transcript. Given the same persisted state and workflow version, it produces the same WorkflowView. State exploration walks the reachable states of a WorkflowDefinition and asserts that every one of them has exactly one phase and no dead end.

None of this makes bugs impossible; the point is that a violation is a defect in Turnframe or in a WorkflowDefinition, detectable by deterministic tests, and never an accepted cost of model variance. The `turnframe.workflow.invariant_violation` metric exists precisely because a violation should be a paged incident, not a statistic.

## How the probabilistic layer is allowed to degrade

Property 4 will fail. The design question is what a failure turns into. Turnframe requires that an understanding error degrade into exactly one of four outcomes, and never into an incorrect side effect.

**Clarification.** An act whose value is missing, or whose value the check refused twice, asks for it; the reducer creates an Interaction when a target resolves to more than one candidate. The user is asked, the case is not touched.

**Safe abstention.** When evidence is missing or a constraint conflicts with the request, the turn produces no command for that act and says so. Reading critical state that fails closes the act rather than guessing.

**Uncommitted proposal.** For risky or regulated commands the policy requires an explicit confirmation. The act becomes a persisted Interaction (a review card, a confirmation) whose semantics are defined by the server. Nothing is committed until the user resolves it, and a stale or invalidated Interaction cannot execute.

**Explicit partial result.** When a turn carries several acts and only some can proceed, the reducer applies the ones that are safe, blocks the others, and makes the split visible as typed results. Every act receives a result; a partial success is never hidden behind a generic success, and every question in the message yields an answer, a clarification, an out-of-scope result, or a source-unavailable notice.

Each of these outcomes costs the user an extra turn or a less complete answer. That is the trade Turnframe makes deliberately: the cost of a semantic error is friction, never a wrong write or a false receipt.

## Metrics per property

Metric names follow the spec's catalogue with the `turnframe.` prefix. None of them may carry user or case text as a label.

| Property | Metrics | What a change means |
|----------|---------|---------------------|
| Side-effect integrity | `turnframe.command.rejected`, `turnframe.command.revision_conflict`, `turnframe.command.idempotency_replay`, `turnframe.interaction.stale`, `turnframe.workflow.invariant_violation` | Rejections, conflicts, replays and stale clicks are guards firing as designed. An invariant violation is a defect and should alert. |
| Operational claim integrity | `turnframe.claim.receipt_emitted`, `turnframe.external.outcome_unknown`, `turnframe.external.reconciled`, `turnframe.interaction.failed` | Receipts should track committed events one to one. Unknown outcomes should be matched by reconciliations over time. |
| Workflow-state consistency | `turnframe.workflow.invariant_violation`, `turnframe.interaction.created`, `turnframe.interaction.resolved` | Violations should be zero. The created-to-resolved ratio shows whether user-owned phases are actually being resolved. |
| Semantic turn completion | `turnframe.task.repaired`, `turnframe.task.escalated`, `turnframe.task.vote_disagreement`, `turnframe.budget.exhausted`, `turnframe.act.refused`, `turnframe.target.ambiguous`, `turnframe.target.missing`, `turnframe.target.unresolved`, `turnframe.command.confirmation_required`, `turnframe.question.unanswered` | These measure how often the model layer had to fall back to one of the four degradation paths. They are the clarification and abstention rates of the dashboard. |
| Conversational quality | Offline judge scores from the evaluation suite; `turnframe.question.answered` as a coverage signal | Not measured on the live critical path. Judges score linguistic quality, completeness and tone. |
| Provider health (cross-cutting) | `turnframe.provider.fallback`, `turnframe.provider.capability_mismatch`, `turnframe.turn.failed` | Provider failures are reported separately so they are not mistaken for understanding failures. |

Turn volume comes from `turnframe.turn.received` and `turnframe.turn.completed`; both are denominators, not properties. `turnframe.command.executed` is the denominator for the side-effect row.

### A rate is not enough for property 4

`turnframe.task.repaired` says how often, across the fleet, a check refused a task's answer whole and sent it back. That is the right shape for a dashboard and the wrong shape for the question anybody actually arrives with, which is about one conversation: *why did the assistant close that turn without doing anything or asking anything?*

It is the hardest failure to see, because there is nothing to look at. The turn produced no command and no event, and so does a turn that correctly had nothing to do. The answer the model wrote was refused before it became an act (a pointer outside the message, a value of the wrong kind, a document of the wrong shape), and the repair round either recovered, in which case nobody counted what it cost, or it did not, in which case the user got a reply that settled nothing.

So every task is written on the turn itself, in `ReplayRecord::tasks`, with its verdict, and `ReplayRecord::discarded_answers` lists the refusals: which task, which round, a stable code and the refusal in words. An evaluation reads them back as a number beside the pass rate, and an operator holding one bad conversation can answer the question from the record instead of reasoning backwards from the source.

The reliability dashboard keeps the following series apart, and a deployment should not merge them: side-effect integrity failures, operational claim integrity failures, semantic interpretation failures, clarification rate, abandonment rate, provider failures, user experience scores.

## Why a single accuracy percentage is rejected

A single "agent accuracy" figure mixes events whose consequences differ by orders of magnitude. A turn in which the assistant asked one clarifying question too many and a turn in which the assistant wrote a value to the wrong record both count as "not accurate", yet the first one is the system working as designed and the second is the failure the whole architecture exists to prevent. Averaging them hides the distinction that matters most.

There are three further reasons:

- **Different owners.** A side-effect failure is a Turnframe or WorkflowDefinition defect. A semantic failure is a prompt, model or evaluation-set problem. A conversational failure is a product decision. One number cannot tell the right team to act.
- **Different targets.** Properties 1 to 3 target zero failures. Property 4 targets a high rate that depends on the model. Property 5 has no universal target. A blended score has no meaningful threshold.
- **Different measurement.** Properties 1 to 3 are asserted deterministically from acts, targets, commands, events, revisions, interaction status and response block types. Properties 4 and 5 need evaluation samples, and property 5 needs judges. Judge votes reduce judge variance; they do not measure execution variance, so folding them into one score misstates what was measured.

The metrics table above is therefore the public shape of "how reliable is it": several answers, not one.

## Release gates

A build is not production-ready until every box is checked. Each gate maps to one or more of the properties above; the safety gates are the executable form of "by construction".

### Safety gates (properties 1 and 2)

- [ ] No consequential command can originate from raw model output.
- [ ] No ambiguous target can execute a mutation.
- [ ] No stale Interaction can execute.
- [ ] No call-to-action meaning comes from a client-supplied value.
- [ ] No critical command lacks idempotency.
- [ ] No mutable case lacks revision checking.
- [ ] No critical success receipt lacks committed event IDs.
- [ ] No external timeout is represented as a definite failure when the outcome may be unknown.
- [ ] No required Interaction can be referenced before persistence.
- [ ] No malformed multi-act response executes a subset.

### Workflow gates (property 3)

- [ ] Exactly one phase for every generated reachable state.
- [ ] Parameterized obligations cover repeated entities.
- [ ] User phases derive blocking Interactions.
- [ ] Terminal outcomes are explicit and domain-correct.
- [ ] Projection behavior is versioned.
- [ ] State exploration finds no dead end lacking an explicit user, system, or external trigger.

### Conversation gates (properties 4 and 5)

- [ ] Text and an Interaction response can coexist in one turn.
- [ ] Multi-action turns are supported.
- [ ] Action plus question works in either order.
- [ ] Questions cannot disappear behind actions.
- [ ] Out-of-order data is accepted when domain-valid.
- [ ] Corrections and negations are resolved before effects.
- [ ] The response remains natural and localized.

### Provider gates (cross-cutting)

- [ ] Capability routing is explicit.
- [ ] Critical stages reject unsupported structured-output modes.
- [ ] Fallback never repeats a possibly committed command.
- [ ] Adapter conformance suites pass.
- [ ] Provider raw data and secrets are redacted.

### Operational gates (measurement itself)

- [ ] Crash recovery is tested at every commit boundary.
- [ ] Live response and reload use the same persisted blocks.
- [ ] Audit records reconstruct command authorization and claims.
- [ ] Metrics separate safety failures from language-quality failures.
- [ ] Canary and rollback exist per workflow.

## What this document does not claim

Turnframe makes no public statement about latency, throughput, or an observed failure rate for any property. Properties 1 to 3 are designed to be unreachable failures and gated by tests; whether a given deployment reaches the target is something its own metrics and audit records show. Property 4 has no number until an evaluation suite has run against a named model and prompt version. Property 5 is judged, not measured, and depends on the product.
