# ADR-007: External actions use outbox/saga and explicit unknown outcomes

- Status: Accepted (2026-09-05)

## Context

Turnframe applications talk to systems that the runtime does not control: an airline's
booking system, a payment gateway, an e-mail relay, a partner API. These calls differ from an
internal database write in two ways that matter for a conversational product. They cannot be
rolled back by aborting a transaction, and they can fail in a way where the runtime does not
know whether the remote effect happened. A request may leave the process, be accepted remotely,
and then the response may be lost to a timeout, a dropped connection, or a crash on our side.

The naive design treats an external call like any other step in the turn: call it inline,
wait, and on error report failure and retry. In a chat interface this produces concrete,
user-visible harm.

- A user says "send the rebooking". The remote intermediary accepts it, but the response times out.
  The runtime reports "sending failed", the user clicks "Send" again, and the rebooking is
  sent twice under two booking references. The traveler now holds two tickets and one is charged.
- The same timeout is reported as a definite failure. The user, believing nothing happened,
  edits the trip and sends a corrected rebooking. The airline now holds an earlier version the
  application never acknowledged, and the application's own view of the case is wrong.
- The runtime commits the local state to "submitted" before the remote system has been called,
  then crashes. On restart nothing dispatches the request; the case looks done and the user is
  told it is done, but nothing was ever sent. This is the failure mode I16 exists to prevent:
  a claim that is not backed by anything that actually occurred.
- A provider fallback or generation retry re-runs the turn after the external call already
  succeeded, repeating the effect (the situation I17 forbids).
- The application flattens every state into "sent" or "failed". A regulated flow that legally
  distinguishes "received by intermediary", "received by authority", "accepted" and "rejected"
  is misreported, and the user makes decisions on a status that does not exist.

The common thread is that uncertainty about the remote world was collapsed into a binary and
then acted upon. The spec's answer, in §4 I15, is that external uncertainty must be a
first-class, represented state, and, in §16.4 and §16.5, that the mechanism carrying external
actions must make blind repetition structurally hard rather than a matter of discipline.

## Decision

1. Every side effect on a system outside the runtime's transactional boundary MUST be executed
   through the outbox/saga model of spec §16.4. Accepting a command MUST commit, in the same
   local transaction as the domain state, a `PendingExternal` event and an outbox row. The
   external call itself MUST NOT be made inside that transaction.
2. A dispatcher MUST read the outbox and call the external system with the row's idempotency
   key. The reference schema (`tf_outbox`) enforces `UNIQUE (destination, idempotency_key)`;
   store implementations MUST preserve that uniqueness so a given external action can exist at
   most once per destination.
3. The runtime MUST NOT claim completion at `PendingExternal`. A completion receipt MAY be
   produced only after an authoritative callback, poll, or reconciliation has committed an
   `Accepted`, `Rejected`, or `Unknown` event. This is I16 applied to external actions.
4. When a request has been transmitted and no authoritative response arrives, the attempt MUST
   be marked `OutcomeUnknown`. It MUST NOT be reported to the user as a definite failure, and
   the runtime MUST NOT retry it blindly. Retry is permitted only when the remote API guarantees
   idempotency for the same key; otherwise the path forward is reconciliation using the remote
   identifiers already stored or a poller.
5. The user-facing status for an unknown outcome MUST be a deterministic, server-authored
   "verification in progress" block. The narrator MAY introduce or explain it but MUST NOT
   replace it or upgrade it into success or failure (spec §17.4).
