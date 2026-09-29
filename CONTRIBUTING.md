# Contributing to Turnframe

Turnframe is a framework for natural conversational applications with deterministic workflows,
durable human interactions and verifiable side effects. Models propose meaning. Deterministic
reducers decide effects. Committed events decide claims. Every contribution is judged first against
that promise and only then against style, performance or convenience.

## Development setup

- Install Rust through `rustup`. The workspace pins the `stable` channel in `rust-toolchain.toml`
  with `rustfmt` and `clippy`; the minimum supported Rust version (MSRV) is **1.88**, declared as
  `rust-version` in the workspace `Cargo.toml`. Changing the MSRV is a reviewed decision, not a
  side effect of picking up a new API.
- Install the auxiliary tools CI uses: `cargo install cargo-deny cargo-audit cargo-semver-checks`.
- Store tests need a PostgreSQL 16 instance. The simplest way is Docker:

  ```sh
  docker run --name turnframe-pg -e POSTGRES_USER=turnframe -e POSTGRES_PASSWORD=turnframe \
    -e POSTGRES_DB=turnframe_test -p 5432:5432 -d postgres:16
  export TURNFRAME_TEST_DATABASE_URL=postgres://turnframe:turnframe@localhost:5432/turnframe_test
  ```

  When the variable is unset, the PostgreSQL store tests are skipped; the in-memory store tests and
  everything else run without a database. Never point the variable at a database you care about:
  the test suite owns the `tf_*` tables it creates.
- Every variable the tests and `examples/console` read is listed in `.env.dev`. Copy it to `.env`,
  which git ignores, and uncomment what you need: those tests and the console load the repository's
  `.env` themselves, and a variable already exported in the shell wins over it. The live provider
  and evaluation tests skip unless their variable is set, and always skip when `CI` is set.

## Local check sequence

Run these before opening a pull request. They mirror `.github/workflows/ci.yml`, including the
`RUSTFLAGS="-D warnings"` and `RUSTDOCFLAGS="-D warnings"` that CI exports, so a warning that is
harmless locally is a failure there.

```sh
export RUSTFLAGS="-D warnings" RUSTDOCFLAGS="-D warnings"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --no-fail-fast
cargo test --workspace --all-features --doc
cargo doc --workspace --all-features --no-deps
cargo +1.88 check --workspace --all-features --all-targets   # MSRV build (rustup toolchain install 1.88 once)
cargo deny --all-features check
cargo audit --ignore RUSTSEC-2023-0071   # rsa, never compiled: see the audit job in ci.yml
cargo bench --workspace --no-run
```

CI additionally runs `cargo-semver-checks` on pull requests and release tags for the published
crates (`turnframe-core`, `turnframe-runtime`, `turnframe-store`, `turnframe-provider`,
`turnframe`). Until a first release exists on crates.io that job only validates manifests, but a
breaking change to a public type must still be called out in the pull request description.

## What a pull request is expected to honour

These expectations restate the working rules of the master specification. Reviewers apply them in
this order.

1. **Safety invariants first, then APIs, then implementation.** A change that touches reduction,
   command execution, interaction resolution or claims must name the invariant it preserves (I1 to
   I20 in the architecture guide) in its description. A PR that wraps a provider SDK or adds a
   generic tool loop without an invariant behind it will be sent back.
2. **The model is never trusted with operational truth.** Model output may read language, propose
   semantic acts and write the reply around what code decided. Code that lets model
   output authorize a consequential effect, or that phrases "created", "sent" or "deleted" without a
   committed domain event or authoritative receipt behind it, is a defect regardless of test status.
3. **Ambiguity fails closed and model answers are all-or-nothing.** An ambiguous target produces a
   clarification `Interaction` and no dependent mutation; a malformed task answer is rejected
   whole, never partially used.
4. **No hidden TODOs on safety boundaries.** A vertical slice is complete only when its revision
   checks, idempotency handling, interaction lifecycle, events, tests and failure behaviour exist.
   `// TODO` on any of those is a blocker, not a note. Put genuinely deferred work in an issue and
   link it.
5. **Typed, boring code.** Prefer explicit types over clever abstraction: `thiserror` errors, no
   `unwrap`/`expect` on runtime paths (tests may opt in with an explicit lint allowance),
   `#![forbid(unsafe_code)]` in generic crates, secrets wrapped and redacted, `tracing` for
   structured logs.
