# Threat Model

Turnframe's technical promise is short: **models propose meaning, deterministic reducers
decide effects, committed events decide claims.** This document explains what that
promise protects, where the trust boundaries sit, which attackers we expect, and which
invariant (I1–I20 in the specification) contains each of them.

Scope is the framework itself: runtime, interaction engine, stores and provider layer.
Host authentication, domain authorization rules and legal or regulatory review stay with
the application. Turnframe assumes an authenticated actor context arrives from the host
and treats everything else as hostile until validated. The tenant boundary the framework
enforces is the **account**; a scope narrower than that (an organization, a workspace, a
legal entity) belongs to the application's `CaseDirectory`, which is why it has a
section of its own below.

## Assets (in order of how bad it is to lose them)

1. **Committed side effects.** A record mutated, a rebooking sent to an airline,
   money moved. The whole design exists so these happen only when a deterministic path
   decided so.
2. **Operational truth.** The `EventLedger` and the command journal. Every receipt shown
   to a user must trace back to a committed event (I16). If the ledger can be forged or
   bypassed, the user can be told something happened that did not, or vice versa.
3. **Tenant isolation.** Cases, interactions, target tokens, commands and events all carry
   an account scope. A tenant must never learn that another tenant's record exists.
4. **Persistent interactions.** An `Interaction` is a durable user decision. Its stored
   options define what a click means (I7). Tampering with an interaction is equivalent
   to forging consent.
5. **Secrets and outbound user data.** Provider API keys, external system credentials,
   and the text, attachments and case state sent to a model provider.
6. **Replay and audit records.** What lets an operator reconstruct why a turn did what it
   did (I20); losing them removes the ability to prove what happened.

## Trust boundaries

Nothing crosses from the untrusted side to the trusted side without a validation step
that the runtime owns.

**Untrusted, always:**

- user text and attachments;
- model output, including structured output that passed schema validation;
- provider response metadata;
- option values and any other field a client sends with an interaction response;
- retrieved documents and read-tool results;
- external callback payloads until their signature has been verified.

**Trusted only after validation:**

- the actor context produced by the host's authentication middleware;
- the mapping from opaque target tokens to real record identifiers;
- the options stored server-side on an `Interaction`;
- policy decisions produced by the command policy layer;
- committed events in the `EventLedger`;
- external receipts whose signature or origin has been verified.

Two consequences follow. The understanding tasks sit on the untrusted side even though the
application operates them: what they answer is a proposal (I9), not a decision. Only the
`TurnReducer`, the interaction engine and the command path may promote data across the
boundary, and they fail closed when they cannot verify what they promote (I19).

## Attacker capabilities and containment

Each subsection names the capability, the containing invariant, the component that
enforces it, and the risk that remains.

### Malicious user text

The user writes text designed to make the assistant mutate something it should not,
target a record it did not name, or produce a receipt for an effect that never committed:
"delete everything", "confirm the last one", or a message embedding fake system prompts.

*Containment.* An operation can only be one of the keys on offer to the turn, a record
only one of the handles in view, and every value points at words of the user's own
messages, a structured interaction response or a server-supplied origin (I9). Each task's
answer is schema-checked whole, and an answer that does not fit is sent back or refused
whole (I18). A separate task checks each act against the words it came from before it
runs. Targets are resolved deterministically; an ambiguous target produces a
`SelectTarget` interaction and no mutation (I8). Consequential commands need a
server-issued origin such as a confirmed interaction, so text alone cannot trigger them
(I12). The whole turn is reduced before any effect runs, so a correction or a cancel in
the same message supersedes the act it names (I10).

*Responsible.* The understanding tasks and their checks (schema, pointers, verification),
`TurnReducer` and target resolution (decision), command policy (confirmation).

*Residual risk.* Low-risk commands whose policy allows direct user acts can still be
triggered by persuasive text. The framework defaults unknown commands to a conservative
policy, but a permissive application-level policy is outside its control.

### Prompt injection through retrieved content or tool output

A document, search result, traveler record or attachment contains text that instructs the
model to act.

