# Consent and acceptance

A legal acceptance ("I accept version 2026-07 of the electronic invoicing
terms") is a fact that a conversational application has to record, prove years
later, and use to gate other work. This document says how to model one with the
types Turnframe already has, and why it is not a new interaction kind.

The recipe is executed by
[`crates/turnframe-core/tests/consent_acceptance_recipe.rs`](../crates/turnframe-core/tests/consent_acceptance_recipe.rs).
Everything asserted below is asserted there, in the same order.

---

## Why this is not a new interaction kind

The request that prompted this document asked for an `InteractionKind` whose
payload references a versioned artifact and whose resolution is retained under
its own rule. Its own description is the reason to keep it out of the interaction
kinds. It says that the acceptance authorizes nothing about the case, that it is
a fact about the person recorded for audit, and that it may gate a workflow
without being part of it.

Every interaction in this library binds to a case and a revision, and that
binding is the whole reason a card is safe to click: the card that confirms a
send is void the moment the trip moves underneath it. A record that is
deliberately *not* about the case has two ways into that model and both are
bad. Either it carries a case reference that means nothing, in which case the
one thing every card is checked for stops being checked for this one, or the
interaction model grows an account-scoped variant, and every adopter pays for a
second binding rule in order to serve one shape.

There is a third option, which is that the acceptance *is* about a case, just
not about the trip. That is the recipe below, and it needs no new type.

---

## The recipe

### 1. Consent is its own workflow, and its case is the person

Model consent as a small `WorkflowDefinition` of its own. Its case identifier is
the account or the person, not the trip, the order or whatever is being
gated:

```rust,ignore
CaseRef::new("consent", person_id, revision)
```

Everything follows from that one decision. The card binds to a revision like
every other card, and the revision it binds to is the person's consent, which
moves when their consent changes and at no other time. Lookups stay
account-scoped, because the case identifier is inside the account. And the
trip workflow never has to explain why one of its cards is not about an
trip.

The phases are the states consent can actually be in (nothing on file, an older
version on file, the published version on file), and the obligation is
parameterized by document, so an application that grows a second agreement lists
two outstanding obligations rather than adding a second boolean.

### 2. The acceptance is an ordinary command behind an ordinary card

The open phases are user-owned and raise a `ConfirmCommand` card with two
options: one that applies the acceptance operation and one that declines. The
command policy is `CommandPolicy::conservative()`, which is already exactly what
a legal acceptance needs: an explicit click, no text resolution, server
receipts only. A confirmation inferred from prose is refused by
`InteractionSpec::validate` before it can ever be persisted, so "the user said
yes in a sentence" cannot become an acceptance.

Consent is never terminal. A new version reopens the obligation, so the
`Accepted` phase is system-owned rather than terminal, and the workflow has no
outcome type worth constructing.

### 3. Where the artifact version is bound

This is the part the request rightly insisted on, and it is worth being precise
about, because "the version is bound to the resolution" is a property of three
separate places rather than a promise.

**In the card's payload hash.** The artifact identity (document, version, and
the digest of the bytes the person was shown) sits in the payload metadata.
`InteractionPayload::hash` covers the whole payload, so two versions of the same
document produce two different cards, and answering one mints a
`CommandOrigin::ConfirmedInteraction` whose `payload_hash` the other cannot
produce. Carrying the content digest and not only the version number matters: a
document re-published under the same number with different bytes changes the
digest, the acceptance on file stops matching what is on the wire, and the
obligation reopens by itself.

**In the stored option.** The accepting option's action is
`StoredInteractionAction::ApplyOperation` whose `arguments` carry the same
artifact. The client sends an option identifier and never an artifact, so the
command is compiled from what the server stored when it rendered the card. A
card that outlived a re-publication therefore compiles into a command naming the
version the person actually saw, and `validate_command` refuses it because that
version is no longer published. The acceptance fails; it does not silently
become an acceptance of something else.

**In the committed event.** The event records document, version and digest
alongside who accepted. The receipt is rendered from the event, so the version
reaches the user without the reply's writer being trusted to remember it, and the
audit years later reads the event rather than a row.

### 4. Retention is the event ledger's

Because the acceptance is a committed event, it is retained under the rule the
event ledger already has, and it is reconstructible: folding the ledger alone
reproduces which version was accepted, by whom, and when. There is no separate
retention mechanism to build, and, more to the point, no `accepted BOOLEAN`
column that a later write can flip without leaving a trace.

If the application must keep the accepted bytes themselves, store them where
artifacts are stored and keep the digest in the event. The event stays small and
the proof stays intact: a stored document whose digest does not match the event
is a document that was replaced.

### 5. Gating another workflow is a projection

The gated workflow reads committed consent when it loads its case, and projects
the gate as a phase:

- while the required version is not on file, the phase is blocked, carries an
  obligation naming the document, and carries a notice explaining what is
  missing;
- the send operation is not among the operations on offer at all in that phase,
  so the model has no vocabulary for it;
- `validate_command` refuses it anyway, because a gate that exists only in the
  vocabulary is a gate a replayed command walks straight through.

The blocked phase is `PhaseOwnership::External` rather than `User`, and that is
deliberate. A user-owned phase must raise a blocking card, and the only card
that would unblock this one belongs to the person's consent case: raising it
here would bind a legal acceptance to a trip revision, which is the shape
this whole document rejects. From the gated workflow's point of view the fact it
is waiting for is outside itself, which is exactly what `External` means.

An acceptance of the *previous* version does not open the gate. The gate is on
the version, not on the existence of some acceptance.

---

## What not to do

- **Do not hang the terms card on the case being gated.** It binds a fact about
  a person to a trip revision, so editing the trip invalidates the legal
  acceptance and accepting the terms is scoped to one document.
- **Do not make the acceptance card revision-independent** to work around the
  previous point. Revision independence is for cards that genuinely do not
  depend on state; it is not a way to attach an unrelated question to a case.
- **Do not store `accepted: true`.** Without the version and the digest it is
  not evidence, and the day the terms change it is quietly wrong.
- **Do not let the acceptance authorize the gated command.** The origin minted
  by accepting is bound to the consent card; the send has its own confirmation.
  Two clicks, two origins, two audit trails.
- **Do not treat declining as an answer that authorizes anything.** It resolves
  the card and mints no origin at all, which is what `ActionClass::NoCommands`
  is for.

---

## Withdrawal, and other shapes of the same thing

Withdrawal is another command on the same workflow, and it produces another
event; the ledger keeps both, which is what makes "consented from March to
November" answerable. A signature requirement rather than a click is the same
recipe with `ConfirmationPolicy::QualifiedSignature` and an
`InteractionKind::ExternalSignature` card. A consent that must be re-confirmed
periodically is the published artifact plus an expiry in the projection, not a
new kind either.

---

## The test behind this

[`crates/turnframe-core/tests/consent_acceptance_recipe.rs`](../crates/turnframe-core/tests/consent_acceptance_recipe.rs)
builds both workflows and checks, in order: that the consent case is the person;
that the artifact version and its digest are inside the card's hash and that the
origin cites that hash; that the compiled command and the committed event carry
the version, and that folding the ledger reproduces it; that a card raised for
the old version cannot accept the new one; that publishing a new version reopens
the obligation; that the gate on the other workflow is a projection over
committed consent and rejects an acceptance of the previous version; and that
accepting authorizes nothing about the gated case.

It runs with `cargo test -p turnframe-core --test consent_acceptance_recipe`.
