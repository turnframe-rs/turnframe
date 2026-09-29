//! Running a task: the call, repairs with the exact error, votes, escalation, records.
//!
//! One run is: resolve the instructions, build the request from the task's messages
//! and schema and its kind's profile, reserve the call, send it, parse and check the
//! answer. A structurally wrong answer goes back to the same model with the error,
//! up to the profile's repairs. Votes run concurrently and are compared with the
//! task's own `agree`; a vote without a strict majority escalates, asks or fails, as
//! the profile says. Every call leaves a record in the scope.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::future::join_all;
use turnframe_core::effort::Effort;
use turnframe_core::hash::Digest;
use turnframe_core::locale::Locale;
use turnframe_core::observe::{NoopObserver, Observer, Signal, SignalLabels};
use turnframe_core::prompt::{PromptSelector, PromptSource};
use turnframe_core::replay::{BudgetReport, TaskParams, TaskRecord, TaskVerdict};
use turnframe_provider::fallback::{FallbackOptions, FallbackStage, execute_with_fallback};
use turnframe_provider::ids::ModelRef;
use turnframe_provider::request::{CacheHint, Message, ModelRequest, OutputSpec};
use turnframe_provider::response::ModelResponse;
use turnframe_provider::router::{ProviderRouter, RoutingPolicy};
use turnframe_provider::structured::{CompiledSchema, SchemaCache, parse_structured};

use crate::budget::{Budget, BudgetBound, BudgetTracker};
use crate::instructions::{self, Instructions};
use crate::profile::{Disagreement, TaskProfile, TaskProfiles};
use crate::task::{ModelTask, TaskId, TaskKind};

/// The request metadata key carrying the task id a call runs as: `u2/extract#repair1`.
pub const TASK_LABEL: &str = "task";

/// The request metadata key carrying the turn a call belongs to, when the scope names it.
pub const TURN_LABEL: &str = "turn";

/// Framing of a repair round when no prompt source supplies `<name>.repair`.
const BUILT_IN_REPAIR: &str = "Your previous answer was not accepted. Answer again with a \
     document that satisfies the schema and fixes this:";

/// What a turn keeps of each call beyond the parsed answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordPolicy {
    /// Keep the request as sent. It carries the user's words.
    pub keep_prompts: bool,
    /// Keep the answer as received, before parsing.
    pub keep_raw_output: bool,
}

/// The spending and the records of one phase of one turn.
#[derive(Debug)]
pub struct TaskScope {
    budget: BudgetTracker,
    records: Mutex<Vec<TaskRecord>>,
    locale: Locale,
    turn: Option<String>,
    profiles: Option<TaskProfiles>,
    effort: Option<Effort>,
}

impl TaskScope {
    /// A scope bounded by `budget`, for a turn in `locale`.
    #[must_use]
    pub fn new(budget: Budget, locale: Locale) -> Self {
        Self {
            budget: BudgetTracker::new(budget),
            records: Mutex::new(Vec::new()),
            locale,
            turn: None,
            profiles: None,
            effort: None,
        }
    }

    /// Runs this scope's tasks under `profiles` instead of the engine's.
    #[must_use]
    pub fn with_profiles(mut self, profiles: TaskProfiles) -> Self {
        self.profiles = Some(profiles);
        self
    }

    /// Labels this scope's task signals with the turn's effort.
    #[must_use]
    pub const fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// The effort this scope's turn runs at, when it was given one.
    #[must_use]
    pub const fn effort(&self) -> Option<Effort> {
        self.effort
    }

    fn labels(&self) -> SignalLabels {
        let labels = SignalLabels::default();
        match self.effort {
            Some(effort) => labels.with_effort(effort),
            None => labels,
        }
    }

    /// Labels every call of this scope with the turn it belongs to, for a trace or a
    /// provider's own logs to group them.
    #[must_use]
    pub fn for_turn(mut self, turn: impl Into<String>) -> Self {
        self.turn = Some(turn.into());
        self
    }

    /// Every call recorded so far, in the order the calls finished.
    #[must_use]
    pub fn records(&self) -> Vec<TaskRecord> {
        self.lock().clone()
    }

