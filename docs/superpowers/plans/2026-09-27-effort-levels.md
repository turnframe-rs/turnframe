# Effort Levels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A turn runs at `low`, `medium` (default, today's behaviour) or `high` effort; `high` spends more model calls (votes, reasoning, a whole-turn check) to read complex messages correctly on the same model.

**Architecture:** Effort is a preset over what the task engine already has. The runtime resolves a turn's level into an `EffortProfile` (task profiles, budgets, pipeline settings); the turn's `TaskScope` carries the profiles so one engine serves every level, and `UnderstandingInput` carries the pipeline settings. `high` adds two pipeline behaviours (re-reading disputed small talk, and a `cross_check` task over the whole turn whose findings send one step of one act back) and one engine option (`Disagreement::Reread`).

**Tech Stack:** Rust 2024 workspace, tokio, serde/TOML, the in-repo task engine (`turnframe-tasks`), scripted providers for tests.

**Spec:** `docs/superpowers/specs/2026-09-27-effort-levels-design.md`

## Global Constraints

- MSRV 1.88: `cargo +1.88 check --workspace --all-features --all-targets` must pass.
- Comment limits (AGENTS.md): `//` at most 4 extras, `///` at most 10 (examples excluded), `//!` at most 15 (tables and examples excluded). State the rule, not the incident.
- Tests: one behaviour per file under `crates/<crate>/tests/`, named as a sentence; unit tests in `mod tests` at the foot of their module. Run the crates touched, not the workspace, inside a task.
- Published copy (docs, CHANGELOG, READMEs, prompts): no em dashes, no «no X, no Y, no Z», no «rather than», no label fragments.
- Models judge language, code checks structure (ADR-015): no string matching that decides meaning.
- Effort changes judgment only: cards, command policy, expected revisions and the claim guard do not read the level.
- `medium` with nothing configured is exactly today's behaviour: every existing test passes unchanged except where a literal gains `effort: None`.
- Commits: local on `main`, conventional messages, no `Co-Authored-By` or session trailers. Never push.
- Live runs spend the user's OpenAI key: announce before running one.
- No subagents: the user asked for none.
- File anchors below name a function or a line of code, because line numbers shift as tasks land.

## Review Focus

1. A click-only turn (no text) forced to `high`: it must still cost no model call. Test added to Task 8.
2. A level override naming a task kind that does not exist (`[effort.high.tasks.extrakt]`): parsing must fail with the name, not silently ignore it. Test added to Task 2.
3. A `high` turn whose message produced no act (only a question, or small talk): the whole-turn check must not run. Test added to Task 7.
4. A `high` repair that the budget cannot pay for: the doubted act must not run on its first reading; it is held. Test added to Task 7.
5. A split vote with `Reread` where every vote failed its checks: no extra call is made, and the task fails as before. Test added to Task 3.

---

### Task 1: `Effort` in core, on the turn, the replay record and signal labels

**Files:**
- Create: `crates/turnframe-core/src/effort.rs`
- Modify: `crates/turnframe-core/src/lib.rs` (add `pub mod effort;`)
- Modify: `crates/turnframe-core/src/turn.rs` (`pub struct TurnInput`)
- Modify: `crates/turnframe-core/src/replay.rs` (`pub struct ReplayRecord`, `ReplayRecord::received`, `full_record` in tests)
- Modify: `crates/turnframe-core/src/observe.rs` (`pub struct SignalLabels`)
- Modify: every `TurnInput {` literal the compiler lists (about 40, across crates, examples and `crates/turnframe/src/quickstart.md`)
- Test: `crates/turnframe-core/tests/a_turn_names_its_effort_or_leaves_it_to_the_configuration.rs`

**Interfaces:**
- Produces: `turnframe_core::effort::Effort { Low, Medium, High }` (`Copy`, `Default = Medium`, serde snake_case, `as_str()`, `FromStr`, `ALL`); `TurnInput::effort: Option<Effort>`; `ReplayRecord::effort: Effort`; `SignalLabels::effort: Option<Effort>` and `SignalLabels::with_effort(self, Effort) -> Self`.

- [ ] **Step 1: Write the failing test**

```rust
//! A turn may force its effort; one that does not leaves it to the configuration, and
//! says nothing about it on the wire.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::effort::Effort;
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::locale::Locale;
use turnframe_core::turn::{ActorContext, TurnInput};

fn turn(effort: Option<Effort>) -> TurnInput {
    TurnInput {
        turn_id: TurnId::from(uuid::Uuid::from_u128(1)),
        conversation_id: ConversationId::nil(),
        actor: ActorContext::new(AccountId::from("aurora"), "u1"),
        text: Some("hello".to_owned()),
        interaction_response: None,
        attachments: Vec::new(),
        origin: None,
        locale: Locale::from("en-GB"),
        effort,
    }
}

#[test]
fn a_turn_names_its_effort_or_leaves_it_to_the_configuration() {
    let unset = serde_json::to_value(turn(None)).unwrap();
    assert!(unset.get("effort").is_none(), "{unset}");
    let read: TurnInput = serde_json::from_value(unset).unwrap();
    assert_eq!(read.effort, None);

    let forced = serde_json::to_value(turn(Some(Effort::High))).unwrap();
    assert_eq!(forced["effort"], "high");
    let read: TurnInput = serde_json::from_value(forced).unwrap();
    assert_eq!(read.effort, Some(Effort::High));

    assert_eq!(Effort::default(), Effort::Medium);
    assert_eq!("low".parse::<Effort>().unwrap(), Effort::Low);
    assert!("extreme".parse::<Effort>().is_err());
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p turnframe-core --test a_turn_names_its_effort_or_leaves_it_to_the_configuration`
Expected: FAIL to compile: `turnframe_core::effort` does not exist.

- [ ] **Step 3: Add the type**

`crates/turnframe-core/src/effort.rs`:

```rust
//! How much judgment a turn buys: more model calls behind each step, never more airline.

use serde::{Deserialize, Serialize};

/// How hard a turn works to be read correctly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effort {
    /// Fewer calls: no reply review, no step prose, a shorter transcript.
    Low,
    /// Today's behaviour, and the default.
    #[default]
    Medium,
    /// More calls: votes, some reasoning, and a check of the whole turn.
    High,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// Stable label, for records, metrics and configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A label that names no level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an effort level: low, medium or high")]
pub struct UnknownEffort(pub String);

impl std::str::FromStr for Effort {
    type Err = UnknownEffort;

    fn from_str(label: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == label)
            .ok_or_else(|| UnknownEffort(label.to_owned()))
    }
}
```

In `TurnInput`, after `locale`:

```rust
    /// The effort this turn runs at, forced by the application; `None` takes the
    /// configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
```

In `ReplayRecord`, after `budget`, and `effort: Effort::Medium` in `received` and in `full_record`:

```rust
    /// The effort the turn ran at.
    #[serde(default)]
    pub effort: crate::effort::Effort,
```

In `SignalLabels`, after `operation`, and a builder beside `with_purpose`:

```rust
    /// The effort of the turn the signal belongs to.
    pub effort: Option<crate::effort::Effort>,
```

```rust
    /// Labels the signal with the turn's effort.
    #[must_use]
    pub fn with_effort(mut self, effort: crate::effort::Effort) -> Self {
        self.effort = Some(effort);
        self
    }
```

Add a unit test at the foot of `replay.rs`'s `mod tests`:

```rust
    #[test]
    fn a_record_written_before_effort_existed_reads_as_medium() {
        let mut value = serde_json::to_value(full_record()).unwrap();
        value.as_object_mut().unwrap().remove("effort");
        let read: ReplayRecord = serde_json::from_value(value).unwrap();
        assert_eq!(read.effort, crate::effort::Effort::Medium);
    }
```

- [ ] **Step 4: Fix every literal**

Run: `cargo check --workspace --all-targets --all-features 2>&1 | grep -A3 "missing field .effort"`
Add `effort: None,` to each `TurnInput {` literal listed, including the one in `crates/turnframe/src/quickstart.md` (a doctest: `cargo test -p turnframe --doc` finds it). Repeat until clean.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p turnframe-core && cargo test --workspace --all-features --no-run`
Expected: PASS, and the workspace builds.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat!: a turn may force its effort level"
```

---

### Task 2: Profiles per scope, and partial profile changes

**Files:**
- Modify: `crates/turnframe-tasks/src/profile.rs` (add `ProfileChange`, `ProfileChanges`, `TaskProfile::with_reasoning`, `TaskProfile::with_review`)
- Modify: `crates/turnframe-tasks/src/engine.rs` (`TaskScope` fields and builders; `TaskEngine::profile`; `prepare` reads it; task signals carry the effort)
- Modify: `crates/turnframe-tasks/src/lib.rs` (re-export `ProfileChange`, `ProfileChanges`)
- Test: `crates/turnframe-tasks/tests/a_scope_runs_its_tasks_under_its_own_profiles.rs`
- Test: unit tests in `profile.rs`

**Interfaces:**
- Consumes: `turnframe_core::effort::Effort` (Task 1).
- Produces: `TaskScope::with_profiles(self, TaskProfiles) -> Self`, `TaskScope::with_effort(self, Effort) -> Self`, `TaskScope::effort(&self) -> Option<Effort>`, `TaskEngine::profile(&self, &TaskScope, TaskKind) -> TaskProfile`; `ProfileChange` (every field `Option`, `apply(&self, TaskProfile) -> TaskProfile`); `ProfileChanges` (TOML map from kind name to `ProfileChange`, rejects unknown names; `with(self, TaskKind, ProfileChange) -> Self`; `apply(&self, TaskProfiles) -> TaskProfiles`); `TaskProfile::with_reasoning(self, Option<ReasoningEffort>) -> Self`; `TaskProfile::with_review(self, bool) -> Self`.

- [ ] **Step 1: Write the failing tests**

`crates/turnframe-tasks/tests/a_scope_runs_its_tasks_under_its_own_profiles.rs`:

