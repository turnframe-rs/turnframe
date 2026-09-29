# turnframe-eval

The model evaluation harness of [Turnframe](https://github.com/turnframe-rs/turnframe): a corpus of
named scenarios, run through a real orchestrator as many times as you ask, checked against
deterministic assertions that need no model, and, separately, graded for language by a judge that
is structurally incapable of ruling on whether anything happened.

## The distinction the whole crate is built around

An evaluation answers two questions, and almost every harness in the wild averages them into one
number that can no longer say which moved:

1. **Does the agent do the right thing?** That is a question about commands, events, revisions and
   cards. It is answered by reading storage: exactly, cheaply, with no model in the loop.
2. **Does it say it well?** That is a question about prose. Only another model can answer it.

A single "agent accuracy" percentage falls by the same amount when the assistant sends a
rebooking it should not have and when it phrases a receipt awkwardly. The specification's dashboard
section says so in as many words, so this crate keeps the categories apart from the corpus file all
the way to the report.

## A judge score is not a substitute for a deterministic assertion

It is the most important thing on this page, so it is before the features rather than after
them: a harness that lets one stand in for the other ships the one defect it was built to catch.

A judge is a language model asked for an opinion about prose. Ask it *"did this turn send the
rebooking?"* and it will answer from the text of the reply, and the text of the reply is precisely
the thing that can be wrong. The one failure mode Turnframe exists to prevent is a confident
sentence about an operation that did not happen; scoring that sentence with a language model is
scoring the defect with the defect. Whether an effect happened is a fact about the command journal
and the event ledger, it costs nothing to read, and it is not a matter of opinion.

The separation is not a convention here: it is the type system, which is why the substitution
cannot be made by accident:

- A judge is handed a `JudgeInput`, and a `JudgeInput` is **two strings**: the question and the
  answer. There is no constructor that takes an `Observation`, a command list, an event list or a
  case revision, so there is no expression in this crate that puts an effect in front of a judge.
- `JudgeCriterion` has exactly **four** variants (`language_quality`, `answer_completeness`,
  `tone`, `operational_claim_integrity`) and is deliberately not `#[non_exhaustive]` and has no
  free-form variant; the last is handed the committed events as settled, never asked about
  them. The moment a harness can define its own criterion, somebody defines "did it send the
  rebooking?".
- `assertions::check` reads storage and takes no provider at all.
- `ItemReport::deterministic_pass_rate` counts samples that satisfied assertions; no judge score is
  even in scope where it is computed.
- `GateThresholds` refuses a side-effect integrity failure outright whatever the judge said, and
  `min_judge_score` is a separate, optional, independently reported threshold.

So: use a judge to find out whether a reply reads well. Use an assertion to find out whether
anything happened. A green judge score over a corpus with no `forbid` lists tells you the assistant
writes nicely about the rebookings it should not have sent.

## Samples measure the model. Votes measure the judge.

```toml
[execution]
samples_per_item = 10

[judging]
votes_per_sample = 3
```

- **`samples_per_item = 10`** runs the item ten times, each against a fresh world. The spread
  between those ten runs is the *agent's* variance. It is the only way to find the scenario that
  works seven times out of ten, and it is why a flaky item is reported as a result rather than
  raised as an error.
- **`votes_per_sample = 3`** asks the judge three times about **one** of those runs, and takes the
  majority. The spread between the three votes is the *judge's* variance. No number of votes will
  ever reveal a flaky agent, because every vote is looking at the same run.

`ItemReport::deterministic_pass_rate` counts samples and only samples; a judge score never enters
it. `CriterionSummary` carries the judge's numbers with `mean_vote_spread` beside them, so a low
grade can be read as "the model wrote badly" or "the judge could not make up its mind".

## Deterministic assertions are the primary mechanism

These need no judge, and the specification forbids delegating them to one. The reason is worth
saying out loud: a judge asked "did this turn send the rebooking?" answers from the text of the
reply, and the text of the reply is precisely the thing that can be wrong. Whether an effect
happened is a fact about the command journal and the event ledger.

| Expectation | What it reads |
|---|---|
| `acts` | the understood acts in the replay record |
| `target_resolution` | how each act's target resolved (exact, ambiguous, missing, unauthorized, stale) |
| `commands` | the command types journaled this turn, in admission order |
| `events` | the event types committed this turn, in append order |
| `case_revision` | the revision each case ended at |
| `interaction_status` | the statuses of a case's cards |
| `blocks` | the kinds of response block, in order |
| `turn_phase` | the phase marker the turn finished in |
| `outcome` | whether the turn completed at all |
| `forbid` | **commands and events that must not appear** |

The last row is the one that catches the failures that matter. A turn that also sent a
rebooking satisfies every positive assertion about setting a name; only an explicit
`forbid.commands = ["trip.rebook"]` notices.

A failure names the expectation, what was expected, and what actually happened:

```text
commands: expected [trip.set_travel_date], got [trip.set_name]
```

## The judge sees two strings

`JudgeInput::new(question, answer)` takes the user's message and the model-authored text of the
reply. There is no constructor that accepts an `Observation`, a command list or an event list, and
`JudgeCriterion` has exactly four variants (`language_quality`, `answer_completeness`, `tone`,
`operational_claim_integrity`), with no free-form escape hatch. So there is no way to ask a model
whether an effect happened, which is not squeamishness: the one failure mode Turnframe exists to
prevent is a confident sentence about an operation that did not occur, and grading such a sentence
with a language model would be scoring the defect with the defect.

Judge calls use `ModelPurpose::OfflineEvaluate` and a strict JSON verdict (`score` 1–5, plus one
sentence). Votes are aggregated by majority; a tie goes to the lower score, because a judging
harness that breaks its own ties upwards is not a measurement. A vote that fails is recorded as a
vote without a verdict and the majority is taken over the rest: a judge that is down is a fact
about the judge, not a regression in the agent.

## Writing an item file

An item is a named scenario: a starting state, the turn a person takes, and what must be true
afterwards. Files are `.toml` or `.json`, and a directory of them is a suite.

```toml
id = "trip.set_name"
name = "Setting the name commits exactly one command"
tags = ["trip", "write"]
judge = ["language_quality"]

[turn]
text = "Set the name to Lisbon offsite"
locale = "en-GB"

[expect]
commands = ["trip.set_name"]
events = ["trip.name_set"]

[[expect.acts]]
kind = "apply_operation"
operation = "trip.set_name"

[[expect.target_resolution]]
act_index = 0
resolution = "exact"
case_id = "trip-1"

[[expect.case_revision]]
workflow = "trip"
case_id = "trip-1"
revision = 4

[expect.forbid]
commands = ["trip.rebook"]
events = ["trip.rebooking_sent"]

[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
revision = 3

[setup.cases.state]
status = "draft"

[setup.cases.state.traveler]
traveler_id = "5c1e0000-0000-4000-8000-000000000001"
display_name = "Marta Bianchi"
```

Notes on the format:

- **Absent means "not asserted"; present means "exactly this".** Leaving `commands` out says
  nothing about commands. Writing `commands = []` asserts the turn compiled none at all.
- **The seeded state is the workflow's own JSON.** This crate never learns the domain types; the
  harness deserializes the value into a `TripState`, a `TravelerState` or whatever the
  application uses.
- **A card answer is named by its case, not by an identifier.** An interaction id is minted while
  the corpus runs, so an item says `[turn.reply] workflow = "trip", case_id = "trip-1",
  option = "confirm"`, and the runner finds the case's blocking card and reads the revision off it.
- **The loader is strict.** Every structure denies unknown fields, and combinations that parse but
  cannot mean anything are refused: a turn with neither text nor a card answer, an `operation` on
  an act kind that has none, a command that is both required and forbidden. A loader that skipped
  `forbbiden` would report a green safety test that checks nothing.

## Running a corpus

The one thing an application writes is an `EvalHarness`: given an item and a sample index, seed the
starting state into your own domain types and hand back an orchestrator. Everything that must not
vary between applications (the sampling, the assertions, the judging and the report) belongs to
the runner.

```rust,ignore
use std::sync::Arc;
use turnframe_eval::prelude::*;

let suite = Suite::load_dir("trip", "corpus/trip")?;
let config = EvalConfig::default()
    .with_samples_per_item(10)
    .with_votes_per_sample(3);

let report = Runner::new(config)
    .with_judge(Arc::new(Judge::new(judge_provider)))
    .run(&suite, &harness)
    .await;

println!("{}", report.summary());
std::fs::write("eval.json", report.to_json()?)?;

let gate = report.gate(&GateThresholds::default());
assert!(gate.passed, "{:?}", gate.violations);
```

The `sample` index is handed to the harness on purpose: a harness driving a scripted provider can
answer differently per sample and exercise variance with no network at all, which is exactly what
this crate's own tests do.

### Running samples in parallel, and when not to

```toml
[execution]
samples_per_item = 10
sample_concurrency = 4
```

The default is `1`, and for an in-memory corpus that is the right answer: the samples run in index
order, and a scripted provider keyed on call order sees exactly the sequence it was written for.

Raise it for a corpus against a **real endpoint**, where a hundred samples in series is hours of
waiting on a network. What changes is the *execution* order: samples start and finish interleaved,
so a harness that shares anything between them (a counter, a queue of scripted answers, a rate
limit) will see a different order every run. What does not change is the result: the samples come
back in index order either way, and the same set of results comes back whatever the setting is.

### The ledger is read to the end

An observation pages the event journal by its sequence cursor until the journal says it has caught
up, so an item over a case with nine thousand prior events sees exactly what an item over a case
with nine sees: its own turn's events, all of them. A ceiling is available:

```toml
[execution]
max_observed_events = 5000
```

It is never silent. An observation that hits it is marked truncated, and every assertion that
reads the event list then fails with `truncated_ledger` rather than passing over the half of the
ledger nobody read. A forbidden event hiding past a cap is the one failure a safety corpus must
never report as green.

## The report

`EvalReport::summary()` is for a human; `EvalReport::to_json()` is what a continuous integration
job archives and what `baseline::compare` reads back. Both keep the reliability categories of the
specification's dashboard apart:

- side-effect integrity, operational claim integrity, semantic interpretation and clarification
  integrity, each with its own failing-sample count;
- clarification rate, abandonment rate and provider failure rate as rates rather than pass/fail;
- user-experience scores from the judge, never mixed into any of the above.

`GateThresholds` defaults to refusing any side-effect integrity failure outright, whatever the
overall pass rate is: that category exists precisely so it is not a percentage.

## Baselines

```rust,ignore
let comparison = compare(&baseline, &current, &ComparisonPolicy::default());
assert!(!comparison.has_signal_regression());
```

Two things look like "the evaluation got worse", and telling them apart saves an afternoon:

- a **deterministic regression**: an item that used to satisfy its assertions and no longer does.
  Behaviour changed. It names the expectations that newly fail and the reliability categories they
  belong to. It is a merge blocker.
- a **judge drift**: the same behaviour, graded differently. A judge model, a judge prompt or the
  judge's own variance moved. It is news about the measurement, and gating a merge on it is how a
  team learns to ignore its own evaluation.

They are separate `ChangeKind` variants and separate questions, and a `DriftTolerance` keeps a
judge breathing by a tenth of a point out of the report.

### When the items themselves changed

When part of an evaluation item is generated by the code under test (the state block a projector
writes is the usual case), changing that code changes the items, and a paired before-and-after
comparison silently stops being paired. Excluding the changed items is the safe default and it is
what most tools do; excluding two thirds of a corpus and printing a percentage over the remaining
third is how three of seven "regressions" end up on items the change could not have touched.

**Exclusion is as loud as the score.** `Comparison::summary()` leads with the pairing, and the JSON
carries `headline` and `excluded` before `changes`:

```text
trip → trip: 1 of 3 paired item(s) compared, 2 excluded (67% of the paired corpus)
NO HEADLINE FIGURE: 67% of the paired corpus was excluded, above the 10% ceiling; a figure over
the remaining 1 of 3 item(s) would read like a measurement of the suite and would not be one
```

Past `ComparisonPolicy::max_excluded_share` there is no figure at all. `Headline` is `Withheld`
rather than `Measured`, and nothing anywhere else on `Comparison` will produce a pass-rate delta,
so a caller cannot read a score off a comparison that dropped most of the corpus even by accident.

### Where each part of an item came from

A corpus says where its parts came from, once per item or once per directory in a `suite.toml`:

```toml
[provenance]
setup = "derived"
expect = "recorded"
```

| Value | Meaning | Tooling may regenerate it | A change to it is |
|---|---|---|---|
| `authored` (default) | a person wrote it | no | a broken pairing: the item is excluded |
| `derived` | the code under test writes it | yes | the intended effect of the change being measured |
| `recorded` | lifted from what the system actually emitted | **never** | a defect in the corpus or the tooling |

`recorded` is the value that is easy to leave out and expensive to be without. A corpus is known in
which every hand-transcribed entry had been wrong from the day it was written and every entry lifted
from a real trace was right; the difference between those two columns is exactly this declaration.
A recorded part that differs between two runs means something regenerated testimony, so
`Comparison::corpus_defects()` names it and `summary()` says, above any figure, that it is a problem
with the corpus rather than a result about the model. It is never folded into a score.

The declaration is checked as strictly as everything else: an unknown part name or an unknown
provenance is a parse error, and naming a part the item does not have is an error. A part counts as
`derived` only when **both** runs declared it (adding the declaration makes the next baseline
comparable, it does not retroactively excuse a difference), and as `recorded` when **either** did.

### The noise floor is a number, not an assumption

A difference is only news when it is bigger than the difference the harness produces with nothing
changed at all:

```rust,ignore
let control = runner.run_control(&suite, &harness).await;
let floor = control.noise_floor();
println!("{}", floor.summary());

let comparison = compare(
    &baseline,
    &current,
    &ComparisonPolicy::default().with_noise_floor(floor),
);
assert!(!comparison.has_signal_regression());
```

`Runner::run_control` runs the same corpus twice against the same code and hands back both reports.
`NoiseFloor` is the largest movement that unchanged system produced against itself. A comparison
holding one labels every change `WithinNoise` or `ExceedsNoise`, so `signal_regressions()` is the
list a merge gate should read and `deterministic_regressions()` is the list a human should skim.
Without a control run every change is `Unmeasured`, and an unmeasured movement counts as signal:
treating the unknown as harmless is how a regression ships.

A large floor is itself the finding. A corpus whose control run moves by half is not a corpus that
tolerates movement of half; it is a corpus too flaky to measure anything, and the honest next step
is more `samples_per_item` rather than a wider tolerance.

## Tests

This crate's own suite runs the harness end to end against the sample travel domain and the
scripted understandings and providers of `turnframe-test`: a correct scenario passes, a wrong command fails with a
message naming both commands, a forbidden effect that appears fails, four samples of a model that
alternates report two distinct behaviours and a pass rate of one half, three judge votes of 5/5/2
aggregate to a majority of 5 with a spread of 3, and a baseline comparison separates the regressing
item from the drifting one. One item answers a confirmation card and asserts that the turn was never
understood at all, and that the provider only wrote the reply: it fails loudly on a call nobody scripted,
so a green run is itself that assertion. No test needs a network or a real model.

The comparison's own claims are tested the same way, end to end through the runner rather than over
hand-built reports:

- two of three items are edited between the runs, and the comparison refuses to produce a headline
  figure, says the share on its first line and puts it ahead of the changes in JSON, and raising
  the ceiling on the very same data brings the figures back, which is what proves the policy did
  the withholding;
- one item declares its seeded world `derived` and another leaves it authored; the first is compared
  and named an intended projection change, the second is excluded with a broken pairing, and neither
  is a regression;
- an item declares its seeded world `recorded` and it changes anyway: the comparison reports a
  corpus defect above any figure and never folds it into one;
- a scripted provider that varies from call to call gives a control run a noise floor of a quarter,
  and a before-and-after that differ by exactly a quarter is reported as within the noise rather
  than as a regression, while the same data without a control run is not excused, because an
  unmeasured movement counts as signal.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
