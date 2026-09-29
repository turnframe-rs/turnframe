# ADR-011: Typed generic domain APIs with internal type erasure (and no proc-macro DSL in 0.1)

- Status: Accepted (2026-09-05)

## Context

Turnframe hosts several workflows inside one runtime. A trip workflow, a traveler workflow and a
support-ticket workflow each carry their own state, phases, obligations, commands, events and
outcomes, yet a single `TurnReducer` must plan one user turn across all of them, a single
`WorkflowRegistry` must look them up by key, and a single `EventLedger` must record what they
committed. Two forces pull in opposite directions.

The first force is the domain author's need for type safety. A workflow author who writes
`compile_act`, `validate_command` or `receipts` against a concrete `TripCommand` enum gets the
compiler to prove that every command variant is handled, that a `ClassifyLine { line_id }` command
carries a real identifier, and that `receipts` only ever sees the events this workflow actually emits.
When the same author is instead handed `serde_json::Value`, three things go wrong in a conversational
application:

- A user asks to "change the travel date to the 30th" while the model proposes a field name the domain
  never defined. With an untyped `set_field` surface the write lands on an unknown key, the projection
  never notices, and the card the user sees next describes a state that the domain does not recognise.
- A policy that must mark `Submit` as high risk and `SetSubject` as immediate is written against a
  string field name. A rename in the domain silently turns the high-risk command into an immediate one
  and the confirmation card stops being asked for.
- A receipt renderer that reads `event["kind"]` misses a new event variant and the user is told
  nothing happened after an effect committed, which is the operational claim defect that ADR-005 exists
  to prevent.

The second force is the runtime's need for heterogeneity. Registries, journals and the reducer cannot
be generic over every workflow type at once; somewhere the concrete types have to be erased so that a
`Vec` of workflows can exist and a persisted command batch can be routed by `WorkflowKey`. If erasure
is done carelessly it leaks into the public surface and the first force is lost.

A third, smaller force is the temptation to hide the resulting boilerplate behind a procedural-macro
DSL before anyone has written enough workflows to know what the boilerplate actually is. Spec §0
rule 11 says to prefer explicit, typed, boring code and to introduce macros only after at least three
real domain implementations prove the repeated pattern; spec §34 says there is no proc-macro DSL in
v0.1 unless normal Rust implementations have proven the exact boilerplate to remove. Spec §8.3
gives the shape of the answer: typed `WorkflowDefinition` and `WorkflowExecutor<W>` for authors, an
internal `ErasedWorkflow` trait for the registry, and a `TypedWorkflowAdapter<W, E>` that serializes
only at the registry boundary.

## Decision

1. The public domain authoring surface MUST be the typed `WorkflowDefinition` trait with its
   associated `State`, `Phase`, `Obligation`, `Command`, `Event` and `Outcome` types, plus the typed
   `WorkflowExecutor<W: WorkflowDefinition>` trait for loading and executing against a store. These
   are the only traits a domain author implements to participate in the Flow Map.
2. `serde_json::Value` MUST NOT be the primary domain API. No public method that a domain author
   implements or calls in the ordinary course of writing a workflow takes or returns an untyped JSON
   value where a domain type exists. Generic field commands MAY exist internally for simple flat
   forms, but public workflow semantics and `CommandPolicy` decisions MUST be expressed on typed
   command variants (spec §14.1).
3. Heterogeneity MUST be achieved through an internal object-safe `ErasedWorkflow` trait consumed by
   `WorkflowRegistry`, and a `TypedWorkflowAdapter<W, E>` that wraps a concrete
   `(WorkflowDefinition, WorkflowExecutor)` pair and implements `ErasedWorkflow` for it.
4. Serialization and deserialization of state, commands and events MUST happen only inside the
   adapter, at the registry boundary. Inside the adapter the domain's own types are used; outside it
   the runtime handles opaque, versioned payloads keyed by `WorkflowKey`.
5. The adapter MUST validate that the schema version carried by a persisted state, command batch or
   event matches the `WorkflowVersion` of the registered definition before deserializing into domain
   types. A mismatch MUST be reported as a typed error and MUST NOT be executed (spec §8.3, I19).
6. `ErasedWorkflow` MAY be exported for advanced adopters who build their own registries, but it
   MUST be marked as an unstable, runtime-facing surface and MUST NOT appear in documentation aimed
   at domain authors.
7. The 0.1 release MUST NOT ship any procedural macro or derive that generates `WorkflowDefinition`,
   command enums, policy tables or receipt renderers. The `turnframe-macros` crate is reserved and
   MUST stay empty in 0.1.
8. A proc-macro DSL MAY be proposed for a later minor version only when at least three real,
   independently written domain implementations exist in the workspace or in known adopters, and the
   proposal names the exact repeated lines each macro removes. The proposal MUST be a new ADR.
9. Declarative macros (`macro_rules!`) MAY be used inside `turnframe-test` for provider
   conformance suites and fixture generation, because they are test-only and remove no domain
   boilerplate.

## Consequences

Positive:

- Domain authors get exhaustive matching, refactor-safe renames and compiler-checked policies on
  their own command and event enums, which directly protects the safety properties that ADR-001,
  ADR-004 and ADR-005 depend on.
- The runtime, the reducer and the stores stay generic-free and object-safe, so a registry can hold
  any number of workflows and a persisted command batch can be routed without monomorphizing the
  whole orchestrator per workflow.
