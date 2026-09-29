# Turnframe architecture guide

Turnframe implements the **Flow Map** architecture: a way to build a conversational application in
which language stays flexible and business effects stay deterministic.

The whole design follows from one sentence.

> The model proposes meaning. Deterministic code decides effects. Committed events decide claims.

This guide explains the pipeline, the invariants that hold it together, the crate map, and the
extension points. The normative statements live in the [architecture decision records](adr/README.md);
this document is the map that ties them together. Where a rule is enforced by code, the responsible
module is named.

---

## 1. Why a conversational application needs this

A conversational assistant that can change real records has to reconcile two things that pull in
opposite directions.

People write like people. One message can confirm a card, correct an earlier instruction, add two
pieces of information the assistant never asked for, ask a question, and forbid a submission. The
assistant should absorb all of that without turning into a one-field-at-a-time wizard.

Business effects have to be exact. The wrong trip must never be modified. A message saying "sent"
must not appear when nothing was sent. A double click must not submit twice. A stale confirmation
must not apply to a record that changed since the card was drawn.

The usual answer, giving a language model a set of write tools and letting it call them, makes the
first requirement easy and the second impossible: the same component that guesses what the user
meant also decides what happens and then narrates what happened. Turnframe splits those three
responsibilities into three different layers with different trust levels.

| Layer | Trust | Responsibility |
|-------|-------|----------------|
| Understanding | untrusted | small model tasks turn language into a structured *proposal* |
| Reduction and execution | authoritative | decide effects, run typed commands |
| Composition | mixed | deterministic receipts, notices and cards; model-authored prose around them, reviewed |

---

## 2. The turn pipeline

Every turn runs the same sequence. The steps are traceable individually, and the replay record
stores the inputs and outputs of each one.

```text
persisted state
  → deterministic workflow projection      (pure, no I/O)
  → untrusted language understanding       (small model tasks, each checked by code)
  → deterministic target resolution        (opaque tokens → authorized cases)
  → deterministic whole-turn reduction     (corrections, constraints, precedence)
  → typed commands and policy checks       (risk, confirmation, origin)
  → transactional commit or external saga
  → domain events and authoritative receipts
  → deterministic response plan
  → the reply: an acknowledgement and answers, written by small reviewed tasks
```

In detail, as the runtime executes it:

1. **Accept the input.** A turn carries text, an interaction response, attachments and an origin
   reference. Text and a card click may coexist; this is a requirement, not an accident. A
   click-only turn needs no model call at all.
2. **Load and project.** The application's persisted state is loaded for every relevant case, and
   each case is projected into a workflow view: exactly one lifecycle phase, zero or more
   parameterized obligations, at most one blocking interaction requirement, informational notices,
   and an outcome only when the workflow is genuinely finished. Projection is a pure function of the
   state snapshot and the workflow version.
3. **Build the briefs.** Each task is shown what it needs and no more: opaque record handles with
   readable labels, the operations the active workflows currently offer, each record's phase and
   open obligations, the card on screen, what the assistant last asked for, and the task's own
   output schema. It never sees write tools or database identifiers.

   A case may also carry a **briefing**: prose the workflow returns for the view it is in, telling
   the understanding how to read a message about a case in that situation. It is the only thing in
   a brief that is instructions and not data, and the only channel a workflow has for guidance that
   is specific to a phase. Because it is a function of the view, a briefing varies exactly as far as
   the projection does (by phase, by which obligations are open, by whether a card is blocking), and
   two cases in the same situation get the same text, which is what makes the text reviewable. It
   is server-authored copy and must never interpolate anything a user wrote. How much of it travels
   is the deployment's number (`BriefingBudget`), and a cut is marked, never silent.

   A choice the runtime cannot honour is **removed** from the schema instead of argued against in
   prose. An operation is one of the keys on offer, a record is one of the handles in view, a card
   answer is one of that card's options. A model that names something unofferable is answering off
   contract and gets a repair round, and never produces a reading that validates, resolves to
   nothing and silently does less than it said.
