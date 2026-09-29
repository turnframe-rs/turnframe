# Changelog

All notable changes to Turnframe are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0]

The first release.

### Added

**Workflows and the Flow Map (`turnframe-core`)**

- `WorkflowDefinition`: a pure projector from persisted state to a `WorkflowView` of exactly one
  lifecycle phase, parameterized obligations, at most one blocking interaction requirement and an
  optional outcome. Workflows declare the operations a view offers as `OperationSpec`s: arguments
  with value shapes and labels, target policy, whether a model may propose them, examples and
  guidance. Obligations can carry the sentence the server asks them with, and the act that answers
  them (`WorkflowDefinition::obligation_act`), so a bare answer completes it. An argument can be the
  name of the record its operation creates (`ArgumentSpec::names_the_record`), so a record created
  and used in one message is shown by that name.
- Dates and amounts as typed expressions (`DateExpr`, `Money`) that code evaluates with the turn's
  clock, so a model never does calendar arithmetic.
- `Understanding`: what the tasks made of a message (units, acts, questions, constraints and what
  was not understood), always a proposal and never an effect.
- The replay record: what the turn loaded, understood, decided, committed and returned, every model
  task with its verdict, the budget it spent, and the answers a check refused.

**Understanding (`turnframe-tasks`, `turnframe-understand`)**

- A task engine for small typed model tasks: a strict schema built per call from closed sets,
  structural checks with a repair round quoting the exact error, in-place retries, votes,
  escalation to a stronger model, per-kind profiles, per-turn budgets (calls, tokens, depth, wall
  clock) and a record of every call. A split vote may be read once more, shown the answers that
  disagreed (`Disagreement::Reread`); profiles change field by field (`ProfileChange`), and a
  turn's scope may carry its own. `verify` reasons at `low` by default, with room in its output cap
  for it, and `medium` casts three votes on `segment` and `route`, reading a split vote once more, and
  two more on a verdict that finds fault.
  The live corpus takes `TURNFRAME_EVAL_LIVE_SAMPLES` and `TURNFRAME_EVAL_LIVE_CONCURRENCY`.
- The understanding pipeline: `segment`, `coverage`, `route`, `locate`, `extract`, `verify` and
  `question_frame`. Values point at the user's own words, a text value also copies them, and a
  verifier checks every act against what the user said. A unit may ask for several operations; a
  correction or cancel supersedes the act it names; a short answer completes the act the assistant
  asked about. A record created in the message that was asked for one completes the act that was
  waiting for it, and takes the name the user gave before when that name found no record. An
  answer read as the act that asked, giving none of what it asked, is routed once more, told so,
  and may take up what the last reply offered; a second route that finds nothing else asks again. A
  request nothing on offer does that reads as a question is answered as one, and a question's tail
  found apart joins it. Words one task read as small talk and another as an act are segmented
  again, told what the check saw; still small talk, they run nothing and are reported unclear, and
  read as a condition they stay small talk. Segmentation votes that differ only by one word at a
  unit's edge agree. An
  act takes its values from its own part of the message, words no other part holds beside it, or
  an earlier message; a value pointed at in another part's words is read again once, told so. The
  same act asked twice is one, two acts of one operation with different values are two, and quotes
  around a value are not part of it, nor the mark ending its sentence unless the copy keeps it. A
  value that may be deduced may share the words that imply it. A correction's own words are the
  user's last word on what they change, and the verifier reads it after the request it corrects.
  An act whose every value lies in another part's words runs nothing and reports nothing, and one
  the verifier finds was not asked for says nothing when its unit asked for something else or only
  coverage found it. A correction naming a unit it cannot change changes something earlier, and
  one of an act the last turn did keeps the values it does not change: the reply records what the
  turn did (`AssistantTurn::done`). Acts asked in one part run in the order the message says them.
  A repair re-reads what the verifier found wanting, and a value it found stated stands when the
  repair gives none. Two readings of one operation in one part that take the same words are both
  read again, and a part that gives its act nothing, right after a copied value, is read as that
  value's tail when the value takes it. A value read across a neighbouring part that asks for the
  same operation is one value the segmentation cut in two. A record argument a reading leaves
  unset is read once more, and a word that only points to a record is not its name. A lone word
  between two parts joins them.
  An act waiting on a record the message creates, whose creating act was dropped as another part's
  read twice, takes the one record of that workflow the message still creates. A record named by the
  name a record this message creates is given is that record, and a name may be pointed at wherever
  it was said. Acts read from one part never share its words: a copied value that runs into
  another act's value is read again once, told those words, and what it still copies too far ends
  where that value begins. A request's value read from the words of a value given beside it is
  that value's own reading, so the two join as one act. Routing a part of a message is shown the other parts by
  their words, as routed on their own, so it does not list what they ask for. A copied value ending on its
  part's last word, where the next part begins, is read again once, told that word: the word
  joining two parts is no value's. An act of a part only coverage found whose every value lies in
  another part runs nothing without being read again. A part read both as creating a record and as giving a listed
  record of that workflow a value from the same words gives the listed record the value. A value the verifier found takes too
  many words, read again to the same words when told so, stands. A stated number is one its words say when they hold numbers, and an act read
  again to nothing after pointing only at another part's words runs nothing. Each decision streams as a `Step`. At `high` effort a `cross_check` reads the
  whole understanding against the message: each finding sends one step of one act back once, and
  the verifier decides: the check alone holds nothing, and an act it doubts was asked for is
  verified again without being told the doubt. An act located again to the record it had
  keeps the values it was verified with. Words read as a dispute that a check reads as an act are
  read again told they were a dispute, and the rule that tells the two apart. A record chosen from
  those listed is that record whatever part's words name it, and another part creating it from
  those same words creates none. An answer is routed to what the assistant asked about, unless its
  words name something else.
