# Roadmap

What is deliberately not in the 0.1 series, and why. Everything here is a decision, not a backlog
of forgotten work: each item was considered, and the reason it waits is recorded so the decision can
be revisited on evidence rather than on memory.

The 0.1 milestones themselves are described in the [architecture guide](architecture.md) and gated
by the [release checklist](release-checklist.md).

---

## Reads before understanding

**Status:** declared, not run in 0.1.

An operation can name the reads whose results its argument filler should see
(`OperationSpec::context_read`), and the task set has an `investigate` task with a profile of its
own, switched off. Neither runs yet. The small tasks were built first without reads because every
task in the chain already sees the records in view, and that covers the sample domains. A read
earns its place when a domain needs a value no record in view holds, such as a traveler looked up
by a name the user typed. It will stay read-only and run before reduction, as ADR-009 requires.

## Model Context Protocol adapters

**Status:** optional, described by the specification, not shipped in 0.1.

Importing external resources as knowledge sources is the natural half: what they return reaches
only the answers, labelled with its source and trust. Exposing selected application operations to
external clients is the harder half, because a tool declaration is not an authorization: write
authorization and confirmation stay in the command path regardless of who is calling. The adapter
waits until knowledge sources have been exercised by real applications.

## The remaining provider adapters

**Status:** after the shipped adapters have passed the conformance suite against real endpoints.

The 0.1 set covers OpenAI and OpenAI-compatible endpoints, Anthropic, Gemini, Bedrock and Ollama.
Azure OpenAI, Mistral, Cohere, xAI and Groq are either profiles of the compatible adapter or thin
native adapters; each one is published only once it passes the same conformance suite, because
conformance is a property of a provider and model combination and never of a brand.

## A procedural macro layer

**Status:** deliberately absent, and not scheduled.

The workspace reserves the macro crate name and ships no macros. A derive for workflow definitions
is introduced only after at least three real domain implementations have proven which boilerplate is
genuinely repeated. Until then, explicit typed code is easier to read, to debug and to replay.

## A hosted evaluation and dashboard surface

**Status:** out of scope.

The evaluation harness produces results; where they are stored and how they are visualized belongs
to the adopter's existing tooling. The reliability dashboard is described as a mapping from panels
to metrics so it can be built on whatever backend already exists.
