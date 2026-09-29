# ADR-016: Understanding is a bounded set of small verified model tasks

- Status: Accepted (2026-09-26)
- Amends: ADR-014 decision 1, ADR-009

## Context

One model call used to interpret a whole turn: split the message into intents, classify each,
choose workflow, operation and record from the whole catalog, extract and format arguments, quote
evidence, recognise corrections, and follow per-record guidance, against one document holding every
case, every operation schema and the transcript. On the test domains that request is about 7.5k
tokens, 19k characters of it the output schema; deployments reach 15k. Small models make wrong
decisions and invent values under that load, and the surrounding runtime grew sample voting,
re-asks and repairs to compensate.

## Decision

1. Understanding runs as a fixed set of task kinds: `segment`, `coverage`, `route`, `locate`,
   `extract`, `verify`, `question_frame`, and the optional `investigate`.
2. Each task receives only the context its decision needs, and answers a strict schema built from
   closed sets that are valid for this turn. The user's message always travels verbatim.
3. Tasks run per unit of the message, concurrently where independent. Structural shortcuts skip a
   task when its answer is already determined (a click, a single admissible record). A single
   offered operation is still a question, since a request may ask for none. `route` lists every
   operation a unit asks for, and each becomes its own act.
4. Every task has its own configurable prompt, model, sampling settings, votes, repairs and
   escalation, and leaves a record on the replay record.
5. Every turn is bounded by `max_model_calls`, `max_chain_depth`, `max_parallel`, prompt tokens and
   wall clock, each with a default. Exhausting a bound never produces a partial effect: a unit not
   understood becomes a notice, every act on a record with such a unit is held, and a failed
   segmentation or constraint fails the turn closed. A constraint only `coverage` found sends the
   segmentation back once, told which words it left out, before it counts as lost.
6. Code assembles the task outputs into one plan before reduction. All of understanding completes
   before any effect (ADR-014); the plan is internal and no longer model-facing.

## Consequences

- The single interpreter, per-turn schema surgery, plan sampling and its tie-breaks, and the
  "says nothing" re-ask are removed.
- A typical turn makes several small calls instead of one large one.
- Each task can be evaluated on its own against a corpus.
- ADR-014's all-or-nothing rule applies per unit and per record, not per model answer.

## Alternatives considered

1. **A public graph engine.** More flexible; adopters would own the reliability of their graphs,
   and no adopter needs a different graph today. The engine records its graph so one can be exposed.
2. **A model planner with workers.** The planning call is again one large decision over the whole
   context, and the shape of calls varies per turn.
3. **Keep one call and shrink its context.** Retrieval narrows the catalog but leaves one call doing
   every job; the recorded baseline measures that path.

## Enforcement

- Task records, the budget report and `turnframe.budget.exhausted{bound}`.
- Per-task prompt snapshots with size budgets.
- The live corpus comparison against the recorded baseline.