```rust
//! A scope that carries profiles runs its tasks under them, over the engine's own: one
//! engine serves turns of every effort.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_core::effort::Effort;
use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskKind, TaskProfiles};

#[tokio::test]
async fn a_scope_runs_its_tasks_under_its_own_profiles() {
    let engine = TaskEngine::builder(router(vec![(
        provider("p", &[answer("red"), answer("red"), answer("blue")]),
        &[],
    )]))
    .build();
    let voting = TaskProfiles::new().adjust(TaskKind::Route, |p| p.with_votes(3));
    let scope = scope().with_profiles(voting).with_effort(Effort::High);
    let id = TaskId::new("u0").child("route");

    let outcome = engine
        .run(
            &scope,
            TaskCall {
                id: &id,
                parent: None,
                depth: 1,
            },
            &PickColour,
            &colours(),
        )
        .await;

    assert_eq!(outcome.accepted().unwrap().colour, "red");
    assert_eq!(scope.records().len(), 3, "three votes, from the scope's profile");
    assert_eq!(engine.profile(&scope, TaskKind::Route).votes, 3);
    assert_eq!(engine.profiles().get(TaskKind::Route).votes, 1, "the engine's own is untouched");
    assert_eq!(scope.effort(), Some(Effort::High));
}
```

Unit tests at the foot of `profile.rs`:

```rust
    #[test]
    fn a_change_keeps_what_it_does_not_name() {
        let changes: ProfileChanges = toml::from_str(
            r#"
            [route]
            votes = 3
            "#,
        )
        .expect("parses");
        let profiles = changes.apply(TaskProfiles::new());
        let route = profiles.get(TaskKind::Route);
        assert_eq!(route.votes, 3);
        assert_eq!(route.max_output_tokens, Some(150), "route's own cap survives");
    }

    #[test]
    fn a_change_to_a_task_that_does_not_exist_is_refused_by_name() {
        let refused = toml::from_str::<ProfileChanges>("[extrakt]\nvotes = 3\n").unwrap_err();
        assert!(refused.to_string().contains("extrakt"), "{refused}");
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p turnframe-tasks`
Expected: FAIL to compile: `with_profiles`, `profile`, `ProfileChanges` do not exist.

- [ ] **Step 3: Implement**

In `profile.rs`, beside the other builders of `TaskProfile`:

```rust
    /// A copy thinking as much as `reasoning` allows.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning;
        self
    }

    /// A copy whose written block is reviewed, or not.
    #[must_use]
    pub fn with_review(mut self, review: bool) -> Self {
        self.review = review;
        self
    }
```

After `TaskProfiles`:

```rust
/// The fields of a [`TaskProfile`] to change; a field left out keeps its value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ProfileChange {
    pub enabled: Option<bool>,
    pub model: Option<String>,
    pub escalate_to: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub timeout_secs: Option<u64>,
    pub votes: Option<u8>,
    pub on_disagreement: Option<Disagreement>,
    pub repairs: Option<u8>,
    pub retries: Option<u8>,
    pub review: Option<bool>,
}

impl ProfileChange {
    /// `profile` with every field this change names.
    #[must_use]
    pub fn apply(&self, mut profile: TaskProfile) -> TaskProfile {
        let Self {
            enabled,
            model,
            escalate_to,
            max_output_tokens,
            reasoning_effort,
            timeout_secs,
            votes,
            on_disagreement,
            repairs,
            retries,
            review,
        } = self.clone();
        if let Some(value) = enabled { profile.enabled = value; }
        if let Some(value) = model { profile.model = Some(value); }
        if let Some(value) = escalate_to { profile.escalate_to = Some(value); }
        if let Some(value) = max_output_tokens { profile.max_output_tokens = Some(value); }
        if let Some(value) = reasoning_effort { profile.reasoning_effort = Some(value); }
        if let Some(value) = timeout_secs { profile.timeout_secs = Some(value); }
        if let Some(value) = votes { profile.votes = value.max(1); }
        if let Some(value) = on_disagreement { profile.on_disagreement = value; }
        if let Some(value) = repairs { profile.repairs = value; }
        if let Some(value) = retries { profile.retries = value; }
        if let Some(value) = review { profile.review = value; }
        profile
    }
}

/// Changes to several task kinds, keyed by the kind's name as TOML writes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProfileChanges {
    changes: BTreeMap<String, ProfileChange>,
}

impl ProfileChanges {
    /// Changes `kind` as `change` says.
    #[must_use]
    pub fn with(mut self, kind: TaskKind, change: ProfileChange) -> Self {
        self.changes.insert(kind.as_str().to_owned(), change);
        self
    }

    /// `profiles` with every change applied.
    #[must_use]
    pub fn apply(&self, mut profiles: TaskProfiles) -> TaskProfiles {
        for kind in TaskKind::ALL {
            if let Some(change) = self.changes.get(kind.as_str()) {
                profiles = profiles.adjust(kind, |profile| change.apply(profile));
            }
        }
        profiles
    }

    /// Every pool tag the changes name, so the pool can be checked for them.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        self.changes
            .values()
            .flat_map(|change| [change.model.clone(), change.escalate_to.clone()])
            .flatten()
            .collect()
    }
}

impl<'de> Deserialize<'de> for ProfileChanges {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let changes = BTreeMap::<String, ProfileChange>::deserialize(deserializer)?;
        if let Some(unknown) = changes
            .keys()
            .find(|name| !TaskKind::ALL.iter().any(|kind| kind.as_str() == name.as_str()))
        {
            return Err(serde::de::Error::custom(format!(
                "`{unknown}` is not a task kind"
            )));
        }
        Ok(Self { changes })
    }
}
```

`ReasoningEffort` must derive `Deserialize` with snake_case names; if it does not, add `#[serde(rename_all = "snake_case")]` to it in `crates/turnframe-provider/src/request.rs`.

In `engine.rs`, `TaskScope` gains two fields, initialised to `None` in `new`:

```rust
    profiles: Option<TaskProfiles>,
    effort: Option<Effort>,
```

```rust
    /// Runs this scope's tasks under `profiles` instead of the engine's.
    #[must_use]
    pub fn with_profiles(mut self, profiles: TaskProfiles) -> Self {
        self.profiles = Some(profiles);
        self
    }

    /// Labels this scope's task signals with the turn's effort.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// The effort this scope's turn runs at, when it was given one.
    #[must_use]
    pub const fn effort(&self) -> Option<Effort> {
        self.effort
    }
```

On `TaskEngine`:

```rust
    /// The profile `kind` runs under in `scope`: the scope's, else the engine's.
    #[must_use]
    pub fn profile(&self, scope: &TaskScope, kind: TaskKind) -> TaskProfile {
        scope.profiles.as_ref().unwrap_or(&self.profiles).get(kind)
    }
```

In `prepare`, replace `let profile = self.profiles.get(kind);` with `let profile = self.profile(scope, kind);`. Where `Signal::TaskCompleted` and `Signal::TaskVoteDisagreement` are labelled, add the effort:

```rust
        let labels = match scope.effort() {
            Some(effort) => labels.with_effort(effort),
            None => labels,
        };
```