    /// What was spent.
    #[must_use]
    pub fn budget_report(&self) -> BudgetReport {
        self.budget.report()
    }

    /// The first bound reached, if any.
    #[must_use]
    pub fn exhausted(&self) -> Option<BudgetBound> {
        self.budget.exhausted()
    }

    /// The turn's locale.
    #[must_use]
    pub const fn locale(&self) -> &Locale {
        &self.locale
    }

    fn push(&self, record: TaskRecord) {
        self.lock().push(record);
    }

    fn mark(&self, task_id: &str, verdict: &TaskVerdict) {
        if let Some(record) = self.lock().iter_mut().find(|r| r.task_id == task_id) {
            record.verdict = verdict.clone();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TaskRecord>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where a task call sits: its identifier, its parent, and how deep its chain already is.
#[derive(Debug, Clone, Copy)]
pub struct TaskCall<'a> {
    /// The task's identifier.
    pub id: &'a TaskId,
    /// The task that asked for this one.
    pub parent: Option<&'a TaskId>,
    /// Calls already made in this chain; the first call of a turn is at depth 1.
    pub depth: u8,
}

/// Why a task produced no answer to use.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskFailure {
    /// The profile switches the task off.
    Disabled,
    /// A bound of the turn's budget was reached.
    Budget(BudgetBound),
    /// No profile can serve the task.
    Routing(String),
    /// Every provider attempt failed.
    Provider(String),
    /// The answers failed their checks, repairs included.
    Invalid {
        /// Stable code of the last failed check.
        code: String,
        /// The failure in words.
        reason: String,
    },
    /// The votes found no majority and the profile treats that as failure.
    Disagreement,
}

impl TaskFailure {
    /// Stable label, for records and metrics.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Disabled => "disabled".to_owned(),
            Self::Budget(bound) => format!("budget_{}", bound.as_str()),
            Self::Routing(_) => "routing".to_owned(),
            Self::Provider(code) => format!("provider_{code}"),
            Self::Invalid { code, .. } => code.clone(),
            Self::Disagreement => "vote_disagreement".to_owned(),
        }
    }
}

/// What a task run produced, and how deep its chain ended.
#[derive(Debug, Clone)]
pub enum TaskOutcome<O> {
    /// An answer to use.
    Accepted {
        /// The answer.
        output: O,
        /// Depth of the last call made.
        depth: u8,
    },
    /// The votes found no majority, and the profile hands the answers back to ask.
    Disagreed {
        /// Every answer the votes produced.
        answers: Vec<O>,
        /// Depth of the last call made.
        depth: u8,
    },
    /// No answer to use.
    Failed {
        /// Why.
        failure: TaskFailure,
        /// Depth of the last call made.
        depth: u8,
    },
}

impl<O> TaskOutcome<O> {
    /// The answer, when there is one to use.
    #[must_use]
    pub fn accepted(self) -> Option<O> {
        match self {
            Self::Accepted { output, .. } => Some(output),
            _ => None,
        }
    }

    /// Depth of the last call made, for the task that follows.
    #[must_use]
    pub const fn depth(&self) -> u8 {
        match self {
            Self::Accepted { depth, .. }
            | Self::Disagreed { depth, .. }
            | Self::Failed { depth, .. } => *depth,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Accepted { .. } => "accepted".to_owned(),
            Self::Disagreed { .. } => "disagreed".to_owned(),
            Self::Failed { failure, .. } => failure.code(),
        }
    }
}

/// Runs model tasks. Cheap to clone; shared by every turn.
#[derive(Clone)]
pub struct TaskEngine {
    router: Arc<dyn ProviderRouter>,
    routing: RoutingPolicy,
    fallback: Arc<FallbackOptions>,
    schemas: SchemaCache,
    profiles: TaskProfiles,
    prompts: Option<Arc<dyn PromptSource>>,
    selector: PromptSelector,
    records: RecordPolicy,
    observer: Arc<dyn Observer>,
}

impl std::fmt::Debug for TaskEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskEngine")
            .field("profiles", &self.profiles)
            .field("records", &self.records)
            .finish_non_exhaustive()
    }
}