4. **Understand.** A fixed chain of small tasks reads the message: `segment` splits it into
   units, `coverage` checks nothing was missed, `route` chooses each request's operations, `locate`
   its record when several could be meant, `extract` each argument, and `verify` checks the act
   against the user's words. Questions are framed on their record and topic. Each step is streamed
   as it is decided. See §5.
5. **Check what the domain would refuse.** Before anything is reduced, the workflow checks each act
   as it would compile it. A value it refuses goes back to extraction once with the domain's own
   sentence, then is asked of the user.
6. **Look up what was named.** A record the user named that is not in view, as a target or as an
   argument, is looked up with `CaseDirectory::find`. One record found is used; several are offered
   on a selection card, or asked for again.
7. **Resolve targets.** Opaque tokens map, server-side and account-scoped, to authorized case
   references. A mention that matches several candidates is ambiguous, and ambiguity never resolves
   to the newest, first, or most plausible candidate.
8. **Reduce the whole turn.** Corrections supersede earlier acts, cancellations win, constraints
   block whole classes of act, hypothetical questions stay questions, and every act receives an
   explicit result. Nothing has executed yet.
9. **Apply policy.** Each compiled command carries a risk class and a confirmation policy.
   Consequential commands need a server-issued origin such as a confirmed interaction bound to the
   right kind of card; a model proposal is not a valid origin, and there is no enum variant that
   could express one.
10. **Persist required interactions, then execute.** Cards that must exist before execution are
    persisted first. Eligible command batches then run with an expected revision and an idempotency
    key, grouped by atomicity scope.
11. **Commit events.** State, events, interaction resolution, the command journal and outbox rows
    commit together where one database allows it. External effects go through an outbox and a saga.
12. **Re-project and compose.** Changed cases are projected again, new required interactions are
    derived and persisted, receipts are generated from committed events, each question gets its own
    answer on an explicit state basis, and the acknowledgement is written from the turn's outcome and
    asks the one thing the work needs next. The reply is an ordered list of typed blocks, persisted
    exactly as returned.

---

## 3. The invariants

The spec states twenty invariants. They are the reason the pipeline has the shape it has. Each one
below names where it lives.

| # | Invariant | Enforced by |
|---|-----------|-------------|
| I1 | Persisted state is authoritative; the transcript is history | projection takes a state snapshot, never a transcript |
| I2 | Projection is pure | `WorkflowDefinition::project` is synchronous and I/O-free |
| I3 | Exactly one lifecycle phase per case | the view holds one phase by type |
| I4 | Zero or more parameterized obligations | obligations carry entity identifiers, so repeated work is representable |
| I5 | At most one blocking interaction per case | the interaction store rejects a second active blocking card |
| I6 | A user-owned phase has a real persisted interaction | the view invariant check, plus persistence before the reply mentions the card |
| I7 | Client input cannot define call-to-action semantics | the client sends identifiers; the server reads the stored option's action |
| I8 | An ambiguous target means no mutation | resolution returns candidates, and the reducer answers with a selection card |
| I9 | Model output is a proposal | every task answer is schema-checked, values point at the user's words, targets and policy are checked before commands exist |
| I10 | Whole-turn planning precedes effects | reduction completes before the first command runs |
| I11 | Every act receives a result | the reduction plan is validated against the act count |
| I12 | Critical command origins are trusted | the origin type has no model-proposal variant, and the policy check gates on the specific confirmation |
| I13 | Optimistic concurrency is mandatory | every command targets an expected revision; a mismatch is a conflict |
| I14 | Idempotency is mandatory | every command carries a derived key; the journal returns the original outcome |
| I15 | External uncertainty is explicit | an unknown outcome is a first-class state, distinct from failure |
| I16 | Events authorize claims | receipts cite committed event identifiers; a claim guard rejects unbacked claims |
| I17 | Provider failure cannot repeat effects | fallback is allowed before commit or for the reply after it, never in between |
| I18 | Model answers are all-or-nothing | structured parsing rejects a task's whole answer |
| I19 | Critical state reads fail closed | an unverifiable ownership, revision or confirmation blocks execution |
| I20 | Replay is possible | the replay record stores versions, hashes, resolutions, decisions and outcomes |