(`run_prepared` needs `scope` passed to where it builds the disagreement labels; it already has it.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-tasks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: a task scope carries its own profiles, and profiles change field by field"
```

---

### Task 3: A split vote read once more

**Files:**
- Modify: `crates/turnframe-tasks/src/profile.rs` (`Disagreement::Reread`)
- Modify: `crates/turnframe-tasks/src/engine.rs` (`run_prepared`'s disagreement match)
- Test: `crates/turnframe-tasks/tests/a_split_vote_is_read_once_more.rs`

**Interfaces:**
- Produces: `Disagreement::Reread` (TOML `on_disagreement = "reread"`). Call id of the extra call: `call.id.call("reread")`, which scripted tasks answer from the task's queue.

- [ ] **Step 1: Write the failing test**

```rust
//! Votes with no majority, under `Reread`, run the task once more shown the answers that
//! disagreed, and its answer stands. With no answer to show, nothing more is sent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_provider::testing::StaticProvider;
use turnframe_tasks::{Disagreement, TaskCall, TaskEngine, TaskId, TaskKind, TaskOutcome, TaskProfiles};

fn rereading(answers: &[serde_json::Value]) -> (TaskEngine, Arc<StaticProvider>) {
    let p = provider("p", answers);
    let profiles = TaskProfiles::new().adjust(TaskKind::Route, |profile| {
        profile
            .with_votes(3)
            .with_repairs(0)
            .on_disagreement(Disagreement::Reread)
    });
    let engine = TaskEngine::builder(router(vec![(Arc::clone(&p), &[])]))
        .profiles(profiles)
        .build();
    (engine, p)
}

#[tokio::test]
async fn a_split_vote_is_read_once_more() {
    // `colours()` allows red and blue: the third vote fails its check, leaving one each.
    let (engine, p) = rereading(&[answer("red"), answer("blue"), answer("violet"), answer("blue")]);
    let scope = scope();
    let id = TaskId::new("u0").child("route");
    let call = TaskCall { id: &id, parent: None, depth: 1 };

    let outcome = engine.run(&scope, call, &PickColour, &colours()).await;

    assert_eq!(outcome.accepted().unwrap().colour, "blue");
    let calls = p.calls();
    assert_eq!(calls.len(), 4);
    let shown = format!("{:?}", calls[3].messages);
    assert!(shown.contains("red") && shown.contains("blue"), "{shown}");
}

#[tokio::test]
async fn votes_that_all_failed_are_not_read_again() {
    let (engine, p) = rereading(&[answer("violet"), answer("violet"), answer("violet")]);
    let scope = scope();
    let id = TaskId::new("u0").child("route");
    let call = TaskCall { id: &id, parent: None, depth: 1 };

    let outcome = engine.run(&scope, call, &PickColour, &colours()).await;

    assert!(matches!(outcome, TaskOutcome::Failed { .. } | TaskOutcome::Disagreed { .. }));
    assert_eq!(p.calls().len(), 3, "no fourth call");
}
```

(`answer("violet")` is outside `colours()`, so that vote fails its check; with repairs at 0 it is unusable.)

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p turnframe-tasks --test a_split_vote_is_read_once_more`
Expected: FAIL to compile: `Disagreement::Reread` does not exist.

- [ ] **Step 3: Implement**

In `Disagreement`:

```rust
    /// Run once more, shown the answers that disagreed; that answer stands.
    Reread,
```

In `run_prepared`'s `match prepared.profile.on_disagreement`, before the catch-all arm:

```rust
            Disagreement::Reread if !answered.is_empty() => {
                let shown: Vec<String> = answered
                    .iter()
                    .map(|(output, _)| serde_json::to_string(output).unwrap_or_default())
                    .collect();
                let mut again = prepared.clone();
                again.messages.push(Message::user(format!(
                    "Readings of this that disagreed:\n{}\n\nRead it again and give the \
                     answer the message supports.",
                    shown.join("\n")
                )));
                let id = call.id.call("reread");
                match self
                    .chain(scope, &again, task, input, tag, depth, id, None)
                    .await
                {
                    Chain::Answered { output, depth, .. } => TaskOutcome::Accepted { output, depth },
                    Chain::Unusable { failure, depth } => TaskOutcome::Failed { failure, depth },
                }
            }
```

`Prepared` must derive `Clone` (it holds `Message`s and strings). The `chain` call's `id` parameter type is whatever `call.id.call(...)` returns in the vote branch; mirror it.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-tasks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: a split vote may be read once more, shown the answers that disagreed"
```

---

### Task 4: Pipeline settings per turn, and disputed small talk read again

**Files:**
- Modify: `crates/turnframe-understand/src/pipeline/mod.rs` (`Settings`: two fields and builders; `understand_checked` and `read` take the turn's settings; `Reading::Lost` becomes `Reading::Again`)
- Modify: `crates/turnframe-understand/src/input.rs` (`UnderstandingInput::settings`, `with_settings`)
- Modify: `crates/turnframe-understand/src/pipeline/units.rs` (`LostConstraint` becomes `Retry`; `add_missed` takes `reread_small_talk`)
- Test: `crates/turnframe-understand/tests/small_talk_read_as_an_act_is_segmented_again_when_asked.rs`
- Test: `crates/turnframe-understand/tests/a_turn_runs_under_its_own_settings.rs`

**Interfaces:**
- Produces: `Settings::reread_small_talk: bool` (default `false`), `Settings::cross_check_rounds: u8` (default `0`), `Settings::with_reread_small_talk(self, bool)`, `Settings::with_cross_check_rounds(self, u8)`, `Settings::with_transcript` (exists); `UnderstandingInput::settings: Option<Settings>`, `UnderstandingInput::with_settings(self, Settings) -> Self`. The pipeline reads `turn.settings.unwrap_or(self.settings)`.

- [ ] **Step 1: Write the failing tests**

`small_talk_read_as_an_act_is_segmented_again_when_asked.rs`:

```rust
//! With `reread_small_talk`, words segmentation read as small talk and coverage as an act
//! go back to segmentation once, told what coverage saw; its second reading stands.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_core::understanding::{NotUnderstoodReason, UnitKind};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::Settings;

#[tokio::test]
async fn small_talk_read_as_an_act_is_segmented_again_when_asked() {
    // [1]I [2]give [3]up
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "Small talk.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 3}}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "cancel", "words": {"from": 1, "to": 3}, "workflow": "unknown"}]}),
        )
        .answer(
            "turn/segment.after_coverage",
            json!({"analysis": "A remark, not a request.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 3}}
            ]}),
        )
        .answer(
            "turn/coverage.after_segment",
            json!({"missed": [{"kind": "cancel", "words": {"from": 1, "to": 3}, "workflow": "unknown"}]}),
        );
    let input = turn("I give up")
        .with_settings(Settings::conservative().with_reread_small_talk(true));
    let run = understand(script, &input).await;

    assert!(run.was_called("turn/segment.after_coverage"));
    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(understanding.units[0].kind, UnitKind::Chitchat);
    assert_eq!(
        understanding.not_understood[0].reason,
        NotUnderstoodReason::Unclear,
        "still disputed after the second reading: reported, never acted on"
    );
}
```

`a_turn_runs_under_its_own_settings.rs`: a request whose act is not mutating is verified only when the turn's settings say `VerifyPolicy::All`, though the understander's own settings say `Mutating`:

```rust
//! The settings a turn carries win over the understander's own.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_understand::{Settings, VerifyPolicy};

#[tokio::test]
async fn a_turn_runs_under_its_own_settings() {
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let input = turn("name Lisbon")
        .with_settings(Settings::conservative().with_verify(VerifyPolicy::Off));
    let run = understand(script, &input).await;

    assert!(!run.was_called("u1/verify"), "verification off for this turn");
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p turnframe-understand --test small_talk_read_as_an_act_is_segmented_again_when_asked --test a_turn_runs_under_its_own_settings`
Expected: FAIL to compile: `with_settings`, `with_reread_small_talk` do not exist.

- [ ] **Step 3: Implement**

`Settings` gains, with `false` and `0` in `conservative()`:

```rust
    /// Whether words read as small talk that coverage reads as an act are segmented
    /// again, told what coverage saw, before they are reported unclear.
    pub reread_small_talk: bool,
    /// Rounds of the whole-turn check; `0` switches it off.
    pub cross_check_rounds: u8,
```

with `const fn with_reread_small_talk(mut self, on: bool) -> Self` and `const fn with_cross_check_rounds(mut self, rounds: u8) -> Self`.

`UnderstandingInput` gains `pub settings: Option<Settings>` (`None` in `new`) and:

```rust
    /// Runs this turn under `settings` instead of the understander's.
    #[must_use]
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }
```

In `units.rs`, replace `pub(crate) struct LostConstraint(pub Span);` with:

```rust
/// Why the segmentation is sent back once.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Retry {
    /// Coverage found a constraint in words no unit holds.
    LostConstraint(Span),
    /// Coverage read as an act words segmentation read as small talk or a dispute.
    Disputed { words: Span, read_as: MissedKind },
}
```

`add_missed` takes `reread_small_talk: bool`; where it now pushes to `covered.unclear` for an act reading, it first does:

```rust
            if reread_small_talk {
                return Err(Retry::Disputed { words: span, read_as: found.kind });
            }
```

and `MissedKind::Constraint => return Err(Retry::LostConstraint(span))`.

In `mod.rs`, `Reading::Lost(Segmentation, Span)` becomes `Reading::Again(Segmentation, Retry)`; `understand_checked` computes `let settings = turn.settings.unwrap_or(self.settings);`, puts it in `Context`, and passes it to `read`. The retry loop keeps its shape; the second reading is run with `reread_small_talk` off, so a dispute that survives it is reported unclear:

```rust
        let mut again: Option<(Segmentation, Retry)> = None;
        let (units, routes, questions) = loop {
            let reread = settings.reread_small_talk && again.is_none();
            match self.read(scope, turn, steps, again.as_ref(), reread).await {
                Ok(read) => break read,
                Err(Reading::Unreadable(unreadable)) => return Understanding::unreadable(unreadable),
                Err(Reading::Again(segmentation, retry)) if again.is_none() => {
                    again = Some((segmentation, retry));
                }
                Err(Reading::Again(..)) => {
                    return Understanding::unreadable(Unreadable::LostConstraint);
                }
            }
        };
```

In `read`, the feedback depends on the retry:

```rust
            Some((previous, retry)) => {
                let feedback = match retry {
                    Retry::LostConstraint(span) => {
                        let (from, to) = span.shown();
                        let said = turn.message.slice(*span).unwrap_or_default();
                        format!(
                            "Words {from} to {to}, «{said}», are in no unit, and a check read them as a \
                             constraint. Every word the message needs belongs to a unit: to the request \
                             it completes, or to a constraint of its own."
                        )
                    }
                    Retry::Disputed { words, read_as } => {
                        let (from, to) = words.shown();
                        let said = turn.message.slice(*words).unwrap_or_default();
                        format!(
                            "Words {from} to {to}, «{said}», were read as small talk, and a check read \
                             them as a {}. Read the message again and say what these words are.",
                            coverage::kind_name((*read_as).into())
                        )
                    }
                };
```

(`coverage::kind_name` is private today; make it `pub(crate)`.) Everywhere `self.settings` is read inside the pipeline, read the `Context`'s settings instead.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-understand`
Expected: PASS, including the existing `small_talk_a_second_reading_takes_for_an_act_runs_nothing` (default settings, no reread).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: a turn carries its pipeline settings, and disputed small talk may be read again"
```

---

### Task 5: The `cross_check` task

**Files:**
- Modify: `crates/turnframe-provider/src/purpose.rs` (`ModelPurpose::CrossCheck`, `ALL: [Self; 14]`, `is_understanding`, `as_str`, `logging_policy`)
- Modify: `crates/turnframe-tasks/src/profile.rs` (`default_for(TaskKind::CrossCheck)`: `max_output_tokens: Some(400)`)
- Modify: `crates/turnframe-core/src/understanding.rs` (`FoundBy::CrossCheck`)
- Create: `crates/turnframe-understand/src/tasks/cross_check.rs`
- Create: `crates/turnframe-understand/prompts/understand/cross_check.md`
- Modify: `crates/turnframe-understand/src/tasks/mod.rs` (`pub mod cross_check;` and a row in the table)
- Modify: `docs/provider-adapters.md` (the purpose list)
- Test: unit tests at the foot of `cross_check.rs`; the prompt-size snapshot test gains the task

**Interfaces:**
- Produces: `tasks::cross_check::{CrossCheck, CrossCheckInput, ShownAct, Finding, CrossChecked}`.

```rust
pub struct ShownAct { pub id: String, pub extra: String, pub arguments: Vec<String> }
pub struct CrossCheckInput {
    pub acts: Vec<ShownAct>,
    pub questions: Vec<String>,
    pub constraints: Vec<String>,
    pub unread: Vec<String>,
    /// Words a question, a constraint, small talk or an argument's value already holds.
    pub held: Vec<Span>,
}
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    Missing { words: Span },
    WrongValue { act: String, argument: String, words: Span },
    WrongRecord { act: String, words: Span },
    NotAsked { act: String },
}
pub struct CrossChecked { pub findings: Vec<Finding> }
```

- [ ] **Step 1: Write the failing unit tests** (at the foot of `cross_check.rs`, written first with the types stubbed)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::UnderstandingInput;

    fn input() -> CrossCheckInput {
        CrossCheckInput {
            acts: vec![ShownAct {
                id: "u1.a1".to_owned(),
                line: "u1.a1 trip.set_name on Trip 1: value «Lisbon» (words 3 to 3)".to_owned(),
                arguments: vec!["value".to_owned()],
            }],
            questions: Vec::new(),
            constraints: Vec::new(),
            unread: Vec::new(),
            held: vec![Span::new(2, 2)],
        }
    }

    fn turn() -> UnderstandingInput {
        // [1]name [2]Lisbon [3]and [4]meals [5]too
        UnderstandingInput::new("name Lisbon and meals too", "en-GB", chrono::NaiveDate::MIN)
    }

    #[test]
    fn a_finding_must_name_an_act_and_an_argument_it_has() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        let wrong = CrossChecked {
            findings: vec![Finding::WrongValue {
                act: "u1.a1".to_owned(),
                argument: "due".to_owned(),
                words: Span::new(1, 1),
            }],
        };
        assert_eq!(task.check(&input(), &wrong).unwrap_err().code, "not_an_argument_of_the_act");
        let unknown = CrossChecked { findings: vec![Finding::NotAsked { act: "u9.a1".to_owned() }] };
        assert_eq!(task.check(&input(), &unknown).unwrap_err().code, "not_in_set");
    }

    #[test]
    fn missing_words_lie_outside_what_was_read() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        let read = CrossChecked { findings: vec![Finding::Missing { words: Span::new(1, 2) }] };
        assert_eq!(task.check(&input(), &read).unwrap_err().code, "words_already_read");
        let fresh = CrossChecked { findings: vec![Finding::Missing { words: Span::new(3, 4) }] };
        assert!(task.check(&input(), &fresh).is_ok());
    }

    #[test]
    fn an_empty_answer_is_an_answer() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        assert!(task.check(&input(), &CrossChecked { findings: Vec::new() }).is_ok());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p turnframe-understand cross_check`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`ModelPurpose::CrossCheck` with doc `/// Check the whole understanding of a message against the message.`, `as_str` `"cross_check"`, counted in `is_understanding` and in the `REDACTED_OUTPUT` arm of `logging_policy`; `ALL` grows to 14 and the purpose test that counts it follows. `FoundBy::CrossCheck` with doc `/// The check of the whole turn.`

