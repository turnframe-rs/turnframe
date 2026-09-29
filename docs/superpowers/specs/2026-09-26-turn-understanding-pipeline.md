# Turn understanding as small verified tasks

- **Status:** Implemented on 2026-09-26; the ADRs (015 to 019) are the current record. Where the
  code departs from this document: `route` lists every operation a unit asks for; a text value
  copies its words beside its pointer; a constraint only `coverage` found sends the segmentation
  back once before it counts as lost; and reads (`investigate`, `context_reads`) are declared but
  not run in 0.1.
- **Date:** 2026-09-26
- **Scope:** the model-facing half of Turnframe (interpretation and narration), the execution
  engine that runs model calls, the domain declarations those calls are built from, and the
  safety defects found while analysing them. The deterministic core (projection, reduction,
  policy, commands, events, cards) keeps its semantics.
- **Delivery:** committed directly on `main`, one commit or more per phase. The phase table in §17
  is the plan.

## 1. Summary

Turnframe asks one model call to understand a whole turn. With small models and a dense context
that call makes wrong decisions and invents values, and a long series of narrow patches around it
has not changed that. This spec replaces the single call with a bounded set of small model tasks, each
with its own slice of context, its own strict output, its own prompt, model and limits, and a
second model task that verifies what the first produced. Code assembles the results into the same
kind of plan the reducer already consumes, so every effect still goes through deterministic
reduction, policy and committed events.

Three rules carry the design:

1. **Models judge language; code checks structure.** No natural-language string is ever compared
   by code to decide meaning.
2. **Absence is an answer.** A task can say a value was not given, and that is the expected
   default.
3. **A verifier can only take away.** A model check may block, repair or ask; it never
   authorizes anything.

## 2. Problem

### 2.1 What was measured

- **Size.** On the bundled test domain with two trips and one traveler (15 operations), one
  interpretation request is about 30k characters, roughly 7.5k tokens. The output schema alone is
  19k characters with 107 `description` strings, most of them rustdoc copied into the schema as
  model-facing text. Four workflows, briefings and a full transcript take a production prompt to
  about 15k tokens on mini models.
- **Arguments are unconstrained.** `JsonArguments` is a JSON document inside a string
  (`turnframe-core/src/plan/mod.rs`), so the most domain-specific part of the answer gets no help
  from structured decoding and is checked only afterwards.
- **One call does about eight jobs:** split the message into intents, classify each as act,
  question or constraint, choose workflow and operation from the whole catalog, choose the record,
  extract and format arguments, quote evidence, recognise corrections and cancellations, and follow
  per-record guidance.

### 2.2 A failure, end to end

Take a trip with no name and the message «the name needs changing». The sample declares
`set_name(value: String)`, `value` required, guarded by `quoted_arguments`: the value must
appear in the words the act quotes. The plan has no way to say "the user wants this operation and
gave no value", so a model fills `value` with the only words available, such as "needs changing";
those words are in the message, so the guard passes and the write commits. When the user then
objects by quoting the value, an act re-proposing it passes the same guard, the runtime answers
with the sample's "nothing changed" sentence, and both writing stages repeat it. Nothing can
represent "the user disputes what the assistant just did".

### 2.3 What the code shows

- **Patches, not design.** Most `CHANGELOG.md` entries add a narrow rule, a schema pruning or an
  instruction after one real conversation failed. `planning.rs` ranks plan samples through three
  layers of tie-breaks (`SampleScore::best`, `least_when_nobody_agrees`, `best_agreed`); the
  interpreter re-asks when a plan "says nothing"; the reducer finishes values the model truncated
  (`extend_dictated_values`); narration carries an echo detector, an own-words retry, byte-span
  cutting of questions and a dozen conditions on when the acknowledgement may speak, each
  documented as replacing a prompt instruction that "did not hold".
- **Literal matching decides meaning** in many places: quote substring grounding, the quoted-value
  check, alias matching (`message_contains_declared_alias`), lexical record naming (`catalog_about`,
  `named_in`), `ClaimVocabulary`, and the echo detector's folded sentence comparison.
- **No per-stage model control.** No stage sets temperature, output limit or timeout. The router
  ignores the purpose it is given, so interpretation and narration can only be split across models
  by hand-building both stages. An invalid answer is never retried on a stronger model.
- **Chains in one turn.** Repairs, the "says nothing" re-ask, read-loop rounds, N samples each
  dry-reduced, and `continues_turn`, which commits a card, re-plans and commits again.
- **Structure.** `interpret.rs` (3.7k lines, ~800 of them JSON-schema surgery), `orchestrator.rs`
  (3.4k, a 25-field `Session`), `compose.rs` (3.3k) and `reduce.rs` (2.5k). The live path and the
  plan-only path duplicate loading, sampling, scoring and reducer setup, and have already drifted
  (notice copy, carried subjects, `continues_turn`).

### 2.4 Defects found on the way