*Containment.* Retrieved content is data, not instruction. What a knowledge source returns
carries a source label and a trust level, and it reaches only the task that answers a
question, after the commit, where nothing it writes can become an effect. The tasks that
understand the message are shown the message, the transcript and the records in view; no
retrieved content is among them. Even an act the user's own words were steered into still
passes through verification, target resolution and policy, which are independent of
prompts. Sandboxed autonomous mode is never eligible for regulated, irreversible or
destructive operations.

*Responsible.* Knowledge sources (labels, sensitivity), the answer task, `TurnReducer`.

*Residual risk.* Injection can degrade answers to questions, which consume retrieved
content by design, or make the model refuse or stall. Neither yields an unauthorized effect.

### Client tampering with option identifiers or values

A modified client sends an option id that was never offered, a free-form value on an
option that forbids one, or a crafted payload meant to redefine what the button means.

*Containment.* The client sends only an interaction id, an option id and an expected case
revision; the server loads the stored option and derives its meaning (I7, ADR-004). An
option id not stored on the interaction is rejected, and a free-form value is accepted
only when the stored option's policy allows it. Payloads are immutable after first display
and carry a `payload_hash` recorded in the command origin, so the journal can prove which
payload authorized which command.

*Responsible.* The interaction engine and the `InteractionStore`.

*Residual risk.* A legitimate user can still click an option they did not intend; that
is a product concern handled by confirmation policy, not a security boundary.

### Cross-tenant identifier guessing

An attacker enumerates interaction ids, case ids, target tokens or command ids to read or
act on another tenant's records, or simply to learn that they exist.

*Containment.* Every store lookup, interaction, target token, command and event is
account-scoped; the reference PostgreSQL schema keys the `tf_*` tables by account and
idempotency is unique on `(account_id, idempotency_key)`. A lookup outside the caller's
account behaves exactly like a missing record. Target tokens are opaque, minted per
turn, and a token from another account resolves to `Unauthorized`,
which never reaches command compilation.

*Responsible.* Store implementations, the target resolver, and the host's actor context.

*Residual risk.* Scoping depends on the host attaching the correct actor context. Timing
differences between the "missing" and "unauthorized" paths are not yet measured.

### A record outside the actor's narrower scope

Many applications have a scope between the tenant and the record: an organization, a
workspace, a legal entity. An actor legitimately inside the account reads or acts on a
case that belongs to a different one of those, and every framework check passes because
the account matched.

**The account is the authorization boundary the runtime enforces. Any narrower scope is
the `CaseDirectory`'s responsibility.** `AccountId` is the only scope the framework's own
lookups carry: `WorkflowExecutor::load` receives an account and a case identifier and
nothing else: in particular not the `ActorContext`, so an executor could not enforce a
narrower scope even if it wanted to. An adopter with a sub-tenant must therefore treat
their directory as security-relevant code rather than as a convenience.

The runtime's half of that bargain is that the directory is asked about **every** case a
turn can address, and not only about the ones it volunteered. A turn considers more than
the directory's answer: a card the conversation left open has to stay answerable, so the
case it sits on and the cases its `SelectTarget` options offer are considered too, and
each of those goes back to the directory, through `CaseDirectory::authorize_case`, before
it is projected or given a token. The default implementation of that method refuses, so
the candidate list is the whole definition of what an actor may address unless the
adopter says otherwise. A directory that scopes `candidates` correctly needs no further
code, and a scope the actor leaves between two turns takes its records with it.

*Containment.* The directory decides which cases exist for a turn, and everything the
model or the client can name is derived from that answer.

- The model never sees a record identifier. `ProposedTarget` offers an opaque
  `TargetToken`, a workflow key for a new case, a free-text mention, or the active
  interaction, never a case id.
- The runtime builds the turn's `TargetResolver` from the cases it loaded for that turn and
  issues one token per case, so a token can only name a case in that set.
  `TargetTokenMap::resolve` re-checks the account on top of that and answers
  `Unauthorized` for another account's token, indistinguishably from an unknown one.