The prompt, `prompts/understand/cross_check.md`:

```markdown
Check a reading of the user's message against the message.

You are shown the message with its words numbered, and what was understood: each act with its operation, its record and each value with the words it came from; each question; each constraint; the words read as nothing to act on. Say where the reading does not say what the message says, and nothing else.

- missing: words that ask for something no act, question or constraint holds. Point at them.
- wrong_value: an act's value the message does not give. Name the act and the argument, and point at the words the value should come from.
- wrong_record: an act aimed at a record the message does not mean. Name the act and point at the words naming the record it means.
- not_asked: an act the message does not ask for. Name it.

A reading that says what the message says has no findings: answer with an empty list. Judge what the message means, never how it is worded.
```

`cross_check.rs`:

```rust
//! `cross_check`: whether what was understood of a message says what the message says.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{any_of, array, object, one_of, span, variant};
use crate::tasks::{check_one_of, check_span};
use crate::words::Span;

const BUILT_IN: &str = include_str!("../../prompts/understand/cross_check.md");

/// The whole-turn check, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct CrossCheck<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> CrossCheck<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// One act as the check is shown it.
#[derive(Debug, Clone)]
pub struct ShownAct {
    /// Its id, `u1.a1`.
    pub id: String,
    /// Its operation, record and values, each value with the words it came from.
    pub extra: String,
    /// Its argument names.
    pub arguments: Vec<String>,
}

/// What the check is shown.
#[derive(Debug, Clone, Default)]
pub struct CrossCheckInput {
    /// Every act understood.
    pub acts: Vec<ShownAct>,
    /// Every question, by its words.
    pub questions: Vec<String>,
    /// Every constraint, by its words.
    pub constraints: Vec<String>,
    /// Words read as nothing to act on.
    pub unread: Vec<String>,
    /// Words already read as a question, a constraint, small talk or a value.
    pub held: Vec<Span>,
}

/// One place the reading does not say what the message says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// Words asking for something nothing holds.
    Missing { words: Span },
    /// A value the message does not give, and the words it should come from.
    WrongValue { act: String, argument: String, words: Span },
    /// A record the message does not mean, and the words naming the one it does.
    WrongRecord { act: String, words: Span },
    /// An act the message does not ask for.
    NotAsked { act: String },
}

/// The check's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossChecked {
    /// Empty when the reading says what the message says.
    pub findings: Vec<Finding>,
}

impl<'a> ModelTask for CrossCheck<'a> {
    type Input = CrossCheckInput;
    type Output = CrossChecked;

    fn kind(&self) -> TaskKind {
        TaskKind::CrossCheck
    }

    fn prompt_name(&self) -> &str {
        "understand.cross_check"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &CrossCheckInput) -> Value {
        let acts: Vec<String> = input.acts.iter().map(|act| act.id.clone()).collect();
        let mut arguments: Vec<String> =
            input.acts.iter().flat_map(|act| act.arguments.clone()).collect();
        arguments.sort();
        arguments.dedup();
        let mut kinds = vec![variant("missing", vec![("words", span())])];
        if !acts.is_empty() {
            if !arguments.is_empty() {
                kinds.push(variant(
                    "wrong_value",
                    vec![
                        ("act", one_of(acts.clone())),
                        ("argument", one_of(arguments)),
                        ("words", span()),
                    ],
                ));
            }
            kinds.push(variant(
                "wrong_record",
                vec![("act", one_of(acts.clone())), ("words", span())],
            ));
            kinds.push(variant("not_asked", vec![("act", one_of(acts))]));
        }
        object(vec![("findings", array(any_of(kinds)))])
    }

    fn render(&self, input: &CrossCheckInput) -> Vec<Message> {
        let list = |title: &str, extras: &[String]| {
            (!lines.is_empty()).then(|| {
                let mut out = format!("{title}:");
                for line in lines {
                    let _ = write!(out, "\n- {extra}");
                }
                out
            })
        };
        let acts: Vec<String> = input.acts.iter().map(|act| act.line.clone()).collect();
        vec![Message::user(render::sections([
            render::last_assistant(self.turn),
            Some(render::message(&self.turn.message)),
            list("Acts understood", &acts).or_else(|| Some("Acts understood: none".to_owned())),
            list("Questions", &input.questions),
            list("Constraints", &input.constraints),
            list("Read as nothing to act on", &input.unread),
        ]))]
    }

    fn check(&self, input: &CrossCheckInput, output: &CrossChecked) -> Result<(), StructuralError> {
        let acts: Vec<String> = input.acts.iter().map(|act| act.id.clone()).collect();
        let words = &self.turn.message;
        for (position, finding) in output.findings.iter().enumerate() {
            let what = format!("finding {}", position + 1);
            match finding {
                Finding::Missing { words: span } => {
                    check_span(&what, *span, words)?;
                    if input.held.iter().any(|held| held.from <= span.to && span.from <= held.to) {
                        return Err(StructuralError::new(
                            "words_already_read",
                            format!("{what}: those words are already read; missing words are words nothing holds"),
                        ));
                    }
                }
                Finding::WrongValue { act, argument, words: span } => {
                    check_one_of("act", act, &acts)?;
                    check_span(&what, *span, words)?;
                    let known = input
                        .acts
                        .iter()
                        .find(|shown| &shown.id == act)
                        .is_some_and(|shown| shown.arguments.contains(argument));
                    if !known {
                        return Err(StructuralError::new(
                            "not_an_argument_of_the_act",
                            format!("{what}: {act} has no argument `{argument}`"),
                        ));
                    }
                }
                Finding::WrongRecord { act, words: span } => {
                    check_one_of("act", act, &acts)?;
                    check_span(&what, *span, words)?;
                }
                Finding::NotAsked { act } => check_one_of("act", act, &acts)?,
            }
        }
        Ok(())
    }
}
```

Add a `cross_check` case to `tests/every_task_prompt_stays_within_its_size_budget.rs` beside the others, and accept its new snapshot with `INSTA_UPDATE=always`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-provider -p turnframe-tasks -p turnframe-understand && INSTA_UPDATE=always cargo test -p turnframe-understand --test every_task_prompt_stays_within_its_size_budget`
Expected: PASS; one new snapshot file.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: a cross_check task reads the whole understanding against the message"
```

---

### Task 6: A note for the task that reads an act again

**Files:**
- Modify: `crates/turnframe-understand/src/tasks/{extract,locate,verify}.rs` (`note: Option<String>` on each input; rendered)
- Modify: the five literal sites of each input (`pipeline/chain.rs`, `tests/every_task_prompt_stays_within_its_size_budget.rs`, and `crates/turnframe-provider-{openai,anthropic,gemini}/tests/live_smoke.rs`): `note: None`
- Modify: `crates/turnframe-understand/src/pipeline/chain.rs` (`Revisit`, `pub(crate) async fn revisit`)
- Test: unit test in each of the three task modules

**Interfaces:**
- Produces: `ExtractInput::note`, `LocateInput::note`, `VerifyInput::note` (`Option<String>`), each rendered as a final section `A check of the whole message found: {note}`.

```rust
pub(crate) enum Revisit {
    /// Extract again with the note, keeping the target; then verify.
    Value { note: String },
    /// Locate again with the note; then extract and verify.
    Record { note: String },
    /// Verify again with the note.
    Asked { note: String },
}
pub(crate) async fn revisit(cx: &Context<'_>, plan: &Planned<'_>, act: &UnderstoodAct, revisit: &Revisit) -> Chained
```

Task ids of a revisit: `<unit>/locate.after_cross_check`, `<unit>/extract.after_cross_check`, `<unit>/verify.after_cross_check`.

