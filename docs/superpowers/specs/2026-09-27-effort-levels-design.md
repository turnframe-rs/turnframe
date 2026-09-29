# Effort levels: more judgment for harder turns

Status: accepted and implemented, 2026-09-27. Changed 2026-09-28 by the owner's decision: `medium`
casts three votes on `segment` and `route` (a split vote is read once more), and `verify` reasons at
`low` at every level; `medium` is no longer exactly the behaviour before levels. Also by the
owner's decision that day, the whole-turn check advises and the verifier decides: a finding sends
its act back once and the check alone never holds an act (its false alarms held right acts on 10
of 68 items in the first live run at `high`), and `extract` does not reason at `high`: replayed,
reasoning made it copy the words naming a field into the value (6 in 20 right against 20 in 20).
At `medium` a verdict finding fault is voted on twice more and the majority
decides (`Settings::doubt_votes`), by the owner's decision: one vote misjudged a right reading about
one time in ten on some items.
Two details differ from the text below: the span
field is `effort`, like every other span field, and a unit the whole-turn check finds is
`FoundBy::CrossCheck`.

## Goal

A turn can be told to spend more model calls to be read correctly. A message like «new trip
for Omar Haddad: three hotel nights at 80, a lounge pass at 15, flying at the end of next month;
register them first, their email is omar@haddad.example; and rename Marta Bianchi to Marta Bianchi
Ferri. How much hand luggage is included?» holds a dozen acts across two workflows, a record created
and used in the same breath, and a side question. At today's level a mini model misreads some of
these a few times in ten. A higher level must make that rare, on the same model, by checking
more.

Success: on a corpus of complex messages, `high` passes clearly more items than `medium` on
gpt-5.4-mini, and `medium` stays where it is on the existing corpus.

## Decisions already taken

- Three levels: `low`, `medium`, `high`. `medium` is the default and is exactly today's
  behaviour.
- The level comes from configuration, and the application can force a level for one turn.
  Nothing raises or lowers it on its own.
- A level does not change the model. Each level's task profiles can name a model, so a
  deployment that wants a stronger model at `high` configures it.
- Effort buys judgment, never authority: cards, command policy, expected revisions and the claim
  guard are code and do not change with the level. A `low` turn is as safe as a `high` one; it is
  more often wrong about what the user meant, and then asks.

## What each level does

| | `low` | `medium` | `high` |
|---|---|---|---|
| Reasoning on understanding tasks | minimal | minimal | low, save `extract` |
| Votes on `segment`, `route` | 1 | 1 | 3, and a split vote is read once more |
| Votes on `verify` | 1 | 1, and a verdict finding fault twice more | 3, and a split vote is read once more |
| Two readings disagree over small talk | the segmentation reads again, told what coverage saw | the segmentation reads again, told what coverage saw | the segmentation reads again, told what coverage saw |
| Whole-turn check | off | off | on, up to two rounds |
| Reply review | off | on | on, with low reasoning |
| Step prose (`NarrationConfig::steps`) | off | as configured | as configured |
| Earlier messages shown to `extract` | 2 | 4 | 6 |
| Understanding budget | as configured | as configured | calls and tokens ×3, depth +4, wall clock ×2 |

`low` keeps every understanding task, coverage and verification included: they are what stops a
wrong act, and a cheap turn must not buy its savings there. It saves the reply review, the step
prose and part of the transcript.

The reasoning row is the one measured so far: replaying a real extraction that failed three times
in three at minimal reasoning, low reasoning got it right twice in three.

## The whole-turn check

A new task, `cross_check`, runs at `high` after every act is extracted and verified. It is shown
the message with its word numbers and a plain list of what was understood: each act with its
operation, its record and each value with the words it came from; each question; each constraint;
the words read as nothing. It answers one question: does this list say what the message says?

```json
{"findings": [
  {"kind": "missing",      "words": {"from": 14, "to": 18}},
  {"kind": "wrong_value",  "act": "u2.a1", "argument": "amount", "words": {"from": 9, "to": 9}},
  {"kind": "wrong_record", "act": "u3.a1", "words": {"from": 22, "to": 23}},
  {"kind": "not_asked",    "act": "u1.a2"}
]}
```

Code checks the answer's structure: words inside the message, act ids from the list, argument
names of that act, and `missing` words that no act, question or constraint already covers. Then
each finding sends one step back with the finding as feedback, the way a verifier's doubt already
does:

- `missing`: the words become a unit found by the check, and are routed, extracted and verified
  like any other.
- `wrong_value`: that argument is extracted again, then verified again. The extraction is told
  the doubt and the words it points at as a possibility, never as the value.