- Because serialization happens in exactly one place per workflow, schema-version checks, redaction
  hooks and replay records (I20) have a single choke point to instrument.
- Without a DSL the 0.1 API stays inspectable: everything a domain does is ordinary Rust that a
  reviewer can read, step through and test with plain `cargo test`.

Negative:

- Writing a workflow is verbose. Six associated types, each with `Serialize`, `DeserializeOwned`,
  `Clone`, `Send`, `Sync` and `'static` bounds, plus eight trait methods, is real ceremony that a
  macro could eventually shorten.
- The adapter pays a serialization round-trip at the registry boundary. The spec requires benchmarks
  for projection, reduction and persistence overhead; this ADR makes no claim about that cost until
  those benchmarks exist.
- Two representations of the same command (typed inside the adapter, opaque outside) can drift if a
  workflow bumps its types without bumping `WorkflowVersion`. Decision 5 turns that drift into a
  loud typed error rather than a silent misexecution, but it still surfaces as a runtime failure
  rather than a compile-time one.

What adopters must do:

- Implement `WorkflowDefinition` and `WorkflowExecutor<W>` with concrete enums and structs. Do not
  reach for `serde_json::Value` fields in commands or events to "keep options open".
- Bump `WorkflowVersion` whenever state, command or event shapes change, and provide a migration or a
  rejection path for older persisted payloads.
- Register each workflow through the builder shown in spec §29; never construct or store erased
  workflows by hand unless you are deliberately building a custom registry on the unstable surface.
- Resist writing local proc-macros around `WorkflowDefinition` in 0.1. Copy the boilerplate, count
  it, and bring the count to the ADR that would introduce a DSL.

## Alternatives considered

1. **Untyped JSON domain API (`serde_json::Value` for state, commands and events).** Simplest for
   the runtime: no erasure layer, one code path. Rejected because it moves every domain guarantee
   from the compiler to runtime string matching, reintroduces `set_field`-style commands that the
   spec explicitly disfavours (§14.1), and produces exactly the misrouted write, mis-scored policy and
   missing receipt failures described in Context. Spec §8.3 rules it out directly.
2. **Fully generic runtime (`Orchestrator<W>` per workflow, no erasure).** Preserves types end to
   end and needs no adapter. Rejected because a conversational turn routinely touches more than one
   workflow (a card confirmation on a traveler case plus a new trip act in the same message, spec
   §29), so a runtime that is monomorphic over a single `W` cannot host the multi-workflow, multi-act
   turns the product requires without a registry of heterogeneous workflows, which brings erasure back
   anyway.
3. **Proc-macro DSL from day one (`#[derive(Workflow)]`, `#[command(risk = "high")]`).** Attractive
   because it hides the associated-type ceremony. Rejected for 0.1 because no three real domain
   implementations exist yet to prove which lines repeat, because macro-generated trait impls are
   harder to audit at a safety boundary, and because spec §0 rule 11 and §34 forbid it until the
   boilerplate is demonstrated. The crate name is reserved so the door stays open.
4. **Trait objects in the public API (`Box<dyn WorkflowDefinition>` with `dyn Any` payloads).**
   Rejected because `WorkflowDefinition` has associated types and cannot be object-safe as written;
   forcing object safety onto the author-facing trait would collapse it into the untyped alternative.

## Enforcement

Invariants from spec §4 implemented or protected by this decision:

- I2 (projection is pure): the typed `project` signature takes `Option<&Self::State>` and returns a
  `WorkflowView`; the adapter only deserializes and forwards, adding no I/O.
- I9 (model output is a proposal): `compile_act` turns a `ResolvedAct` into typed `Self::Command`
  values; there is no path from model JSON to a command that bypasses the domain's typed compiler.
- I16 (events authorize claims): `receipts` is typed over `&[Self::Event]`, so a receipt cannot be
  rendered from anything other than this workflow's committed event type.
- I19 (critical state reads fail closed): the adapter's schema-version check rejects a persisted
  payload whose version does not match the registered definition instead of guessing.
- I20 (replay is possible): the workflow version and schema version are recorded at the single
  serialization boundary.

Tests and release gates that prove it:

- §27.1 pure unit tests: workflow projection and policy evaluation run against typed domain values;
  a unit test on the adapter asserts that a version-mismatched payload yields a typed error and
  performs no execution.
- §27.2 property tests: serialization round trips through the adapter for arbitrary `State`,
  `Command` and `Event` values, and the "no high-risk command without trusted origin" property runs
  on typed command variants rather than string names.
- §27.3 state exploration tests: `WorkflowModel<W>` in the test kit (`turnframe-test`) is typed over `W`, so exploration
  exercises the same enums the runtime executes.
- §33 workflow gates "Projection behavior is versioned" and safety gate "No consequential command can
  originate from raw model output" both rely on the typed boundary described here.
- A workspace check that `turnframe-macros` exports no procedural macros in the 0.1 series.

Responsible crates and modules:

- `turnframe-core` owns `WorkflowDefinition`, `WorkflowView`, `WorkflowKey`, `WorkflowVersion` and
  the typed command envelope types.
- `turnframe-runtime` owns `WorkflowExecutor<W>`, `ErasedWorkflow`, `TypedWorkflowAdapter<W, E>`
  and `WorkflowRegistry`, and is the only place where domain payloads are serialized or deserialized.
- `turnframe-test` owns `WorkflowModel<W>` and the property-test helpers.
- `turnframe-macros` is reserved and empty; any change to that fact requires a superseding ADR.
