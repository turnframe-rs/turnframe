# Working on Turnframe

Read `CONTRIBUTING.md` first: it holds the development setup, the local check
sequence CI mirrors, and what a pull request is judged against. This file holds
the rules that are easy to break without noticing.

## Comments are not a book: hard rule

Twenty-two per cent of this crate's Rust is comment, and four hundred blocks run
past fifteen lines. That is not documentation, it is a book nobody reads, and a
comment nobody reads is worse than none: it is trusted, it goes stale, and it
hides the two lines that mattered.

The limits are hard. A change that exceeds one is sent back.

| Kind | Limit | Where the rest goes |
|---|---|---|
| `//` inline | **4 lines** | the commit message |
| `///` item doc | **10 lines**, examples excluded | `docs/` |
| `//!` module header | **15 lines** | `docs/`, linked by path |

Examples do not count against the limit: a runnable example is tested and cannot
go stale. Prose can. Neither does a module header's table of modules: it is
navigation, not prose, and a reader arriving at a crate needs it.

**State the rule, not the incident.** The story of the turn that went wrong
belongs in the commit that fixed it, where `git log -S` finds it and where it
cannot rot. The comment says what is true now and why the code is shaped that
way, in the fewest lines that survive a reader who has never seen the incident.

Wrong, and this is real: it is what this rule exists to remove.

```rust
/// # What it cost
///
/// Asked for a travel date and told «domani», one plan wrote
/// `2026-08-15`, a date a month in the PAST, on a turn that had run no read
/// at all and carried no date anywhere. The read that answers «what day is
/// it» existed and was never called; nothing obliged it, and nothing noticed
/// afterwards. […twenty more lines…]
```

Right:

```rust
/// An argument that is only honest when a read stands behind it: «tomorrow» is
/// a date only to somebody who knows today's. Absent argument, no requirement.
```

**Before adding a long block, ask where it belongs.** A design decision goes in
`docs/adr/`. An invariant goes in `docs/architecture.md`, beside the twenty it
joins. A war story goes in the commit. What is left is usually two lines.

**Deleting prose is not losing it.** The history holds every comment ever
written; `git log -p` on the file is the archive. Do not keep a paragraph
because removing it feels lossy.

**Some prose is pinned by a test, and that wins.**
`turnframe-eval/tests/documentation.rs` asserts that one sentence is a heading in
the crate documentation, before the feature tour, with the mechanism beside it. Prose has no compiler, so a test is the compiler. Run
the crate's tests after trimming a header, not only its doctests.

## The core knows no domain: hard rule

The library's prompts and code state rules for every domain. None of them names a trip, a
traveler, a payer or any other thing a domain holds, and none quotes a user's message from
a transcript. When a live run shows a misreading, fix it with a rule stated in general terms, with
placeholders (`«set A to X»`) where an example is needed, or with the sample domain's own
configuration: its operations' examples and descriptions, glossary and briefings.
`no_core_prompt_names_a_sample_domain` checks the prompts; a review checks the rest.

## Tests

One behaviour per file under `crates/<crate>/tests/`, named as a sentence:
`a_record_named_around_the_created_name_is_the_one_created.rs`, `card_only_operation.rs`.
Unit tests go in a `mod tests` at the foot of the module they test.

A few files predate this and hold a suite each (`reduce_scenarios.rs` is the
largest). They are not a licence to add to them: a new behaviour gets its own
file, and a variant of one already there joins its file.

Run the crates you touched, not the workspace: `cargo test -p turnframe-core -p
turnframe-runtime`. The full sequence in `CONTRIBUTING.md` is the pre-push gate,
run once, not the inner loop.

## Public API

`OperationSpec`, `NarrationConfig` and their neighbours are plain structs with
public fields, so a new field breaks every literal that builds one, in this workspace and in every
adopter. That is deliberate and it is a breaking change: call it out in the pull
request and in `CHANGELOG.md`.