- [ ] **Step 1: Write the failing unit tests** (one per task module; extract shown, locate and verify mirror it with their own input literal)

In `extract.rs`:

```rust
    #[test]
    fn a_note_from_the_whole_turn_check_is_shown() {
        let turn = UnderstandingInput::new("name Lisbon", "en-GB", chrono::NaiveDate::MIN);
        let spec = OperationSpec::new("trip.set_name").summary("Set the name.");
        let workflow = WorkflowBrief::new("trip");
        let input = ExtractInput {
            label: "Request",
            words: Span::new(0, 1),
            spec: &spec,
            workflow: &workflow,
            record: RecordContext::Nothing,
            arguments: Vec::new(),
            record_choices: BTreeMap::new(),
            continues: None,
            transcript: 0,
            note: Some("the value of value is in «Lisbon»".to_owned()),
        };
        let rendered = format!("{:?}", Extract::new(&turn).render(&input));
        assert!(rendered.contains("A check of the whole message found: the value of value is in «Lisbon»"));
    }
```

In `locate.rs`, the same test with:

```rust
        let key = WorkflowKey::from("trip");
        let input = LocateInput {
            label: "Request",
            words: Span::new(0, 1),
            spec: &spec,
            workflow: &key,
            candidates: Vec::new(),
            allow_new: true,
            allow_not_listed: false,
            note: Some("the record meant is named in «Lisbon»".to_owned()),
        };
        let rendered = format!("{:?}", Locate::new(&turn).render(&input));
```

In `verify.rs`, the same test with:

```rust
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Request",
            words: Span::new(0, 1),
            meaning: "Set the name.".to_owned(),
            record: "Trip 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            note: Some("the message may not ask for this act".to_owned()),
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
```

Each asserts `rendered` contains `A check of the whole message found: ` followed by its note. Import what each module's `mod tests` lacks (`OperationSpec`, `WorkflowBrief`, `WorkflowKey`, `BTreeMap`).

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p turnframe-understand a_note_from_the_whole_turn_check_is_shown`
Expected: FAIL to compile: no field `note`.

- [ ] **Step 3: Implement**

Each input gains `/// What the whole-turn check found, when this call reads the act again.` `pub note: Option<String>,`. Each `render` appends, as its last section:

```rust
            input.note.as_ref().map(|note| format!("A check of the whole message found: {note}")),
```

In `chain.rs`, `Chain` gains `note: Option<String>` and `after: &'static str` (`""` for a first reading, `".after_cross_check"` for a revisit). `target()`, `extract_input()` and `verify()` set `note: self.note.clone()` on the inputs they build, and name their calls `format!("locate{}", self.after)` (and so on). `run` builds the chain with `note: None, after: ""`. Then:

```rust
/// A finding of the whole-turn check, sending one step of one act back.
#[derive(Debug, Clone)]
pub(crate) enum Revisit {
    Value { note: String },
    Record { note: String },
    Asked { note: String },
}

/// Reads `act` again from the step `revisit` names, the note shown to that step's task.
pub(crate) async fn revisit(
    cx: &Context<'_>,
    plan: &Planned<'_>,
    act: &UnderstoodAct,
    revisit: &Revisit,
) -> Chained {
    let (Revisit::Value { note } | Revisit::Record { note } | Revisit::Asked { note }) = revisit;
    let mut chain = Chain {
        cx,
        plan,
        carried: plan.pending.map(|p| p.given.clone()).unwrap_or_default(),
        unit: TaskId::new(if plan.id.act == 1 { plan.unit.to_string() } else { plan.id.to_string() }),
        parent: plan.parent.clone(),
        depth: plan.depth,
        note: Some(note.clone()),
        after: ".after_cross_check",
    };
    match revisit {
        Revisit::Record { .. } => chain.run().await,
        Revisit::Value { .. } => chain.run_from(act.target.clone()).await,
        Revisit::Asked { .. } => chain.verify_again(act.clone()).await,
    }
}
```

`run` is split so the part after locating is `run_from(target)`; `run` becomes `target()` then `run_from(target)`. `verify_again` rebuilds `Extracted` from `act.arguments` minus the carried ones and runs the tail of `verified` with the note:

```rust
    async fn verify_again(mut self, mut act: UnderstoodAct) -> Chained {
        let mut extracted = Extracted::default();
        for (name, argument) in &act.arguments {
            if !self.carried.contains_key(name) {
                extracted.arguments.insert(name.clone(), argument.clone());
            }
        }
        let verdict = match self.verify(&act.target, &extracted, "verify").await {
            Ok(verdict) => verdict,
            Err(reason) => return self.not_understood(reason, Some(act.target)),
        };
        if verdict.confirmed() {
            return Chained::Act(act);
        }
        if verdict.overall == Overall::NotRequested {
            return self.not_understood(NotUnderstoodReason::NotRequested, Some(act.target));
        }
        let at_fault = verdict.at_fault();
        for name in &at_fault {
            act.arguments.remove(name);
        }
        if !at_fault.is_empty() {
            act.status = ActStatus::NeedsValue { arguments: at_fault, reason: None };
        }
        Chained::Act(act)
    }
```

(`verify`'s `name` argument gains `self.after` inside the method, as the other steps do.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-understand && cargo test -p turnframe-provider-openai -p turnframe-provider-anthropic -p turnframe-provider-gemini --no-run`
Expected: PASS; the live smoke tests build.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: an act can be read again from one step, told what the whole-turn check found"
```

---

### Task 7: The whole-turn check in the pipeline

**Files:**
- Create: `crates/turnframe-understand/src/pipeline/cross_check.rs` (`impl Understander { async fn cross_checked(..) }` and the rendering of `CrossCheckInput`)
- Modify: `crates/turnframe-understand/src/pipeline/mod.rs` (call it after `names_the_waiting_record`; `mod cross_check;`)
- Modify: `crates/turnframe-understand/src/progress.rs` (`Step::CrossChecked { round: u8, findings: usize }`, `Step::CrossCheckSkipped { round: u8, code: String }`, and their `describe`)
- Test files (one behaviour each), in `crates/turnframe-understand/tests/`:
  - `a_whole_turn_check_that_finds_nothing_changes_nothing.rs`
  - `a_value_the_whole_turn_check_doubts_is_extracted_again.rs`
  - `a_record_the_whole_turn_check_doubts_is_located_again.rs`
  - `an_act_the_whole_turn_check_says_was_not_asked_is_verified_again.rs`
  - `words_the_whole_turn_check_finds_unread_are_read.rs`
  - `an_act_two_checks_still_doubt_is_held.rs`
  - `a_whole_turn_check_the_budget_cannot_pay_for_is_skipped.rs`
  - `a_turn_with_no_act_is_not_checked_whole.rs`

**Interfaces:**
- Consumes: `Settings::cross_check_rounds` (Task 4), `CrossCheck` (Task 5), `chain::revisit` and `Revisit` (Task 6), `FoundBy::CrossCheck` (Task 5).
- Produces: the pipeline behaviour below; nothing new public beyond the `Step` variants.

Behaviour, for `rounds = settings.cross_check_rounds` (`0` skips all of this):

```
for round in 1..=rounds:
    if no act was understood: stop (no call)
    run cross_check (task id "turn/cross_check" for round 1, "turn/cross_check.round{n}" after)
    failed or budget refused: Step::CrossCheckSkipped { round, code }; stop
    no findings: Step::CrossChecked { round, findings: 0 }; stop
    Step::CrossChecked { round, findings }
    if round < rounds: repair each finding
        Missing { words }        -> a new unit (Request, FoundBy::CrossCheck, next unit id),
                                    routed and framed like a unit coverage adds, planned and chained
        WrongValue { act, .. }   -> chain::revisit(.., Revisit::Value { note })
        WrongRecord { act, .. }  -> chain::revisit(.., Revisit::Record { note })
        NotAsked { act }         -> chain::revisit(.., Revisit::Asked { note })
    else (last round): hold each finding
        WrongValue { act, argument, .. } -> remove the argument; NeedsValue { arguments: [argument], reason: None }
        WrongRecord / NotAsked            -> Chained::NotUnderstood { reason: Unclear, aimed: Some(target) }
        Missing { words }                 -> a new unit, NotUnderstood { reason: Unclear }
    a repair the budget refuses holds that act as the last round would
```

The note for each repair is written by code from the finding's structure, never from free text:

```rust
fn note(finding: &Finding, turn: &UnderstandingInput) -> String {
    let said = |span: &Span| turn.message.slice(*span).unwrap_or_default().to_owned();
    match finding {
        Finding::WrongValue { argument, words, .. } => {
            format!("the value of {argument} is in «{}»", said(words))
        }
        Finding::WrongRecord { words, .. } => format!("the record meant is named in «{}»", said(words)),
        Finding::NotAsked { .. } => "the message may not ask for this act".to_owned(),
        Finding::Missing { words } => format!("«{}» asks for something", said(words)),
    }
}
```

`CrossCheckInput` is built from what the pipeline holds after the chains: each `Chained::Act` as a `ShownAct` (`id` = `act.id.to_string()`; `line` = `"{id} {operation} on {record}: {name} {value} (words a to b); …"` using `render::understood` for values and the record's label from `turn.record(token)`, `"a new {workflow} record"` for `ActTarget::New`, `"the record {act} creates"` for `ActTarget::SameTurn`); questions and constraints by the words of their units; `unread` from the not-understood units' words; `held` from the spans of question, constraint, chitchat and dispute units and of every argument's excerpt.

- [ ] **Step 1: Write the failing tests**

Each test scripts a turn with the support module's trip (`turn(...)`), passes `Settings::conservative().with_cross_check_rounds(2)` through `with_settings`, and scripts `turn/cross_check` (and `turn/cross_check.round2` where a second round runs). The first, in full:

```rust
//! A whole-turn check with no findings leaves the understanding as it was, after one call.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::Settings;

#[tokio::test]
async fn a_whole_turn_check_that_finds_nothing_changes_nothing() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("turn/cross_check", json!({"findings": []}));
    let input = turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(act.arguments["value"].value, ArgumentValue::Json("Lisbon".into()));
    assert!(run.was_called("turn/cross_check"));
    assert!(!run.was_called("turn/cross_check.round2"), "nothing found, nothing checked again");
}
```

The others, by what they script and assert:

- `a_value_the_whole_turn_check_doubts_is_extracted_again`: message «name Lisbon for March» ([1]name [2]Lisbon [3]for [4]March); `u1/extract` gives words 2–2, verify confirms; `turn/cross_check` answers `{"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value", "words": {"from": 2, "to": 4}}]}`; `u1/extract.after_cross_check` gives words 2–4; `u1/verify.after_cross_check` confirms; `turn/cross_check.round2` answers no findings. Assert the value is «Lisbon for March» and the extract call after the check shows `A check of the whole message found: the value of value is in «Lisbon for March»`.
- `a_record_the_whole_turn_check_doubts_is_located_again`: two trips in view (`trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")])`), message «set the Haddad trip name to Lisbon»; `u1/locate` picks `r1`; the check answers `wrong_record` for `u1.a1` with the words «Haddad»; `u1/locate.after_cross_check` picks `r2`; extract and verify after the check confirm; round 2 finds nothing. Assert the target is trip 2's token.
- `an_act_the_whole_turn_check_says_was_not_asked_is_verified_again`: the check answers `not_asked` for `u1.a1`; `u1/verify.after_cross_check` answers `overall: not_requested`. Assert no act, and a `NotUnderstood` with `NotRequested`.
- `words_the_whole_turn_check_finds_unread_are_read`: message «name Lisbon and fly tomorrow»; segmentation holds words 1–2 only and coverage finds nothing; the check answers `missing` words 4–5; `u2/route` routes `SET_DATE`, `u2/extract` gives tomorrow, `u2/verify` confirms; round 2 finds nothing. Assert two acts, and the second unit is `FoundBy::CrossCheck`.
- `an_act_two_checks_still_doubt_is_held`: both rounds answer `wrong_value` for `u1.a1`'s `value`. Assert `NeedsValue { arguments: ["value"], .. }` and no `value` argument.
- `a_whole_turn_check_the_budget_cannot_pay_for_is_skipped`: the understanding runs in a scope whose budget allows exactly the calls before the check (`understand_in(script, &input, Budget { max_model_calls: Some(4), ..Budget::understanding() })`, a support helper added here). Assert the act is `Ready` as the first reading left it, `turn/cross_check` was not sent, and a `CrossCheckSkipped` step was published. A second test in the same file: the budget pays for round 1 and not for the repair; assert the doubted act is held.
- `a_turn_with_no_act_is_not_checked_whole`: message «what is the name?» read as a question only. Assert `turn/cross_check` was never called.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p turnframe-understand whole_turn`
Expected: FAIL: the check is never called.

- [ ] **Step 3: Implement** `pipeline/cross_check.rs` as specified above, and in `understand_checked`, after `names_the_waiting_record(turn, &mut chained);`:

```rust
        if cx.settings.cross_check_rounds > 0 {
            self.cross_checked(&cx, &mut units, &mut planned, &mut chained, &mut not_understood, &questions)
                .await;
        }
