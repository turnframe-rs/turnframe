# Changelog

All notable changes to Turnframe are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1]

Published: `turnframe-provider-openai`, `turnframe-tasks`, `turnframe-understand`, `turnframe-test`
and the `turnframe` facade at 0.2.1. Every other crate stays at 0.2.0. The facade asks for the four
at 0.2.1.

### Fixed

**`turnframe-provider-openai`**

- GPT-6 models run. The adapter knew reasoning models only by the `gpt-5`, `o1`, `o3` and `o4`
  names, so a `gpt-6` model was sent `max_tokens`, a temperature and a seed, and OpenAI refused
  every call with `unsupported_parameter`. Every `gpt` from `gpt-5` on is now a reasoning model,
  `is_reasoning_model` included: it is sent `max_completion_tokens` and its reasoning effort, and
  never a temperature or a seed.
- The least effort is one the model takes: `none` on `gpt-6-luna` and `gpt-6-sol`, and `low` on
  `gpt-6-astra` and `gpt-6.1-sol`, which refuse `none`. Reasoning counts against a task's output
  cap, so on those two the small tasks keep less room for their answer.

**`turnframe-tasks`**

- The reply, the answer and the progress line are written at the least effort, as every
  understanding task is read. They sent none, so a model whose default effort reasons spent the
  reply's cap thinking: on `gpt-6-luna`, 12 replies in 284 came back empty and the turn fell back
  to the reply code writes. Their temperature is still the model's own. On `gpt-5.x` nothing
  changes; an `o` model writes at `low`, and a Gemini model declaring reasoning controls no longer
  thinks before it writes.
- Builds without warnings on Rust 1.99, which deprecates `AtomicU32::fetch_update` for
  `try_update`, a name the 1.88 MSRV does not have.

**`turnframe-understand`**

