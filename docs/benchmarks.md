# Benchmarks

Turnframe makes **no public performance claim**. The specification is explicit about the order of
events: measure the pieces separately first, and only then decide whether there is anything worth
saying. This page describes what is measured, what the numbers deliberately do not cover, and how to
run them yourself.

## Why these three things, separately

A turn spends its time in four places: projecting a workflow view, reducing a proposal into
commands, persisting a commit, and waiting for a model. Only the last of those is large, and that is
exactly why the first three have to be measured on their own. A single end-to-end number would be
dominated by the network and would hide a projector that grew quadratic in obligations, or a commit
path that walked its journal twice.

So the benchmarks are split by cost centre, and within a cost centre by the thing that makes the
cost grow:

| Crate | Benchmark | What it measures |
|-------|-----------|------------------|
| `turnframe-core` | `projection` | `WorkflowDefinition::project` on a trip-shaped domain, at four view sizes, and `check_view` over the projected views. |
| `turnframe-core` | `hashing` | `canonical_json`, `digest_hex` and `Digest::of_canonical` on a payload shaped like the things the workspace actually hashes. |
| `turnframe-store` | `persistence` | `CommitStore::commit` on the in-memory store at three batch sizes, and one page of the event stream at three page sizes. |
| `turnframe-provider` | `structured_parsing` | `parse_structured` end to end, then schema validation and `serde` deserialization separately, then schema compilation cold against a cache hit. |
| `turnframe-provider` | `stream_reconstruction` | `StreamAccumulator` and `reconstruct` over a realistically chunked answer: three-character text deltas and tool-call arguments arriving in eight-byte fragments. |

Projection and invariant checking are separate rows on purpose. Projection is a pure function whose
cost tracks the number of obligations it lists; `check_view` erases the view first, which
canonicalizes and hashes *every* obligation, so it grows far faster. Measured together, the second
would hide inside the first.

The same reasoning splits parsing into three rows. When a structured turn gets slower, the useful
question is whether it was the `jsonschema` pass or `serde`, and a single number cannot answer it.

## What is deliberately not claimed

**Nothing here is a latency target, an SLO, or a comparison with any other library.** These are
microbenchmarks of deterministic code paths, run on one developer machine.

**The store numbers are a floor, not a database.** They measure the in-memory implementation: one
mutex, no serialization to a wire, no network, no fsync. Their value is that when the PostgreSQL
store is measured, the difference between the two is attributable to the database rather than to the
bookkeeping the store contract imposes. Do not read the in-memory commit number as "a commit costs
this much".

**Provider latency is not here.** A model call is four to six orders of magnitude larger than
anything on this page, and it belongs to the provider, not to Turnframe. What is measured is the
deterministic work the library does around the call: parsing, validating and reassembling.

**Reduction is not yet measured.** The reducer in `turnframe-runtime` runs once per turn and does no
I/O; its benchmark has not been written, and this page will grow a row when it is.

**These benchmarks do not run in continuous integration.** Continuous integration compiles them
(`cargo bench --workspace --no-run`), so that they cannot rot, but it does not time them: a shared
runner produces numbers too noisy to gate on, and a gate nobody trusts is worse than no gate.

## Running them

```sh
# Everything, properly (minutes).
cargo bench -p turnframe-core -p turnframe-store -p turnframe-provider

# One file.
cargo bench -p turnframe-core --bench projection

# One row.
cargo bench -p turnframe-provider --bench structured_parsing -- 'parse_structured/4'

# A quick smoke pass with short sampling, for a sanity check rather than a number.
cargo bench -p turnframe-core --bench hashing -- --warm-up-time 0.5 --measurement-time 1.5 --sample-size 20

# What continuous integration does: compile only.
cargo bench --workspace --no-run
```

Criterion writes its reports to `target/criterion/`. Running a benchmark twice compares the second
run against the first, which is the intended way to use this: **a change measured against your own
baseline on your own machine is meaningful, and an absolute number quoted from someone else's
machine is not.**