6. For regulated external statuses the application MUST store the external receipt or reference
   (a ticket number) and MUST keep the remote states distinct (prepared, validated, awaiting
   confirmation, submitted, received by intermediary, received by authority, accepted,
   rejected, issued, not delivered, delivered, completed, or the domain's equivalent). They
   MUST NOT be collapsed into a generic "done".
7. Crash recovery MUST follow spec §23.1: if an external outcome is unknown at restart, the
   runtime reconciles; it MUST NOT re-run interpretation and command execution.
8. The error model MUST carry uncertainty as a distinct variant (`ExternalOutcomeUnknown`),
   classified separately from retryable failures, and it MUST record whether a reconciliation
   job is required (spec §24).

## Consequences

Positive:

- Duplicate transmission requires two things to go wrong at once: the idempotency key must be
  lost and the outbox uniqueness constraint must be bypassed. Neither happens through a user
  clicking twice or a dispatcher restarting.
- The chat never lies in either direction. It cannot say "sent" before something authoritative
  says so, and it cannot say "failed" when the truthful answer is "we do not know yet".
- Every external attempt has a durable audit trail (outbox row, `PendingExternal` event,
  terminal event), which is what makes `turnframe.external.outcome_unknown` and
  `turnframe.external.reconciled` meaningful metrics and what lets replay (I20) explain a
  case's history.
- Regulated flows keep their real state machine instead of a two-state summary.

Negative:

- Latency: the user sees "verification in progress" or "submitted" rather than an immediate
  "accepted", because acceptance now comes from the callback or poller rather than the turn.
- Operational surface: adopters must run a dispatcher and a reconciliation path (webhook
  handler or poller) alongside the request path, and monitor the outbox for stuck rows.
- Design effort: every external destination needs an explicit answer to "is this endpoint
  idempotent for our key?" and "how do we look this attempt up remotely later?".

What adopters must do:

- Model each external destination as an outbox destination with a stable idempotency key
  derived from the command, never from the turn or the message.
- Implement the authoritative feedback path (callback or poller) that commits the terminal
  event; without it, actions stay pending forever by design.
- Provide the localized copy for the "verification in progress" receipt and for each distinct
  remote state their domain recognises.
- Never write code that translates a transport timeout into a failure event.

## Alternatives considered

1. Inline external calls with retry-on-error.
   Rejected. It is the design that produces the double-send and false-failure scenarios above.
   A retry loop cannot distinguish "the request never left" from "the request landed and the
   reply was lost", and in a chat product the user amplifies the mistake by acting on the
   wrong report.
2. Two-phase commit or distributed transactions across the local database and the remote system.
   Rejected. The external systems Turnframe targets (authorities, intermediaries, third-party
   APIs) do not expose a prepare/commit protocol, so the coordinator would be fictional. Even
   where a partner did offer it, the lock-holding and blocking semantics fit a request/response
   API badly and a conversational latency budget worse.
3. Optimistic local commit with compensation ("mark as sent, undo if the call fails").
   Rejected. Compensation assumes the failure is observable. The case this ADR is about is the
   one where it is not: after a timeout there is nothing to compensate against because the
   outcome is unknown. It also violates I16 by emitting a success claim before any committed
   event authorises it.
4. Treating `Unknown` as an internal implementation detail hidden behind a generic "pending"
   spinner with automatic retry.
   Rejected. It is the collapsed-status problem in a friendlier costume: the user still cannot
   tell "we are waiting for the airline" from "we do not know whether the airline received
   anything", and automatic retry reintroduces duplicate effects.

## Enforcement

Invariants from spec §4 implemented by this decision:

- I15 (external uncertainty is represented explicitly) is the core of the decision.
- I16 (events authorize claims) governs when a completion receipt may appear.
- I14 (idempotency is mandatory) is carried into the external boundary by the outbox key.
- I17 (provider failure cannot repeat effects) and I20 (replay is possible) are supported by
  committing the outbox row and `PendingExternal` event with the domain state.

Tests and release gates that prove it:

- Spec §27.4 runtime integration scenario 15: an external timeout becomes `OutcomeUnknown`,
  not blind retry. Scenario 10 (a failed command cannot produce a resolved-looking receipt) and
  scenario 20 (no critical success phrase without a matching receipt/event) cover the claim side.
- Spec §27.7 chaos injection points "before/after outbox dispatch" and "after remote request but
  before response" must recover idempotently with truthful status.
- Spec §27.2 property tests on repeated idempotency keys and duplicate interaction clicks.
- Spec §33 safety gates: "No external timeout is represented as a definite failure when the
  outcome may be unknown", "No critical command lacks idempotency", "No critical success receipt
  lacks committed event IDs". Operational gate: "Crash recovery is tested at every commit
  boundary".

Responsible crates and modules:

- `turnframe-store` owns the object-safe outbox store trait and the transactional boundary that
  commits state, events, and outbox rows together.
- `turnframe-store-postgres` owns the `tf_outbox` reference table and its uniqueness constraint.
- `turnframe-runtime` owns dispatch, the `OutcomeUnknown` transition, reconciliation
  coordination, crash-recovery behaviour, and the server-authored verification-in-progress block.
- `turnframe-telemetry` emits `turnframe.external.outcome_unknown` and
  `turnframe.external.reconciled`.
- `turnframe-test` provides the command doubles and chaos fixtures used by the scenarios above.