- Refusing to give a value («you won't get A from me») is a request, so the refusal is recorded.
  It was read as a condition to leave A as it is, and nothing was recorded.
- Saying what one would rather a record hold («I'd rather A») is a request. It could be read as
  small talk, and nothing was done.
- The check takes words that place a day without naming it («the end of the month», «next
  Friday») as stating the day they place, and still finds a day they do not place different. It
  had called such a date incomplete and asked for it again.

**`turnframe-test`**

- The trip sample adds a new extra with `add_extra`: a payer given for an extra the trip already
  holds is read as `assign_payer`, without asking for a new extra's price.

### Changed

- `gpt-6-luna` is the model the benchmarks and the site measure, and the default of the OpenAI
  live smoke test, the evaluation harness and the console example. On it the live corpus passed
  227 of 228 samples, merged from a whole run and two re-runs of the items this release's rules
  act on, for $0.37, and ten simulated conversations reached their goal ten times with no dead
  end. The `gpt-5.4-mini` runs of 0.2.0 stay in the benchmarks.
- The live smoke test's plain calls ask for the least effort, as every understanding task does: at
  its default effort a reasoning model can spend a small output cap reasoning and answer nothing.
- The site reads its version from the facade, the version applications depend on, and says
  small models where it said mini and flash ones.

## [0.2.0]

Published: every crate at 0.2.0, on the workspace version again. Every crate depends on
`turnframe-core`, whose public API changed, so all of them move together.

This release implements ADR-021, a conversation always moves forward: progress is guaranteed by
code, the assistant's offers are data the next turn reads first, a correction keeps what it does
not restate, and conversations are evaluated by simulated users.

### Breaking

**`turnframe-core`**

- `WorkflowDefinition::next_steps` returns typed offers and sees the state:
  `fn next_steps(&self, state: Option<&Self::State>, view: &ViewOf<Self>) -> Vec<NextStep>`,
  where it was `fn next_steps(&self, view: &ViewOf<Self>) -> Vec<LocalizedText>`. A `NextStep` is
  an operation, its words and the arguments already known:
  `NextStep::new(operation, words).with_arguments(json)`, whose arguments are named: a value that
  is not an object gives none. The erased form takes the case and the state. **Migration:** wrap
  each sentence as `NextStep::new(<the operation it offers>, sentence)`.
- `AssistantTurn::offers: Vec<Offer>`: the next steps a reply offered, each with its record,
  operation, words and arguments by name (an offer to open a record none of exists names one still
  to create, with an empty case id). A literal that builds an `AssistantTurn` adds
  `offers: Vec::new()`. It is left out of the serialized turn when empty, so stored turns read as
  before.

**`turnframe-provider`**

- `ModelPurpose::TakeUp`, the small task that reads a message against the offers of the last
  reply. `ModelPurpose::ALL` has sixteen purposes.

**`turnframe-understand`**

- `UnderstandingInput::offers: Vec<OfferBrief>` and `with_offer`: the offers of the last reply,
  each as its words and the act it runs.
- `ExtractInput::corrected`: the dates a correction changes, by argument. A literal that builds an
  `ExtractInput` adds `corrected: BTreeMap::new()`.

### Added

**`turnframe-core`**

- `WorkflowDefinition::record_operations`, defaulted to none: every operation a record of the
  workflow may offer, in any phase, shown to understanding while none of its records is in view.
- `WorkflowDefinition::state_after`, defaulted to none: the state a valid command leaves, when the
  workflow can tell without executing it. The erased form is `ErasedWorkflow::state_after`.

**`turnframe-runtime`**

- A next step is offered only when the domain would take it now (I22): the operation is on offer
  for the record and, when its arguments are complete, the act compiles and every command
  validates against the state.
- The offers a reply made are recorded on the turn, and the next message is read against them
  first: «yes, rebook it» after an offer runs that offer on its record, with nothing routed or
  located again. A message that takes up no offer is routed as before.
- A question asked again says it is still needed: when a reply would ask what the last one asked,
  of the same record, with no refusal to explain it, it says it needs it to go on, offers the
  record's next steps beside it, and code's own reply says so (`AskCopy::again`).
- A question no fact answers is told where its record stands: what it holds and what it still
  needs, from the writer, and what it holds from code's own reply.
- A request only a record of a workflow can do, when none of its records exists, is told there is
  none yet and offered to open one (`AskCopy::open_new`); «yes» takes the offer up.
- The operations understanding is shown are the ones the reduction knows: an act asking what a
  record could do, with none of its workflow loaded, reaches the domain and is never refused as
  unknown.
- A next step whose values are asked when it is taken up is dry-run with its operation's own
  example values filling them, so a step the domain would refuse whatever the values is not
  offered.
- Every next step is to be offered: the writer is told to offer every item, and the review asks
  whether the reply offers every one of them, by their number.
- A question that asks for something to be done is answered with where it stands, from the facts
  and the workflow's guidance, and not declined as unanswerable.
- A question about what can be done that names no record is answered with what the records in
  view hold beside what is on offer: «what is the new flight, and what does it cost?» read as
  what can be done still reaches the quote.
- A card an earlier turn left open is the way forward of the next reply that reaches its record:
  the reply points to it instead of ending on the question to go on
  (`CompositionInput::open_cards`).
- One message that opens a record and acts on it runs both: an act on a record an earlier act of
  the message opens is compiled and validated against the state that opening leaves, when the
  workflow says what its commands leave (`state_after`). Before, it was checked against no state,
  and a domain that needs the record refused it.

**`turnframe-understand`**

- The take-up task (`tasks::take_up`): which offer of the last reply a part of the message takes
  up, whether it declines them («no, that's all»), or none, from a closed list. A part that
  declines is small talk: nothing is reported as not understood.
- A request routed to create a record, whose words hold the whole label of a record of that
  workflow already in view («open Trip 1 and name it …»), is routed once more, told the record
  exists: a second record is not started for it unless the words ask for a new one. On that
  second reading, readings that find no operation may win the vote (`Route::again`), so a doubt
  keeps the record that exists.
- A value in quotes is what the quotes hold even when the mark ending the sentence follows them,
  and without the sentence's comma or full stop written inside them («“Lisbon offsite,”»).
- A full stop after the value's last word or address ends the sentence, whatever the copy keeps:
  «Lisbon offsite.» is «Lisbon offsite», «Trip to Rio.» is «Trip to Rio». Only an initialism
  keeps its own («S.r.l.»), and so does a word whose stop a comma follows («Inc., thanks»).
- One record asked for twice in a message, the same creation with the same values read in two
  parts, is created once.
- Two acts of one message waiting on the same operation and record, whose values agree and
  together complete each other, are one act: a request its parts cut in two.
- A value pointed at in another part's words is that part's only when an act of it uses them
  for something else; an act that only re-reads the words another act asked for is a second
  reading, and goes.
- An answer routed to the act the assistant asked about, whose record is still to create, creates
  it: it no longer targets nothing, and the record it waited for opens once registered.
- A record named in a message that also creates the one record of its kind with no name is that
  record, and gives it the name: it is not looked up, and not registered twice.
- An answer read as giving the text value the assistant asked for, or one its record still
  needs, no value is read once more, told the user's own words are the value even when they are
  also a record's label.
- An act follows the acts of the message it waits on: one waiting on a record created later in
  the message is no longer refused as depending on something not done.
- A creation read twice, the second placed on the first, is the creation once the first reading
  is dropped: it creates its own record and never waits on itself.
- A part that asks for no operation but names a listed record by its whole label («open Trip 1»)
  asks where that record stands: it is answered as a question about that record's state, whatever
  the question's frame read, never routed again, never reported as not understood, and holds
  nothing else the message asks of that record. The reply ends on what the record still needs.
- A request nothing on offer does, about a workflow none of whose records in view offers anything
  now, asks where they stand: nothing can be done on them, and the reply says why, where it used
  to report the request not understood. So does such a request whose only reading was found not
  asked for.
- An answer to what the last reply asked, read only as acts the user did not ask for, is read
  again as the act that asked, and kept only when its own words give the value: an answer routed
  to the previous turn's operation no longer loses the value it gave.
- Words taken for an answer but read as another operation than the one asked for are checked as
  a request for it, not as an answer to a question they do not answer.
- A part whose only reading was found not asked for asks nothing of its record, and no longer
  holds the other acts on it: «rebook her on the quoted flight; the fare difference is fine»
  rebooks, where it used to wait.
- An optional value the check finds the user did not give is left out, and the act goes on
  without it: «add a checked bag for her» adds the bag and no longer asks who pays.
- The check of a value copied from the message is told the words of its part the value leaves
  out, and judges the value shown: «the A is X» read as X is no longer found to take too much.
  It also reads a question whether an operation can be done («can I O?») as asking for it.
- A date's year is the user's only when the words it points at say it: a year taken from today's
  date is none, and the date is placed by its argument's direction.
- A correction keeps what it does not restate: a date corrected without its year takes the year of
  the date it changes, done last turn or stated earlier in the message, and is not read against
  today.

**`turnframe-eval`**

- `simulate`: conversations held by simulated users. A goal in TOML says what the person wants,
  the manners they talk in, the world before and the state that proves it reached; a model plays
  the person; code scores each conversation on reached, turns, dead ends, loops, parts not
  understood, refused acts, offers refused and failed turns. `tests/simulated_users.rs` runs five
  travel desk goals live on demand, on a day of the year they are written for. See
  `docs/evaluation.md`.

**`turnframe-test`**

- The trip sample offers another extra always, and the rebooking of the quoted leg once a quote
  is in, as typed next steps.
- The trip sample lists what a trip can do (`record_operations`), and its open operation says it
  is for a trip not listed yet: opening, showing or going to a listed trip asks nothing of it.
- The trip sample adds an extra with its payer when the message gives one («paid by the
  airline»): `NewExtra::payer`, `AddExtraArgs::payer` and `TripEvent::ExtraAdded::payer`.
- The trip, traveler and claim samples say what a command leaves (`state_after`, from their own
  `apply`), so «open a trip and call it …» opens the trip and names it.
- The trip sample states where its rebooking stands (`rebooking`, once one is quoted), and tells
  the answer that a rebooking sent to the airline can be neither changed nor confirmed until the
  airline replies.

**Examples**

- The console shows the offers beside the reply, as a surface would.

## [0.1.2]

Published: `turnframe-understand`, `turnframe-runtime`, `turnframe-test` and the `turnframe`
facade at 0.1.2. Every other crate stays at 0.1.0. The facade asks for the three at 0.1.2.

### Changed

**`turnframe-runtime`**

- Every reply the runtime writes ends on a way forward: the ask, the card on screen, the next
  steps, or a question to go on (`AskCopy::go_on`, «What would you like to do next?»). A reply of
  answers alone, or of nothing, ends on it; the reply code writes when the writer's is refused
  says what was not done and ends on the ask or the next steps; the writer is told to end on it,
  and its review checks that it does.
- A question is not read as asking for general knowledge when no knowledge source is configured,
  so it is answered from what the records hold and what can be done, never with «the sources were
  not available».

**`turnframe-understand`**

- A value the message itself holds is taken from the message, even when the reading points at an
  earlier message holding the same words: a correction no longer reverts to the value it corrects.
- Small talk read again because a check read an act in it stands when the second reading finds
  nothing on offer for it: nothing is reported as not understood.
- `UnderstandingInput::knowledge` and `with_knowledge`: whether a knowledge source can answer a
  question about the domain in general.

**`turnframe-test`**

- The trip sample offers as next steps only what its view shows can be done: another extra, and no
  longer a rebooking it cannot know is quoted. The scripted passing review answers the new checks.

**Examples**

- The console prints a card that asks without blocking, such as which record was meant, with its
  numbered options, and a number answers it.

## [0.1.1]

Published: `turnframe-understand`, `turnframe-runtime`, `turnframe-test` and the `turnframe`
facade at 0.1.1. Every other crate stays at 0.1.0. The facade asks for the three at 0.1.1, so
moving to `turnframe` 0.1.1 brings their fixes; its crates.io page now opens with the wordmark,
badges and links the repository's README has.

### Added

- `examples/refund-desk`: a shop's refund desk under nine attacks, from a model that reads the
  wrong order to a payment provider that never answers, each run recorded from the runtime for the
  demonstration on turnframe.rs. It brings its own `order` workflow. The example is not published.

### Changed

**`turnframe-understand`**

- An answer to the assistant's question is located on the record the question asked about. When
  the last reply changed one record and asked about another, `locate` is told both apart
  (`Expectation::record`), for a unit that answers the question.
- An act that would ask for a value is verified first, as every mutating act is: when the verdict
  finds nobody asked for it, the part is not understood and nothing is asked. It costs one verify
  call per asking act.
- A record the assistant asked for by a name nothing holds yet is said to be no record, so a
  request to create it is routed to the operation that creates it.
- A repaired segmentation vote no longer shows its repair as the gist of the message.

**`turnframe-runtime`**

- The ask about a record the turn did not reach names that record: in the question code writes
  (`AskCopy::elsewhere`, `«{record}: {question}»`), in a note to the writer, and in a review check.
- An act waiting for a record the user named is let go once a record of that name is in view and
  the turn that brought it left the act undone.

**`turnframe-test`**

- The trip sample's no-change explanation says the name is already there, and asks for no other.

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