Each benchmark builds its inputs outside the measured closure. Where an operation consumes its input
(committing a bundle, pushing a stream event), the input is produced with criterion's
`iter_batched`, so the construction is setup and only the operation is timed.

The core and store benchmarks define their own small workflow rather than importing the sample
domains from `turnframe-test`. `turnframe-test` depends on `turnframe-core`, `turnframe-store` and
`turnframe-provider`, so a dev-dependency pointing back at it is a cycle Cargo tolerates but the
publication order in [the release checklist](release-checklist.md) does not, and it would drag
three crates into every `cargo bench -p turnframe-core`. The benchmark domain is modelled on the
sample trip all the same: several phases, simultaneous obligations, a parameterized obligation
per extra with no payer, and a blocking rebooking card.

## Indicative numbers

Measured on the machine this was developed on: a developer laptop, `x86_64` Linux, release profile,
short sampling (20 samples, 1.5 s measurement window). **These are indicative only. They are not
published claims, they were not measured on dedicated hardware, and they should not be quoted
anywhere as a property of Turnframe.** Reproduce them on your own machine before drawing any
conclusion.

| Row | Median |
|-----|--------|
| `projection/project`, empty case | ≈ 36 ns |
| `projection/project`, 3 obligations | ≈ 48 ns |
| `projection/project`, 33 obligations | ≈ 773 ns |
| `projection/project`, blocking confirmation | ≈ 147 ns |
| `projection/check_view`, 3 obligations | ≈ 341 ns |
| `projection/check_view`, 33 obligations | ≈ 13.9 µs |
| `hash/canonical_json`, small / 8 extras / 64 extras | ≈ 0.75 µs / 4.3 µs / 31.6 µs |
| `hash/digest_hex`, same payloads | ≈ 0.21 µs / 0.90 µs / 2.8 µs |
| `hash/of_canonical`, same payloads | ≈ 1.1 µs / 5.1 µs / 32.5 µs |
| `store/commit_bundle`, 1 / 8 / 64 events | ≈ 0.51 µs / 2.0 µs / 14.1 µs |
| `store/event_stream_page`, 32 / 128 / 512 events | ≈ 8.2 µs / 35.9 µs / 149.7 µs |
| `provider/parse_structured`, 1 / 4 / 16 acts | ≈ 2.0 µs / 8.8 µs / 37.9 µs |
| `provider/schema_validate`, 1 / 4 / 16 acts | ≈ 0.20 µs / 0.56 µs / 2.1 µs |
| `provider/deserialize`, 1 / 4 / 16 acts | ≈ 0.65 µs / 2.6 µs / 13.7 µs |
| `provider/schema`, cold compile / cache hit | ≈ 27.7 µs / 5.9 µs |
| `provider/stream_accumulator`, short / typical / long | ≈ 1.9 µs / 7.1 µs / 22.7 µs |
| `provider/reconstruct`, short / typical / long | ≈ 2.6 µs / 7.9 µs / 25.7 µs |

Three observations the table supports, and one it does not.

Canonicalization dominates hashing: `canonical_json` is roughly ten times `digest_hex` on the same
payload, so the cost of an identity is the cost of sorting a JSON document, not the cost of BLAKE3.

The schema cache earns its lock. A cold compile is about five times a cache hit, and a hit is not
free either: it still canonicalizes and digests the schema to build its key.

Projection is comfortably below the noise floor of a network call at every size measured here, which
is what §28 asks of it.

What the table does **not** support is any statement about a real deployment. Every row is a
microbenchmark of one function on one machine, with the database, the network and the model removed.

## The corpus against a real model

The live corpus (`crates/turnframe-eval/tests/live_corpus/`, see [evaluation](evaluation.md)) is the
one measurement here about models rather than code. It holds 76 items on the travel desk, 46 in
English and 30 in Italian: 8 showcase items, 8 complex messages, 8 conversations of several turns,
and the rest one kind of turn each. The showcase items are the cases the guarantees exist for: a
card the airline re-quoted after it was shown, an airline that has not answered, a leg the user
asked to keep while the other is rebooked, and acts in one message that depend on each other.

