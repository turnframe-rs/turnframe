# The travel-disruption desk

The sample domain Turnframe's examples, tests and live corpus run against: a desk that handles
the disruption of one booking. It is chosen because it reaches the guarantees a simpler domain
never does: a card that goes stale when the airline re-quotes under it, an airline that does not
answer (so nothing may be claimed), acts that depend on a record the same message creates, and a
leg the traveler asked to keep, guarded by the domain and by the turn.

Three workflows: `trip`, `traveler`, `claim`, in English and Italian.

## The travel desk

### `trip`: the disruption case of one booking

State: the booking reference; the traveler; a name; a preferred travel date; the legs of the
booking, each with a flight, a route, a date and time, a status (on time, delayed, cancelled) and
whether it is protected; the extras the case adds, each with a description, a quantity, a unit
price and who pays; the rebooking offer the airline quoted, if any; the external status of a
rebooking sent.

Obligations, asked in this order: the traveler, the name, the travel date, a payer for each extra.

| Operation | Arguments | Notes |
|---|---|---|
| `trip.open` | `traveler` (record, optional) | a new case; the traveler may be named, listed, or created in the same message |
| `trip.set_name` | `value` (text copied from the words) | «call it Lisbon offsite» |
| `trip.set_travel_date` | `value` (date) | relative dates computed by code; a date in the past or more than a year out is refused |
| `trip.add_extra` | `description` (text), `quantity` (integer, may be deduced), `unit_price` (money) | «two extra bags at 40 euro each» |
| `trip.change_extra` | `extra` (a listed extra), then any of `description`, `quantity`, `unit_price` | a change with nothing to change compiles to nothing |
| `trip.assign_payer` | `extra`, `payer` (`traveler`, `company`, `airline`) | a closed set, read in both languages |
| `trip.change_traveler` | `traveler` (record) | |
| `trip.protect_leg` | `leg` (a listed leg) | the domain then refuses every command that would change that leg, in this turn and later |
| `trip.request_rebooking` | `leg` | shows the rebooking card for the quoted offer; refused without an offer, or for a protected leg |
| `trip.rebook` | none | applied only by the card; an external saga sent to the airline through the outbox |
| `trip.withdraw` | none | withdraws a case before a rebooking is sent |

Not proposable by the model: the airline's re-quote of an offer, which bumps the revision, and the
recording of the airline's answer to a rebooking.

The rebooking card: «Rebook this flight? Leg 1 (FCO→LIS) on AZ612, 2026-10-05 13:10, €84.00 more.
Confirm / Keep my flight», its options naming exactly the commands they authorize, at the revision it was shown for.

### `traveler`: a passenger profile

State: full name, email, loyalty number (unanswered, answered, or declined with a reason), status
(draft, active, archived). Obligations: name, email, loyalty number.

Operations: `traveler.create_draft` (name optional), `traveler.set_full_name`, `traveler.change_email`
(an address holds no space), `traveler.set_loyalty_number` (a format check),
`traveler.decline_loyalty_number` (reason `not_applicable`, `unknown` or `withheld`),
`traveler.activate` (by its card: «Activate this traveler? Activate / Keep as draft»),
`traveler.archive`, `traveler.delete` (drafts only).

### `claim`: an expense claim read from a receipt

State: the receipt attached; the fields proposed from it (merchant, total, date), each possibly
edited; the reference; the review status. Operations:
`claim.create_draft`, `claim.attach_receipt`, the proposal from the attachment,
`claim.revise_proposed_field` (typed `merchant`, `total`, `date`), the review card (Record /
Discard / Not now), `claim.set_reference`, `claim.abandon_review`, `claim.discard_receipt`.

## The keep-unchanged constraint

- `ConstraintKind` gains `KeepUnchanged`: leave something unchanged, its words say what.
  Segmentation reads it as it reads the other constraints.
- A turn with such a constraint runs one small task, `respects`, for each mutating act: shown the
  constraint's words and the act as the pipeline describes it (operation, record, values), it
  answers whether the act changes what the words protect. It is a model judgment of language; code
  checks the answer's structure.
- An act judged to change it does not run. The reply says it was left alone because the user
  asked. The constraint holds nothing beyond this turn; the domain's lock is what lasts.
- No public struct gains a field for this beyond what the enum's new variant and the task's own
  types need; each such change is a breaking change named in the changelog.

## The showcase

Four guarantees, each with a scripted deterministic test and a step of the `travel-desk` example:

1. A stale card. The rebooking card quotes €84 more; the airline re-quotes €132 before the click;
   the click is refused as stale, nothing is charged, and a new card shows €132.
2. An airline that does not answer. The confirmed rebooking goes out through the outbox and the
   fake airline never answers: the outcome is unknown, the reply says the request was sent and not
   confirmed, and the claim guard refuses any sentence saying it is done. A later check records the
   answer.
3. Dependent acts. «Register Marta Bianchi, put her on this trip and add a checked bag for her»: the
   traveler is created, and the trip's acts wait on it within the turn.
4. A protected leg. «Rebook the outbound on tomorrow's first flight, but don't touch the return, and
   don't confirm anything yet»: the rebooking of the outbound, the return protected by the domain,
   the turn's constraint holding any act on the return, and the do-not-submit constraint keeping
   the card unconfirmed. A misread of which leg is held by the constraint and refused by the lock.

## The corpus

The live corpus holds 68 items in these categories, about a third in Italian: value, asked,
correction, cancel, dispute, multi, question, ability, card, conversation (eight multi-turn items),
complex. Eight showcase items follow the four guarantees: the protected leg in English and Italian (one
easy to misread), dependent acts across `trip` and `traveler`, the unknown outcome (the reply
claims nothing), and a stale click reached through a conversation whose earlier turn re-quotes the
fare.

Each rule the engine learned from a misreading keeps an item: a value that keeps to its part, a
correction that is the user's last word, a joining word that is no value's, a full stop a comma
follows, a record named by the name a creation gives, an answer that gives another value than the
one asked, a doubt voted on again, and the rest the scripted tests hold.

## Out of scope

- Engine features beyond the keep-unchanged constraint.
- A real airline integration: the sample airline answers, refuses or never answers, as a test sets it.