Reliability is measured against these separately, never as one accuracy number. See the
[reliability model](reliability-model.md).

---

## 4. Flow Map: projection, not execution

[The Flow Map](flow-map.md) is the five-step order every turn follows; its first step is a
projector. Given a workflow version and a state snapshot it returns a view; it
executes nothing and reads nothing.

The view separates four concepts that a naive checkpoint list conflates.

- **Phase** is where the case is in its lifecycle. Exactly one applies. Zero or several is a defect
  in the map, and the invariant checker says so.
- **Obligations** are what is still open, possibly several at once, and possibly parameterized:
  the payer of extra seven is a different obligation from the payer of extra two, which is why a
  static checkpoint list cannot model a trip with several extras.
- **The blocking interaction requirement** is the one decision the user owns right now. At most one
  per case, because an unqualified "yes" must have exactly one meaning.
- **The outcome** exists only when the workflow is genuinely complete, and completion is a predicate
  over persisted or authoritative external state, never a sentence in the transcript. A complete
  case still has a state: an outcome is something the case *carries*, not something it becomes by
  ceasing to exist (§13).

A domain author implements the projector, the operations each view offers, act compilation, the
command policy, command validation and receipt rendering. Execution is a separate trait, because some applications
update rows, some call services and some use event-sourced aggregates. Turnframe requires an event
journal for claims, but it does not require event sourcing for state.

The registry holds several workflows at once. Domain authors write typed code; the runtime talks to
an erased interface and only serializes at that boundary. Untyped JSON is never the primary domain
API.

Projection behaviour is versioned. When the meaning of a projection changes, the workflow version
changes with it, so a replayed turn is read the way it was when it ran.

Fixture coverage alone does not prove a projector. The test kit explores reachable states in
breadth-first order under configurable limits and asserts the invariants over every state it finds:
one phase, unique and stable obligation identifiers, a blocking interaction exactly where the user
owns the step, terminal outcomes where they are expected, no case that ends by disappearing, and no
dead end without an explicit user, system or external trigger.

---

## 5. Understanding is a proposal

Understanding emits semantic acts, not tool calls. An act says what the user appears to want in the
vocabulary of the active workflows: apply this operation to that record, start this workflow, answer
the card on screen. Beside the acts it records questions with an answer basis and a topic, and
constraints such as "do not submit anything yet".

A message is not read by one large prompt. Each task answers one narrow question under a strict
schema built for the turn in hand, and code checks every answer before the next task sees it:

| Task | Question | What code checks |
|------|----------|------------------|
| `segment` | which units the message holds | every unit points at words the message has, and no two share a word |
| `coverage` | whether a request or question was missed | a found unit must lie in words no unit holds, or in words read as small talk or a dispute, which it may turn into a question; read there as an act, the words go back to segmentation once; a second reading of small talk runs nothing and is reported unclear, and a second reading of a dispute stands; a question right after a question is its tail; a constraint in words no unit holds sends the segmentation back once, then fails the turn closed; a lone word between two parts joins them, and is no constraint or request of its own |
| `route` | which offered operations a request asks for, in order | each answer is one of the keys on offer, or none alone; one asked for twice is listed twice |
| `locate` | which record it is about | the answer is a handle in view, a new record, or one the user named |
| `extract` | each argument's value | the value points at the user's words, and a text value copies them; dates and amounts are computed by code |
| `verify` | whether the act matches what was said | the verdict names each argument it doubts, and why |
| `question_frame` | which record and subjects a question is about | handles and subjects come from the brief |
| `cross_check` | whether the whole reading says what the message says (`high` effort only) | each finding names an act and argument the reading has, or words nothing holds |

**The prompts know no domain.** A task's instructions state rules for every workflow and name
nothing of one. What a model needs to know about a domain reaches it from that domain's
configuration: operation summaries, argument labels and descriptions, examples, a glossary and a
record's briefing. A summary may be written per language (`OperationSpec::summary_in`), and a turn
reads the one in its language. `no_core_prompt_names_a_sample_domain` in `crates/turnframe-test/tests/` fails
when a prompt names a sample domain's workflows, fields, values or records.