Measured on 28 to 30 September 2026 against OpenAI's `gpt-5.4-mini` at the default `medium`
effort, three samples per item, one turn at a time. Cost is priced at $0.75 per million input
tokens and $4.50 per million output tokens, reasoning included. Latency is a whole turn, reply
included, over every turn a run played, the earlier turns of a conversation among them.

| Run | Samples passed | Items at 3/3 | Model calls | Input tokens | Output tokens (reasoning) | Turn p50 / p95 | Cost |
| --- | --- | --- | --- | --- | --- | --- | --- |
| First | 221/228 (96.9%) | 70/76 | 3,351 | 2,577,929 | 192,289 (32,546) | 6.5 s / 11.9 s | $2.80 |
| Second | 222/228 (97.4%) | 71/76 | 3,369 | 2,606,707 | 186,979 (26,145) | 6.6 s / 12.2 s | $2.80 |
| Third | 223/228 (97.8%) | 73/76 | 3,400 | 2,665,077 | 190,725 (28,606) | 7.2 s / 12.1 s | $2.86 |
| Fourth | 217/228 (95.2%) | 68/76 | 3,439 | 2,726,282 | 195,363 (32,402) | 7.9 s / 15.2 s | $2.92 |
| Fifth | 220/228 (96.5%) | 70/76 | 3,359 | 2,644,648 | 186,465 (25,798) | 7.1 s / 11.8 s | $2.82 |
| Sixth | 224/228 (98.2%) | 72/76 | 3,411 | 2,681,584 | 190,613 (30,612) | 7.2 s / 13.0 s | $2.87 |
| Seventh | 225/228 (98.7%) | 74/76 | 3,393 | 2,663,742 | 188,795 (29,050) | 8.6 s / 16.2 s | $2.85 |
| Eighth | 225/228 (98.7%) | 73/76 | 3,369 | 2,647,418 | 185,826 (26,650) | 9.0 s / 15.4 s | $2.82 |
| Ninth | 228/228 (100.0%) | 76/76 | 3,383 | 2,669,662 | 185,202 (26,632) | 7.7 s / 13.6 s | $2.84 |
| Tenth | 227/228 (99.6%) | 75/76 | 3,434 | 2,831,091 | 205,090 (42,505) | 8.6 s / 15.9 s | $3.05 |

Each run read a later state of this release's code. Every misreading a run showed became a rule
stated in general terms, or a sentence of the sample domain's own configuration, with a scripted
test where code changed, before the next run. The fourth run measured a change to how every message
is split, made for one item, that split «Lisbon for March» in two; it was taken back, and the item
got a narrower fix.

The ninth run first passed 223 of 228. One sample of `multi.two_fields.it` ran the date before the
name, because a rule new in that run ordered the acts of one part by where their values stand and
trusted a date pointed at the whole part. The others were readings, and each became a rule: a
traveler named by a pronoun taken for a new name, a named traveler read as none, a name the
segmentation cut in two, and in `complex.discursive.it` a chatty opening read as a condition that
held every act and a date whose words a re-read dropped. The items the rules act on were measured
again on the code that has them, three samples each, whatever their result: the twelve whose parts
asked for more than one act in the seventh, eighth or ninth run, the eighteen that name a record or
ask for one operation in two parts, and the nineteen whose segmentation votes differed by one word
or read small talk again. The last measurement of each item stands, so every item reads code that
holds each rule acting on it: nineteen read the code as released, nine read it before later rules
that do not act on them, and 48 keep their ninth-run samples: in every run traced, none of the new
rules acted on them, save the sentence every extraction now reads. The row above counts the calls,
tokens and turns of exactly the samples kept, and the report is
`crates/turnframe-eval/baselines/2026-09-29-medium-x3-gpt-5.4-mini.json`.

Every item passed 3 of 3 in the ninth run as it stands. That is a merge, not one run of the released
code, and every whole run before it missed at least one sample: expect a whole run to miss a sample
or two, each a reading the reply then asks about or leaves undone.