- A keep-unchanged constraint (`ConstraintKind::KeepUnchanged`): asked to leave something as it
  is, the turn judges each act that changes a record with a small `respects` task, and one that
  would change what the words keep runs nothing (`NotUnderstoodReason::KeptUnchanged`) while the
  rest of the turn runs. A judgment that cannot be had holds every act on that record. The reply
  says what was left alone, quoting the user (`notice::KEPT_UNCHANGED`).
- The task prompts state rules for every domain and name nothing of one. What a model needs to
  know about a domain comes from that domain's operations (summaries, argument labels and
  descriptions, examples), glossary and briefings; a test fails when a prompt names what a sample
  domain holds. `verify` reads an answer against the question it answers, and a correction
  against the changes the last turn reported.

**Turns (`turnframe-runtime`)**

- Effort levels (ADR-020): `low`, `medium` (the default; it votes on segmentation and routing, and twice
  more on a verdict that finds fault)
  and `high`, set in `OrchestratorConfig::effort` and forced for one turn with `TurnInput::effort`.
  `high` votes, reasons (save `extract`, which copies values better without) and checks the whole
  reading; `low` skips the reply review and the step prose. Each level
  is changed field by field in configuration, a stronger model included. The level is on the
  replay record and labels the turn and task metrics; no level changes what policy allows.

- The orchestrator: loads cases through a `CaseDirectory` (which can look up a record the user
  named), understands the message, reduces the whole turn, applies policy, executes typed commands
  with an expected revision and an idempotency key, commits events, and writes the reply.
- The reducer: one explicit result per act, constraints such as «do not send it» applied to the
  whole turn, ambiguity turned into a selection card, dependent acts planned in the turn that asked
  for them and carried on a confirmation card when their prerequisite waits for one. A card whose
  workflow names no subject shows the values it would write. A record the user names that nobody
  has registered is asked for again, with an offer to register it when its workflow can; why a
  value is asked for again is on screen as a notice in the server's own words. The act keeps
  waiting across later turns (`Expectation::StillWaiting`, the newest three), and a record
  registered under that name, or with none, completes it in the turn that registers it, a record
  still to create included. A value given beside its request in the same message joins that
  request's act.
- Persistent interactions: confirmation, selection and review cards whose options the server owns;
  a click authorizes exactly the commands its card names, and a second click repeats the result. A
  card is bound to the revision it was drawn at: once the record moves, from the conversation or
  from outside it, a click on it is refused as stale and the next turn draws the card again from
  the record as it stands.