6. **Macros only after three domains.** A macro or proc-macro DSL is accepted only when at least
   three real domain implementations in the repository show the same boilerplate it removes. Until
   then, write the trait implementation by hand.
7. **Provider quirks stay outside the core.** `turnframe-core` must not depend on provider crates,
   database crates or a web framework; provider adapters depend on `turnframe-provider`, not on
   runtime internals. Feature flags gate optional integration surfaces only; they never create
   materially different safety semantics.
8. **No performance or reliability numbers in docs.** Latency and reliability targets are gates
   measured by benchmarks and observation; documentation describes mechanisms, not results.

## Commit messages

Use Conventional Commits: `type(scope): imperative summary`, where `type` is one of `feat`, `fix`,
`docs`, `refactor`, `test`, `perf`, `build`, `ci` or `chore`, and `scope` is a crate short name
(`core`, `runtime`, `store-postgres`, `provider-openai`, ...) or `adr`. Mark breaking public API
changes with `!` after the scope and a `BREAKING CHANGE:` footer. Keep the subject under 72
characters and explain the *why* in the body; the diff already shows the *what*.

## Architecture decision records

Every load-bearing boundary needs an ADR under `docs/adr/`, numbered sequentially
(`ADR-015-short-title.md`) and following the existing sections: Status, Context, Decision,
Consequences, Alternatives considered, Enforcement. A boundary is load-bearing when changing it
would alter a fundamental invariant, the trust boundary between model and reducer, the storage
contract, or the public trait surface of `turnframe-core`. A PR that changes such a boundary must
add or supersede an ADR in the same change; a PR that merely implements an accepted ADR should
reference it. ADRs describe decisions and reasoning, never who made them.

## Adding a provider adapter

1. Create `crates/turnframe-provider-<name>` depending on `turnframe-provider` (and on
   `turnframe-core` for shared types), never on `turnframe-runtime` internals. Implement
   `ModelProvider`: report `ProviderKey`, `ModelKey` and honest `ProviderCapabilities`, and map the
   wire protocol to the normalized `ModelRequest`, `ModelResponse` and `ModelStream` types. Provider
   specific extras go into the typed extension map or an adapter builder, never into core types.
2. Wire the adapter into the provider conformance suite in `turnframe-test`. Every adapter must pass
   the full list: valid structured response, malformed JSON, unknown fields, missing required
   fields, multiple acts, tool/read request IDs, streaming reconstruction, empty output, refusal,
   timeout, rate limit, authentication failure, context overflow, cancellation, retry
   classification, redaction of secrets, and no silent capability downgrade. The suite runs against
   `wiremock` fixtures in CI; live smoke tests are optional and must be gated behind an environment
   variable so CI never needs real credentials.
3. Never fall back to prompt-only JSON when a model cannot meet structured-output requirements for a
   critical stage. Declare the missing capability and let the router reject or reroute.
4. Add the adapter behind a feature flag in the `turnframe` facade crate, document supported models
   in the adapter README, and add an ADR only if the adapter forces a new normalized capability.

## Adding a store implementation

Implement the six store traits from `turnframe-store`: `ConversationStore`, `InteractionStore`,
`CommandJournal`, `EventJournal`, `OutboxStore` and `ReplayStore`. Interaction resolution must be a
compare-and-swap on the stored revision; command idempotency keys must be persisted before or
atomically with execution and must be unique per account; events must be append-only. Run the shared
store contract tests from `turnframe-test` against the new backend the same way the in-memory and
PostgreSQL stores do, and document the transaction scope your backend offers so adopters know which
atomicity guarantees of the reducer they actually receive.

## Review checklist

- Does the PR name the invariant it preserves, and does a test fail if that invariant is broken?
- Are all model outputs treated as proposals, with no path from model text to a write or a claim?
- Are revision checks, idempotency keys and event emission present on every new mutation?
- Does an ambiguous target or malformed model response end in an `Interaction` or a rejection,
  never a partial effect?
- Are errors typed, secrets redacted, and `unwrap`/`expect` absent from runtime paths?
- Does `turnframe-core` still avoid provider, database and web-framework dependencies?
- Is a new abstraction justified by existing repeated code rather than anticipated reuse?
- Are ADRs, rustdoc and the changelog updated, with doc examples that compile as tests?
- Does the full local check sequence pass with `-D warnings`?