```

(`units`, `planning.planned` and `planning.not_understood` become mutable locals, since a `missing` finding adds a unit, its plans and possibly an unread entry.) New units take ids after the largest in `units`, and their acts are chained with a `Context` whose `creations` include the first round's.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-understand`
Expected: PASS, every existing test included (they run with `cross_check_rounds = 0`).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: at high effort the whole turn is checked, and what it doubts is read again or held"
```

---

### Task 8: Effort in the runtime

**Files:**
- Create: `crates/turnframe-runtime/src/effort.rs` (`EffortConfig`, `EffortOverrides`, `EffortProfile`, `resolve`)
- Modify: `crates/turnframe-runtime/src/config.rs` (`OrchestratorConfig::effort`, `conservative()`, `validate()`, `with_effort`)
- Modify: `crates/turnframe-runtime/src/lib.rs` (`pub mod effort;`)
- Modify: `crates/turnframe-runtime/src/orchestrator/session/mod.rs` (the session resolves its `EffortProfile` once, from `input.effort` or `config.effort.default`, and sets `record.effort`)
- Modify: `crates/turnframe-runtime/src/understand.rs` (`Sources::effort`; `run` scopes with the level's budget, profiles and effort; `input` adds `with_settings`)
- Modify: `crates/turnframe-runtime/src/orchestrator/session/understand.rs` and `crates/turnframe-runtime/src/planning.rs` (pass `effort` in both `Sources` literals; step prose only when `effort.steps`)
- Modify: `crates/turnframe-runtime/src/compose/mod.rs` (`CompositionInput::effort` and `with_effort`; `compose` scopes with `reply_budget` and the level's profiles)
- Modify: `crates/turnframe-runtime/src/narrate/mod.rs` (review decided by `self.engine.profile(self.scope, TaskKind::Acknowledge).review`)
- Modify: `crates/turnframe-runtime/src/orchestrator/mod.rs` (turn signals labelled with the turn's effort)
- Test files, in `crates/turnframe-runtime/tests/`:
  - `a_turn_forced_to_high_runs_at_high.rs`
  - `a_turn_left_alone_runs_at_the_configured_level.rs`
  - `at_low_the_reply_is_not_reviewed_and_acts_are_still_verified.rs`
  - `a_click_costs_no_call_at_any_effort.rs`
- Test: unit tests at the foot of `effort.rs`

**Interfaces:**
- Consumes: `Effort`, `TurnInput::effort`, `ReplayRecord::effort`, `SignalLabels::with_effort` (Task 1); `TaskScope::with_profiles`, `with_effort`, `TaskEngine::profile`, `ProfileChanges`, `ProfileChange`, `TaskProfile::with_reasoning`, `with_review` (Task 2); `Disagreement::Reread` (Task 3); `Settings` fields and `UnderstandingInput::with_settings` (Task 4); `TaskKind::CrossCheck` (Task 5).
- Produces:

```rust
pub struct EffortOverrides {            // #[serde(default, deny_unknown_fields)], #[non_exhaustive]
    pub tasks: ProfileChanges,
    pub budget: Option<Budget>,
    pub reply_budget: Option<Budget>,
    pub settings: Option<Settings>,
}
pub struct EffortConfig {               // #[serde(default, deny_unknown_fields)], #[non_exhaustive]
    pub default: Effort,
    pub low: EffortOverrides,
    pub medium: EffortOverrides,
    pub high: EffortOverrides,
}
pub struct EffortProfile {              // #[non_exhaustive]
    pub effort: Effort,
    pub tasks: TaskProfiles,
    pub budget: Budget,
    pub reply_budget: Budget,
    pub settings: Settings,
    pub steps: bool,
}
pub fn resolve(config: &OrchestratorConfig, effort: Effort) -> EffortProfile
```

- [ ] **Step 1: Write the failing unit tests** (foot of `effort.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorConfig;
    use turnframe_provider::request::ReasoningEffort;
    use turnframe_tasks::{Disagreement, TaskKind};

    #[test]
    fn medium_is_the_configuration_as_it_is() {
        let config = OrchestratorConfig::conservative();
        let medium = resolve(&config, Effort::Medium);
        assert_eq!(medium.tasks, config.understanding.tasks);
        assert_eq!(medium.budget, config.understanding.budget);
        assert_eq!(medium.reply_budget, config.narration.budget);
        assert_eq!(medium.settings, config.understanding.settings);
        assert!(medium.steps);
    }

    #[test]
    fn high_buys_votes_reasoning_and_the_whole_turn_check() {
        let config = OrchestratorConfig::conservative();
        let high = resolve(&config, Effort::High);
        let segment = high.tasks.get(TaskKind::Segment);
        assert_eq!(segment.votes, 3);
        assert_eq!(segment.on_disagreement, Disagreement::Reread);
        assert_eq!(high.tasks.get(TaskKind::Extract).reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(high.tasks.get(TaskKind::Verify).votes, 3);
        assert_eq!(high.settings.cross_check_rounds, 2);
        assert!(high.settings.reread_small_talk);
        assert_eq!(high.settings.transcript, 6);
        assert_eq!(
            high.budget.max_model_calls,
            config.understanding.budget.max_model_calls.map(|calls| calls * 3)
        );
    }

    #[test]
    fn low_drops_the_review_and_the_step_prose_and_keeps_verification() {
        let config = OrchestratorConfig::conservative();
        let low = resolve(&config, Effort::Low);
        assert!(!low.tasks.get(TaskKind::Acknowledge).review);
        assert!(!low.steps);
        assert_eq!(low.settings.verify, config.understanding.settings.verify);
        assert_eq!(low.settings.transcript, 2);
    }

    #[test]
    fn a_deployment_changes_a_level_field_by_field() {
        let effort: EffortConfig = toml::from_str(
            r#"
            default = "high"
            [high.tasks.extract]
            model = "large"
            "#,
        )
        .unwrap();
        let mut config = OrchestratorConfig::conservative();
        config.effort = effort;
        let high = resolve(&config, config.effort.default);
        let extract = high.tasks.get(TaskKind::Extract);
        assert_eq!(extract.model.as_deref(), Some("large"));
        assert_eq!(extract.reasoning_effort, Some(ReasoningEffort::Low), "the level's own change stays");
    }

    #[test]
    fn a_level_naming_a_task_that_does_not_exist_is_refused() {
        let refused = toml::from_str::<EffortConfig>("[high.tasks.extrakt]\nvotes = 3\n").unwrap_err();
        assert!(refused.to_string().contains("extrakt"), "{refused}");
    }
}
```

- [ ] **Step 2: Write the failing integration tests**

`a_turn_forced_to_high_runs_at_high.rs` runs the real pipeline over scripted tasks (`Harness::builder().understanding_tasks(tasks)`), with a message the harness's trip answers, three segment answers and three route answers queued (votes), and a `turn/cross_check` answer with no findings:

```rust
//! A turn forced to high effort runs at high: its understanding calls reason, its
//! segmentation votes, the whole turn is checked, and the record says so.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::effort::Effort;
use turnframe_core::ids::TurnId;
use turnframe_provider::request::ReasoningEffort;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "name Lisbon";