**Evidence is a pointer.** A value is the words the user wrote: the model copies them and points
at them by word number, and code keeps the copy only when the words it points at repeat it. A date
or an amount is given as the user said it and evaluated by code against today. When the verifier
says a value takes too much or too little, extraction runs again with that exact finding. A value
is its part's when it lies in the part's own words, the words it continues, or words no other part
holds next to them; one pointed at in another part's words is read again once, told whose words
they are. In a correction, a value read in the correction's own words is the user's last word.

**All-or-nothing, one task at a time.** A task answer that breaks its schema or its check goes back
once with the exact error; a call the provider refused, filtered or dropped is sent again in place.
A task can also cast several votes or escalate to a stronger model, per its profile. Only an answer
that passes is used, and a unit that cannot be read reaches the user as a notice that says so.

**What waits, waits across turns.** A reply records what it asked for, and the next message is
read as its answer. An answer that gives none of what was asked is routed once more, told so: it
may take up something the reply offered instead. An act left waiting for a record the user named that nobody has registered is
also carried from reply to reply, the newest three, until it is done: when a later message
registers a record of that workflow under the name the user gave, or under none, code completes
the act in the same turn and the new record takes that name. A record registered under another
name completes nothing.

**A closed catalog.** The model may only choose operations that active workflows and policy have
registered for this actor, in this phase. It cannot invent an operation, and it cannot invent a
record identifier, because it never sees one.

**Budgets and records.** Every task runs under the turn's budget of calls, tokens, depth and time,
and each call is recorded with the prompt it ran under, the model that served it and its verdict,
so the replay record says how the turn was read. The steps stream as `TurnEvent::Step`, and with
`NarrationConfig::steps` each is also said in the user's language for a preview.

**Effort.** A turn runs at `low`, `medium` (the default) or `high`, from `OrchestratorConfig::effort`
or forced by the application in `TurnInput::effort` (ADR-020). `medium` reads how a message is
split and routed three times, and a split vote is read once more, shown the answers that
disagreed; a verdict of verify that finds fault is voted on twice more, and the majority decides. `high` votes on segmentation,
routing and verification, gives the reading tasks but extraction some reasoning (with it, a mini model copied the words
naming a field into the value), and runs `cross_check`: each finding sends one step of one act back once, and the
verifier decides; the check alone holds nothing. `low` drops the reply review and the step prose
and keeps every understanding task. At every level the verifier reasons a little before it judges:
at minimal reasoning it misjudged multi-part messages, and it is the one check between a reading and
a command. No level changes what policy, cards or the claim guard allow.

The default mode reads and proposes; reduction, policy, typed commands and events decide. A
sandboxed autonomous mode exists for reversible domains and is refused for regulated, irreversible,
destructive, monetary, credential and signature operations.

---

## 6. Reduction is where decisions happen

The reducer is the deterministic core. It takes the input, the untrusted understanding and a context of
views, catalogs and policies, and it returns an execution plan with no side effects at all.

It resolves the entire turn before anything runs, which is what makes "change the address, actually
leave it as it was" produce no mutation instead of a mutation followed by an apology. The default
precedence rules are explicit: a cancellation supersedes the act it cancels; a correction supersedes
the act it corrects, as the understanding links them (two acts it leaves unlinked are two acts, and
the reducer never guesses a correction from a repeated operation); "do not submit" blocks every
submission in the turn; a hypothetical question never becomes an action; an ambiguous target blocks
only the acts that depend on it while independent acts and questions proceed; a structured card
response binds more strongly than a free-text answer inferred by a model; and a high-risk
confirmation cannot be resolved from inferred text at all.

Every act comes out with exactly one result: ready to execute, awaiting confirmation, needing
clarification, rejected with a code, superseded by a correction, or no change. Nothing disappears
silently, and a partial result is stated as a partial result rather than hidden behind a generic
success.

