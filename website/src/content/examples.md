# Examples

Five programs live under [`examples/`](examples/), each built on the facade. Four of them script
what each message is understood to say, run on the in-memory stores and print what the runtime
did, so they run without an API key, a database or a network. The fifth, the console, talks to
a real model.

```sh
git clone {{repo}}
cd turnframe
cargo run -p travel-desk
```

## The travel desk

[`examples/travel-desk`](examples/travel-desk/src/main.rs) walks the four guarantees in order, on
a trip whose outbound flight the airline cancelled.

1. **Dependent acts.** «Register Marta Bianchi, put her on this trip and add a checked bag at 40
   euros»: the traveler does not exist when the message arrives, so putting her on the trip waits,
   within the turn, for the act that registers her.
2. **A protected leg.** «Rebook the outbound on the flight the airline offered, but don't touch
   the return, and don't confirm anything yet»: the rebooking stops at its card, the return is
   locked by the domain, and a later misreading that would change it is held by the turn's own
   condition.
3. **A stale card.** The airline re-quotes the flight from €84 to €132 more while the card is on
   screen. The click on the old card is refused, because the card is bound to revision 5 and the
   trip is at revision 6, and the next card shows the new fare.
4. **An airline that does not answer.** The confirmed rebooking goes out through the outbox and
   the airline never replies. The outcome is recorded as unknown, never retried blindly, and the
   reply says the request was sent, not confirmed. The late answer arrives twice and is recorded
   once.

```text
-- 7. A stale card: the airline re-quotes before the click ---------------
  airline  the same flight, now 132.00 EUR more: the trip moves to revision 6
  user     [clicks "confirm" on the card shown at revision 5]

  refused  interaction stale: bound 5, current 6
```

## The refund desk

[`examples/refund-desk`](examples/refund-desk/src/runs.rs) is the demonstration on the home page:
a shop's refund desk under nine attacks. The model reads the wrong order or the wrong amount, or an
order of another shop; the user takes the refund back mid-message, double-clicks, or has the app
send the turn twice; a colleague refunds part of the order while the card is open, the payment
provider takes the refund and never answers, or the model provider fails halfway through. Only the
model's reading is scripted. Everything after it is the runtime, and none of the nine sends a
refund it should not.

```sh
cargo run -p refund-desk                                                   # every run, station by station
cargo run -p refund-desk -- --record website/src/data/refund-runs.json     # the recording the site plays
```

The example defines its own `order` workflow, so it shows the shape of a domain written outside the
test kit. A test fails when the site's recording no longer matches what the runtime does.

## Traveler onboarding

[`examples/traveler-onboarding`](examples/traveler-onboarding/src/main.rs) is a flat collection
workflow. Two fields nobody asked for arrive in one message and commit under one revision; the
email settles the last field and raises the activation card; a click activates the traveler; a new
address on the active traveler applies at once.

```sh
cargo run -p traveler-onboarding
```

## A question and an action in one turn

[`examples/mixed-question-and-action`](examples/mixed-question-and-action/src/main.rs) sends a
click on the rebooking card together with a message: «Actually, do not rebook anything yet. While
you are there, change the name to Lisbon, October, and tell me what the fare difference is.» The
whole turn is reduced before anything runs, so the prohibition reaches the rebooking the click
authorized before it reaches the domain. Nothing is rebooked and a notice says so, the name change
commits with a receipt citing its event, and the question is answered on a stated basis.

```sh
cargo run -p mixed-question-and-action
```

## The console

[`examples/console`](examples/console/src/main.rs) runs the travel desk interactively against a
real model, from one seeded trip: two legs, the outbound cancelled, and the airline's offer for it.

```sh
cp .env.dev .env     # put a key in .env; the console loads it
cargo run -p console # OpenAI, Anthropic or Gemini, whichever key is set; else a local Ollama
```

Each turn prints the understanding's steps as they are decided, then what was understood, then what
the runtime did with it. The two lists differ, and the gap is the design. Try rebooking the
outbound while keeping the return, registering a traveler and putting them on the trip in one
message, or giving two fields in one sentence.

| Setting | What it does |
| --- | --- |
| `TURNFRAME_MODEL` | the model to use with whichever provider key is set |
| `OLLAMA_URL` | an Ollama daemon somewhere other than the local default |
| `TURNFRAME_CONFIG` | a TOML file merged over the conservative configuration |
| `TURNFRAME_TRACE=1` | every event and model call written to the gitignored `traces/` as JSON Lines |
| `TURNFRAME_LOCALE=it-IT` | replies written in Italian |
| `/effort high` | the turns that follow spend more calls to read you |

What a user of a real surface would read is printed in bright white, and what the console adds to
explain the turn is dimmed.

## The sample domains

The refund desk brings its own `order` workflow. The others run on the sample workflows in `turnframe-test`, which the `test-kit` feature exposes
as `turnframe::testing`: a `trip` (the disruption case of one booking), a `traveler` (a passenger
profile) and a `claim` (an expense read from a receipt). They are complete domains, with Italian
copy beside the English, and [the recipes](/docs/recipes) explain the shapes they prove.