fn segmented() -> serde_json::Value {
    json!({"analysis": "Sets the name.", "units": [
        {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"}
    ]})
}

#[tokio::test]
async fn a_turn_forced_to_high_runs_at_high() {
    let tasks = Arc::new(
        ScriptedTasks::new("scripted", "small")
            .answer("turn/segment", segmented())
            .answer("turn/segment", segmented())
            .answer("turn/segment", segmented())
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", json!({"arguments": {"value": {"kind": "words", "text": "Lisbon", "message": "current", "from": 2, "to": 2}}}))
            .answer("u1/verify", json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}))
            .answer("u1/verify", json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}))
            .answer("u1/verify", json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}))
            .answer("turn/cross_check", json!({"findings": []})),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .without_narration()
        .build()
        .await;
    let turn_id = TurnId::from(uuid::Uuid::from_u128(1));
    let mut turn = harness.turn(turn_id, TEXT);
    turn.effort = Some(Effort::High);

    harness.handle(turn).await.unwrap();

    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    let calls = tasks.calls();
    assert!(calls.iter().all(|call| call.reasoning_effort == Some(ReasoningEffort::Low)));
    assert_eq!(calls.iter().filter(|call| call.metadata.get("task").is_some_and(|t| t.starts_with("turn/segment"))).count(), 3);
    assert!(calls.iter().any(|call| call.metadata.get("task").is_some_and(|t| t.starts_with("turn/cross_check"))));
    assert_eq!(harness.replay(turn_id).await.effort, Effort::High);
}
```

(Use the constant scripted tasks use for the task label, `turnframe_tasks::testing::TASK_LABEL`, in place of `"task"`, and the extract answer shape the understand tests use for a text value.)

`a_turn_left_alone_runs_at_the_configured_level.rs`: the same turn with `turn.effort = None` and a config whose `effort.default` is `Effort::High` behaves as above; with the default config, one segment call and no `turn/cross_check`, and the record says `Medium`.

`at_low_the_reply_is_not_reviewed_and_acts_are_still_verified.rs`: a narrating harness (`narrating()` provider acknowledging once), `turn.effort = Some(Effort::Low)`; assert `provider.calls_for(ModelPurpose::Review)` is empty, the understanding tasks called `u1/verify`, and the subject was set.

`a_click_costs_no_call_at_any_effort.rs`: for each of `Effort::ALL`, a card click turn (`text: None`, `interaction_response: Some(..)`, `effort: Some(level)`) on the trip's send card; assert no understanding task was called.

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test -p turnframe-runtime effort`
Expected: FAIL to compile: `crate::effort` does not exist.

- [ ] **Step 4: Implement `effort.rs`**

```rust
//! The effort a turn runs at, resolved into the profiles, budgets and settings it runs
//! under. `medium` with nothing configured is the configuration as it is.

use serde::{Deserialize, Serialize};
use turnframe_core::effort::Effort;
use turnframe_provider::request::ReasoningEffort;
use turnframe_tasks::{Budget, Disagreement, ProfileChange, ProfileChanges, TaskKind, TaskProfiles};
use turnframe_understand::Settings;

use crate::config::OrchestratorConfig;

/// What a deployment changes about one level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct EffortOverrides {
    /// Task profile changes, over the level's own.
    pub tasks: ProfileChanges,
    /// The understanding budget, replacing the level's.
    pub budget: Option<Budget>,
    /// The reply budget, replacing the level's.
    pub reply_budget: Option<Budget>,
    /// The pipeline settings, replacing the level's.
    pub settings: Option<Settings>,
}

/// The default level, and what a deployment changes about each.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct EffortConfig {
    /// The level of a turn that does not force one.
    pub default: Effort,
    pub low: EffortOverrides,
    pub medium: EffortOverrides,
    pub high: EffortOverrides,
}

impl EffortConfig {
    fn overrides(&self, effort: Effort) -> &EffortOverrides {
        match effort {
            Effort::Low => &self.low,
            Effort::High => &self.high,
            _ => &self.medium,
        }
    }
}

/// One level, resolved: what a turn at that level runs under.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EffortProfile {
    pub effort: Effort,
    pub tasks: TaskProfiles,
    pub budget: Budget,
    pub reply_budget: Budget,
    pub settings: Settings,
    /// Whether step prose may be written, when narration asks for it.
    pub steps: bool,
}

const READING: [TaskKind; 8] = [
    TaskKind::Segment,
    TaskKind::Coverage,
    TaskKind::Route,
    TaskKind::Locate,
    TaskKind::Extract,
    TaskKind::Verify,
    TaskKind::QuestionFrame,
    TaskKind::CrossCheck,
];

fn change() -> ProfileChange {
    ProfileChange::default()
}

/// The level's own changes, before a deployment's.
fn shipped(effort: Effort) -> ProfileChanges {
    let mut changes = ProfileChanges::default();
    match effort {
        Effort::Low => {
            let mut review = change();
            review.review = Some(false);
            changes = changes.with(TaskKind::Acknowledge, review);
        }
        Effort::High => {
            for kind in READING {
                let mut reasoning = change();
                reasoning.reasoning_effort = Some(ReasoningEffort::Low);
                if matches!(kind, TaskKind::Segment | TaskKind::Route) {
                    reasoning.votes = Some(3);
                    reasoning.on_disagreement = Some(Disagreement::Reread);
                }
                if kind == TaskKind::Verify {
                    reasoning.votes = Some(3);
                }
                changes = changes.with(kind, reasoning);
            }
            let mut review = change();
            review.reasoning_effort = Some(ReasoningEffort::Low);
            changes = changes.with(TaskKind::Review, review);
        }
        _ => {}
    }
    changes
}

fn scaled(budget: Budget) -> Budget {
    let mut scaled = budget;
    scaled.max_model_calls = budget.max_model_calls.map(|calls| calls.saturating_mul(3));
    scaled.max_prompt_tokens = budget.max_prompt_tokens.map(|tokens| tokens.saturating_mul(3));
    scaled.max_chain_depth = budget.max_chain_depth.map(|depth| depth.saturating_add(4));
    scaled.max_wall_clock_secs = budget.max_wall_clock_secs.map(|secs| secs.saturating_mul(2));
    scaled
}

/// What a turn at `effort` runs under.
#[must_use]
pub fn resolve(config: &OrchestratorConfig, effort: Effort) -> EffortProfile {
    let base = &config.understanding;
    let mut settings = base.settings;
    let mut budget = base.budget;
    let mut steps = true;
    match effort {
        Effort::Low => {
            settings = settings.with_transcript(2);
            steps = false;
        }
        Effort::High => {
            settings = settings
                .with_transcript(6)
                .with_reread_small_talk(true)
                .with_cross_check_rounds(2);
            budget = scaled(budget);
        }
        _ => {}
    }
    let overrides = config.effort.overrides(effort);
    let tasks = overrides.tasks.apply(shipped(effort).apply(base.tasks.clone()));
    EffortProfile {
        effort,
        tasks,
        budget: overrides.budget.unwrap_or(budget),
        reply_budget: overrides.reply_budget.unwrap_or(config.narration.budget),
        settings: overrides.settings.unwrap_or(settings),
        steps,
    }
}
```

