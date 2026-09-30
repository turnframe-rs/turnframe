# Architecture decision records

Turnframe records every load-bearing boundary as an architecture decision record (ADR). An ADR
explains the forces behind a decision, states the decision in normative language, lists its
consequences and the alternatives that were rejected, and names the invariants, tests, release
gates and crates that enforce it. ADRs describe decisions and reasoning, never who made them.

ADR-001 to ADR-014 are the set the master specification requires (§35). ADR-015 to ADR-019 record
the redesign of turn understanding and of the reply around it, and ADR-020 how much of it a turn
buys. A
change to any of these boundaries must supersede or amend the relevant record in the same change;
see [CONTRIBUTING.md](../../CONTRIBUTING.md) for the format and the numbering rule for new records.

Every record follows the same sections: Status, Context, Decision, Consequences, Alternatives
considered, Enforcement.

## Index

| ADR | Title | Invariants it implements or protects |
| --- | --- | --- |
| [ADR-001](ADR-001-the-llm-is-an-untrusted-interpreter-not-the-command-authority.md) | The LLM is an untrusted interpreter, not the command authority | I8, I9, I12, I16, I18, I19 |
| [ADR-002](ADR-002-flow-map-is-a-pure-workflow-projector.md) | Flow Map is a pure workflow projector | I1, I2, I3, I4, I6, I20 |
| [ADR-003](ADR-003-lifecycle-phase-and-obligations-are-separate.md) | Lifecycle phase and obligations are separate | I3, I4, I5, I6 |
| [ADR-004](ADR-004-persistent-interactions-own-cta-semantics.md) | Persistent interactions own CTA semantics | I5, I6, I7 (supporting I12, I13, I14, I16, I19) |
| [ADR-005](ADR-005-domain-events-authorize-operational-claims.md) | Domain events authorize operational claims | I16 (supporting I6, I11, I15, I17, I20) |
| [ADR-006](ADR-006-optimistic-concurrency-and-idempotency-are-mandatory.md) | Optimistic concurrency and idempotency are mandatory | I13, I14 (supporting I17, I19) |
| [ADR-007](ADR-007-external-actions-use-outbox-saga-and-explicit-unknown-outcomes.md) | External actions use outbox/saga and explicit unknown outcomes | I15 (supporting I14, I16, I17, I20) |
| [ADR-008](ADR-008-provider-capability-routing-and-no-silent-downgrade.md) | Provider capability routing and no silent downgrade | I17 (supporting I9, I18, I19, I20) |
| [ADR-009](ADR-009-controlled-agentic-mode-is-read-only-before-reduction.md) | Controlled agentic mode is read-only before reduction | I8, I9, I10, I12, I16, I17, I18, I19 |
| [ADR-010](ADR-010-ordered-response-blocks-replace-reply-plus-card-side-channels.md) | Ordered response blocks replace reply-plus-card side channels | I6, I16, I20 (supporting I10, I17) |
| [ADR-011](ADR-011-typed-generic-domain-apis-with-internal-type-erasure-and-no-proc-macro-dsl-in-0-1.md) | Typed generic domain APIs with internal type erasure (and no proc-macro DSL in 0.1) | I2, I9, I16, I19, I20 |
| [ADR-012](ADR-012-snapshot-storage-is-allowed-the-event-journal-remains-mandatory-for-claims.md) | Snapshot storage is allowed; the event journal remains mandatory for claims | I1, I13, I14, I15, I16, I20 |
| [ADR-013](ADR-013-ambiguity-creates-an-interaction-and-never-a-recency-based-mutation-target.md) | Ambiguity creates an interaction and never a recency-based mutation target | I8 (supporting I5, I6, I7, I9, I11) |
| [ADR-014](ADR-014-whole-turn-reduction-precedes-effects.md) | Whole-turn reduction precedes effects | I10, I11, I17, I18 |
| [ADR-015](ADR-015-models-judge-language-code-checks-structure.md) | Models judge language; code checks structure | I9, I18 |
| [ADR-016](ADR-016-understanding-is-a-bounded-set-of-small-verified-model-tasks.md) | Understanding is a bounded set of small verified model tasks | I9, I10, I11, I18, I20 |
| [ADR-017](ADR-017-a-click-authorizes-exactly-the-commands-its-card-names.md) | A click authorizes exactly the commands its card names | I7, I12 |
| [ADR-018](ADR-018-dependent-acts-are-planned-in-their-originating-turn.md) | Dependent acts are planned in their originating turn | I10, I12, I17 |
| [ADR-019](ADR-019-the-reply-is-written-by-small-tasks-and-reviewed-before-it-is-shown.md) | The reply is written by small tasks and reviewed before it is shown | I6, I16, I17 |
| [ADR-020](ADR-020-effort-buys-judgment-never-authority.md) | Effort buys judgment, never authority | I9, I18 |
| [ADR-021](ADR-021-a-conversation-always-moves-forward.md) | A conversation always moves forward | I21, I22 |

## How the records relate

The records form one argument rather than isolated ones. ADR-001 places the model on the
untrusted side of the boundary; ADR-014 fixes the point at which its proposal becomes decidable
(the whole turn, reduced before any effect); ADR-009 confines the agentic loop to read-only
context acquisition ahead of that point (read-only reads are declared but not run in 0.1); ADR-013 says what happens when a target cannot be
decided (a persistent selection interaction, never a guess). ADR-002 and ADR-003 define the
workflow view the reducer works against, and ADR-011 keeps that surface typed for domain authors
while letting the runtime host many workflows. ADR-004 makes user decisions durable and
server-owned, ADR-006 makes writes revision-checked and idempotent, ADR-007 carries the same
guarantees across the external boundary, and ADR-012 fixes the persistence shape that makes
committed events available as evidence. ADR-005 and ADR-010 close the loop on the reply: what the
user is told happened comes from committed events, delivered as ordered, persisted blocks.
ADR-008 keeps the whole chain honest about the provider that produced the proposal.

The later records change how the model is used, not what it is trusted with. ADR-015 splits the
work between models, which judge language, and code, which checks structure. ADR-016 turns the one
interpreting call into a bounded set of small verified tasks, and ADR-019 does the same for the
reply. ADR-017 ties a click to exactly the commands its card names, and ADR-018 plans dependent
acts in the turn that asked for them, so no model runs after a commit except to write the reply.
ADR-020 lets a turn spend more of those small tasks, or fewer, without changing what any of them
may authorize.

## Reading order for newcomers

Read ADR-001, ADR-014 and ADR-005 first; together they are the technical promise ("models propose
meaning, deterministic reducers decide effects, committed events decide claims") in normative
form. Then read ADR-002 and ADR-003 for the Flow Map, ADR-004 and ADR-013 for interactions, and
the remaining records as needed.

The explanatory guides in [`docs/`](../) (the reliability model, the interactions guide, the
provider-adapter guide, the threat model and the release checklist) build on
these records; where a guide and an ADR disagree, the ADR and the master specification win.
