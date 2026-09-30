# The Flow Map

The Flow Map is the architecture Turnframe is built around. It answers the question every turn has
to settle before it does anything: where is each case in its workflow, what is still owed, and what
is the user being asked right now? And it fixes who decides what in a turn: five steps, run in
order, each handing the next only what that step may decide.

```text
persisted state ──1──▶ workflow view ──2──▶ proposed acts ──3──▶ typed commands
                                                                       │
            reply ◀──5── committed events ◀──── execution ◀──4── interactions
```

1. **Persisted state determines the workflow view.**
2. **Small model tasks propose what a message means.**
3. **Reducers decide effects.**
4. **Interactions authorize consequential actions.**
5. **Committed events determine what may be claimed.**

A model takes part in one step of five, and only as a reader. Everything that changes a record, and
everything the reply says about it, is decided by code from persisted data.

## 1. Persisted state determines the workflow view

Each workflow implements a projector, `WorkflowDefinition::project`: the stored record and the
workflow version in, a `WorkflowView` out. The view holds four things that a checkpoint list would
blur together.

- **One lifecycle phase.** Exactly one applies; zero or several is a defect the invariant check
  reports.
- **Open obligations.** What is still owed, several at once if need be, and parameterized: the payer
  of one extra is a different obligation from the payer of another.
- **At most one blocking interaction.** The one decision the user owns right now, so that an
  unqualified «yes» has one meaning.
- **An outcome**, only when the workflow is genuinely finished, as a predicate over persisted or
  authoritative external state.

The projector is pure: synchronous, with no clock, no network and no transcript. The same state and
workflow version always give the same view, so a reload, a second device and a replay of the turn
all see the same step. Everything downstream reads from the view: the operations a turn may offer,
the questions the reply asks (`obligation_sentence`, and `obligation_act` when one act answers a
question), the briefing understanding is given, and what the user may do next.

The test kit explores every reachable state of a workflow and checks the view on each: one phase,
stable obligation identifiers, a blocking card exactly where the user owns the step, and no dead
end. Why the projector may never read anything is recorded in
[ADR-002](adr/ADR-002-flow-map-is-a-pure-workflow-projector.md).

## 2. Small model tasks propose what a message means

Understanding is shown the view and nothing it could act on: readable labels for opaque record
handles, the operations each record offers in its current phase, its open obligations, the card on
screen and what the assistant last asked. A fixed chain of narrow tasks reads the message
(`segment`, `coverage`, `take_up`, `route`, `locate`, `extract`, `verify`), each answering one question under
a strict schema that code checks before the next task runs. Values point at the user's own words,
and dates and amounts are computed by code from what the model points at.

The result is an `Understanding`: units, proposed acts, questions and constraints. It is a proposal
and never an effect. A misreading ends in a question, a held act or nothing done, because the steps
after this one do not trust it. [The architecture guide](architecture.md) walks through each task.

## 3. Reducers decide effects

The whole turn is reduced before anything runs. Opaque handles resolve to authorized cases, and a
mention that fits several records becomes a selection card, never a guess. Corrections replace what
they correct and keep what they do not change; a cancellation wins; «don't confirm anything yet»
holds every submission in the turn. Every act gets an explicit result.

Each surviving act compiles, through the workflow, into a typed command with an expected revision
and an idempotency key. A command aimed at a revision that moved is a conflict, and a command sent
twice has one effect.

## 4. Interactions authorize consequential actions

Each command carries a policy: its risk, and whether it needs a click. A consequential command runs
only with a server-issued origin, such as a confirmed card of the right kind; a model's proposal is
not an origin, and no type in the API could express one. The card is persisted before the reply
mentions it, bound to the revision it was drawn for, and a click on a card drawn before the record
changed is refused as stale. [Persistent interactions](interactions.md) describes the lifecycle.

## 5. Committed events determine what may be claimed

Commands commit their events, and receipts are rendered from those events, never from a model's
prose. The reply may say something happened only when a committed event backs it: a claim guard
refuses a reply that claims more, and an external call whose outcome is unknown stays unknown until
it is reconciled. After the commit the changed cases are projected again, and the loop closes:
the next turn starts from the new persisted state. [What the reply may say](composition.md) holds
the rules.

## A workflow is a graph

Read the other way round, the Flow Map is a map in the ordinary sense: each workflow is a directed
graph. Its phases are the nodes, the operations each view offers are the edges out of a node, and a
case is always on exactly one node. This is the sample trip workflow of the travel desk:

```text
(no case) ── open ──▶ Collecting      edits: name, date, extras, payers, traveler
                         │
                         │ request_rebooking, once nothing is owed
                         ▼
                   Awaiting confirmation      [card: Rebook · Keep my flight]
                         │                     any edit ──▶ back to Collecting
                         │ a click on the card drawn for this revision
                         ▼
                   Dispatching ── refused ──▶ Refused ── again ──▶ Awaiting
                         │
                         │ accepted
                         ▼
                   Ticketed ── delivered ──▶ Notified
                         └── not delivered ──▶ Not notified

withdraw: Collecting, Awaiting confirmation or Refused ──▶ Withdrawn
```

Each step of the Flow Map is a statement about this graph.

- **The projector says which node the case is on.** It reads the stored record, never the
  conversation, so the case cannot be on two nodes, or on a node the record does not support.
- **Understanding can only name an edge that leaves that node.** The operations on offer are the
  outgoing edges, and they are the closed list a model chooses from. An edge may carry a condition
  the domain checks: asked to rebook a trip that still owes a payer, the domain refuses and says
  why, and the case stays where it is.
- **Some edges are guarded by a card.** From `Awaiting confirmation`, the edge to `Dispatching` has
  no operation a model could propose: only a click on the card drawn for the case's current revision
  takes it, and any edit before the click moves the case back to `Collecting`, which takes the card
  down.
- **Some edges belong to someone else.** From `Dispatching` the airline decides, and its answer is
  recorded as an event when it arrives. Until then the case stays where it is, and the reply says
  the request was sent, not confirmed.
- **The event ledger is the path the case has taken,** and the reply may describe only edges on it.

Because a workflow is a finite graph, it can be walked. The test kit explores it breadth-first from
the empty case, projects every node it reaches, and checks the view on each: one phase, stable
obligations, a card exactly where the user owns the step, and no node without a way out except the
finished ones.

## What a workflow author writes

A domain supplies the projector and what hangs off its view: the operations each view offers, how
an act compiles into commands, each command's policy and validation, and how its events read as
receipts. Execution is a trait of its own, so a workflow may update rows, call a service or keep an
event-sourced aggregate. The [recipes](recipes.md) show the shapes, and the sample travel desk in
`turnframe-test` implements all of them.

The guarantees this buys are listed, property by property, in the
[reliability model](reliability-model.md).