(`Budget`'s field types decide the exact `saturating_*` calls; `Settings::with_transcript` exists as `pub const fn with_transcript(mut self, transcript: usize) -> Self`, add it if its name differs.)

- [ ] **Step 5: Wire it**

- `OrchestratorConfig` gains `/// The default effort, and what each level changes.` `pub effort: EffortConfig,` (`EffortConfig::default()` in `conservative()`), a `with_effort(mut self, effort: EffortConfig) -> Self`, and in `validate()` each level's budget override is checked like `understanding.budget`.
- The session holds `effort: EffortProfile`, resolved once where the session is created: `crate::effort::resolve(config, input.effort.unwrap_or(config.effort.default))`; `self.record.effort = self.effort.effort`.
- `Sources` gains `pub effort: &'a EffortProfile`. In `understand::run`:

```rust
    let scope = turnframe_tasks::TaskScope::new(sources.effort.budget, sources.locale.clone())
        .for_turn(sources.turn.to_string())
        .with_profiles(sources.effort.tasks.clone())
        .with_effort(sources.effort.effort);
```

  and `input()` ends with `turn = turn.with_settings(sources.effort.settings);`.
- Step prose: `said: (narration.enabled && narration.steps && self.effort.steps).then_some(said)`.
- `CompositionInput` gains `effort: Option<&'a EffortProfile>` through its builder macro (`with_effort`); the session passes `Some(&self.effort)`. In `compose`:

```rust
        let (budget, profiles) = match input.effort {
            Some(effort) => (effort.reply_budget, Some(effort.tasks.clone())),
            None => (self.narration.budget, None),
        };
        let mut scope = TaskScope::new(budget, locale.clone()).for_turn(input.turn.turn_id.to_string());
        if let Some(profiles) = profiles {
            scope = scope.with_profiles(profiles);
        }
        if let Some(effort) = input.effort {
            scope = scope.with_effort(effort.effort);
        }
```

- `narrate/mod.rs`: `if !self.engine.profile(self.scope, TaskKind::Acknowledge).review {`.
- `orchestrator/mod.rs`: the turn's labels are `SignalLabels::none().with_effort(input.effort.unwrap_or(self.config.effort.default))` for `TurnReceived`, `TurnDuration`, `TurnCompleted` and `TurnFailed`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p turnframe-runtime`
Expected: PASS, every existing test included.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: a turn runs at the effort it forces, else the configured one"
```

---

### Task 9: Effort in telemetry

**Files:**
- Modify: `crates/turnframe-telemetry/src/metrics.rs` (`LabelKey::Effort`, `ALL` to 9, `as_str` `"effort"`, `value_of`; `documented_labels` adds `Effort` to the turn signals and to `TaskCompleted`)
- Modify: `crates/turnframe-telemetry/src/tracing.rs` (`field::EFFORT = "effort"`; pushed from `labels.effort`; listed wherever the field set is enumerated)
- Modify: `docs/telemetry.md` (the label and the field)
- Test: unit tests beside the existing label tests in `metrics.rs` and `tracing.rs`

- [ ] **Step 1: Write the failing tests**

In `metrics.rs`'s tests:

```rust
    #[test]
    fn a_turn_is_counted_by_its_effort() {
        let labels = SignalLabels::none().with_effort(turnframe_core::effort::Effort::High);
        assert!(documented_labels(Signal::TurnCompleted).contains(&LabelKey::Effort));
        assert_eq!(LabelKey::Effort.value_of(&labels).as_deref(), Some("high"));
    }
```

In `tracing.rs`'s tests, the same labels produce a span field `effort = "high"`, asserted the way the existing purpose field test asserts `purpose`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p turnframe-telemetry`
Expected: FAIL to compile: no `LabelKey::Effort`.

- [ ] **Step 3: Implement** as listed under Files: `Self::Effort => labels.effort.map(|effort| effort.as_str().to_owned())`, and `push(field::EFFORT, labels.effort.map(|effort| effort.as_str().to_owned()));`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p turnframe-telemetry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: turns and tasks are measured by their effort"
```

---

### Task 10: The console chooses a level

**Files:**
- Modify: `examples/console/src/main.rs` (`/effort low|medium|high`; the level on every turn's input)
- Modify: `examples/console/src/print.rs` (`help` lists `/effort`; the `spent` line names the level)

- [ ] **Step 1: Implement**

In the input loop, beside `/state`:

```rust
            command if command.starts_with("/effort") => {
                match command.trim_start_matches("/effort").trim().parse::<Effort>() {
                    Ok(level) => {
                        effort = level;
                        println!("  {}", style::muted(&format!("Effort: {level}.")));
                    }
                    Err(error) => println!("  {}", style::warning(&error.to_string())),
                }
                continue;
            }
```

with `let mut effort = Effort::Medium;` before the loop and `effort: Some(effort),` in the `TurnInput`. `print::understood` receives the level and prints `spent      7 call(s), 3106 prompt tokens, effort high`. `help()` gains `/effort low|medium|high   how hard each turn works to read you (medium)`.

- [ ] **Step 2: Check it**

Run: `cargo clippy -p console -- -D warnings && printf '/effort high\n/effort extreme\n/quit\n' | NO_COLOR=1 cargo run -q -p console 2>&1 | tail -4`
Expected: `Effort: high.` then the unknown-level message. No model call: `/effort` is handled before a turn is built. (If no key is set the console falls back to Ollama at start; the commands still run.)

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat: the console sets the effort of the turns that follow"
```

---

### Task 11: The live corpus at each level, with a complex section

**Files:**
- Modify: `crates/turnframe-eval/tests/live_corpus.rs` (`TURNFRAME_EVAL_EFFORT`; the level in the report header and on each turn)
- Modify: `crates/turnframe-eval/tests/support/mod.rs` (the harness passes `effort` into `TurnInput`)
- Create: eight items in `crates/turnframe-eval/tests/live_corpus/`, named `complex_*.toml`, tagged `complex`
- Test: the existing corpus-loading test parses them (run `cargo test -p turnframe-eval`)

The items, synthetic, each with its expectations as `case_state` checks where the record exists before the turn and `events` where the turn creates records:

1. `complex_trip_in_full_en`: an empty trip for Marta Bianchi in view. «Fill in this trip: 3 nights at the hotel at 80 euros a night and a lounge pass at 15 euros, the name is September offsite, and I would rather fly at the end of next month.» Expect `case_state` `/name` «September offsite» (ignore case), `/travel_date` the end of next month, `/extras/0/quantity` 3, `/extras/0/unit_price_cents` 8000, `/extras/1/unit_price_cents` 1500, `/extras/2` absent.
2. `complex_new_traveler_for_a_trip_it`: an empty trip with no traveler, no traveler records. «Registra la viaggiatrice Nadia Rinaldi, email nadia@rinaldi.example, e mettila su questo viaggio; poi aggiungi due notti d'albergo a 400 euro l'una.» Expect `/traveler/display_name` «Nadia Rinaldi», an extra of 2 × 40000 cents.
3. `complex_rename_and_question_en`: traveler Marta Bianchi active, trip 1 collecting. «Change Marta Bianchi's name to Marta Bianchi Ferri, and while you are at it, what is still missing on the trip?» Expect `/full_name` «Marta Bianchi Ferri»; the trip's extras unchanged.
4. `complex_correction_inside_en`: trip 1 collecting. «Add an extra for the hotel night at 150 euros, no wait, 135, and one for the airport transfer at 40 euros; don't rebook anything yet.» Expect two extras at 13500 and 4000 cents and no third; forbid events `trip.rebooking_requested`, `trip.rebooking_sent`.
5. `complex_discursive_it`: trip 1 and traveler Marta Bianchi (loyalty number untouched). «Allora, ieri ho sentito la viaggiatrice e mi ha confermato tutto, quindi metti come nome offsite ottobre e la data del viaggio al 30 novembre; ah, il numero fedeltà della viaggiatrice è AZ1234567.» Expect `/name` «offsite ottobre» (ignore case), `/travel_date` 30 November, traveler `/loyalty_number/answered/value` «AZ1234567».
6. `complex_two_trips_en`: trips 1 (Marta Bianchi) and 2 (Omar Haddad) collecting. «On the Haddad trip set the name to Porto, and on the Bianchi one add 2 nights at the hotel at 60 euros.» Expect `trip-2` `/name` «Porto» and its extras unchanged; `trip-1` gains an extra of 2 × 6000 cents and keeps its name.
7. `complex_earlier_value_it`: history: user «il viaggio è per l'offsite di Lisbona», assistant «Che cosa aggiungo alla pratica?». Message «3 notti d'albergo a 50 euro a notte, e come nome metti quello che ti ho detto prima.» Expect an extra of 3 × 5000 cents; `/name` one of «offsite di Lisbona», «l'offsite di Lisbona» (ignore case).
8. `complex_traveler_complete_and_question_it`: traveler Nadia Rinaldi in collection. «Per Nadia Rinaldi l'email è nadia@rinaldi.example e il numero fedeltà AZ7654321. Di solito quanto bagaglio a mano è incluso nel biglietto?» Expect the email and loyalty number set, the activation card active, and no `traveler.activated` event.

- [ ] **Step 1: Write the items and run the offline corpus tests**

Run: `cargo test -p turnframe-eval`
Expected: PASS: every item parses, and the scripted checks that walk the corpus accept them.

- [ ] **Step 2: Add the level to the live run**

```rust
/// The effort every turn of the run is forced to: low, medium or high.
const EFFORT_VARIABLE: &str = "TURNFRAME_EVAL_EFFORT";
```

Read it next to the model variable (`medium` when unset; an unknown value stops the run with the parse error), pass it to the harness, print `effort {level}` in the report header, and write it into the JSON report.

- [ ] **Step 3: Run offline**

Run: `cargo test -p turnframe-eval`
Expected: PASS (the live test skips without a key).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test: complex messages in the live corpus, and runs at each effort"
```

---

### Task 12: Documents

**Files:**
- Create: `docs/adr/ADR-020-effort-buys-judgment-never-authority.md` (status Accepted; context, decision, consequences; in the style of ADR-019)
- Modify: `docs/adr/README.md` (the index row)
- Modify: `docs/architecture.md` (the `cross_check` row in the task table; a short «Effort» subsection after the table: levels, what each changes, what none changes)
- Modify: `docs/reliability-model.md` (effort changes how often a turn is read right; the safety rows do not move with it)
- Modify: `crates/turnframe-runtime/README.md`, `crates/turnframe-understand/README.md` (a paragraph each)
- Modify: `CHANGELOG.md` (Added: effort levels, the whole-turn check, `Disagreement::Reread`, `ProfileChange`; Changed, breaking: `TurnInput::effort`, `ReplayRecord::effort`, `FoundBy::CrossCheck`, `ModelPurpose::CrossCheck`)
- Modify: `README.md` (the console paragraph mentions `/effort`)
- Modify: `docs/superpowers/specs/2026-09-27-effort-levels-design.md` (status: accepted, implemented; note the telemetry field is `effort`, as every other span field, and `FoundBy::CrossCheck`)

- [ ] **Step 1: Write them.** Copy rules from Global Constraints apply to every line.
- [ ] **Step 2: Check them**

Run: `cargo test -p turnframe-eval --test documentation && git diff --stat -- '*.md' && git diff -- '*.md' | grep '^+' | grep -c '—'`
Expected: the pinned documentation test passes; the em dash count is 0.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs: effort levels, the whole-turn check, and ADR-020"
```

---

### Task 13: Measure, gate, record

- [ ] **Step 1: Announce the spend, then run the live corpus at each level** (gpt-5.4-mini, one sample each; about 58 items × 3 levels)

```bash
set -a; source .env; set +a
for level in low medium high; do
  TURNFRAME_EVAL_LIVE_KEY="$OPENAI_API_KEY" TURNFRAME_EVAL_LIVE_MODEL=gpt-5.4-mini \
  TURNFRAME_EVAL_EFFORT=$level TURNFRAME_EVAL_LIVE_REPORT=target/live-$level.json \
  cargo test -p turnframe-eval --test live_corpus -- --nocapture 2>&1 | tail -20
done
```

- [ ] **Step 2: Record the results** in `docs/benchmarks.md`: a table per level with items passed (complex section and the rest), model calls, prompt tokens, p50 and p95 turn latency, read from the three reports. State the success criterion from the spec and whether it was met, and name the items `high` still fails.

- [ ] **Step 3: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --no-fail-fast
cargo test --workspace --all-features --doc
cargo doc --workspace --all-features --no-deps
cargo +1.88 check --workspace --all-features --all-targets
cargo deny --all-features check
cargo audit
cargo bench --workspace --no-run
```

Expected: all pass; `cargo audit` reports only RUSTSEC-2023-0071 (pre-existing, via sqlx/rsa). Check the comment limits on the whole diff since the spec commit.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "docs: effort levels measured on the live corpus"
```