Commands are grouped by atomicity scope. Mutations to the same case default to committing together,
so a message that sets three fields either applies as a unit or not at all, unless the domain
deliberately models partial application.

---

## 7. Commands, policy and origins

Turnframe pushes domains toward specific commands rather than a generic field setter, because policy
and risk attach to meaning: rebooking a flight is not the same kind of act as setting a trip's
name, even though both write a row.

Each command carries an envelope: who, which case, which expected revision, which idempotency key,
and which origin. The origin is the load-bearing part. It can be a direct low-risk user act, a
confirmed interaction bound to a specific card, an internal policy decision, or a verified external
callback. There is deliberately no variant for "the model asked for it", so no amount of downstream
code can construct one.

The policy of a command combines a risk class, a confirmation requirement, an atomicity scope and a
claim mode. An unknown command is treated conservatively by default. The confirmation check is
specific rather than generic: a card that asked "which trip did you mean?" does not authorize an
irreversible submission, and a requirement for professional review is not satisfied by the user's
own click.

Concurrency and repetition are handled at the same layer. Every mutable case has a revision, and
every commit checks the expected revision in the same statement, so a stale write becomes a conflict
instead of an overwrite. Every command carries a derived idempotency key, and a repeated delivery
returns the original outcome rather than repeating the effect.

---

## 8. Interactions are durable objects

Cards, confirmations, reviews and choices are persisted records, not ephemeral model output. That is
what makes them safe to click twice, safe to reload, and safe to reason about after a crash.

An interaction stores its payload immutably, hashes it, binds itself to a case revision unless it
explicitly declares independence, and owns the semantics of each option server-side. The client
sends an interaction identifier, an option identifier, an expected revision and optionally a
free-form value; it can never send an action. A second click on a resolved card returns the original
resolution. A card belonging to another account or conversation is indistinguishable from one that
does not exist. A card whose case has since changed is stale and cannot execute. A card is only
resolved once the command it authorized has committed; if execution fails, the card does not show a
successful receipt.

The full lifecycle, including the status machine, is documented in the
[interactions guide](interactions.md).

---

## 9. Claims come from events

The last mile is where conversational systems usually lie. A model that was told "the submission was
requested" will happily write "your trip has been sent".

Turnframe separates authorship. The server owns whether something happened, which fields changed,
the command status, the external submission status, the availability of a card, the labels and
meanings of calls to action, revision warnings, artifacts and external references. The model owns
answers to questions, acknowledgement, transitions, explanations of validation errors, and summaries
of proposed changes backed by a deterministic diff.

Receipts are generated from committed events and rendered by the domain in the user's locale. The
reply may introduce a receipt, but it cannot replace one, and a claim guard checks structurally that
no operational outcome is asserted without a receipt that cites event identifiers.

