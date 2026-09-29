# turnframe-tasks

Small, typed model tasks for Turnframe.

A task is one narrow question put to a model: a short instruction, the context the
question needs and nothing else, and an answer held to a strict schema built for
this call. The engine runs it under the task kind's own profile (model tier,
temperature, output cap, reasoning effort, votes, repairs, escalation), inside the
turn's budget, and records every call it makes.

Structural checks belong to the task; a failed check is sent back to the same model
once with the exact error. Votes are compared with the task's own notion of
agreement, and a vote without a majority escalates, asks, or fails, as the profile
says. A call that would exceed the budget is not sent.

See `docs/adr/ADR-016-understanding-is-a-bounded-set-of-small-verified-model-tasks.md`.
