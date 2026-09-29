# The hostile refund demo

A second demonstration for turnframe.rs, beside the travel desk replay. A visitor picks one way to
break a refund and watches the turn not move the money. It is for somebody who will not read the
architecture: the failure has to be understood in a glance, and the guard that stops it has to be
visible where it acts.

Every frame the page shows is recorded from the real runtime. Only the model is scripted, wrong on
purpose where the attack says so, and the page says which parts are scripted.

## What the visitor sees

A home section, numbered 01, after the manifesto: «Break it», «Nine ways to break a refund.»,
«None of them moves the money.» Controls on the left, one attack at a time; on the right the
message, then five stations in a column:

1. **Reading**: what the model proposed the message means (scripted).
2. **Proposal**: the act as understanding hands it over: operation, record, values.
3. **Reducer**: target resolution, domain checks, policy, what each act became.
4. **Decision**: blocked, a card, or a commit; a card is drawn as the real card, with its click.
5. **Ledger**: committed events with their revisions, receipts, the outbox. An empty ledger says so.

Picking an attack replays its frames station by station. The station that stops it carries a
«Stopped here» stamp. The run ends on one verdict computed from the recording: «Nothing moved»,
«Waiting on a click», or «One refund, recorded once». Each attack has its own link,
`/#break-<id>`.

## The refund desk

A shop's support desk, defined inside the new example `examples/refund-desk`, not in the test kit:
the travel sample, the live corpus and the benchmarks do not move.

The agent's account lists two orders. A third exists in another account and is never listed.

| Order | Customer | Paid | Delivered | Listed |
|---|---|---|---|---|
| 381 | Giulia Neri | €129.00 by card | 12 days ago | yes |
| 318 | Luca Moretti | €189.00 by card | 20 days ago | yes |
| 402 | (another shop) | €129.00 | 5 days ago | no |

### The `order` workflow

State: number, customer, amount paid, amount refunded, delivery date, the pending refund (amount
requested), the external status of a refund sent, and the provider's reference once it answers.

| Operation | Arguments | Notes |
|---|---|---|
| `order.request_refund` | `amount` (money) | proposable; records the request and raises the card |
| `order.refund` | none | card-only: the click sends the pending refund to the payment provider |
| `order.decline_refund` | none | card-only: the other button; drops the pending request |

Rules, all in the domain's pure transition:

- A refund cannot exceed what was paid minus what was already refunded.
- A refund is possible within 30 days of delivery.
- One pending refund at a time; a refund already sent is not requested again.
- The card's amount is what the request asked, capped at what is left to refund; when the cap
  applies, the card shows the change as a before and after entry.

Policy: `order.refund` is `ExternalRegulated`, `ExplicitClick`, an external saga through the
outbox, `ServerReceiptOnly`. The payment provider is an `OutboxSender` the example scripts per run:
it accepts, times out after the send, or delivers its answer twice. Eligibility is a question
answered from the order's state.

Narration is off, so every line of every reply is the server's.

## The ten runs

The message is «Refund order 381 for €129» unless the row says otherwise.

| Id | Group | Attack | Stopped at | Verdict |
|---|---|---|---|---|
| `no-attack` | none | none | none | the card, the click, sent, accepted: one refund, recorded once |
| `wrong-order` | the model | reads order 318 | decision | the card names order 318 and Luca Moretti; the scripted user declines; nothing moved |
| `wrong-amount` | the model | reads €1,290 | reducer | refused by the domain: more than was paid; no card; nothing moved |
| `unseen-order` | the model | «Refund order 402 for €129»: another shop's order, read as written | reducer | the model has no id to give; the desk's directory finds no such order; nothing moved |
| `take-back` | the user | «Refund order 381 for €129. Actually don't refund it yet, just tell me whether it's eligible.» | reading | the refund is superseded in the same message; the question is answered from the order's state, in the scripted model's words; nothing moved |
| `double-click` | the user | clicks Refund twice | decision | the second click is already done; one row in the outbox; one refund, recorded once |
| `sent-twice` | the user | the app resends the same turn | decision | the second copy is refused whole: a turn with this id is already on record; one card, then one refund |
| `stale-card` | the world | a colleague refunds €30 while the card is open | decision | the click is bound to the old revision and is refused whole; asked again, the desk draws a new card showing €129.00 → €99.00, waiting for a click |
| `timeout` | the world | the payment provider takes the refund and never answers | ledger | `OutcomeUnknown`; the reply says sent, not confirmed; never retried; reconciled: the answer, delivered twice, is recorded once |
| `model-down` | the world | the model provider fails halfway through the reading | reading | the part it failed on is not understood; no act; nothing moved |

How the two that need more than a scripted reading are made:

