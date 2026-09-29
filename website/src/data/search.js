// What a search result shows for each page: its title tag and meta description, written for the
// words people search with. The page itself keeps its own heading. Decision records are
// described from their status line (src/lib/docs.mjs).

export const HOME = {
  title: 'Turnframe: deterministic LLM workflows for Rust',
  description:
    'A Rust library for LLM assistants that change real records. Small checked model tasks read each message, typed reducers decide effects, events back every claim.',
};

export const DOCS = {
  '': {
    title: 'Introduction to Turnframe, deterministic LLM workflows in Rust',
    description:
      'Turnframe is a Rust library for conversational LLM apps that write to real records: models propose meaning, reducers decide effects, events decide claims.',
  },
  installation: {
    title: 'Install Turnframe: the Rust crate, feature flags and providers',
    description:
      'Add Turnframe to a Rust project: the dependency line, feature flags for OpenAI, Anthropic, Gemini, Bedrock, Ollama and PostgreSQL, and the crate family.',
  },
  quickstart: {
    title: 'Quickstart: one LLM conversation turn in Rust · Turnframe',
    description:
      'One conversational turn end to end in Rust, with no API key, database or network: understanding, reduction, a commit and a receipt backed by its event.',
  },
  examples: {
    title: 'Rust LLM examples: a travel desk and a live console · Turnframe',
    description:
      'Runnable Rust examples: a travel desk that walks four guarantees, traveler onboarding, a question and an action in one turn, and a console for real LLMs.',
  },
  'flow-map': {
    title: 'The Flow Map: the architecture of an LLM workflow turn · Turnframe',
    description:
      'The five steps of every Turnframe turn: persisted state sets the view, small model tasks propose, reducers decide, cards authorize, events back claims.',
  },
  architecture: {
    title: 'Architecture guide: the Flow Map turn pipeline · Turnframe',
    description:
      'How Turnframe reads a message with small verified LLM tasks, reduces the whole turn to typed commands under policy, and lets only committed events back a reply.',
  },
  'reliability-model': {
    title: 'Reliability model for LLM workflows · Turnframe',
    description:
      'Five reliability properties for an LLM workflow: three hold by construction, two are probabilistic and may only fail safe. How each is enforced and measured.',
  },
  interactions: {
    title: 'Persistent interactions: durable confirmation cards · Turnframe',
    description:
      'Confirmation cards as persistent, server-owned records: options whose meaning the server keeps, revision binding, stale clicks, and one blocking card per case.',
  },
  composition: {
    title: 'What an LLM reply may claim: event-backed receipts · Turnframe',
    description:
      'What an assistant is allowed to say: receipts rendered from committed events, an acknowledgement reviewed before it is shown, and an answer for every question.',
  },
  'provider-adapters': {
    title: 'LLM provider adapters: OpenAI, Anthropic, Gemini, Bedrock, Ollama',
    description:
      'One provider-neutral model layer for OpenAI, Azure OpenAI, Anthropic, Gemini, Vertex AI, AWS Bedrock and Ollama, with capability routing and bounded fallback.',
  },
  persistence: {
    title: 'The persistence contract: store traits and their rules · Turnframe',
    description:
      'The seven store traits behind Turnframe, the rules every implementation keeps, the atomicity model, and how to bring your own database.',
  },
  'revision-migration': {
    title: 'Adding a revision column to an existing database · Turnframe',
    description:
      'Putting an optimistic-concurrency revision on PostgreSQL tables that never had one, so Turnframe can run over the database you already have.',
  },
  telemetry: {
    title: 'Telemetry: metrics and OpenTelemetry for LLM turns · Turnframe',
    description:
      'The turnframe.* metrics, tracing spans and OpenTelemetry bridge, with prompt and completion text kept out by default and no user text in any label.',
  },
  evaluation: {
    title: 'Evaluating an LLM assistant per turn and per task · Turnframe',
    description:
      'Evaluate an LLM assistant against a contract: deterministic assertions per turn, a score per understanding task, and samples kept apart from judge votes.',
  },
  recipes: {
    title: 'Recipes from the sample domains · Turnframe',
    description:
      'Shapes the sample domains prove without new framework types: values read from a document and reviewed, fields with three states, and proving an executor.',
  },
  'consent-and-acceptance': {
    title: 'Recording consent and legal acceptance · Turnframe',
    description:
      'Modelling a legal acceptance with the types Turnframe already has: a workflow of its own, an ordinary card, a bound artifact version and the event ledger.',
  },
  'canary-and-rollback': {
    title: 'Canary releases and rollback per workflow · Turnframe',
    description:
      'Releasing a conversational assistant one workflow at a time: a shadow run, an authoritative slice, then everywhere, and how to roll one workflow back.',
  },
  'threat-model': {
    title: 'Threat model: prompt injection and trust boundaries · Turnframe',
    description:
      'Trust boundaries of an LLM app that writes records: malicious text, prompt injection, tampered clicks, cross-tenant guessing, and what contains each one.',
  },
  'security-policy': {
    title: 'Security policy and reporting a vulnerability · Turnframe',
    description:
      'How to report a vulnerability in Turnframe, which versions are supported, what counts as a security issue, and hardening guidance for applications.',
  },
  benchmarks: {
    title: 'Benchmarks: the deterministic core and a live LLM corpus · Turnframe',
    description:
      'Microbenchmarks of the deterministic core and the pass rate of a live LLM corpus, with what each number shows and what it does not claim.',
  },
  roadmap: {
    title: 'Roadmap: what waits until after 0.1, and why · Turnframe',
    description:
      'What Turnframe leaves out of 0.1 on purpose: reads before understanding, MCP adapters, more providers, a macro layer and hosted evaluation.',
  },
  'release-checklist': {
    title: 'Release checklist and production-readiness gates · Turnframe',
    description:
      'The gates Turnframe must pass before it is called production-ready: safety, workflow, conversation, provider and operational checks, each with its evidence.',
  },
  changelog: {
    title: 'Changelog · Turnframe',
    description:
      'Every notable change to Turnframe, the Rust library for deterministic conversational LLM workflows, in the Keep a Changelog format.',
  },
  contributing: {
    title: 'Contributing to Turnframe',
    description:
      'How to contribute to Turnframe: the development setup, the local check sequence CI mirrors, what a pull request honours, and how to add an adapter or a store.',
  },
  adr: {
    title: 'Architecture decision records · Turnframe',
    description:
      'The load-bearing decisions behind Turnframe: model output, reducers, events, cards, providers and effort, each with its alternatives and its enforcement.',
  },
  api: {
    title: 'Turnframe API reference: rustdoc on docs.rs',
    description:
      'The Rust API reference of Turnframe: rustdoc for the facade and every crate on docs.rs, how to build it locally, and the types to read first.',
  },
};
