# Security Policy

Turnframe is a framework for deterministic conversational workflows. Its central promise is a
security property: models propose meaning, deterministic reducers decide effects, and committed
events decide claims. A bug that lets untrusted input bypass that chain is treated as a
vulnerability, not as a functional defect.

## Supported versions

| Version | Supported                                   |
| ------- | ------------------------------------------- |
| 0.1.x   | Yes, security fixes land in the latest 0.1 patch release |
| < 0.1   | No, pre-release snapshots receive no fixes  |

Until the project reaches 1.0, only the most recent minor line receives fixes. When a new minor
line is published, the previous line remains supported for security fixes for 90 days.

## Reporting a vulnerability

Please do not open a public issue for anything you believe is a security problem.

Report it privately through the repository's Security tab, "Report a vulnerability" (GitHub
private vulnerability reporting). Only the maintainers can read the report.

Include the affected crate and version, a description of the invariant you believe is violated, a
minimal reproduction (a failing test against `turnframe-test` fixtures is ideal), and your
assessment of impact. You will receive an acknowledgement within 3 working days and a triage
decision within 10 working days.

## Coordinated disclosure

We follow a 90-day coordinated disclosure window, counted from the acknowledgement. During that
window we will keep you informed of progress, agree on a publication date, and credit you in the
advisory unless you prefer otherwise. If a fix ships earlier, disclosure may be brought forward by
mutual agreement; if a fix is genuinely blocked, we may ask for an extension and will explain why.
Fixes are published as patch releases together with a GitHub Security Advisory and a `CHANGELOG`
entry marked `Security`.

## What counts as a security issue in this library

The master specification defines a set of fundamental safety invariants (spec §4, I1–I20). Any
reproducible path that violates one of them inside the framework's own code is a vulnerability,
regardless of whether it needs a malicious model, a malicious user, or just an unlucky race.
Concrete examples:

- **Model output reaching a critical command** without passing through schema validation,
  evidence validation, target resolution, policy checks and the TurnReducer (I9, I12). A raw
  proposal from a TurnInterpreter must never be a valid origin for a consequential command.
- **Client-defined CTA semantics** (I7): an interaction response whose meaning is taken from the
  client payload instead of the stored Interaction option.
- **Cross-tenant existence leak** (spec §25.4): any store lookup, Interaction, target token,
  command or event that lets a caller with a guessed identifier learn that another account's
  record exists, including through timing or error-message differences.
- **Claim without event** (I16): a response block that asserts an operational outcome ("sent",
  "saved", "deleted") that is not backed by a committed EventLedger entry or a verified external
  receipt.
- **Partial execution of a model array** (I18): executing a valid subset of acts when another act
  in the same structured response was malformed.
- **Repeated effects across provider failure** (I14, I17): a retry or fallback that re-executes a
  mutation plan after an effect may have committed, or an idempotency key that is not stable.
- **Fail-open critical reads** (I19): executing a command when interaction ownership, case
  revision, authorization or confirmation state could not be verified.
- **Ambiguous target mutation** (I8): selecting a mutation target by recency, order or model
  confidence when more than one candidate remains.
- **Secret or unredacted provider payload exposure**: API keys, authorization headers or raw
  provider bodies appearing in logs, traces, metric tags, `Display` output or error messages.
- **Prompt injection escalation**: retrieved content, tool output or attachments that manage to
  change domain authorization or command policy rather than being treated as data.
- Memory-safety issues in `unsafe` code, dependency advisories with a reachable path, and
  deserialization of untrusted input that can panic or exhaust resources in the runtime.

## What is not a security issue

- A model producing wrong, misleading or invented content in **narration text**, as long as no
  claim about an operational effect is made without a backing event. Narration quality is a
  product and evaluation concern (`turnframe-eval`), not a security boundary.
- A model refusing, timing out or returning low-quality proposals that the reducer correctly
  rejects or turns into a clarification.
- Vulnerabilities in a hosting application's own command handlers, authentication middleware or
  domain logic. Turnframe treats the actor context it receives as already authenticated.
- Issues in upstream model providers' services themselves. Report those to the provider.
- Missing hardening features that the specification lists as optional (regional routing,
  retention policies) when they are documented as not yet implemented.

If you are unsure which side of the line a finding falls on, report it privately anyway.

## Hardening guidance for adopters

- **Secrets.** Pass provider credentials through the secret-wrapping types the provider crates
  expose (built on `secrecy`), never through prompts, configuration echoed to logs or plain
  `String` fields. Keep external credentials inside your command handlers; the interpretation
  layer must never be able to read them.
- **Redaction.** Enable the redaction hooks on provider requests and on your tracing subscriber.
  Authorization headers and raw provider bodies are redacted by default; do not disable that in
  production. Never place PII in metric tag values.
- **Provider allowlists.** Configure the provider router with an explicit allowlist per data
  sensitivity level and per tenant, so that a workflow handling sensitive fields cannot be routed
  to a provider that is not cleared for it. Apply field-level redaction before model calls where
  the domain requires it.
- **Tenancy.** Scope every store, Interaction, target token, command and event to an account id
  derived from your authentication middleware, not from request bodies. When using
  `turnframe-store-postgres`, keep the `tf_*` tables behind a database role that cannot bypass
  the account column, and consider row-level security for defense in depth.
- **Trust boundaries.** Treat user text, attachments, model output, provider metadata, client
  option values, retrieved documents and unverified callbacks as untrusted (spec §25.1). Verify
  external callback signatures before they can produce events.
- **Determinism as a safeguard.** Keep your WorkflowDefinition projection pure and free of I/O,
  keep critical commands behind server-issued origins such as a confirmed Interaction, and keep
  the release-gate invariant tests from `turnframe-test` in your own CI.
- **Dependencies.** Run `cargo audit` and `cargo deny` (the repository ships a `deny.toml`) on
  every build and pin to a supported release line.