- `sent-twice` runs `handle_turn` twice with the same turn input. The runtime refuses the second
  copy whole: the turn's id is already on record (`StoreError::Conflict`).
- `take-back` is the one run that narrates: only a model writes an answer to a question, so its
  scripted provider words the answer from the facts the order gives.
- `model-down` is the one run that uses the real understanding pipeline: a `ScriptedTasks`
  provider answers segmentation and routing, and fails the extraction with a timeout. Everything
  after it is the pipeline's own handling.

## The recorder

`examples/refund-desk`, a workspace member with `publish = false`, like `travel-desk`.

- `cargo run -p refund-desk` prints the ten runs for a person, in the travel desk's style.
- `cargo run -p refund-desk -- --record <path>` writes the recording as JSON.

Each run seeds fresh in-memory stores (`FakeStores`, `InMemoryExecutor` over the example's
`PureWorkflow`), a directory scoped to the desk's account, fixed turn ids and a fixed clock. The
understanding of each message comes from `UnderstandingBuilder`, except in `model-down`.

The frames are built from what the runtime returns and records, never written per run: the
understanding, the reduction plan (each act's result and target resolution), the policy
decisions, the interactions created, invalidated and resolved, the command outcomes with their
revisions, the outbox rows, and the response blocks. The only authored text per run is the
attack's one-line description and, for world events, the line saying what happened (a colleague's
refund, a provider that does not answer). The verdict is computed from the ledger and the open
cards.

Output is stable by construction. Any identifier the runtime mints that is not stable across runs
is replaced by a short name in order of appearance (`evt_1`, `card_1`).

### The recording

```json
{
  "recorded_with": "cargo run -p refund-desk -- --record",
  "version": "0.1.0",
  "orders": [{ "label": "Order 381", "customer": "Giulia Neri", "paid": "€129.00", "delivered": "12 days ago" }],
  "runs": [
    {
      "id": "wrong-order",
      "group": "model",
      "label": "Picks the wrong order",
      "attack": "The model reads order 318.",
      "message": "Refund order 381 for €129",
      "stopped_at": "decision",
      "verdict": { "kind": "nothing_moved", "text": "Nothing moved" },
      "frames": [
        { "station": "reading", "kind": "act", "scripted": true, "text": "order.request_refund on Order 318, amount €129.00" },
        { "station": "reducer", "kind": "result", "state": "held", "text": "…" },
        { "station": "decision", "kind": "card", "id": "card_1", "title": "…", "body": "…", "options": ["Refund", "Don't refund"] },
        { "station": "decision", "kind": "click", "option": 1 },
        { "station": "ledger", "kind": "empty", "text": "No event. Nothing moved." }
      ]
    }
  ]
}
```

Frame kinds: `message`, `act`, `result`, `policy`, `world`, `card`, `click`, `notice`, `event`,
`receipt`, `outbox`, `reply`, `empty`. `state` is one of `proposed`, `decided`, `held`,
`committed`, `persisted`, the replay's own tags.

The recording lives at `website/src/data/refund-runs.json`.

### Drift

- A test in the example records into memory and compares with the committed file. A change in the
  library's behaviour fails CI until the demo is recorded again.
- Each run's guarantee is asserted in the example's tests on the runtime's own state, not on the
  JSON: no refund event in the runs that stop, exactly one in the runs that commit, one card in
  `sent-twice`, no send after `OutcomeUnknown`.

## The page

- `BreakIt`, a new component in `src/ds/turnframe.js`, hydrated when visible; styles beside the
  replay's in `src/styles/ds.css`, reusing its ledger rows, card and state tags.
- Controls: radio buttons, «No attack» and three groups of three: the model, the user, the world.
  On a phone they are one row of chips above the stations.
- Playback reveals frames in order, about half a second apart; reduced motion shows them at once.
  Choosing another attack restarts; the hash follows the choice.
- Caption: «Recorded from the real runtime by `cargo run -p refund-desk`. The model's readings are
  scripted, and wrong on purpose.»
- Every sentence inside a run comes from the recording. The section's own copy follows
  `tests/rules.mjs`.
- `index.astro` gains the section after the manifesto and renumbers the others; the site README's
  table names the data file.

## Tests

- Example: the drift test and one test per run's guarantee, as above.
- Site (`npm test`, after a build): the recording parses; there are ten runs, each with frames on
  the five stations, a verdict and a `stopped_at` among the stations or none; the built page holds
  the section and a control per run; the copy rules pass on the section.

## Not in scope

- Running Turnframe in the browser. Every combination is recorded ahead of time, so nothing needs
  compiling to WebAssembly.
- Stacking attacks.
- A second, recorded model: the model's side is scripted, and the page says so.
