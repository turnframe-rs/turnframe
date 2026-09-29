# ADR-019: The reply is written by small tasks and reviewed before it is shown

- Status: Accepted (2026-09-26)
- Amends: ADR-010, ADR-005

## Context

One narration call used to write the whole reply: an acknowledgement, the answers to every question
in one batch, and the words around the receipts. It was handed the plan, the transcript and a brief,
and it streamed text as it wrote. Three things went wrong with small models. A reply stated an
outcome no receipt backed, so the runtime matched phrases against it and withheld blocks after they
had streamed, which a phrase list cannot do reliably: it misses a paraphrase and blocks a denial.
A turn that asked for a record and got it ended on a statement, leaving the user to guess what came
next. And one batched answer was dropped when any other answer in the batch was malformed.

## Decision

1. Code gathers the turn's outcome into one document: what was done, what was not and why, what the
   user disputed, what the turn started, the one thing to ask next, and the card on screen.
2. Code chooses the ask: a change the user contested, else the first value an act waits for, else
   the first open obligation of the record the turn is on. The workflow words an obligation; that
   sentence is also the server's own question.
3. The acknowledgement task writes from that document alone. Each question gets its own answer task,
   run beside the others.
4. A review task answers a checklist of yes-or-no checks, only those that apply: whether the reply
   asks the ask and nothing else, claims beyond its material, contradicts the screen, or answers a
   question answered in its own block. A reply that fails is written once more with the findings;
   one that fails again is dropped, and the question code wrote stands in for it.
5. The reply is published whole, after the commit. What streams while a turn runs is progress: the
   phase, and each understanding step as it is decided, in words a model may write when the
   deployment asks for it.

## Consequences

- The phrase guards, the withheld blocks and the text deltas are removed; ADR-005's claim guard
  stays structural and reads the record.
- A turn ends on a question whenever the work needs something, even when no model writes the reply.
- A reply costs two or three small calls. A deployment whose model does not need review switches it
  off in the acknowledgement's profile.
- A malformed answer loses only its own question, which is reported as not written.

## Alternatives considered

1. **Keep one narration call, with a better prompt.** The call still has to judge what happened,
   what to ask and what to answer at once, which is the load small models fail under.
2. **Let the model choose the next question.** It picks inconsistently between turns, and a turn
   with no model then asks nothing.
3. **Guard the prose with vocabulary.** ADR-015 records why no string comparison decides what a
   reply claims.

## Enforcement

- `a_disputed_change_is_asked_for_again` in `crates/turnframe-runtime/tests/`: a reply that fails
  review twice gives way to the server's question.
- `nothing_that_states_an_outcome_is_streamed_before_the_commit` and
  `the_acknowledgement_arrives_whole_after_the_commit` in
  `crates/turnframe-runtime/tests/streaming_and_recovery.rs`.
- `a_review_asks_only_the_checks_that_apply` in `crates/turnframe-runtime/src/narrate/tasks.rs`.
- `three_questions_get_three_answers_in_the_order_asked` in
  `crates/turnframe-runtime/tests/answers.rs`.