/// Builds a [`TaskEngine`].
#[derive(Debug)]
pub struct TaskEngineBuilder {
    engine: TaskEngine,
}

impl TaskEngineBuilder {
    /// Base routing policy; each call adds its profile's model tag.
    #[must_use]
    pub fn routing(mut self, routing: RoutingPolicy) -> Self {
        self.engine.routing = routing;
        self
    }

    /// Transport retry and fallback options.
    #[must_use]
    pub fn fallback(mut self, options: FallbackOptions) -> Self {
        self.engine.fallback = Arc::new(options);
        self
    }

    /// The profiles every task kind runs under.
    #[must_use]
    pub fn profiles(mut self, profiles: TaskProfiles) -> Self {
        self.engine.profiles = profiles;
        self
    }

    /// A prompt source asked for each task's instructions, pinned or labelled by `selector`.
    #[must_use]
    pub fn prompts(mut self, source: Arc<dyn PromptSource>, selector: PromptSelector) -> Self {
        self.engine.prompts = Some(source);
        self.engine.selector = selector;
        self
    }

    /// What each record keeps beyond the parsed answer.
    #[must_use]
    pub fn records(mut self, policy: RecordPolicy) -> Self {
        self.engine.records = policy;
        self
    }

    /// Where the task signals go.
    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.engine.observer = observer;
        self
    }

    /// The engine.
    #[must_use]
    pub fn build(self) -> TaskEngine {
        self.engine
    }
}

/// What a task run needs beside the task: resolved once, shared by votes and repairs.
#[derive(Clone)]
struct Prepared {
    kind: TaskKind,
    parent: Option<String>,
    profile: TaskProfile,
    instructions: Instructions,
    repair: Instructions,
    schema: CompiledSchema,
    messages: Vec<Message>,
}

/// One chain of calls: the first call and its repairs.
enum Chain<O> {
    Answered {
        output: O,
        depth: u8,
        record: String,
    },
    Unusable {
        failure: TaskFailure,
        depth: u8,
    },
}

impl TaskEngine {
    /// An engine over `router` with the shipped profiles.
    #[must_use]
    pub fn builder(router: Arc<dyn ProviderRouter>) -> TaskEngineBuilder {
        TaskEngineBuilder {
            engine: Self {
                router,
                routing: RoutingPolicy::new(),
                fallback: Arc::new(FallbackOptions::new()),
                schemas: SchemaCache::new(),
                profiles: TaskProfiles::new(),
                prompts: None,
                selector: PromptSelector::Latest,
                records: RecordPolicy::default(),
                observer: Arc::new(NoopObserver),
            },
        }
    }

    /// The profiles in force.
    #[must_use]
    pub const fn profiles(&self) -> &TaskProfiles {
        &self.profiles
    }

    /// The profile `kind` runs under in `scope`: the scope's, else the engine's.
    #[must_use]
    pub fn profile(&self, scope: &TaskScope, kind: TaskKind) -> TaskProfile {
        scope.profiles.as_ref().unwrap_or(&self.profiles).get(kind)
    }