- A mention is matched by `TargetResolver::resolve_mention` against the labels and aliases
  of those same cases, with no fuzzy matching and no fallback lookup, so a phrase cannot
  reach a case the directory withheld. Several matches are `Ambiguous` and become a
  selection card; none is `Missing`.
- An interaction response is validated against the account that owns the card:
  `InteractionEngine::accept` reads the record through the interaction store's
  `get(&actor.account_id, …)` and turns another tenant's identifier into `NotFound`.
- An origin reference is server-validated. The runtime passes the token to
  `CaseDirectory::resolve_origin`, which does receive the `ActorContext`, and binds it
  only to the record the directory returned; an unbound token is `Unauthorized` in
  `TargetResolver::resolve_origin`, so a guessed one reveals nothing.
- A case an open card names is not admitted on the card's word. It is offered to
  `CaseDirectory::authorize_case`, which receives the `ActorContext` and the conversation;
  a refusal means the case is never projected, no token is issued for it, the card is
  dropped from the turn (so it neither blocks nor answers an `ActiveInteraction`
  target), and a click on it is rejected as `NotFound`, the same answer an identifier
  that never existed gets. The one case admitted without asking is a case with no state:
  a `StartWorkflow` mints an identifier and the confirmation that would create the record
  is written against it, so there is no record to authorize and nothing to read, and
  refusing it would strand the card whose whole purpose is to bring the record into
  being.

*The failure mode.* An adopter who assumes the executor enforces the narrower scope will
write a `load` that trusts its arguments: the case identifier came from the framework, so
it must be legitimate. It is legitimate *for the account*, and says nothing about the
entity. The result is a cross-tenant read within one account: one organization's case
loaded, projected and narrated into another organization's conversation, with no invariant
broken and nothing to log. Three directory mistakes produce it, and they are the three
methods that see the actor: returning candidates outside the actor's current scope,
resolving an origin token without checking the record belongs to it, and overriding
`authorize_case` to admit a case the current actor could not otherwise reach.

*The alternative.* Encoding the narrower scope inside the case identifier works too, and
it is unguessable in the same way a token is: a case of another organization is simply a
case the directory never names and the executor never finds. The trade is that it needs no
directory discipline at all, but it puts the scope into a key that appears in stored
rows (the `tf_*` tables, idempotency keys, ledger entries and replay records) and
cannot easily be changed afterwards. A scope fixed for the life of the case argues for the
identifier; one that can be renamed, merged, or that a record can move between argues for
the directory.

*Responsible.* The application's `CaseDirectory`, and the host's actor context.

*Residual risk.* Nothing in the framework can detect a directory that returns too much: an
over-broad candidate list is indistinguishable from a correct one from the runtime's side,
and so is an `authorize_case` that says yes to everything. The framework can only
guarantee that the question is asked about every candidate, which it now does; what the
answer should be is knowledge the framework does not have.