The words around the blocks are written by three small tasks. `Acknowledge` writes one to three
sentences from the turn's outcome, gathered by code: what was done (each receipt's own words), what
was not and why (each refusal's own sentence), and the one thing to ask next, which code chooses
from the first value an act is waiting for or the first open obligation of the record the turn is
on. It is told what is already on screen, so it does not repeat it, and it is never shown the words
of a refused request or of a question answered elsewhere. `Review` checks it with a yes-or-no
checklist of only the checks that apply; a reply failing twice is dropped and the question code
wrote stands in. `Answer` writes one block per question from that question's facts. See
[composition](composition.md).

External statuses are never collapsed into "done". Prepared, validated, awaiting confirmation,
submitted, received by an intermediary, received by the airline, accepted, rejected, issued, not
delivered, delivered and completed are different states, and a timeout after transmission is an
explicit unknown outcome that triggers reconciliation rather than a blind retry.

The reply itself is an ordered list of typed blocks: answers, transitions, receipts, notices,
interactions, artifacts. The same blocks are persisted and returned on reload, so a reloaded
conversation is not reconstructed from prose. Streaming publishes phases and understanding steps as
they happen, and blocks only once the turn has committed; a reviewed reply arrives whole.

---

## 10. Providers are replaceable

The runtime depends on normalized capabilities and responses, never on a vendor's wire format.

Each provider and model pair is a profile with declared capabilities, because capability is a
property of the combination, not of the brand. A task declares what it needs: understanding requires
genuine structured output, and prompt-only JSON is rejected by default, never silently accepted. If no candidate can meet the requirement, the operation fails
loudly instead of degrading quietly.

Fallback is bounded by commit. Before any command executes, trying another provider is safe. After
commit, another provider may only write the reply, because the events already decide the claims.
Rerunning understanding and execution after effects may have committed is never allowed.

Every adapter passes the same conformance suite: valid structured responses, malformed JSON, unknown
and missing fields, multiple acts, tool call identifiers, streaming reconstruction, empty output,
refusal, timeout, rate limiting, authentication failure, context overflow, cancellation, retry
classification, secret redaction and the absence of silent capability downgrade. The published
compatibility table is generated from those results rather than asserted. See the
[provider adapters guide](provider-adapters.md).

---

## 11. Persistence is pluggable

Turnframe defines persistence as traits: conversations and their typed blocks, interactions with
compare-and-set resolution, a command journal keyed by idempotency, an event journal that serves as
the claim ledger, an outbox for external effects, and replay records.

An adopter may implement those traits over whatever database they already run, and prove the
implementation with the conformance suite that ships with the traits. The PostgreSQL implementation
is a reference, not a requirement: it exists so there is a working answer for teams that want one,
and so the transactional idioms the invariants need are demonstrated concretely. The deterministic
in-memory implementation is what the tests use, and it can inject failures at each boundary so crash
behaviour is tested rather than assumed.

Snapshot storage is allowed. The event journal is mandatory, because claims depend on it.

---

## 12. Crate map

```text
turnframe                    facade; feature flags select providers, store and telemetry
├── turnframe-core           pure types, the Flow Map projector, invariant checks
├── turnframe-tasks          small verified model tasks: repairs, retries, votes, budgets, records
├── turnframe-understand     the understanding pipeline over those tasks
├── turnframe-runtime        the turn pipeline: reduce, execute, compose the reply, trace, replay
├── turnframe-store          persistence traits, in-memory store, store conformance suite
│   └── turnframe-store-postgres   reference implementation with migrations
├── turnframe-provider       provider-neutral model layer, routing, adapter conformance suite
│   ├── turnframe-provider-openai      OpenAI, Azure OpenAI, OpenAI-compatible endpoints
│   ├── turnframe-provider-anthropic
│   ├── turnframe-provider-gemini
│   ├── turnframe-provider-bedrock
│   └── turnframe-provider-ollama
├── turnframe-test           fixtures, scripted providers and tasks, sample domains, exploration
├── turnframe-eval           evaluation harness: samples, assertions, per-task scores, judge votes
├── turnframe-telemetry      tracing spans, metrics, optional OpenTelemetry bridge
└── turnframe-macros         reserved; no macros ship in the 0.1 series
```

The dependency rules are strict. The core crate depends on no provider, database, web framework or
application crate; it has no async runtime and no HTTP client. Provider adapters depend on the
provider abstraction and the shared core types, never on runtime internals. Applications depend on
core and runtime and implement the domain traits. Feature flags select optional integrations; they
never create materially different safety semantics.

---

## 13. What an adopter writes

Building an application means implementing a small number of things and leaving the rest to the
framework.

- A **state type** and a **projector** for each workflow, plus the phase, obligation, command, event
  and outcome types.
- The **operations** each view offers, with argument schemas, so understanding has a closed
  vocabulary, and optionally a sentence for each obligation, which the reply asks as its question.
- **Act compilation** from a resolved act to typed commands, and **command validation**.
- A **command policy** per command, which is where risk and confirmation are declared.
- An **executor** that applies commands to your storage with an expected revision and returns
  committed events.
- **Receipt rendering** from events into localized operational copy.
- A **case directory** that lists the records an actor may address, and finds one by the words it
  was named by.
- Optionally: a knowledge provider for answers, a prompt source, a custom router, and a store
  implementation.

Everything else (understanding and its checks, target resolution, precedence rules, the interaction
lifecycle, idempotency, the claim guard, the reply, the ordered blocks and the replay record) is the
framework's job.

**A case's identity outlives its content.** Removal is a status, never an absence. A workflow whose
working document is *consumed* on success is an ordinary shape (a draft that becomes a record, an
application that becomes an account, a cart that becomes an order), and the tempting implementation
is to delete the row the moment the work succeeds. Do not. Move the case to a terminal status
instead, the way the traveler sample moves it to `Deleted` and keeps the row; the record the
workflow produced is a different thing, with its own identity and usually its own workflow.

The reason is that the executor has one way to say "there is no state here" and two opposite
questions to answer with it. `WorkflowExecutor::load` returns `None` for a case nobody has created,
and the case revision is then zero; a completed case whose row was deleted returns exactly the same
`None` at exactly the same revision, and the contract forbids the obvious workaround of leaving the
revision where it was. **An absent state therefore means *not yet*, and never *no longer*.** The
projector that reads absence as completion and the one that reads it as a fresh start cannot both
be right, and nothing downstream can tell them apart: either projection is legal, the invariant
checker is satisfied by both, and no error is raised anywhere. What the user gets is an assistant
that congratulates them on finishing and then asks them to start over.

After that rule an absent state has exactly one remaining use: **a case that has been named but not
created yet.** The application's case directory offers such a case to a turn, and the first command
that commits is what brings it into existence; until then there is nothing to load. That is the
state a projector describes as a pre-draft phase: the obligations the case will need, no outcome,
and no blocking card pretending work is already under way. Every other use of it is a defect, and a
type whose only valid use is unstated invites the invalid ones.

The rule is enforced rather than merely written down. The state explorer in `turnframe-test` walks
every reachable state anyway, so it reports a projector that gives an absent state a terminal phase
or an outcome (`CaseEndsByDisappearing`), and a transition model that answers a command by dropping
the case instead of moving it to a status (`TransitionRemovesCase`). They are the same defect seen
from the projector and from the model, and both are a test failure now instead of a production one.

Some shapes look at first like something the framework should grow a type for, and are better
written as one of these small workflows. A legal acceptance is the clearest example: it has a
versioned artifact, a retention requirement and the power to gate other work, and it still fits
the existing types without an interaction kind of its own. The
[consent and acceptance guide](consent-and-acceptance.md) is the recipe, with a test behind it.

---

## 14. Failure behaviour

Failures are explicit and typed rather than collapsed into a generic error. Each one is classified
by whether it can be retried, whether an effect may already have happened, which user-facing message
key applies, how severe it is operationally, and whether it requires reconciliation.

Crash recovery is driven by a persisted turn phase. If nothing was journaled, understanding can
safely restart. If commands are pending, they resume by idempotency key. If they committed, the
response is regenerated from events and stored answer tasks without re-executing anything. If an
external outcome is unknown, the system reconciles instead of retrying blindly.

The test suite injects failures at each of those boundaries, because a crash-recovery story that has
never been executed is a hypothesis.

---

## 15. Where to go next

- [Reliability model](reliability-model.md): what is guaranteed by construction, what is
  probabilistic, and how each is measured.
- [Composition](composition.md): what the reply may say, and how it is written.
- [Evaluation](evaluation.md): measuring a model against the contract, per turn and per task.
- [Architecture decision records](adr/README.md): the normative decisions, one per boundary.
- [Interactions](interactions.md) and [provider adapters](provider-adapters.md): the two extension
  surfaces most adopters touch first.
- [Threat model](threat-model.md): trust boundaries and what contains each attacker capability.
- [Adopting over a database with no revision column](../crates/turnframe-store-postgres/docs/revision-migration.md):
  the recipe for putting a revision on tables that never had one, with a test that executes the
  statements it prints.
- [Consent and acceptance](consent-and-acceptance.md): modelling a legal acceptance without a new
  interaction kind.
- [Canary and rollback](canary-and-rollback.md): releasing one workflow at a time, and undoing it.
- [Release checklist](release-checklist.md): the gates that decide when this is production-ready.
