# Recipes from the sample domains

The sample workflows in `turnframe-test` are arguments as much as fixtures: each
shows that a shape people ask the framework to grow a word for is already
expressible. This page holds the reasoning; the code holds the proof.

## Proposed values awaiting review (`workflows::claim`)

A document arrives, something reads values out of it, those values are proposed
but not applied, a card asks the user to confirm them, and only then do they
become state. Expense capture, identity onboarding, contract review and trip
ingestion all have this shape.

A proposal is **domain state**, and modelling it as such gets every property the
request asks for out of the vocabulary that already exists. A framework that grew
the concept would force every projector, including the ones for domains that
never see a document, to reason about a state they do not have.

**1. Put the proposal in the state.** `Proposal` is an ordinary field of
`ClaimState`, holding the values read from one `AttachmentId` together with a
per-value `edited` mark. Nothing in `turnframe-core` knows what it is. Accepting
the review copies those values into `RecordedField`s, which keep their
provenance, so "where did this number come from, and did a human change it" stays
answerable after the proposal is gone.

**2. Let the projector say it in the domain's own words.** While a proposal is
open the view carries one `ReviewProposedField` obligation per proposed value and
one `ProvideField` obligation per required value the document did not yield,
both parameterized, so three proposed values are three distinct obligations
rather than one checkpoint that flickers. A notice names the document and counts
the values, and a second one appears once a human has changed any of them. That
is "these N fields are proposed from attachment A", expressed in obligations and
notices.

**3. Put the proposal in the payload, and the hash follows.** The review card is
an `InteractionKind::ReviewChanges` whose diff entries are the proposed values and
whose metadata carries a digest of the proposal alone. The payload hash therefore
covers the proposal and not the rest of the case: editing the hand-typed
reference, which no proposal covers, leaves the hash untouched, while changing one
proposed value changes it. That is not a feature; it is what putting the proposal
in the payload *means*. Contrast `workflows::trip`, whose send card hashes a
preview of the whole trip, because the whole trip is what that card is
about. Note also what is not in the metadata: the case revision. Revision binding
is the interaction's own mechanism, and copying the revision into the payload
would make the hash change on every unrelated edit, exactly the property being
sought here.

**4. Make editing a proposed value a different operation from answering.**
`REVISE_PROPOSED_FIELD` and `ACCEPT_PROPOSAL` are two keys, two commands and two
policies: revising is low-risk and leaves the review open, accepting is a
`SensitiveDataChange` that a `ReviewCard` must authorize. Being different keys,
they are distinguishable everywhere downstream (plan, journal, receipt, replay
record) without anything inferring intent. There is deliberately **no** operation
key for `ProposeFields`: reading a document is something the application does, not
something a user asks for in a sentence, so the understanding cannot invent a
reading nobody performed.

**5. Choose what abandoning does, explicitly.** The framework's answer to an
abandoned review is the ordinary interaction lifecycle: the card expires or is
invalidated and nothing is committed. What happens to the proposal is the domain's
decision, and this domain makes two different ones. *Not now* declines and commits
nothing, so the proposal survives and the card is derived again from state on the
next turn: declining is not abandoning. *Discard this reading* issues
`AbandonReview`, which drops the proposal, **keeps the document**, records which
document's reading was thrown away, and returns the case to `Extracting` with a
notice, so the same document can be read again. Another domain could freeze the
proposal instead, or discard the document with it. The point is that the choice is
written down in the domain rather than left to whatever the framework happens to
do.

**What it costs.** Two enums, one struct and a projector that reads them. No
change to the view, no change to the interaction kinds, and no obligation on any
other domain to know that proposals exist.

## A collected field has three states, not two (`workflows::traveler`)

Two of the traveler sample's fields are an `Option<String>`, which is the honest
shape for a value that is either there or not. The third is a `FieldState`, and
the difference is the recipe.

A real collection workflow does not have two states per field. It has three: a
field nobody has asked about yet, a field the user answered, and a field the user
was asked about and **declined**. The last two are both empty, and the
distinction lands directly on the obligation model, where getting it wrong
produces a defect every individual turn passes.

Keep the obligation open on a decline and the assistant asks for the same value
on every turn, for ever. Each turn behaves exactly as the projection instructed
it, so no single turn looks wrong; only the conversation does. Drop the
obligation without recording anything and the projection can no longer tell a
decline from an answer, so every later question about completeness (may this
traveler be activated? is this record fit to trip against?) is answered from
a state that has forgotten what happened.

The recipe is three moves:

1. **Make the third state real in the persisted state.** `FieldState` has
   `Untouched`, `Answered` and `Declined`, and `is_settled` is the predicate
   completeness is written against, not `is_some()`.
2. **Close the obligation and keep the reason in the view.** The projection drops
   the obligation as soon as the field is settled and attaches a notice whose
   *code* carries the reason. Putting it in the code rather than only in the
   prose is what lets another projection, a prompt or a report branch on it
   without parsing a sentence.
3. **Let the domain carry which declines are worth revisiting.** A decline has
   several human meanings (the datum does not exist, the user does not know it,
   the user has it and will not share it), and only the middle one can change.
   `worth_asking_again` says so once, in the domain, instead of leaving every
   caller to guess.

Declining is how an open question is closed, not how an answer is erased:
declining a field the user already answered is refused, while a field declined as
`Unknown` can still be answered later.

## Proving your executor, including the half-commit

An implementer who writes `load` and `execute` over their own tables will test
the happy path, will test a stale revision, will probably test a repeated key,
and will still get partial-batch recovery wrong, because producing a
half-committed batch takes deliberate effort and the bug it hides is invisible
until a process dies between two commands.

The defect is not exotic. The executor stores its idempotency memory per *batch*
rather than per *envelope*, so a batch that half-committed has only two possible
answers (all of it and none of it), and both are wrong. Or it re-checks
`current_revision == expected_revision` on the resume, sees the revision its own
half-commit produced, and reports a conflict for a batch that is simply
unfinished. The user is then told their own turn collided with itself.
