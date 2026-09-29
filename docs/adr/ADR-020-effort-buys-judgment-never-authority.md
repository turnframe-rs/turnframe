# ADR-020: Effort buys judgment, never authority

- Status: Accepted (2026-09-27), decision 3 revised 2026-09-28
- Amends: ADR-016

## Context

One pass of small verified tasks reads most messages correctly on a mini model with no reasoning.
It misreads the long ones: a message that fills in a trip, registers the traveler it is for,
renames another and asks a question on the way holds a dozen acts, and each step that runs once can
be the step that goes wrong. Replayed against gpt-5.4-mini, an extraction that failed three times
in three with no reasoning succeeded twice in three with a little. A deployment wants to spend more
calls on such a turn, on the same model, and fewer on a cheap surface.

## Decision

1. A turn runs at one of three levels: `low`, `medium` or `high`. `medium` is the default: the
   pipeline as ADR-016 describes it, with three votes on segmentation and routing, a split vote
   read once more, and a verdict finding fault voted on twice more. The level comes from
   configuration, and the application may force a level for one turn. Nothing changes the level
   on its own.
2. A level is a preset over what the task engine already has: each task's profile (votes, reasoning,
   review, model), the budgets, and the pipeline's settings. A deployment changes any of it per
   level, field by field; a level may name a stronger model, and none does unless configured.
3. `high` votes on segmentation, routing and verification, reads a split vote once more shown the
   answers that disagreed, gives the reading tasks but extraction some reasoning, and checks the whole
   understanding against the message in up to two rounds.
   Each finding of that check sends one step of one act back, once, and the verifier decides: the
   check alone never holds an act. A check that held what its last round still doubted held right
   acts on its false alarms.
4. `low` drops the reply review, the step prose and part of the transcript. It keeps every
   understanding task, coverage and verification included.
5. No level changes what code enforces: cards, command policy, expected revisions and the claim
   guard read no level.

## Consequences

- A `low` turn is as safe as a `high` one. It is more often wrong about what the user meant, and
  then asks.
- `high` costs about three times the calls of `medium`; its budgets scale with it, and a check the
  budget cannot pay for is skipped and recorded.
- The level is on the replay record and labels the turn and task metrics, so its cost is measured
  per level.
- `TurnInput` gains a field, and `ModelPurpose` gains `cross_check`.

## Alternatives considered

1. **Effort as a bigger model.** Simpler, and it adds no second reading: a larger model misreads
   less often and nothing notices when it does.
2. **An agent loop at `high`.** It reads until satisfied, with no bound a reviewer can state, and
   gives up the small checked tasks ADR-016 rests on.
3. **Raising the level from how complex a message looks.** Deferred: the application knows its
   surface, and a level that moves on its own is a cost nobody chose.

## Enforcement

- `a_turn_forced_to_high_runs_at_high` and `a_turn_left_alone_runs_at_the_configured_level` in
  `crates/turnframe-runtime/tests/`.
- `at_low_the_reply_is_not_reviewed_and_acts_are_still_verified` and
  `a_click_costs_no_call_at_any_effort` in `crates/turnframe-runtime/tests/`.
- `a_doubt_the_verifier_answered_again_holds_nothing` and `a_whole_turn_check_the_budget_cannot_pay_for_is_skipped`
  in `crates/turnframe-understand/tests/`.
- `a_split_vote_is_read_once_more` in `crates/turnframe-tasks/tests/`.