| # | Defect | Status |
|---|---|---|
| D1 | A click's `ConfirmedInteraction` origin is given to any act citing that click (`reduce.rs` `origin_for`); `satisfies_confirmation` checks only the card kind, and the origin does not carry the card's `command_refs`. A model-proposed act on a click turn can pass an `ExplicitClick` gate it has no click for. | verified by reading; to reproduce by test |
| D2 | Recovery treats `Pending` journal rows (commands waiting on a confirmation) as resumable, and `resume_turn` executes them with no policy re-check. | reported by analysis; to verify |
| D3 | A card moves to `Resolving` before interpretation; a failed turn does not restore it. | reported by analysis; to verify |
| D4 | A model-proposed `AnswerActiveInteractionFromText` or `SelectTarget` reduces to `ReadyToExecute` with no commands, and nothing settles the card or resumes its deferred act. | reported by analysis; to verify |
| D5 | `ResourceBudget` exists only in sandbox mode and is checked after interpretation has spent it. | verified by reading |

## 3. Goals, non-goals, success

**Goals**

- Reliable turn understanding on small, cheap models, measured per task.
- Every model call small, strictly shaped, individually configurable, bounded and recorded.
- Configuration an adopter can read: one config, from code or TOML, with defaults.
- A codebase of small modules, with prose where AGENTS.md puts it.

**Non-goals**

- Changing what the deterministic core guarantees. Invariants I1–I20 hold, with the amendments
  in §15.
- A public graph DSL. The engine records its graph so one can come later.
- Fine-tuning, embeddings infrastructure, or a hosted evaluation surface.

**Success**

- On the same corpus and the same small models, the new pipeline beats the recorded baseline of
  today's interpreter on semantic completion, with zero side-effect and zero claim failures.
- No model call receives context its task does not need; every task's prompt has a snapshot and a
  size budget.
- Every call's prompt, model, settings, output and verdict are on the replay record.
- Every turn is bounded; exhausting a bound degrades into a notice or a question, never a partial
  effect.

**Decisions already taken**

| Question | Decision |
|---|---|
| Public API | Clean break. 0.1 is unpublished; the single-call interpreter is removed, not kept beside the new one. |
| Narration | Model-written, split into small tasks per block, each on facts code selects. |
| Shape | A fixed set of task kinds on a small typed task engine, not a public graph engine and not a model planner. |
| Matching | No literal matching of natural language anywhere; semantic checks are model tasks. |
| Delivery | Directly on `main`. |

## 4. Principles

- **P1. Models judge language; code checks structure.** Code checks closed-set membership (ids,
  enums, pointers), schema validity, arithmetic, policy, revisions, idempotency and event backing.
  Code never uses substring, folding, alias, keyword or word-overlap comparison to decide meaning.
- **P2. Small tasks on sliced context.** Each task receives only what its decision needs, with
  the static part first so provider prompt caches survive. The user's message always travels
  verbatim.
- **P3. Every choice is an enum of what is valid now.** Operations, records, card options,
  pointers into the message and same-turn act references are closed sets built per call.
- **P4. Absence is representable and preferred.** `not_given` is listed first for every
  argument; a required argument not given makes the act ask, never write.
- **P5. A verifier can only take away.** Verification may block, repair, or turn an act into a
  question. "Confirmed" authorizes nothing.
- **P6. Understanding completes before any effect, and no model runs after a commit** except the
  narration tasks.
- **P7. Every model call is configured, bounded and recorded individually.**
- **P8. Degrade without half-changes.** A unit that is not understood holds every act on the
  same record; a lost constraint or a failed segmentation fails the turn closed.

## 5. Architecture

```text
input ─► load and project cases                 kept, pure
      ─► UNDERSTAND                             turnframe-understand on turnframe-tasks
      │    structural shortcuts: click, single admissible record, single operation
      │    segment ─┬─► per unit, in parallel:
      │             │     route ─► locate (if >1 record) ─► extract ─► verify ─► check
      │             ├─► coverage (can only add units)
      │             └─► constraints, card answers, question bases come from segment
      │    assemble (code) ─► one UserTurnPlan
      ─► resolve, reduce, policy                kept; plan no longer model-facing
      ─► persist cards, execute, commit         kept; dependent acts ordered
      ─► NARRATE                                runtime narrate/ on turnframe-tasks
      │    acknowledge (≤1), answer (1 per question), review
      ─► ordered blocks, expectations, replay record with one entry per task
```

| Crate | Change |
|---|---|
| `turnframe-tasks` (new) | `ModelTask` trait, scheduler, budgets, repair, voting, escalation, `TaskRecord`, test kit for scripting task outputs. Depends on core and provider only. |
| `turnframe-understand` (new) | The understanding task kinds, context rendering, value expressions, assembly, built-in prompts. Depends on core, provider and tasks. |
| `turnframe-provider` | `RequestParams`, `TaskKind` replaces `ModelPurpose`, routing by task kind and profile tag, escalation candidates. |
| `turnframe-core` | `OperationSpec`, `ArgumentSpec`, value types, `ActId`, pointers, new act results, expectations; old `ActDefinition` and model-facing plan schema removed. |
| `turnframe-runtime` | `orchestrator.rs` split into one module per turn step; the plan-only path runs the same steps on read-only stores; `interpret.rs`, `read_loop.rs` and sample ranking deleted; `compose.rs` becomes `narrate/`; `reduce.rs` split. |
| `turnframe-store`, `-postgres` | Assistant turn stores expectations and receipt-to-act links; migration. |
| `turnframe-eval`, `turnframe-test` | Per-task expectations and scoring; scripted provider per task kind. |

## 6. Understanding

### 6.1 Pointers into the message

Code splits the message into words on whitespace (punctuation stays attached) and renders it
numbered, one line per sentence, a sentence ending at a word that ends in `.`, `!`, `?` or `…`:

