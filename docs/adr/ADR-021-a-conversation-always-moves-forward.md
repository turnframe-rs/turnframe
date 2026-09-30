# ADR-021: A conversation always moves forward

- Status: Accepted (2026-09-30)
- Amends: ADR-019

## Context

Turnframe guarantees safety: a misreading never writes to the wrong record, a stale click
authorizes nothing, a receipt is backed by a committed event. Every console conversation so far
kept those guarantees. They failed on progress, and the failures had one shape:

- a reply that only reported, leaving the user nothing to say;
- the same question coming back after it was answered;
- a next step offered that the domain then refused, three times running;
- a card named in the reply that the user could not see or press;
- a question answered «the sources needed to answer this were not available» while the records
  held the answer.

Each was patched for the phrasing that showed it, and each patch was followed by another phrasing.
Two things stand out. Most misreadings were the user answering something the assistant itself had
said: an offer in its prose, its last question, its card. And the live corpus measures single
messages written to pass: it reads 228 of 228 while a free conversation breaks within ten turns.

## Decision

1. **Progress is guaranteed by code, as effects are.** Whatever the model reads:
   1. Every reply ends on a way forward: the ask, the card on screen, the next steps, or a question
      to go on. (Released in 0.1.2.)
   2. A reply offers only what the domain accepts now. A next step is an operation with its record,
      and code dry-runs it against the view (the act compiles and the command validates) before
      offering it; one the domain would refuse is not offered.
   3. The same question is not asked twice in a row without a reason. When the ask equals the last
      turn's ask and the message did not answer it, the reply says why it asks again and offers the
      other ways forward.
   4. A card the reply mentions is a card the surface can act on: every card and offer travels as
      data in the turn's blocks, whether or not it blocks.
   5. Every question gets an answer, or where things stand and what can be done. An answer the facts
      cannot give falls back to the record's state and its next steps.
2. **The assistant's offers and questions are data.** A turn records what it put in front of the
   user: its ask (operation, record, arguments asked), its next steps (operations, records,
   arguments already known) and its card. The next turn reads the message first against that list:
   one small task with a closed schema chooses which offer the message takes up, or none, before
   routing over the whole catalogue. «Yes», «the first», «I confirm» and «that one» take up an
   offer; «what do you mean?» after a question is answered from the asked operation's declared
   values. `WorkflowDefinition::next_steps` returns typed offers (an operation and its words), not
   sentences.
3. **Conversations are evaluated by simulated users.** A new evaluation runs goal-driven
   conversations: a model plays a user with a goal («open a trip for a new traveler, name it, add a
   bag, rebook the outbound») and a manner (terse, wordy, typing errors, changes its mind,
   complains), talks to the runtime for up to a set number of turns, and stops at the goal or the
   limit. Code scores every conversation from the stores and the transcript: goal reached (checked
   on state), turns taken, dead ends, loops (the same ask twice), parts not understood, offers the
   domain refused, and guarantee violations, which must be zero. A run reports each class as a rate
   over many conversations, with the transcripts of the worst ones.
4. **A correction keeps what it does not restate.** A correction that gives part of a value, a
   day and month without the year of the date it corrects, keeps the rest from the corrected
   value, by code; it is not read against today.

## Consequences

- Typed next steps change a public trait method: 0.2.0, named in the changelog.
- Reading a message against the offers costs one small call when offers exist.
- A simulated-user run costs a few dollars; runs are announced and budgeted like live corpus runs.
  The live corpus stays as the regression floor for single messages.
- A failure found in a conversation is fixed as its class, by a guarantee or by structure, and a
  fix is kept only when the class's rate falls. Rewording a prompt for one phrasing is no longer
  how a failure is fixed.

## Alternatives considered

- **Patching each transcript.** It does not converge: language has more phrasings than patches.
- **A larger model.** It lowers the rate of misreadings and leaves every class of progress failure
  in place, since nothing guarantees progress.
- **One conversation-level prompt that reads everything.** It brings back the single reading ADR-016
  replaced, with its failures.