- `wrong_record`: that act is located again.
- `not_asked`: that act is verified again, not told the doubt (told it, the verifier agreed
  with a false alarm half the time). The verifier decides; the check alone never removes an act.

After the repairs the check runs once more. A finding sends its act back once: raised again, the
verifier has already answered it, and it holds nothing. A finding first raised in the second round
is read again like one from the first. An empty answer changes nothing. Two rounds at most, inside the turn's budget; a check the budget cannot pay for is
skipped and recorded as skipped.

## A split vote is read once more

`Disagreement` gains `Reread`: when the votes find no majority, the task runs once more and is
shown the answers that disagreed, then its answer stands. `high` uses it on `segment`,
`route` and, by the owner's decision of 2026-09-28, `verify`. It is an engine option, so a deployment can set it on any task.

## Where the level is chosen

- `OrchestratorConfig::effort: EffortConfig`: the default level, `medium` unless configured, and
  each level's adjustments. It sits beside `understanding` and `narration` because it governs
  both.
- `TurnInput::effort: Option<Effort>`: the application forces a level for this turn. The turn's
  level is this, else the configured default.
- The level is recorded in the replay record and on the turn's span (`turnframe.effort`), and
  labels the turn's call and token metrics, so its cost is visible per level.

## Configuration

A level is the configured profiles and budgets with that level's adjustments on top, then the
deployment's own overrides for that level. A deployment that tunes `route` tunes it at every
level, and `high` still adds its votes unless told otherwise:

```toml
[effort]
default = "medium"

[effort.high.tasks.extract]
model = "large"           # extract at high runs on the pool's "large" model

[effort.high.budget]
max_model_calls = 120
```

`effort.low`, `effort.medium` and `effort.high` take the same shape: `tasks` (task profiles),
`budget` (understanding), `reply_budget`, `settings` (verify policy, transcript, whole-turn check
rounds, reread on disagreement). With nothing configured, `medium` is exactly
`understanding.tasks`, `understanding.budget`, `understanding.settings` and `narration.budget` as
they are today.

## How it runs

- The turn resolves its level before understanding starts.
- The `TaskScope` of each phase carries that level's profiles and budget. The engine uses the
  scope's profiles when present, else its own, so one engine serves every level.
- `UnderstandingInput` carries the level's pipeline settings; the understander reads them per
  turn.
- The composer's scope carries the level's reply profiles the same way.

## Public API changes

- New: `turnframe_core::effort::Effort` (`#[non_exhaustive]`), `TaskKind::CrossCheck`,
  `ModelPurpose::CrossCheck`, `Disagreement::Reread`, `EffortConfig` and `EffortProfile` in the
  runtime configuration (`OrchestratorConfig` is `#[non_exhaustive]`, so its new field breaks
  nothing), `TaskScope::with_profiles`.
- Breaking: `TurnInput` gains `effort`. Every literal that builds a `TurnInput` must add
  `effort: None`. Called out in the CHANGELOG.
- `ModelPurpose::ALL` grows to 14; an adapter's purpose routing names the new purpose.

## Evaluation

A `complex` section of the live corpus, eight items in English and Italian, each several acts
across workflows with a side question and at least one of: a record created and used in the same
message, a correction inside the message, a constraint («don't send it yet»), a value given in an
earlier message, discursive filler. Each item states the events, cards and answers it expects.

`live_corpus` gains `TURNFRAME_EVAL_EFFORT` and runs the complex section and the existing 50 at
each level on gpt-5.4-mini. `docs/benchmarks.md` reports, per level: items passed, model calls,
prompt tokens, and p50 and p95 turn latency.

## Tests

Scripted, one behaviour per file:

- a turn forced to `high` runs its tasks with `high`'s profiles, and one with no override runs at
  the configured level;
- a split vote with `Reread` runs once more, shown the answers;
- each finding kind of the whole-turn check sends back exactly its step, and an empty answer
  changes nothing;
- a doubt the verifier answered, raised again, holds nothing; one first raised in the last round
  is read again;
- a check the budget cannot pay for is skipped and recorded;
- at `low`, the reply is not reviewed and understanding still verifies every mutating act;
- the level is in the replay record.

## Out of scope

- Raising the level automatically from how complex a message looks. Decided against for now;
  the application forces a level when it wants one.
- Changing what a level may authorize. It changes judgment only.

## Documents

ADR-020 (effort buys judgment, never authority); `docs/architecture.md` (the check, levels);
`docs/reliability-model.md` (what a level changes and what it does not); `docs/benchmarks.md`;
the runtime and understanding READMEs; the CHANGELOG; the console gains `/effort low|medium|high`
and prints the level on each turn's cost line.