    /// Runs `task` on `input`.
    pub async fn run<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
    ) -> TaskOutcome<T::Output> {
        self.run_inner(scope, call, task, input, None).await
    }

    /// Runs `task` again after `previous` was found wanting, telling the model why.
    ///
    /// For a semantic check made by another task (a verifier) and not by `check`.
    pub async fn run_with_feedback<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
        previous: &T::Output,
        feedback: &str,
    ) -> TaskOutcome<T::Output> {
        self.run_inner(scope, call, task, input, Some((previous, feedback)))
            .await
    }

    async fn run_inner<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
        feedback: Option<(&T::Output, &str)>,
    ) -> TaskOutcome<T::Output> {
        let outcome = match self.prepare(scope, call, task, input, feedback).await {
            Ok(prepared) => self.run_prepared(scope, call, task, input, &prepared).await,
            Err(failure) => TaskOutcome::Failed {
                failure,
                depth: call.depth,
            },
        };
        let labels = scope
            .labels()
            .with_purpose(task.kind().as_str())
            .with_error_code(outcome.label());
        self.observer
            .observe_labeled(&Signal::TaskCompleted, &labels);
        outcome
    }

    async fn prepare<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
        feedback: Option<(&T::Output, &str)>,
    ) -> Result<Prepared, TaskFailure> {
        let kind = task.kind();
        let profile = self.profile(scope, kind);
        if !profile.enabled {
            return Err(TaskFailure::Disabled);
        }
        let schema =
            self.schemas
                .compile(&task.schema(input))
                .map_err(|error| TaskFailure::Invalid {
                    code: "schema_compile".to_owned(),
                    reason: error.to_string(),
                })?;
        let name = task.prompt_name();
        let instructions = instructions::resolve(
            self.prompts.as_ref(),
            &self.selector,
            name,
            scope.locale(),
            task.instructions(),
        )
        .await;
        let repair = instructions::resolve(
            self.prompts.as_ref(),
            &self.selector,
            &format!("{name}.repair"),
            scope.locale(),
            BUILT_IN_REPAIR,
        )
        .await;
        let mut messages = task.render(input);
        if let Some((previous, note)) = feedback {
            messages.push(Message::assistant(
                serde_json::to_string(previous).unwrap_or_default(),
            ));
            messages.push(Message::user(format!("{}\n\n{note}", repair.text)));
        }
        Ok(Prepared {
            kind,
            parent: call.parent.map(|parent| parent.as_str().to_owned()),
            profile,
            instructions,
            repair,
            schema,
            messages,
        })
    }

    async fn run_prepared<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
        prepared: &Prepared,
    ) -> TaskOutcome<T::Output> {
        let tag = prepared.profile.model.as_deref();
        let votes = prepared.profile.votes.max(1);
        if votes == 1 {
            let id = call.id.as_str().to_owned();
            return match self
                .chain(scope, prepared, task, input, tag, call.depth, id, None)
                .await
            {
                Chain::Answered { output, depth, .. } => TaskOutcome::Accepted { output, depth },
                Chain::Unusable { failure, depth } => {
                    self.escalate(scope, call, task, input, prepared, failure, depth)
                        .await
                }
            };
        }

        let temperature = Some(prepared.profile.vote_temperature);
        let chains = join_all((1..=votes).map(|vote| {
            let id = call.id.call(format!("vote{vote}"));
            self.chain(
                scope,
                prepared,
                task,
                input,
                tag,
                call.depth,
                id,
                temperature,
            )
        }))
        .await;
        let depth = chains
            .iter()
            .map(|chain| match chain {
                Chain::Answered { depth, .. } | Chain::Unusable { depth, .. } => *depth,
            })
            .max()
            .unwrap_or(call.depth);
        let answered: Vec<(&T::Output, &str)> = chains
            .iter()
            .filter_map(|chain| match chain {
                Chain::Answered { output, record, .. } => Some((output, record.as_str())),
                Chain::Unusable { .. } => None,
            })
            .collect();
        if let Some(winner) = majority(task, &answered, usize::from(votes)) {
            for (index, (_, record)) in answered.iter().enumerate() {
                if !winner.contains(&index) {
                    scope.mark(record, &TaskVerdict::Outvoted);
                }
            }
            let output = answered[winner[0]].0.clone();
            return TaskOutcome::Accepted { output, depth };
        }

        self.observer.observe_labeled(
            &Signal::TaskVoteDisagreement,
            &scope.labels().with_purpose(prepared.kind.as_str()),
        );
        match prepared.profile.on_disagreement {
            Disagreement::Reread if !answered.is_empty() => {
                let shown: Vec<String> = answered
                    .iter()
                    .map(|(output, _)| serde_json::to_string(output).unwrap_or_default())
                    .collect();
                let mut again = prepared.clone();
                again.messages.push(Message::user(format!(
                    "Readings of this that disagreed:\n{}\n\nRead it again and give the answer \
                     the message supports.",
                    shown.join("\n")
                )));
                let id = call.id.call("reread");
                match self
                    .chain(scope, &again, task, input, tag, depth, id, None)
                    .await
                {
                    Chain::Answered { output, depth, .. } => {
                        TaskOutcome::Accepted { output, depth }
                    }
                    Chain::Unusable { failure, depth } => TaskOutcome::Failed { failure, depth },
                }
            }
            Disagreement::Escalate => {
                self.escalate(
                    scope,
                    call,
                    task,
                    input,
                    prepared,
                    TaskFailure::Disagreement,
                    depth,
                )
                .await
            }
            Disagreement::Clarify => TaskOutcome::Disagreed {
                answers: answered
                    .into_iter()
                    .map(|(output, _)| output.clone())
                    .collect(),
                depth,
            },
            _ => TaskOutcome::Failed {
                failure: TaskFailure::Disagreement,
                depth,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn escalate<T: ModelTask>(
        &self,
        scope: &TaskScope,
        call: TaskCall<'_>,
        task: &T,
        input: &T::Input,
        prepared: &Prepared,
        failure: TaskFailure,
        depth: u8,
    ) -> TaskOutcome<T::Output> {
        let escalates = matches!(
            failure,
            TaskFailure::Invalid { .. }
                | TaskFailure::Disagreement
                | TaskFailure::Provider(_)
                | TaskFailure::Routing(_)
        );
        let Some(tag) = prepared
            .profile
            .escalate_to
            .as_deref()
            .filter(|_| escalates)
        else {
            return TaskOutcome::Failed { failure, depth };
        };
        self.observer.observe_labeled(
            &Signal::TaskEscalated,
            &scope
                .labels()
                .with_purpose(prepared.kind.as_str())
                .with_error_code(failure.code()),
        );
        let id = call.id.call("escalation");
        match self
            .chain(
                scope,
                prepared,
                task,
                input,
                Some(tag),
                depth.saturating_add(1),
                id,
                None,
            )
            .await
        {
            Chain::Answered { output, depth, .. } => TaskOutcome::Accepted { output, depth },
            Chain::Unusable { failure, depth } => TaskOutcome::Failed { failure, depth },
        }
    }

    /// One call and its repair rounds, starting at `depth`.
    #[allow(clippy::too_many_arguments)]
    async fn chain<T: ModelTask>(
        &self,
        scope: &TaskScope,
        prepared: &Prepared,
        task: &T,
        input: &T::Input,
        tag: Option<&str>,
        depth: u8,
        id: String,
        temperature: Option<f32>,
    ) -> Chain<T::Output> {
        let mut messages = prepared.messages.clone();
        let mut depth = depth;
        let mut failure = TaskFailure::Invalid {
            code: "no_answer".to_owned(),
            reason: String::new(),
        };
        let rounds = prepared.profile.repairs.saturating_add(1);
        for round in 0..rounds {
            let record_id = if round == 0 {
                id.clone()
            } else {
                format!("{id}#repair{round}")
            };
            if let Err(bound) = scope.budget.reserve(depth) {
                self.observer.observe_labeled(
                    &Signal::BudgetExhausted,
                    &scope.labels().with_error_code(bound.as_str()),
                );
                let mut record = self.record(prepared, &record_id, depth, None);
                record.verdict = TaskVerdict::Failed {
                    code: format!("budget_{}", bound.as_str()),
                };
                scope.push(record);
                return Chain::Unusable {
                    failure: TaskFailure::Budget(bound),
                    depth,
                };
            }
            let mut retries = prepared.profile.retries;
            let mut call_id = record_id.clone();
            let (request, answer) = loop {
                let request = self.request(scope, prepared, &messages, temperature, &call_id);
                let answer = self.send(scope, prepared, tag, &request).await;
                match &answer {
                    Err(TaskFailure::Provider(kind)) if retries > 0 && retried_in_place(kind) => {
                        let mut record = self.record(prepared, &call_id, depth, Some(&request));
                        record.verdict = TaskVerdict::Failed {
                            code: format!("provider_{kind}"),
                        };
                        scope.push(record);
                        if let Err(bound) = scope.budget.reserve(depth) {
                            return Chain::Unusable {
                                failure: TaskFailure::Budget(bound),
                                depth,
                            };
                        }
                        retries -= 1;
                        call_id =
                            format!("{record_id}#retry{}", prepared.profile.retries - retries);
                    }
                    _ => break (request, answer),
                }
            };
            let mut record = self.record(prepared, &call_id, depth, Some(&request));
            let response = match answer {
                Ok((response, served_by, latency)) => {
                    record.provider_key = Some(served_by.provider.clone());
                    record.model_key = Some(served_by.model.clone());
                    record.input_tokens = Some(response.usage.input);
                    record.output_tokens = Some(response.usage.output);
                    record.latency_ms = u64::try_from(latency.as_millis()).ok();
                    response
                }
                Err(failed) => {
                    record.verdict = TaskVerdict::Failed {
                        code: failed.code(),
                    };
                    scope.push(record);
                    return Chain::Unusable {
                        failure: failed,
                        depth,
                    };
                }
            };
            let raw = response.text();
            if self.records.keep_raw_output {
                record.raw_output = Some(raw.clone());
            }
            match judge(task, input, &prepared.schema, &response) {
                Ok(output) => {
                    record.parsed = serde_json::to_value(&output).ok();
                    record.verdict = TaskVerdict::Accepted;
                    scope.push(record);
                    return Chain::Answered {
                        output,
                        depth,
                        record: record_id,
                    };
                }
                Err((code, reason)) => {
                    record.verdict = TaskVerdict::Rejected {
                        code: code.clone(),
                        reason: reason.clone(),
                    };
                    scope.push(record);
                    if round + 1 < rounds {
                        self.observer.observe_labeled(
                            &Signal::TaskRepaired,
                            &scope
                                .labels()
                                .with_purpose(prepared.kind.as_str())
                                .with_error_code(code.clone()),
                        );
                        messages.push(Message::assistant(raw));
                        messages.push(Message::user(format!(
                            "{}\n\n{reason}",
                            prepared.repair.text
                        )));
                        depth = depth.saturating_add(1);
                    }
                    failure = TaskFailure::Invalid { code, reason };
                }
            }
        }
        Chain::Unusable { failure, depth }
    }

    fn request(
        &self,
        scope: &TaskScope,
        prepared: &Prepared,
        messages: &[Message],
        temperature: Option<f32>,
        task_id: &str,
    ) -> ModelRequest {
        let profile = &prepared.profile;
        let timeout = scope
            .budget
            .call_timeout(profile.timeout_secs.map(Duration::from_secs));
        let mut request = ModelRequest::new(prepared.kind)
            .with_system(prepared.instructions.text.clone())
            .with_output(OutputSpec::json(
                prepared.kind.as_str(),
                prepared.schema.schema().clone(),
            ))
            .with_timeout(timeout)
            .with_cache_hint(CacheHint::System);
        for message in messages {
            request = request.with_message(message.clone());
        }
        if let Some(temperature) = temperature.or(profile.temperature) {
            request = request.with_temperature(temperature);
        }
        if let Some(tokens) = profile.max_output_tokens {
            request = request.with_max_output_tokens(tokens);
        }
        if let Some(effort) = profile.reasoning_effort {
            request = request.with_reasoning_effort(effort);
        }
        // A task id is a label by construction; one that is not is left off, not sent.
        let _ = request.metadata.insert(TASK_LABEL, task_id);
        if let Some(turn) = &scope.turn {
            let _ = request.metadata.insert(TURN_LABEL, turn.clone());
        }
        request
    }

    async fn send(
        &self,
        scope: &TaskScope,
        prepared: &Prepared,
        tag: Option<&str>,
        request: &ModelRequest,
    ) -> Result<(ModelResponse, ModelRef, Duration), TaskFailure> {
        let routing = match tag {
            Some(tag) => self.routing.clone().with_required_tag(tag),
            None => self.routing.clone(),
        };
        let candidates = self
            .router
            .select(prepared.kind, &request.requirements(), &routing)
            .map_err(|error| {
                crate::signals::observe_routing_error(self.observer.as_ref(), &error);
                TaskFailure::Routing(error.to_string())
            })?;
        let stage = if prepared.kind.is_critical() {
            FallbackStage::PreCommit
        } else {
            FallbackStage::PostCommitNarration
        };
        let _permit = scope.budget.permit().await;
        let started = Instant::now();
        let outcome = execute_with_fallback(&candidates, request, stage, &self.fallback)
            .await
            .map_err(|failure| {
                crate::signals::observe_attempts(self.observer.as_ref(), &failure.attempts);
                // The detail is sanitized and redacted by the adapter: safe to log.
                tracing::warn!(
                    target: "turnframe.tasks",
                    task = request.metadata.get(TASK_LABEL).unwrap_or_default(),
                    kind = failure.error.kind().as_str(),
                    detail = failure.error.detail().map_or("", |detail| detail.as_str()),
                    "a task call failed at the provider"
                );
                TaskFailure::Provider(failure.error.kind().as_str().to_owned())
            })?;
        let latency = started.elapsed();
        crate::signals::observe_attempts(self.observer.as_ref(), &outcome.attempts);
        scope.budget.record_tokens(outcome.response.usage.input);
        let served_by = outcome.served_by();
        self.observer.observe_duration(
            &Signal::TaskLatency,
            latency,
            &scope
                .labels()
                .with_purpose(prepared.kind.as_str())
                .with_provider(served_by.provider.clone())
                .with_model(served_by.model.clone()),
        );
        Ok((outcome.response, served_by, latency))
    }

    fn record(
        &self,
        prepared: &Prepared,
        task_id: &str,
        depth: u8,
        request: Option<&ModelRequest>,
    ) -> TaskRecord {
        let mut record = TaskRecord::new(task_id, prepared.kind.as_str(), TaskVerdict::Accepted);
        record.parent.clone_from(&prepared.parent);
        record.depth = depth;
        record.prompt_ref = Some(prepared.instructions.reference.clone());
        if let Some(request) = request {
            record.params = params_of(request);
            record.input_digest = Some(digest_of(request));
            if self.records.keep_prompts {
                record.rendered = serde_json::to_value(request).ok();
            }
        }
        record
    }
}

/// Parses and checks one answer, naming what failed in words a repair can quote.
fn judge<T: ModelTask>(
    task: &T,
    input: &T::Input,
    schema: &CompiledSchema,
    response: &ModelResponse,
) -> Result<T::Output, (String, String)> {
    let output: T::Output = parse_structured(response, schema)
        .map_err(|error| ("schema".to_owned(), error.to_string()))?;
    task.check(input, &output)
        .map_err(|error| (error.code.to_owned(), error.message))?;
    Ok(output)
}

/// The largest group of agreeing answers, when it is a strict majority of `votes`.
fn majority<T: ModelTask>(
    task: &T,
    answered: &[(&T::Output, &str)],
    votes: usize,
) -> Option<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, (output, _)) in answered.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|group| task.agree(answered[group[0]].0, output))
        {
            Some(group) => group.push(index),
            None => groups.push(vec![index]),
        }
    }
    let largest = groups
        .into_iter()
        .max_by_key(|group| (group.len(), usize::MAX - group[0]))?;
    (largest.len() * 2 > votes).then_some(largest)
}

fn params_of(request: &ModelRequest) -> TaskParams {
    let mut params = TaskParams::default();
    params.temperature = request.temperature;
    params.max_output_tokens = request.max_output_tokens;
    params.reasoning_effort = request
        .reasoning_effort
        .map(|effort| effort.as_str().to_owned());
    params.seed = request.seed;
    params.timeout_ms = u64::try_from(request.timeout.as_millis()).unwrap_or(u64::MAX);
    params
}

/// Digest of what the model was shown: instructions, messages and schema, not the id.
fn digest_of(request: &ModelRequest) -> Digest {
    let shown = serde_json::json!({
        "system": request.system,
        "messages": request.messages,
        "output": request.output,
    });
    Digest::of_bytes(&serde_json::to_vec(&shown).unwrap_or_default())
}

/// Whether a provider failure of this kind may pass on the same call sent again. A
/// request the provider found invalid, or credentials it refused, fail the same way.
fn retried_in_place(kind: &str) -> bool {
    matches!(
        kind,
        "refusal" | "content_filter" | "malformed" | "transport" | "server" | "timeout" | "other"
    )
}
