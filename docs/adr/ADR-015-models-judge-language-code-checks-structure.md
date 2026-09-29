# ADR-015: Models judge language; code checks structure

- Status: Accepted (2026-09-26); decision 5 amended the same day

## Context

Turnframe keeps effects deterministic by checking model output before anything happens. Many of
those checks were written as string comparisons over natural language: a quote had to be a
substring of the message, a value had to appear in the words an act cited, an intention was
accepted when a declared alias occurred in the message, a record was chosen when its label
appeared in the text, a claim was blocked when certain words appeared in the reply, and an echo was
detected by comparing folded sentences.

A check of that shape passes the failure it was written for. Asked «the name needs changing», a
model can write `"needs changing"` as the name; those words are in the message, so the
quoted-value check admits it and the write commits. Aliases, carrier-word lists and keyword checks
are dictionaries by another name: they are language-specific, they go stale, and each one needs
another exception.

## Decision

1. **Code checks structure.** Code may decide by closed-set membership (identifiers, enums,
   option ids, pointers into the message), schema validity, arithmetic, policy, revisions,
   idempotency, and whether a committed event backs a claim.
2. **Code does not decide meaning by comparing natural-language strings.** No substring,
   case-folding, alias, keyword or word-overlap comparison decides what a user meant, whether a
   value was stated, which record was named, or what a reply claims.
3. **Semantic cross-checks are model tasks.** Whether a value is what the user said, whether a
   message holds a request nobody covered, whether a reply claims something its facts do not back:
   each is a small model task with its own prompt, model and limits.
4. **A semantic check can only take away.** It may block, request a repair, or turn an act into a
   question. It never authorizes an effect; policy, confirmations and events still decide those.
5. **Point, do not quote.** A task that refers to the user's words returns positions in a numbered
   rendering of the message, and code slices the exact words. A text value also copies its words,
   because a small model counts word numbers worse than it copies: the copy may only narrow the
   pointer to a run of the words it already covers, compared without case or edge punctuation. It
   never moves the pointer, never becomes the value, and words the pointer does not hold are sent
   back for a repair rather than looked for elsewhere.
6. **The model understands; code computes.** A relative date or an amount is returned as a typed
   expression (`DateExpr`, `Money`) and evaluated by code with the turn's clock and locale.

## Consequences

- Quote grounding, the quoted-value check, dictated-value extension, alias matching, lexical record
  naming, `ClaimVocabulary` and the echo detector are removed.
- Evidence becomes pointer-exact (structural) and model-verified (semantic).
- More model calls per turn, each small. Their cost and latency are bounded by ADR-016's budgets.
- A verifier's false negative costs a question to the user, never a wrong write.

## Alternatives considered

1. **Sharper string checks.** Every refinement moves the failure; none can tell a value the user
   gave from words that name the field.
2. **Language-specific dictionaries.** They grow per language and per domain and still miss
   paraphrase.
3. **No cross-check at all.** Leaves the first model's mistakes unexamined, which is the status quo
   this record replaces.

## Enforcement

- `turnframe-understand` has no API that takes two natural-language strings and returns a
  decision; review rejects one.
- Task records on the replay record show every verification verdict.
- The live corpus includes items where the only words available are carrier words.