One consequence is worth stating because it is a behaviour, not a risk. A directory whose
`candidates` is deliberately narrower than "every case this actor may reach" (a working
set, one screen's records, the recently touched ones) will find that a card outliving its
case's place in that list becomes unanswerable, with nobody's scope having changed. That
is what `authorize_case` is for: such a directory overrides it and answers for the cases
it knows the actor may reach but chose not to list.

### Replayed clicks and duplicated deliveries

A double-click, a network retry, or an attacker replaying a captured request redelivers
the same interaction response to repeat an effect.

*Containment.* Every command carries a stable idempotency key persisted before or
atomically with execution (I14, ADR-006); a repeat returns the original outcome without
repeating the effect. Every command targets an expected case revision; a stale revision
yields a conflict, never a blind overwrite (I13). A second click on a resolved interaction
returns the original resolution, and a changed revision invalidates interactions bound to
the prior one, so a replayed click against a moved case is rejected as stale.

*Responsible.* `CommandJournal`, interaction engine compare-and-swap, case store.

*Residual risk.* An external system called through the outbox is protected only if it
honors the idempotency key; when it does not, the saga marks the outcome `Unknown` and
requires reconciliation instead of a blind retry (I15, ADR-007).

### Provider compromise or misbehaviour

A provider returns malformed, adversarial or subtly wrong structured output; a fallback
provider has weaker capabilities; a provider logs or retains the data it receives.

*Containment.* Model output is a proposal at every stage (I9, ADR-001); a compromised
provider can at most propose acts that still require pointers into the user's words,
verification, target resolution and policy approval, and every answer is all-or-nothing
(I18). The understanding tasks require a structured-output capability declared per
provider-model pair; the router
refuses a silent downgrade to prompt-only output (ADR-008). Fallback happens only before
any command executes or for the reply after the commit, never around a possibly committed
effect (I17). Providers receive keys through secret wrappers, never through prompts, and
never hold external credentials, which belong to command handlers. Field-level redaction
and sensitivity-based allowlists limit what leaves the boundary.

*Responsible.* Provider layer (`turnframe-provider` and adapters) for capabilities,
routing and redaction; `TurnReducer` for treating output as a proposal.

*Residual risk.* A provider that proposes plausible but wrong low-risk acts degrades
correctness without violating any invariant. Data already sent to a provider is outside
the framework's control; redaction reduces but does not remove that exposure. Conformance
is per provider-model combination and cannot be assumed for untested pairs.

### External callback forgery

An attacker posts a fabricated callback claiming an external submission was accepted or
rejected, hoping the runtime commits a false status event and shows the user a receipt.

*Containment.* Callback payloads are untrusted until signature verification. A command
with origin `ExternalCallback` records whether the signature was verified, and only
verified receipts may produce committed status events, from which user-visible receipts
derive (I16, ADR-005). Reconciliation by poller or remote identifier is an independent
path to the authoritative state, so a forged callback cannot by itself leave the case in
a false terminal state.

*Responsible.* Outbox dispatcher, the application's callback verifier, `EventLedger`.

*Residual risk.* Signature verification is application-provided per external system; a
weak verifier is not something the framework can detect. Replayed *valid* callbacks are
contained by idempotency on the callback id.

## Summary table

| Capability | Containing invariant | Responsible component | Residual risk |
|---|---|---|---|
| Malicious user text | I8, I9, I10, I12, I18 | Understanding tasks and their checks, `TurnReducer`, target resolution, command policy | Permissive application policy for low-risk direct acts |
| Prompt injection via retrieved content / tool output | I9, I18; retrieved content reaches only answers | Knowledge sources, the answer task, `TurnReducer` | Degraded answers; denial of useful service |
| Client tampering with option ids / values | I7, I19 | Interaction engine, `InteractionStore` | Unintended legitimate clicks (product concern) |
| Cross-tenant id guessing | Account scoping (§25.4), I19 | Stores, target resolver, host actor context | Host supplies wrong actor; timing side channels unmeasured |
| Record outside a narrower scope (organization, workspace, legal entity) | Account scoping (§25.4); every candidate authorized by the directory, whatever brought it into the turn | `CaseDirectory`, host actor context | An over-broad directory, or a permissive `authorize_case`, is undetectable |
| Replayed clicks / duplicate delivery | I13, I14, I15 | `CommandJournal`, interaction CAS, case store | External systems that ignore idempotency keys |
| Provider compromise / misbehaviour | I9, I17, I18, no silent downgrade | Provider layer, router, `TurnReducer` | Plausible wrong low-risk proposals; data already sent |
| External callback forgery | I16, verified origin | Outbox dispatcher, application verifier, `EventLedger` | Weak application-side signature verification |

## Detection

Contained attacks must still be visible. `turnframe.task.repaired`, `turnframe.act.refused`,
`turnframe.target.ambiguous`, `turnframe.interaction.stale`, `turnframe.command.rejected`,
`turnframe.command.idempotency_replay`, `turnframe.command.revision_conflict`,
`turnframe.provider.capability_mismatch` and `turnframe.external.outcome_unknown` each
count one containment path firing; a sustained rise from one account is worth an alert.
Metric tags never carry user or case text and logs redact authorization headers and raw
provider bodies, so detection does not itself become a leak.