- The reply: one message per turn, the transition block, written last from the turn's outcome, the
  answers and the notices, and reviewed against a checklist that includes leaving nothing out; a
  turn that only answers replies with its answers as written, and a question about the values
  one record's field may take asks for what that record still needs. The next turn reads the reply
  back once. One answer task per question, and a
  question code chooses when the work needs something, after a
  question about a record too, and after a message that did nothing, from the records in view. A
  question that follows up the last exchange is shown both of its messages. A card on a record the
  turn reached is what the reply points to next; no other question is asked beside it. A record
  that owes nothing more ends the reply on what its workflow offers next
  (`WorkflowDefinition::next_steps`, empty by default). A question about which
  values a field takes is answered from the workflow's declaration with no model call, naming every
  value in its words as well as in the block's data. When no model writes a reply that passes, the
  server's own question stands in.
- Server copy in English and Italian: the five copy structs resolve by the turn's language,
  `OrchestratorBuilder::locales` makes the orchestrator refuse to start while a declared language
  lacks any sentence, and `ServerCopy::translated` adds or replaces a language field by field.
  `WorkflowDefinition::noun` names one record of a workflow in the user's language, for the
  sentences the server writes about it.
- Streaming of the turn's phases and understanding steps, with the reply published whole after the
  commit; optional step prose in the user's language (`NarrationConfig::steps`).
- The outbox for external effects, with explicit unknown outcomes and a reconciliation hook, and
  crash recovery at every commit boundary.
- Local tracing: `TracedProvider`, `OrchestratorBuilder::trace` and `JsonlTrace` write a turn from
  message to reply as JSON Lines.

**Storage (`turnframe-store`, `turnframe-store-postgres`)**

- Seven object-safe store traits with a deterministic in-memory implementation and an executable
  conformance suite, and a PostgreSQL implementation with migrations and expected-revision
  transactions that runs the whole suite as its proof.

**Providers (`turnframe-provider` and adapters)**

- Provider-neutral requests and responses, one `ModelPurpose` per model task, capabilities declared
  per provider-model profile, routing that never downgrades a structured-output requirement,
  fallback bounded by the commit, redaction of secrets, and a conformance suite every adapter runs.
- Adapters for OpenAI and OpenAI-compatible endpoints, Anthropic, Google Gemini and Vertex AI, AWS
  Bedrock Converse and Ollama, each rewriting schemas into its provider's dialect and refusing what
  it cannot carry.

**Prompts, telemetry, testing and evaluation**

- `turnframe-prompt`: prompts compiled in from the adopter's repository, a bounded cache and an
  optional Langfuse v4 source; the prompt behind every task call is recorded.
- `turnframe-telemetry`: `turnframe.*` metrics, tracing spans with the OpenTelemetry GenAI
  attributes, session grouping on every span, an optional OpenTelemetry bridge, and a reliability
  dashboard description that keeps safety failures apart from language quality.
- `turnframe-test`: scripted providers and tasks, fake stores, the sample travel desk (a trip with
  its legs, extras and the airline's offer; a traveler profile; an expense claim read from a
  receipt), a sample airline that answers, refuses or never answers, bounded workflow exploration,
  property strategies and conformance suites. A card confirms a rebooking and activates a
  traveler, whose fields are given in text; a leg the traveler keeps is locked by the domain;
  `with_cards()` puts cards in front of the other consequential steps.
- `turnframe-eval`: a corpus of scenarios checked by deterministic assertions, judges limited to
  prose, repeated samples, gates, baselines and paired comparisons, scores for each understanding
  task, and a live corpus that runs against a real model. An item can play a conversation
  (`before` turns, a card reply by workflow) and check the records it created
  (`workflow_state`, `case_count`). A state expectation may accept any of several right readings
  (`one_of`). A `before` turn may be a change from outside the conversation (`external`: the
  domain's own command, applied between turns as its system would), such as an airline re-quoting
  a fare under a card already on screen.
- `turnframe`: the facade crate, with every family member behind a feature flag.
- Examples: a travel-disruption desk that walks the four guarantees (dependent acts, a protected
  leg, a stale card, an airline that does not answer), a traveler onboarding flow and a mixed
  question-and-action turn, all running with no key and no database, and a console that runs the
  travel desk against a real model from one seeded trip, printing what a user would read apart
  from its own diagnostics and tracing a session to one file.

### Notes

- This is the first release: nothing it changes was published before. The public structs of the
  domain API (`OperationSpec`, `NarrationConfig` and their neighbours) have public fields, so from
  this release on a new field is a breaking change, named here.
- What the live corpus measured on this release, and what it cost, is in
  [`docs/benchmarks.md`](docs/benchmarks.md). No production figure is claimed.