```text
S1: [0]I [1]want [2]to [3]set [4]name
```

A task that refers to the user's words returns word ranges (`{"from": 3, "to": 4}`), and code
slices the exact text. A range out of bounds is a structural failure and gets a repair. Evidence
offsets on the plan are computed from the ranges, so nothing is searched. Messages longer than a
configured word count (default 400) are pointed at sentence granularity instead.

### 6.2 Task kinds

| Task | Runs | Sees | Returns |
|---|---|---|---|
| **segment** | once per turn with text | numbered message; last assistant message; open card (question, option labels); the expectation; one line per workflow | units: `kind`, `span`, `workflow` (enum or `unknown`), `correction_of` / `cancel_of`, `continues_previous`, and inline payloads for enum-only kinds |
| **coverage** | once, beside the unit chains | numbered message, the units found | `missed: [{span, kind}]`, appended as new units |
| **route** | per `request` / `correction` unit | the unit, its workflow's operation summaries, that workflow's cases' phase, open obligations and briefing, the expectation | `operation`: enum of offered operations, `start_workflow`, `none` |
| **locate** | per act when more than one record is admissible | candidates: label, phase, identifying fields; `new` when allowed | `record`: enum of tokens, `new`, `not_listed{span}`, `ambiguous` |
| **extract** | per act | the unit, the numbered message, the target record's state, the operation's guidance, argument labels and value shapes, its examples, the workflow glossary, a short transcript window | per argument: `not_given` or a value in its shape (§6.3) |
| **verify** | per mutating act (policy configurable) | the message, a cited earlier message, the operation's meaning, the record's label, the arguments rendered for a person | per argument `stated` / `not_stated` / `different` / `incomplete`; overall `confirmed` / `not_requested` / `wrong_record`; a reason |
| **question_frame** | per question when the record or subject is unclear | the question, candidate records, declared subjects (the workflow's enumeration subjects and narratable state fields) | `record`, `subjects` (enums) |
| **investigate** | per unit, only where configured | the unit, read tool descriptions | read requests, bounded; results reach only that unit's later tasks |

Unit kinds: `request`, `question`, `constraint`, `correction`, `cancel`, `card_answer`,
`dispute`, `provides_value`, `chitchat`. A `constraint` carries its kind (the existing
`ExecutionConstraint` set), a `card_answer` its option (enum of the open card's options), a
`question` its answer basis. `segment` returns a short `analysis` field before the typed fields.

Structural shortcuts, which make no call: a turn that is only a click; a single admissible record
(no `locate`); a workflow offering a single operation for a unit already assigned to it (no
`route`); a `provides_value` unit answering a pending act (no `route`, no `locate`, `extract` for
the missing arguments only).

`not_listed{span}` replaces today's mention search. The span goes to the application's
`CaseDirectory::find(workflow, words)`, which looks records up however the application stores
them; the framework compares no text. The candidates it returns go back to `locate` once; none
found is a refusal with its notice.

### 6.3 Argument value shapes

The model-facing shape of an argument follows its Rust type and its declared source:

| Argument | Model returns | Code does |
|---|---|---|
| text, source `User` (default) | a pointer into the message, or `not_given` | slices the user's exact words |
| text, source `User`, declared `written` | text, or `not_given` | nothing; `verify` judges it |
| schema enum | one of the values, or `not_given` | nothing |
| `NaiveDate` | a `DateExpr` | evaluates it with the turn clock, timezone and the argument's declared direction |
| `turnframe::values::Money` | `{amount: "10.50", currency: enum}` | parses the decimal, checks the pattern |
| `RecordRef<W>` | enum of `W`'s tokens, same-turn acts creating a `W`, `not_given` | resolves to a `CaseRef`, minted for a same-turn act |
| number, bool | the value, or `not_given` | schema validation |
| source `Inferred` | the value | nothing; `verify` checks consistency |
| source `Server(read)` | nothing, not on the schema | fills it from the declared read after assembly |

`DateExpr` is one of `absolute {year?, month, day}`, `relative {unit, amount}`,
`weekday {day, which: this|next|last}`, `period_end {period, which}`,
`period_start {period, which}`. «domani» is `relative {day, 1}`; the model never computes a date.

A value from an earlier message is marked `from: m<k>`, an index into the transcript window the
task was shown.

### 6.4 Verify and check

For each mutating act, after `extract`:

1. **verify** (model). Anything other than `confirmed` gets one repair of `extract` carrying the
   verifier's reason, then one more verification. Still unconfirmed, the act becomes `NeedsValue`
   for the arguments at fault, or is dropped as `NotUnderstood` when `not_requested`. It never
   writes.
2. **check** (code). Values are evaluated (dates, money, record refs), validated against the
   operation's schema, then the domain's pure `compile_act` and `validate_command` are dry-run
   against the target's state. A `DomainRejection` with an `argument` pointer gets one repair of
   `extract` with the domain's explanation, and then becomes `NeedsValue` with that explanation as
   the question. Without a pointer it is an ordinary refusal.

### 6.5 Assembly

Code builds one `Understanding` (`turnframe_core::understanding`) from the task outputs, in
message order. It replaces `UserTurnPlan` as the reducer's input:

- Each act gets `ActId = u<unit>.a<n>`. Command ids, card keys and minted case ids derive from
  `ActId`, not from list position.
- A `correction` unit's act supersedes the act of the unit named by `correction_of`. Acts with no
  link are independent; the reducer no longer guesses corrections from repeated operations.
- A `cancel` of a same-turn unit supersedes it; a cancel of earlier work is routed like a request.
- A `card_answer` settles the open card through `ResolutionChannel::ModelInterpreted` and
  resumes its deferred act. That channel satisfies only `ConfirmationPolicy::None`, so a card
  whose commands need a click gets a notice asking for the click (fixes D4).
- A `dispute` becomes a fact tied to the receipt it contests, never an act.
- Evidence is computed: spans from pointers, the click, attachments, the origin, `from: m<k>`
  markers and read results.

The plan is internal. Its schema is no longer model-facing, so rustdoc on plan types stops being
prompt text.

### 6.6 Degradation

- A unit whose tasks fail after repair and escalation, or that the budget did not reach, becomes
  `NotUnderstood`: a deterministic notice quoting its words.
- If any unit aimed at a record is `NotUnderstood`, every act on that record is `Held`.
- A failed `segment`, or a `constraint` unit that fails, fails the whole turn closed: no acts,
  and a deterministic notice saying the message could not be read.
- A question that cannot be framed gets the existing unsupported block.

### 6.7 Dependent acts

`extract` offers same-turn acts as values of `RecordRef` arguments, and assembly records
`depends_on`. The prerequisite's case id is minted at planning time. The reducer orders the turn:

| Prerequisite | Dependent act |
|---|---|
| `ReadyToExecute` | compiled after it; its batch commits after the prerequisite's batch |
| `AwaitingConfirmation` | `AwaitingPrerequisite`, stored on the prerequisite's card as a `DeferredAct`; on the click the confirmed commands run first, then the dependent act compiles and runs under its **own** origin and policy, raising its own card if its policy asks |
| refused, declined, `NotUnderstood` | refused with its own notice |

This replaces `continues_turn`, `settle_card_early`, the catalog's `already_done`, and the second
commit. With D1 fixed, a click authorizes exactly its card's commands.

### 6.8 Expectations

After narration the runtime records, on the assistant turn:

- `AwaitingValue { pending_act, missing }` for each `NeedsValue` act: operation, record and the
  arguments already given;
- `AwaitingValue { case, obligation }` for the obligation the acknowledgement was told to ask
  about (§10);
- the open card, as today.

The next `segment` sees them as «the assistant asked for: the name of Trip 1». A unit of
kind `provides_value` completes the pending act through the shortcut in §6.2. Expectations last
one turn. Offers the model writes in prose are not tracked.

### 6.9 Reads

- Declared reads are run by code: unconditional reads, `context_reads` per operation (results go
  only into that operation's `extract` context), and `Server` arguments.
- `requires_reads` is retired: dates no longer need a read (§6.3).
- Model-requested reads are the optional `investigate` task, bounded by rounds and calls,
  read-only by type. `OrchestrationMode::ReadAgentic` and `ControlledAgentic` collapse into this
  option; the sandbox mode stays as a risk ceiling.

## 7. The task engine (`turnframe-tasks`)

### 7.1 Trait

```rust
pub trait ModelTask: Send + Sync {
    type Input: Send + Sync;
    type Output: DeserializeOwned + JsonSchema + Send;

    fn kind(&self) -> TaskKind;
    /// Output schema for this input, with its dynamic enums.
    fn schema(&self, input: &Self::Input) -> Schema;
    /// Messages, static prefix first.
    fn render(&self, input: &Self::Input, instructions: &str) -> Vec<Message>;
    /// Structural checks only: ranges, enum membership, cross-field shape.
    fn check(&self, input: &Self::Input, output: &Self::Output) -> Result<(), StructuralError>;
    /// Whether two answers are the same answer, for voting.
    fn agree(&self, a: &Self::Output, b: &Self::Output) -> bool;
}
```

### 7.2 Scheduling

Each unit runs as one async chain (`route → locate → extract → verify → check`); chains run
concurrently under a `max_parallel` semaphore, `coverage` beside them. Results are collected by
`TaskId` (`turn/segment`, `u2/extract`, `u2/extract#repair1`, `u2/route#vote2`), never in
completion order, and each chain writes only its own slot; a second write to a slot is an error.
Assembly is therefore a function of the task outputs alone.

### 7.3 Budgets

Two budgets, one for understanding and one for narration. Every bound is configurable;
`Budget::unbounded()` is an explicit opt-in.

| Bound | Understanding | Narration |
|---|---|---|
| `max_model_calls`, reserved before a call is sent | 32 | 12 |
| `max_chain_depth`: dependent calls in a row, each repair, vote round and escalation counting one | 8 | 4 |
| `max_parallel` | 6 | 4 |
| `max_prompt_tokens`, from provider-reported usage; no new task once reached | 100 000 | 40 000 |
| wall clock | 30 s | 20 s |
| per-call timeout | 20 s | 20 s |

Exhaustion stops new tasks, cancels in-flight ones at the wall-clock bound, degrades as §6.6
says, and is reported on the replay record and as `turnframe.budget.exhausted{bound}`. `max_chain_depth`
is the answer to "a max call chain": the longest path segment → route → locate → extract → verify
→ repair → verify → escalation is eight.

### 7.4 Repair, voting, escalation

- **Repair** answers a structural failure or verifier feedback, once by default, on the same
  model, carrying the exact error. Structural errors are rendered by code from the structure
  («`operation` must be one of: …»). The framing sentence is a prompt binding per task kind.
- **Voting** runs `votes` samples concurrently at a sampling temperature and compares them with
  `agree`. A strict majority wins. Otherwise `on_disagreement` decides: `Escalate` (one run on the
  escalation model), `Clarify` (the unit becomes a question), or `Fail` (the unit degrades).
- **Escalation** is about quality, not transport: still invalid after repair, votes without a
  majority, or unconfirmed after repair. The task runs on its profile's `escalate_to` model and
  the result passes the same checks and verification. With no escalation model configured the
  unit degrades. Provider fallback on transport errors stays in the provider layer, unchanged.

### 7.5 Default profiles

| Task | Tier | Temperature | Votes | Repairs | Output cap | Notes |
|---|---|---|---|---|---|---|
| segment | small | 0 | 1 | 1 | 800 | `analysis` field first |
| coverage | small | 0 | 1 | 0 | 200 | on |
| route, locate, question_frame | small | 0 | 1 | 1 | 150 | `thorough`: votes 3, escalate on disagreement |
| extract | small | 0 | 1 | 1, plus 1 from verify | 600 | |
| verify | small, preferably another model family | 0 | 1 | 0 | 300 | policy `Mutating`; `ByRisk`, `All`, `Off` available |
| investigate | small | 0 | 1 | 1 | 400 | off |
| acknowledge | small | provider default | 1 | review rewrite | 300 | review on |
| answer | small | provider default | 1 | review rewrite | configurable | review off, streams |
| review | small | 0 | 1 | 0 | 200 | |

Reasoning effort defaults to the lowest level a provider offers for understanding tasks.

### 7.6 Provider changes

- `ModelRequest` gains `RequestParams { temperature, max_output_tokens, timeout,
  reasoning_effort, seed }`; each adapter maps them, with a conformance row per parameter.
- `TaskKind` replaces `ModelPurpose`, with an `evaluate` kind for the `turnframe-eval` judge.
  Pool profiles carry tags (`small`, `large`, `vision`, any
  name), and a task profile selects by tag; the router honours the task kind.
- Structured output: understanding tasks, `verify` and `review` require native schema, function
  or grammar constraint; prompt-only JSON is refused for them. `acknowledge` and `answer` produce
  free text.
- The five adapters' duplicated prompt-only JSON hint moves to one place in `turnframe-provider`.

### 7.7 Replay record and observability

The replay record gains `tasks: Vec<TaskRecord>` and a `BudgetReport`, and stores read results.

```rust
pub struct TaskRecord {
    pub task_id: TaskId,
    pub parent: Option<TaskId>,
    pub depth: u8,
    pub kind: TaskKind,
    pub prompt_ref: PromptRef,          // built-in prompts carry a content-hash ref too
    pub model: ModelRef,
    pub params: RequestParams,
    pub input_digest: Digest,
    pub rendered: Option<Vec<Message>>, // only with PrivacyConfig::store_model_prompts
    pub raw_output: Option<String>,
    pub parsed: Option<serde_json::Value>,
    pub verdict: TaskVerdict,           // Accepted, Repaired, Rejected{code}, Outvoted, Escalated
    pub usage: Option<TokenUsage>,
    pub latency_ms: u64,
}
```

It replaces `normalized_plan` (the record keeps the assembled plan the reducer saw, with its hash),
`discarded_answers` and the flat `provider_attempts`. Two replays follow: offline, feeding recorded
outputs to reproduce assembly and reduction with no model; and per task, re-running one task kind
live against another prompt or model.

Tracing opens one span per task under its parent. Metrics: `task.completed{kind, verdict}`,
`task.latency_ms`, `task.tokens`, `task.repaired`, `task.escalated`, `task.vote_disagreement`,
`budget.exhausted{bound}`.

### 7.8 Steps a consumer can show

Every decision of the understanding is published as it is made: a typed `Step` (segmented,
routed, located, extracted, verified, repairing, checked, not understood, assembled) sent to a
`StepSink` the caller passes in. Each step carries its facts as data and a plain one-line
`describe()`. The runtime forwards steps on the turn stream as `TurnEvent::Step`, before any
outcome event; a step states what was understood, never what happened, so the publication gate
does not hold it back. An opt-in narration task, `narrate.step`, turns steps into short prose for
a "thinking" preview, with its own prompt binding and profile; it is off by default and never
blocks the turn. The console prints steps as they arrive.

## 8. Configuration and prompts

### 8.1 One configuration

`TurnframeConfig` (serde) holds `understanding` (task profiles, budget), `narration` (task
profiles, budget, tone) and today's interaction, execution, privacy and observability sections.
The builder and TOML share one shape; the library reads no environment.

```toml
[understanding.budget]
max_model_calls = 32
max_chain_depth = 8

[understanding.tasks.route]
votes = 3
on_disagreement = "escalate"
escalate_to = "large"

[understanding.tasks.verify]
model = "small-b"
policy = "mutating"

[narration.tasks.acknowledge]
temperature = 0.7
review = true
```

`TurnframeConfig::default()` is §7.3 and §7.5. `TurnframeConfig::thorough(escalate_to)` adds
votes on `route`, `locate` and `question_frame` and escalation to the named tag.
`validate()` returns typed errors, and building the orchestrator checks that every tag the config
names exists in the pool.

### 8.2 Instructions and generated context

Each task kind has one short instruction text, shipped as `prompts/<stage>/<task>.md`, compiled in
with a content-hash `PromptRef`, and overridable through `PromptSource` under `understand.segment`,
`understand.extract`, `narrate.acknowledge` and so on, plus a `.repair` binding. Lookup tries
`<name>.<locale>` before `<name>`. There is no template engine.

The data a task needs is rendered by the framework as labelled sections in a fixed order, from the
declarations relevant to that task only:

| Task | Sections |
|---|---|
| segment | workflow summaries, the expectation, the open card |
| route | the workflow's operation summaries, its phase briefing |
| locate | candidates' identifying fields |
| extract | operation guidance, argument labels, descriptions and value shapes, examples, record state, workflow glossary |
| verify | operation meaning, argument labels, values rendered for a person |
| acknowledge, answer | §10 |

The `extract` prompt for `set_name` on the test domain, about 250 tokens. Examples show the
words a pointer would select:

```text
[instructions: understand.extract]
Operation: trip.set_name: Name the trip.
Record: Trip 1 · Collecting · name: none
Argument value (name / nome): what the traveler calls the trip, in the user's words. Required.
Examples:
  «the name is Lisbon for March» → value: «Lisbon for March»
  «the name needs changing» → value: not_given
Message: S1: [0]I [1]want [2]to [3]set [4]name
```

Every task's rendered prompt on the sample domains is snapshot-tested with a size budget. Every
model-facing schema string is written with `#[schemars(description = "...")]`; rustdoc is never
model-facing.

## 9. Domain API

### 9.1 Operations

`ActDefinition` becomes `OperationSpec`, `#[non_exhaustive]`, built with a builder so a new field
stops breaking adopters' literals.

```rust
OperationSpec::new("trip.set_name")
    .summary("Name the trip.")
    .target(TargetPolicy::ExistingCase)
    .mutating()
    .arguments::<SetNameArgs>()
    .argument("value", |a| a.label("name").label_in("it-IT", "nome").required())
    .example("the name is Lisbon for March", json!({ "value": "Lisbon for March" }))
    .example_not_given("the name needs changing", ["value"])
```

- `ArgumentSpec`: labels per locale, description, required, source (`User` default, `Inferred`,
  `Server(read)`), `written` for text the model may phrase, date direction (`Future`, `Past`,
  `Any`).
- Registry build fails when an argument name is not in the argument type's schema or an example
  does not deserialize into it.
- Kept: target policy, mutability, availability (`Proposable`, `CardOnly`), guidance.
- Removed: `quoted_arguments`, `transcribed_arguments`, `requires_reads`, `ActClaim`.

### 9.2 Workflows

| `WorkflowDefinition` | Change |
|---|---|
| `summary()`, `glossary()` | added |
| `interpretation_catalog(view)` | becomes `operations(view) -> Vec<OperationSpec>` |
| `narratable_state` | `StateField` gains `identifying`, shown by `locate` |
| `transition_briefing`, `answer_briefing` | merge into `narration_guidance(view, block)` |
| `validate_command` | `DomainRejection` gains `argument: Option<String>` |
| `confirmation_subject` | loses `continues_turn` |
| `intention_aliases`, `obligation_references`, `act_subject` | removed |
| projection, `briefing`, `obligation_sentence`, `enumerations`, `nothing_changed`, `compile_act`, `command_policy`, `receipts`, `build_interaction`, start rules, `artifacts` | unchanged |

`CaseDirectory` gains `find(workflow, words)` for the `not_listed` path (§6.2), with a default
that finds nothing.

### 9.3 Plan types

The reducer's input is `turnframe_core::understanding::Understanding`, which replaces the
model-facing `UserTurnPlan`, `ProposedAct` and `EvidenceRef`:

- `UnderstoodAct` carries `id: ActId`, `depends_on`, a target (`Record`, `New`, `SameTurn`,
  `Card`, `NotListed`, `Ambiguous`, `Nothing`), parsed arguments with the words that state each,
  and a status (`Ready`, `NeedsValue`, `Held`).
- Questions carry their words, record, subjects and `continues_previous`; the plan carries
  constraints, the card answer, disputes, superseded acts and units not understood.
- `PlannedActResult` gains `NeedsValue { arguments }`, `NotUnderstood`, `Held { reason }` and
  `AwaitingPrerequisite { act }`.
- `CommandOrigin::ConfirmedInteraction` carries the card's `command_refs` (D1).

## 10. Narration

Receipts, notices and cards stay deterministic, in the ADR-010 block order. Model blocks are
written from facts code selects.

**acknowledge**, at most one per turn, from a `TurnOutcome`:

```rust
pub struct TurnOutcome {
    pub done: Vec<ReceiptSummary>,
    pub not_done: Vec<NotDone>,       // refused, declined, held, not understood, with reasons
    pub disputes: Vec<Dispute>,       // what was contested, and which receipt
    pub ask: Option<Ask>,             // exactly one, chosen by code
    pub card: Option<CardSummary>,
}
```

`ask` is the first `NeedsValue` argument (a dispute reopens one), otherwise the first open
obligation of a record the turn touched. A missing value is asked for, never reported: a
`NeedsValue` act produces no notice, and the acknowledgement says what the user can do and asks
for the value («Yes, you can set the name: what should it be?»). When the acknowledgement is
skipped or fails review, a deterministic question built from the argument's label takes its place. The task also sees the verbatim message, the question
texts it must leave to the answers, a transcript window (default 4 messages) and the workflow's
`narration_guidance`. When `done`, `not_done`, `disputes` and `ask` are all empty the task is
skipped without a call. Because code chose `ask`, recording it as the expectation (§6.8) is exact.

**answer**, one per question, in parallel: the question's words, the previous assistant message
when `continues_previous`, the framed record's state on the declared basis, the workflow's
enumerations for the framed subject, knowledge chunks with their trust level, and the answer
guidance. It returns text or `cannot_answer{reason}`, which becomes the unsupported block.

**review**, per written block (acknowledgements by default): does the text claim something not in
its facts, contradict a notice beside it, restate a receipt or notice already on screen, answer
something not asked, or, for an acknowledgement, ask for anything other than `ask`? A failing
verdict gets one rewrite with the issues, reviewed again; still failing, an acknowledgement is
dropped (receipts, notices and cards still say what is true) and an answer becomes the unsupported
block. The structural claim guard stays.

**Disputes.** The assistant turn keeps each receipt's link to its act. A `dispute` on a receipt
reopens that operation's argument as a `NeedsValue` pending act; nothing is reverted. The
acknowledgement owns the mistake and asks for the value; an answer, if asked, says what is wrong.

**Streaming.** A reviewed block is published whole; a block without review streams after the
commit, as today.

Removed: `EmptyTurnTransition`, the `transition_speaks` conditions, question byte-span cutting,
dropping user text on refusal, guidance suppression, the `preceding_reply` exceptions,
`FactRelevance` ordering, the echo detector and own-words retry, `ClaimVocabulary`, batched
answers, `CONTEXT_MESSAGES`, and the unbound narrator preamble.

## 11. Safety fixes

Each starts with a test that fails on today's code. A test that passes records the hole as absent
and the fix is dropped.

| # | Fix |
|---|---|
| D1 | `ConfirmedInteraction` carries `command_refs`; `satisfies_confirmation` requires the command to be among them; no model-proposed act receives a click origin. |
| D2 | Recovery never executes a `Pending` command without its confirmation, and re-checks policy on resume. |
| D3 | A card is admitted to `Resolving` only after understanding succeeds; a failed turn restores `Active`. |
| D4 | Typed card answers resolve cards through the model-interpreted channel (§6.5). |
| D5 | Budgets apply to every turn and are reserved before spending (§7.3). |

## 12. Evaluation and tests

- **Corpus (Phase 0).** Realistic turns on the three test domains in English and Italian:
  multi-intent messages, corrections and cancels, the §2.2 shape and other missing values,
  disputes, short answers to an asked value, relative dates, amounts, typed card answers,
  ambiguous records, dependent acts, questions mixed with acts, chitchat. Items may state
  per-task expectations (units with spans and kinds, routes, records, arguments including
  `not_given`, verify verdicts) beside the turn-level ones.
- **Baseline (Phase 0).** Today's interpreter runs the corpus live on two or three small models
  through `TURNFRAME_EVAL_LIVE_*`; the report is committed as data under `turnframe-eval`, never
  quoted in documentation (CONTRIBUTING rule 8). Live runs spend on the developer's keys and are
  announced before they start.
- **Acceptance (Phase 7).** Same corpus, same models: the new pipeline beats the baseline on
  semantic completion with zero side-effect and claim failures. Reports gain per-task accuracy,
  calls, tokens, latency, repairs, escalations, vote disagreements, prompt refs and models.
- **Tests.** Per task: prompt snapshot with size budget, schema snapshot, structural checks.
  Engine: budget exhaustion, cancellation, ordering, voting, escalation. Assembly: property tests
  that equal task outputs give equal plans and `ActId`s. Offline replay of recorded outputs.
  Provider conformance rows for `RequestParams`. The scripted provider scripts per task kind; the
  60 runtime test files move to task scripts or drive the reducer directly.
- **Console.** Prints each turn's task trace and reads a `TURNFRAME_CONFIG` TOML path.

## 13. Removed

- `interpret.rs`: the single call, `InterpreterPrompt`, per-turn schema surgery, `repair_detail`,
  `says_nothing`, `keeping_constraints_of`, the catalog payload.
- `read_loop.rs` as a separate loop; `plan_samples` and sample ranking in `planning.rs`.
- `continues_turn`, `settle_card_early`, `already_done`.
- Literal mechanisms: quote substring grounding, `values_are_quoted`, `extend_dictated_values`,
  `fold_for_matching` for meaning, `message_contains_declared_alias`, `catalog_about` lexical
  matching, `named_in`, `ClaimVocabulary`, the echo detector.
- Narration patches listed in §10.
- Dead code: `NarrationConfig::allow_retry_after_commit`, `OrchestrationMode::ControlledAgentic`,
  `NarratableFact::ProposedChange`, `Composition::withheld_claims`, unused purposes.

## 14. Code and documentation rules for this work

- AGENTS.md comment limits apply to every new and touched file. Incident stories are deleted, not
  moved; invariants go to `docs/architecture.md`, decisions to ADRs.
- One responsibility per module, and no module over 800 lines excluding its tests.
- The shadow path and the live path share the turn-step modules; the shadow path is those steps
  over `ReadOnlyStores`.
- `docs/architecture.md`, `docs/reliability-model.md`, `docs/composition.md`,
  `docs/provider-adapters.md` and `docs/evaluation.md` are rewritten where they describe replaced
  mechanisms, and their known drift is fixed on the way.

## 15. ADRs and invariants

| Record | Content |
|---|---|
| ADR-015 (new) | Models judge language; code checks structure. |
| ADR-016 (new) | Understanding is a bounded set of small verified model tasks, assembled by code into one plan before any effect. |
| ADR-017 (new) | A click authorizes exactly the commands its card names. |
| ADR-018 (new) | Dependent acts are planned in their originating turn; no model runs after a commit except narration. |
| ADR-001 (amend) | Evidence is pointer-exact and model-verified. |
| ADR-009 (amend) | The read loop becomes the optional `investigate` task. |
| ADR-014 (amend) | Decision 1: the plan may be assembled from several model calls, all before reduction; all-or-nothing applies per unit and per record (§6.6). |

I9 reads "schema-, pointer-, verification-, target- and policy-checked"; I18 reads "a unit is
understood whole or not at all, and a record's acts stand or fall together"; I12 gains "a
confirmation authorizes only the commands its card names".

## 16. Migration for adopters

A `docs/migration-0.1-understanding.md` guide, written in Phase 8, covering: `ActDefinition` to
`OperationSpec`; argument labels, sources and examples; `DateExpr`, `Money` and `RecordRef`
argument types; removed declarations and what replaces each; `TurnframeConfig` and its TOML;
prompt binding names; the Postgres migration; replay record changes; the test kit's per-task
scripting.

## 17. Plan

Every phase ends with the crates it touched building and passing their tests. The full
CONTRIBUTING sequence runs at the end of Phase 8.

| # | Phase | Deliverables | Exit check |
|---|---|---|---|
| 0 | Baseline and holes | failing tests for D1–D3, then fixes; corpus with per-task expectation format; live baseline report; ADR-015–018 drafts | holes closed or recorded as absent; baseline committed |
| 1 | Provider | `RequestParams` in all adapters; `TaskKind`; tag routing; escalation candidates; conformance rows | adapter suites green with the new rows |
| 2 | `turnframe-tasks` | trait, scheduler, budgets, repair, voting, escalation, `TaskRecord`, scripting test kit | engine tests green standalone |
| 3 | Core types | `OperationSpec`, `ArgumentSpec`, value types and `DateExpr` evaluation, `ActId`, pointers, new act results, expectations, beside the old types | workspace green, nothing switched |
| 4 | `turnframe-understand` | all understanding tasks, word pointers, context rendering, assembly, built-in prompts with snapshots and size budgets | pipeline tests green on the test domains with scripted tasks |
| 5 | Runtime switch | turn-step modules; shared shadow path; dependent acts and ordered batches; typed card answers; expectations with store and Postgres migration; new replay record; old interpreter, read loop, sampling and old types deleted; test domains migrated with labels, examples and new copy; steps forwarded as `TurnEvent::Step` and printed by the console | end-to-end runtime tests green on the new pipeline |
| 6 | Narration | `TurnOutcome`, acknowledge, answer, review, disputes, streaming rule, opt-in `narrate.step`; patches deleted | narration tests green |
| 7 | Evaluation | per-task scoring and report fields; live run against the baseline; defaults tuned; console task trace and config path | acceptance met |
| 8 | Documentation | architecture and guides rewritten, ADRs final, migration guide, CHANGELOG, READMEs, comment sweep | full CONTRIBUTING sequence green |

## 18. Risks

| Risk | Mitigation |
|---|---|
| Latency from several sequential calls | structural shortcuts, per-unit parallelism, `coverage` off the critical path, prompt caching on static prefixes; measured in Phase 7 |
| Pointer errors on long messages | sentence-granular pointers above a word count; structural repair |
| Verifier false negatives add friction | measured per task; `verify` policy and model are configurable; a false negative asks, never writes |
| Decomposition does not beat a compact single call | Phase 0 baseline and Phase 7 comparison decide it on data |
| Constrained output hurts reasoning on small models | `analysis` field on `segment`; per-task setting |
| Test migration cost | its own work item in Phase 5; reducer tests move to direct reducer input |

## 19. Prior art

- Decomposed prompting (Khot et al., 2022), least-to-most prompting (Zhou et al., 2022), ReWOO
  (Xu et al., 2023), LLMCompiler (Kim et al., 2023): decomposition, deterministic joins.
- FnCTOD (2024): dialogue state tracking as select-then-fill function calling; Rasa CALM's
  command generators and flow retrieval, including why its multi-step generator lost multi-intent
  messages.
- Self-consistency (Wang et al., 2022); DSPy best-of-N and refine; instructor's validation reask;
  Pydantic AI output validators; rig's `Extractor` and invalid-tool-call repair.
- LangGraph `Send` fan-out, channel conflict errors and recursion limits; Google ADK workflow
  agents; Anthropic, "Building effective agents" (workflows, evaluator-optimizer) and tool input
  examples; AutoAgents executors and hooks.
- Context degradation: Chroma "Context Rot" (2025), "Lost in the Middle" (Liu et al., 2023),
  NoLiMa, LongFuncEval, IFScale.
