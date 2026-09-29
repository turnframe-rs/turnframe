# turnframe-understand

Turn understanding for Turnframe, as small verified model tasks.

A message is split into units, and each unit is routed to the operations it asks for.
Each of those acts is located on a record, and its arguments extracted and verified. Each
step is its own short task, run by `turnframe-tasks` under its own profile and the turn's
budget. Code then assembles the
answers into one `Understanding`, which the reducer reads. Nothing has an effect until the
whole message is understood.

Models judge language and code checks structure. A task points at the user's words by
number; a text value also copies them, which may narrow the pointer and never move it.
Dates and amounts come back as expressions that code evaluates. Every choice is a closed set built for the call. A verifier checks each
value against what the user said, and it can only take away: a value it doubts is asked
for again.

Each decision is also published as a `Step`, so an application can show the reading as it
happens.

Words segmentation read as small talk and coverage as an act go back to segmentation once,
told what coverage saw; a second reading of small talk runs nothing. A turn's `Settings` can
ask for more: a `cross_check` of the whole reading against the message, whose findings send one
step of one act back and whose last round holds what it still doubts. The runtime's `high`
effort turns it on (ADR-020).

See `docs/adr/ADR-015-models-judge-language-code-checks-structure.md` and
`docs/adr/ADR-016-understanding-is-a-bounded-set-of-small-verified-model-tasks.md`.
