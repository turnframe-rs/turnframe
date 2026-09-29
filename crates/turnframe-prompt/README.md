# turnframe-prompt

Prompt sources for [Turnframe](https://github.com/turnframe-rs/turnframe): prompts
compiled into your binary from files in your own repository, a bounded cache,
and an optional adapter for a Langfuse project.

The *contract* (the `PromptSource` trait, the selector, the error family and
the `PromptRef` a replay record stores) lives in `turnframe-core`. This crate
holds the implementations of it, and nothing else. The runtime depends on the
trait and on no implementation, so an application that configures no source
pulls no prompt-loading code, opens no socket and needs no credential. The
dependency graph says so; the documentation is not being asked to.

## What it is for

A replayable system should be able to answer "which prompt produced this turn".
Before this crate, the prompt reached the model layer as an opaque string, and
the replay record could not name it.

A `PromptRef` fixes that with three fields: the name that was asked for, the
version that came back, and the BLAKE3 digest of the exact text. The digest is
what makes the record worth reading: a name and a version are labels somebody
can move, while a content hash either matches the text or does not.

## Why the repository stays the recommended source of truth

`FilePromptSource` is the setup to reach for in production. The reason is not
convenience.

**A turn's meaning must not change because somebody edited a registry entry
while the system was running.** When the prompt is a file in the repository,
changing what the assistant does is a code review, a commit and a deploy, the
same path as any other behaviour change, recorded in the same history. When the
prompt is fetched at runtime, an edit made in a browser changes a production
system with nobody's name on it, and the turns either side of that edit are no
longer comparable even though the binary never changed.

The second reason is availability. A prompt compiled into the binary cannot fail
to load, so a registry being unreachable cannot stop the assistant from
answering.

This ordering comes from experience rather than taste: an application built on
this architecture moved from runtime prompt fetching to repository-served
prompts with continuous integration mirroring the readable copy into the
registry, and dropped the prompt-to-trace version link on the way, because the
registry had stopped being authoritative.

## Using the file source

Put the prompts in a directory of your own repository, next to `Cargo.toml`:

```text
prompts/
  understand.extract.md
  narrate.acknowledge.md
```

Declare them once. `prompt_dir!` reads the files at compile time, so a missing
or misspelled one is a build error rather than a failed turn:

```rust,ignore
use std::sync::Arc;
use turnframe_prompt::{FilePromptSource, PromptSource};

turnframe_prompt::prompt_dir! {
    /// The prompts this service ships.
    pub static PROMPTS in "prompts" with ".md" {
        "understand.extract",
        "narrate.acknowledge",
    }
}

let source: Arc<dyn PromptSource> = Arc::new(PROMPTS);
```

Hand it to the runtime. Every model task asks it for its own instructions by
name, `understand.<task>` or `narrate.<task>`; a name the source does not hold
keeps the text built into the library. The record of each call cites the prompt
it ran under:

```rust,ignore
let orchestrator = Orchestrator::builder()
    // ... workflows, providers, stores ...
    .prompt_source(source)
    .build()?;
```

There is also an explicit form for when the prompt name and the file name
differ, and a plain table for when you would rather not use a macro at all:

```rust
use turnframe_prompt::{FilePromptSource, PromptFile};

static FILES: &[PromptFile] = &[PromptFile::new("greeting", "Say hello.")];
static PROMPTS: FilePromptSource = FilePromptSource::new(FILES);

let loaded = PROMPTS.get("greeting").expect("declared above");
assert_eq!(loaded.text(), "Say hello.");
assert!(loaded.reference().matches(loaded.text()));
```

### The version nobody maintains

A file's version is derived from its content: the first sixteen hexadecimal
characters of its BLAKE3 digest. Change a word and the version changes; change
nothing and it does not. There is no number to bump and no way for two builds to
claim the same version for different text.

## Opting into the registry

The `langfuse` feature adds `LangfusePromptSource`, which fetches prompts from a
Langfuse project over the **Langfuse v4 public API**: `GET
/api/public/v2/prompts/{name}`, authenticated with the project's public and
secret keys over HTTP Basic. (`v2` there is the prompt resource's own version:
Langfuse v4 versions each resource separately, and prompts are at `v2`.)

Enabling it **introduces a runtime dependency on a remote service**. Every
prompt it serves is an HTTP round trip inside a user's turn, and when Langfuse
is unreachable the instructions come from whatever the cache still holds, or,
on a cold process, from nowhere at all. The file source remains the recommended
default; this exists for teams that want registry-managed prompts and accept
that trade knowingly.

```rust,ignore
use turnframe_prompt::langfuse::{LangfusePromptSource, CLOUD_EU};

let source = LangfusePromptSource::builder()
    .base_url(CLOUD_EU)
    .public_key(std::env::var("LANGFUSE_PUBLIC_KEY")?)
    .secret_key(std::env::var("LANGFUSE_SECRET_KEY")?)
    .build()?
    .into_cached();
```

The secret key is an `ApiKey`: it has no `Display` and no `Serialize`, renders
as `ApiKey(REDACTED)`, and no error variant in this crate has a field it could
reach.

Text prompts are supported. A chat-shaped prompt is refused rather than
flattened into one string, because flattening would change its meaning and the
reference would then name text the registry does not hold.

## Pin a version

If you use the registry, select a version rather than a moving label:

```rust,ignore
use turnframe_prompt::PromptSelector;

let orchestrator = Orchestrator::builder()
    // ...
    .prompt_source(source)
    .prompt_selector(PromptSelector::version("12"))
    .build()?;
```

A pinned version is immutable, so **a registry edit cannot silently change a
running system's behaviour**: changing behaviour goes back to requiring a
deploy, which is the property the repository-served setup has for free. The
pinned version is also what the replay record cites, so an audit of a turn names
something that cannot have been rewritten underneath it. The adapter re-checks
the version the registry returns against the pin and refuses a mismatch rather
than serving it.

Tracking `production` instead is a legitimate choice (an edit takes effect
without a deploy), but it should be a deliberate one.

## Caching, and what a turn survives

`CachedPromptSource` wraps any source with a bounded LRU cache and a freshness
window. Its rule is one sentence: **a fetch failure never takes down a turn
while a previously fetched version is still held.**

Survivable, so the cache logs a warning and serves what it holds: the registry
unreachable, timing out or answering 5xx; a rate limit; a rejected or unentitled
credential; the prompt deleted from the registry; an unparseable answer.

Not survivable: the error reaches the caller, because there is nothing truthful
to serve: the first load of a name and selector on a cold process; a load whose
entry was evicted or explicitly invalidated. A cold start against a registry
that is down is a real outage, and the only defence against it is the file
source.

The runtime adds one more layer above that: if the source still cannot answer,
the stage falls back to the instructions compiled into the library and the
replay record carries **no reference** for it. That absence is the audit signal.

## Links

- [Repository](https://github.com/turnframe-rs/turnframe)
- `turnframe_core::prompt`: the trait, the selector and the reference
- `OrchestratorBuilder::prompt_source` and `prompt_selector` in `turnframe-runtime`: how a
  configured source reaches every model task, and how the reference reaches the replay record

## License

MIT OR Apache-2.0.