The tenth run is one whole run of the 0.2.0 code, with three items played at once, which is why its
turns run longer, and its report is
`crates/turnframe-eval/baselines/2026-09-30-medium-x3-gpt-5.4-mini.json`. Whole runs of earlier
states of 0.2.0 passed 225 and 217 of 228, and each miss became a rule with a test: an answer read
as the previous turn's operation, a check that found «the A is X» read as X to hold too much, a
«can I O?» the check found not asked for, among others. The one sample the tenth run missed, in
`value.add_extra.en`, is a single verdict of the check that an extra's description was incomplete:
read again, the same verdict is «stated» twelve times in twelve. Three changes came after it, and
the items whose turns they act on were run again on the code as released, three samples each: the
thirteen whose check is shown the words of a part a value leaves out passed 38 of 39, and the eight
where the check found a value the user did not give passed 24 of 24. The one miss asked who pays
for an extra the user gave no payer for, which the last of the three changes stopped; no turn of the
tenth run met the change that lets a part found not asked for hold nothing.

**What does not vary.** In no sample of the ten runs did a rebooking go to the airline without a
click on a card showing its current fare, a click on a card drawn before the fare changed run, or an
outcome the airline had not given get recorded. Code enforces these, not a model: seven items forbid
a rebooking nobody clicked for, and the silent airline's forbids a recorded outcome. Every failure
above is a reading: a value in the wrong place, a part left unread, an act the user did not ask for.
The worst was a trip named from the words of a dispute («that is wrong») in the first two runs,
fixed since.

**What is not measured in this release.** Other models, the other vendors' adapters, and the `low`
and `high` levels were not run on this corpus. Each vendor's adapter has a live test of its schema
dialect against the real endpoint, which is a different claim. Seventy-six items at three samples
each can rank two versions of the code on this corpus and this model, and nothing more.

## Conversations with simulated users

The corpus above measures single messages. [Simulated users](evaluation.md) measure conversations:
a model plays a person with a goal and a manner, and code scores what the stores and the turns show.
Measured on 30 September 2026 against OpenAI's `gpt-5.4-mini`, which also played the users, at the
default `medium` effort: the five travel desk goals of `tests/simulated_users/`, ten manners, one
conversation each. The first two runs held them on the harness's fixed day in 2023, so a user who
gave no year was read in 2023 or 2024 and a goal written for 2026 could be missed; from the third
on, the conversations are held on 30 September 2026, a day of the year the goals are written for.

| Run | Reached | Turns | Dead ends | Loops | Not understood | Refused | Offers refused | Violations |
|-----|---------|-------|-----------|-------|----------------|---------|----------------|------------|
| First, before the fixes it found | 8/10 | 58 | 0 | 2 | 6 | 6 | 0 | 0 |
| Second | 9/10 | 40 | 0 | 1 | 9 | 3 | 0 | 0 |
| Third, on the goals' day | 9/10 | 37 | 0 | 0 | 6 | 0 | 0 | 0 |
| Fourth | 9/10 | 27 | 0 | 0 | 1 | 0 | 0 | 0 |
| Fifth | 10/10 | 37 | 0 | 0 | 0 | 0 | 0 | 0 |
| Sixth | 10/10 | 31 | 0 | 1 | 2 | 0 | 0 | 0 |
| Release | 10/10 | 28 | 0 | 1 | 3 | 0 | 0 | 0 |

Each run read a later state of this release's code, and what a run showed became a rule stated in
general terms, or a sentence of the sample domain's configuration, with a test, before the next:
the third run's four «confirm the rebooking» turns after the rebooking had left, read as not
understood; the fourth run's name taken with the words asking for it; the fifth run's question
about the quoted flight answered as what can be done, without the quote; the sixth run's
rebooking held by a part the check found not asked for. The release run's three parts not
understood are in one conversation: an email and a loyalty number given in the message that
registered the traveler, then the traveler's name given as the trip's. Ten conversations are one
sample each: a rate moves by a conversation at a time, and two runs of the same code differ. What
does not vary is the last column: a dead end or an offer the domain refused is a broken guarantee,
and none occurred in any run.
