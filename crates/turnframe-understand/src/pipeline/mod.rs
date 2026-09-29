//! The understanding pipeline: segment; then route, frame and coverage side by side;
//! then one chain per act, all concurrently; then assembly by code.
//!
//! Units are numbered in message order, then the ones coverage adds. Every call runs
//! through the task engine under the turn's [`TaskScope`], so the budget bounds it and
//! the scope records it.

mod chain;
mod cross_check;
mod reroute;
mod respects;
mod units;

use std::collections::BTreeMap;

use futures::future::join_all;
use turnframe_core::ids::{OperationKey, WorkflowKey};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, NotUnderstood, NotUnderstoodReason, QuestionTopic, Superseded,
    Understanding, UnderstoodQuestion, UnitId, UnitKind, Unreadable, WordRange,
};
use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskOutcome, TaskScope};

use crate::assemble;
use crate::check::{ActChecker, NoChecks};
use crate::input::{Expectation, PendingAct, UnderstandingInput, WorkflowBrief};
use crate::progress::{Routing, Step, StepSink, UnitSummary};
use crate::tasks::coverage::{Coverage, Found};
use crate::tasks::question_frame::{QuestionFrame, QuestionInput};
use crate::tasks::route::{NONE, Route, RouteInput, START};
use crate::tasks::segment::{Segment, Segmentation, SegmentedUnit};

pub(crate) use chain::Chained;
use chain::{Creation, Planned};
pub(crate) use units::Seg;

/// Which acts the verifier checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerifyPolicy {
    /// Acts that change a record.
    #[default]
    Mutating,
    /// Every act.
    All,
    /// None.
    Off,
}

/// How the pipeline runs, beside the task profiles and the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Settings {
    /// Which acts are verified.
    pub verify: VerifyPolicy,
    /// How many earlier messages extraction sees.
    pub transcript: usize,
    /// Whether words read as small talk that coverage reads as an act are segmented
    /// again, told what coverage saw, before they are reported unclear.
    pub reread_small_talk: bool,
    /// Rounds of the whole-turn check; `0` switches it off.
    pub cross_check_rounds: u8,
    /// Verdicts cast again on one that finds fault; the majority of all decides. `0`
    /// takes the first.
    pub doubt_votes: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self::conservative()
    }
}

impl Settings {
    /// Mutating acts verified, four earlier messages shown to extraction, disputed small
    /// talk read again.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            verify: VerifyPolicy::Mutating,
            transcript: 4,
            reread_small_talk: true,
            cross_check_rounds: 0,
            doubt_votes: 0,
        }
    }

    /// Sets which acts are verified.
    #[must_use]
    pub const fn with_verify(mut self, verify: VerifyPolicy) -> Self {
        self.verify = verify;
        self
    }

    /// Sets how many earlier messages extraction sees.
    #[must_use]
    pub const fn with_transcript(mut self, messages: usize) -> Self {
        self.transcript = messages;
        self
    }

    /// Sets whether disputed small talk is segmented again.
    #[must_use]
    pub const fn with_reread_small_talk(mut self, reread: bool) -> Self {
        self.reread_small_talk = reread;
        self
    }

    /// Sets the rounds of the whole-turn check; `0` switches it off.
    #[must_use]
    pub const fn with_cross_check_rounds(mut self, rounds: u8) -> Self {
        self.cross_check_rounds = rounds;
        self
    }

    /// Sets how many verdicts are cast again on one that finds fault.
    #[must_use]
    pub const fn with_doubt_votes(mut self, votes: u8) -> Self {
        self.doubt_votes = votes;
        self
    }
}

/// What reads a turn: the task pipeline, or a double that returns what a test wrote.
#[async_trait::async_trait]
pub trait TurnUnderstander: Send + Sync {
    /// Understands one turn, recording its calls in `scope` and its steps in `steps`,
    /// with the domain's own check of each act.
    async fn understand(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        steps: &dyn StepSink,
        checker: &dyn ActChecker,
    ) -> Understanding;
}

#[async_trait::async_trait]
impl TurnUnderstander for Understander {
    async fn understand(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        steps: &dyn StepSink,
        checker: &dyn ActChecker,
    ) -> Understanding {
        self.understand_checked(scope, turn, steps, checker).await
    }
}

/// Understands turns. Cheap to clone; shared by every turn.
#[derive(Clone)]
pub struct Understander {
    engine: TaskEngine,
    settings: Settings,
}

impl std::fmt::Debug for Understander {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Understander")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// What a routed unit asks for.
#[derive(Debug, Clone)]
enum Routed {
    Act {
        action: ActAction,
        workflow: WorkflowKey,
        task: TaskId,
        depth: u8,
    },
    Nothing(NotUnderstoodReason),
}

/// The units of a message, where each routes, and its framed questions.
type Read = (
    Vec<Seg>,
    BTreeMap<UnitId, Vec<Routed>>,
    BTreeMap<UnitId, UnderstoodQuestion>,
);

/// Why a reading of the message stopped.
enum Reading {
    /// The message cannot be read.
    Unreadable(Unreadable),
    /// Coverage sent the segmentation back once, and why.
    Again(Segmentation, units::Retry),
}

/// Acts planned from routes, and what corrections and cancels removed.
struct Planning<'a> {
    planned: Vec<Planned<'a>>,
    superseded: Vec<Superseded>,
    not_understood: Vec<NotUnderstood>,
}

pub(crate) struct Context<'a> {
    pub engine: &'a TaskEngine,
    pub scope: &'a TaskScope,
    pub turn: &'a UnderstandingInput,
    pub settings: Settings,
    pub checker: &'a dyn ActChecker,
    pub steps: &'a dyn StepSink,
    pub creations: Vec<Creation>,
    /// The words of every unit of the message.
    pub units: Vec<crate::words::Span>,
}

fn summary(turn: &UnderstandingInput, unit: &Seg) -> UnitSummary {
    UnitSummary {
        id: unit.id,
        kind: unit.kind(),
        text: turn.message.slice(unit.span).unwrap_or_default().to_owned(),
    }
}

const fn label_of(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Correction => "Correction",
        UnitKind::Cancel => "Cancel",
        UnitKind::ProvidesValue => "Answer",
        UnitKind::Question => "Question",
        _ => "Request",
    }
}

pub(super) fn routable(unit: &Seg) -> bool {
    match unit.kind() {
        UnitKind::Request | UnitKind::ProvidesValue => true,
        UnitKind::Correction | UnitKind::Cancel => unit.refers_to.is_none(),
        _ => false,
    }
}

impl Understander {
    /// An understander running its tasks on `engine`, with default settings.
    #[must_use]
    pub fn new(engine: TaskEngine) -> Self {
        Self {
            engine,
            settings: Settings::default(),
        }
    }

    /// Replaces the settings.
    #[must_use]
    pub const fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// The settings in force.
    #[must_use]
    pub const fn settings(&self) -> Settings {
        self.settings
    }

    /// Understands one turn with no domain check. Every call is recorded in `scope` and
    /// bounded by its budget; every decision is published to `steps` as it is made.
    pub async fn understand(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        steps: &dyn StepSink,
    ) -> Understanding {
        self.understand_checked(scope, turn, steps, &NoChecks).await
    }

    /// Understands one turn, checking each act with the domain before it is kept.
    pub async fn understand_checked(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        steps: &dyn StepSink,
        checker: &dyn ActChecker,
    ) -> Understanding {
        if turn.message.is_empty() {
            return Understanding::default();
        }
        steps.step(Step::Reading {
            words: turn.message.len(),
        });
        let settings = turn.settings.unwrap_or(self.settings);
        // A constraint coverage finds in words no unit holds, or a dispute over small talk
        // when the settings ask, sends the segmentation back once, told what coverage saw.
        // A constraint still lost after that fails the turn closed.
        let mut lost: Option<(Segmentation, units::Retry)> = None;
        let (mut units, routes, mut questions) = loop {
            let reread = settings.reread_small_talk && lost.is_none();
            match self.read(scope, turn, steps, lost.as_ref(), reread).await {
                Ok(read) => break read,
                Err(Reading::Unreadable(unreadable)) => {
                    return Understanding::unreadable(unreadable);
                }
                Err(Reading::Again(segmentation, retry)) if lost.is_none() => {
                    lost = Some((segmentation, retry));
                }
                Err(Reading::Again(..)) => {
                    return Understanding::unreadable(Unreadable::LostConstraint);
                }
            }
        };
        let mut planning = plan(turn, &units, &routes);
        for item in &planning.not_understood {
            steps.step(Step::NotUnderstood {
                unit: item.unit,
                reason: item.reason.clone(),
            });
        }
        let mut cx = Context {
            engine: &self.engine,
            scope,
            turn,
            settings,
            checker,
            steps,
            creations: creations(&planning.planned),
            units: units.iter().map(|unit| unit.span).collect(),
        };
        let mut chained = join_all(
            planning
                .planned
                .iter()
                .map(|planned| chain::run(&cx, planned)),
        )
        .await;
        self.routed_again(&mut cx, &units, &mut planning, &mut chained)
            .await;
        complete_the_waiting_acts(turn, &mut chained);
        self.kept_to_own_words(&cx, &planning.planned, &mut chained)
            .await;
        self.tails_read_again(&cx, &units, &planning.planned, &mut chained)
            .await;
        if cx.settings.cross_check_rounds > 0 {
            let reading = cross_check::Reading {
                units: &mut units,
                planned: &mut planning.planned,
                chained: &mut chained,
                not_understood: &mut planning.not_understood,
                questions: &mut questions,
            };
            self.cross_checked(&cx, reading).await;
            // An act the check read again lost what the waiting acts gave it: given again.
            complete_the_waiting_acts(turn, &mut chained);
        }
        joins_its_request(&units, &mut chained);
        pieces_of_one_value(&units, &mut chained);
        once_each(&mut chained);
        given_not_created(turn, &mut chained);
        not_asked_quietly(&units, &mut chained);
        named_as_created(turn, &mut chained);
        the_one_created(&cx.creations, &mut chained);
        copies_keep_to_their_words(turn, &mut chained);
        self.respected(&cx, &units, &mut chained).await;
        for outcome in &chained {
            if let Chained::NotUnderstood { unit, reason, .. } = outcome {
                steps.step(Step::NotUnderstood {
                    unit: *unit,
                    reason: reason.clone(),
                });
            }
        }
        let understanding = assemble::assemble(
            &units,
            planning.superseded,
            planning.not_understood,
            chained,
            questions,
        );
        let by_status = |wanted: fn(&ActStatus) -> bool| -> Vec<ActId> {
            understanding
                .acts
                .iter()
                .filter(|act| wanted(&act.status))
                .map(|act| act.id)
                .collect()
        };
        steps.step(Step::Assembled {
            ready: by_status(|status| matches!(status, ActStatus::Ready)),
            asking: by_status(|status| matches!(status, ActStatus::NeedsValue { .. })),
            held: by_status(|status| matches!(status, ActStatus::Held { .. })),
            questions: understanding.questions.len(),
        });
        understanding
    }

    /// Segments the message, looks for what the segmentation missed, and routes every
    /// unit. `retry` is the segmentation to repair and the words it left a constraint in.
    async fn read(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        steps: &dyn StepSink,
        retry: Option<&(Segmentation, units::Retry)>,
        reread_small_talk: bool,
    ) -> Result<Read, Reading> {
        let (segment, coverage_id) = match retry {
            None => (TaskId::new("turn/segment"), TaskId::new("turn/coverage")),
            Some(_) => (
                TaskId::new("turn/segment.after_coverage"),
                TaskId::new("turn/coverage.after_segment"),
            ),
        };
        let call = TaskCall {
            id: &segment,
            parent: None,
            depth: 1,
        };
        let task = Segment::new(turn);
        let outcome = match retry {
            None => self.engine.run(scope, call, &task, &()).await,
            Some((previous, retry)) => {
                let feedback = match retry {
                    units::Retry::LostConstraint(spans) => {
                        let named: Vec<String> = spans
                            .iter()
                            .map(|span| {
                                let (from, to) = span.shown();
                                let said = turn.message.slice(*span).unwrap_or_default();
                                format!("{from} to {to}, «{said}»")
                            })
                            .collect();
                        format!(
                            "Words {}, are in no unit, and a check read them as a constraint. \
                             Every word the message needs belongs to a unit: to the request it \
                             completes, or to a constraint of its own.",
                            named.join("; words ")
                        )
                    }
                    units::Retry::Disputed {
                        words,
                        read_as,
                        dispute,
                    } => {
                        let (from, to) = words.shown();
                        let said = turn.message.slice(*words).unwrap_or_default();
                        let checked = crate::tasks::coverage::kind_name((*read_as).into());
                        // Told the wrong first reading, a model takes the check's word for them.
                        if *dispute {
                            format!(
                                "Words {from} to {to}, «{said}», were read as a dispute, and a \
                                 check read them as a {checked}. Read the message again and say \
                                 what these words are: words that only say a reported change is \
                                 wrong, giving nothing to put in its place, are a dispute."
                            )
                        } else {
                            format!(
                                "Words {from} to {to}, «{said}», were read as small talk, and a \
                                 check read them as a {checked}. Read the message again and say \
                                 what these words are."
                            )
                        }
                    }
                };
                self.engine
                    .run_with_feedback(scope, call, &task, &(), previous, &feedback)
                    .await
            }
        };
        let (segmentation, depth) = match outcome {
            TaskOutcome::Accepted { output, depth } => (output, depth),
            TaskOutcome::Disagreed { .. } => {
                return Err(Reading::Unreadable(Unreadable::Segmentation {
                    code: "vote_disagreement".to_owned(),
                }));
            }
            TaskOutcome::Failed { failure, .. } => {
                return Err(Reading::Unreadable(Unreadable::Segmentation {
                    code: failure.code(),
                }));
            }
        };
        let mut units = units::numbered(turn, &segmentation, &segment, depth);
        if let Some((
            _,
            units::Retry::Disputed {
                words,
                dispute: false,
                ..
            },
        )) = retry
        {
            units::small_talk_stays(&mut units, *words);
        }
        // After a repair round the analysis speaks of the repair, not of the message: a
        // repaired vote is `…#vote1#repair1`.
        let repaired = scope.records().iter().any(|record| {
            record
                .task_id
                .strip_prefix(segment.as_str())
                .is_some_and(|rest| rest.starts_with('#') && rest.contains("#repair"))
        });
        steps.step(Step::Segmented {
            // A reading told what a check saw speaks of the check, as a repaired one does.
            analysis: if repaired || retry.is_some() {
                String::new()
            } else {
                segmentation.analysis.clone()
            },
            units: units.iter().map(|unit| summary(turn, unit)).collect(),
        });
        let found = Found {
            units: units.iter().map(|unit| (unit.kind(), unit.span)).collect(),
        };
        let coverage_call = TaskCall {
            id: &coverage_id,
            parent: Some(&segment),
            depth: depth.saturating_add(1),
        };
        let coverage_task = Coverage::new(turn);
        let (coverage, (mut routes, mut questions, asked)) = futures::join!(
            self.engine
                .run(scope, coverage_call, &coverage_task, &found),
            self.routes_and_frames(scope, turn, &units, steps),
        );
        units::asked_for(&mut units, &asked);
        if let TaskOutcome::Accepted { output, depth } = coverage {
            let unrouted = |id: UnitId| {
                matches!(
                    routes.get(&id).map(Vec::as_slice),
                    Some([Routed::Nothing(NotUnderstoodReason::NoOperation)])
                )
            };
            let reread = units::reread_as_questions(&mut units, &output.missed, unrouted);
            if !reread.is_empty() {
                let asked: Vec<Seg> = units
                    .iter()
                    .filter(|unit| reread.contains(&unit.id))
                    .cloned()
                    .collect();
                for id in &reread {
                    routes.remove(id);
                }
                steps.step(Step::Covered {
                    added: asked.iter().map(|unit| summary(turn, unit)).collect(),
                });
                let (_, framed, _) = self.routes_and_frames(scope, turn, &asked, steps).await;
                questions.extend(framed);
            }
            match units::add_missed(
                turn,
                &mut units,
                &output.missed,
                &coverage_id,
                depth,
                reread_small_talk,
                retry.is_some(),
            ) {
                Err(retry) => return Err(Reading::Again(segmentation, retry)),
                Ok(covered) => {
                    for id in covered.unclear {
                        routes.insert(id, vec![Routed::Nothing(NotUnderstoodReason::Unclear)]);
                    }
                    for id in &covered.extended {
                        let whole = units.iter().find(|unit| unit.id == *id).map(|u| u.range);
                        if let (Some(question), Some(whole)) = (questions.get_mut(id), whole) {
                            question.words = whole;
                        }
                    }
                    let late: Vec<Seg> = units
                        .iter()
                        .filter(|unit| covered.added.contains(&unit.id))
                        .cloned()
                        .collect();
                    if late.is_empty() {
                        return Ok((units, routes, questions));
                    }
                    steps.step(Step::Covered {
                        added: late.iter().map(|unit| summary(turn, unit)).collect(),
                    });
                    let (more_routes, more_questions, asked) =
                        self.routes_and_frames(scope, turn, &late, steps).await;
                    units::asked_for(&mut units, &asked);
                    routes.extend(more_routes);
                    questions.extend(more_questions);
                }
            }
        }
        Ok((units, routes, questions))
    }

    async fn routes_and_frames(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        units: &[Seg],
        steps: &dyn StepSink,
    ) -> (
        BTreeMap<UnitId, Vec<Routed>>,
        BTreeMap<UnitId, UnderstoodQuestion>,
        Vec<UnitId>,
    ) {
        let others_of = |unit: &Seg| -> Vec<crate::words::Span> {
            units
                .iter()
                .filter(|other| other.id != unit.id && routable(other))
                .map(|other| other.span)
                .collect()
        };
        let routes = join_all(
            units
                .iter()
                .filter(|unit| routable(unit))
                .map(|unit| async move {
                    let routed = self.route(scope, turn, unit, others_of(unit), None).await;
                    report_routes(steps, unit.id, &routed);
                    (unit.id, routed)
                }),
        );
        let frames = join_all(
            units
                .iter()
                .filter(|unit| unit.kind() == UnitKind::Question)
                .map(|unit| async move { (unit.id, self.frame(scope, turn, unit).await) }),
        );
        let (routes, frames) = futures::join!(routes, frames);
        let mut routes: BTreeMap<UnitId, Vec<Routed>> = routes.into_iter().collect();
        let mut questions = BTreeMap::new();
        let mut asking = Vec::new();
        for (id, (question, asks_for_it)) in frames {
            if asks_for_it && let Some(unit) = units.iter().find(|unit| unit.id == id) {
                asking.push(unit.as_request());
            }
            questions.insert(id, question);
        }
        // A question whether one thing can be done asks for it, when an operation does it.
        let rerouted = join_all(asking.iter().map(|unit| async move {
            (
                unit.id,
                self.route(scope, turn, unit, others_of(unit), None).await,
            )
        }))
        .await;
        let mut asked = Vec::new();
        for (id, routed) in rerouted {
            if routed
                .iter()
                .any(|route| matches!(route, Routed::Act { .. }))
            {
                report_routes(steps, id, &routed);
                routes.insert(id, routed);
                questions.remove(&id);
                asked.push(id);
            }
        }
        (routes, questions, asked)
    }

    /// Routes `unit`, shown `others`, the other parts routed on their own; routed again,
    /// the task is told `again` of the first reading.
    async fn route(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        unit: &Seg,
        others: Vec<crate::words::Span>,
        again: Option<String>,
    ) -> Vec<Routed> {
        // Every workflow's operations are on offer: the one segmentation guessed is a
        // guess, and a value given is routed like a request, the question it answers in
        // view, so words asking for something else are that request.
        let workflows: Vec<&WorkflowBrief> = turn.workflows.iter().collect();
        let input = RouteInput {
            label: label_of(unit.kind()),
            words: unit.span,
            workflows,
            note: again,
            others,
        };
        // One operation on offer is still a question: the request may ask for none.
        if let [_none] = input.choices().as_slice() {
            return vec![Routed::Nothing(NotUnderstoodReason::NoOperation)];
        }
        let name = if input.note.is_some() {
            "route.again"
        } else {
            "route"
        };
        let id = unit.task().child(name);
        let call = TaskCall {
            id: &id,
            parent: Some(&unit.parent),
            depth: unit.depth.saturating_add(1),
        };
        match self
            .engine
            .run(scope, call, &Route::new(turn), &input)
            .await
        {
            TaskOutcome::Accepted { output, depth } => {
                // A new record comes first, so what the request asks of it can follow.
                let mut chosen = output.operations;
                chosen.sort_by_key(|choice| !choice.starts_with(START));
                chosen
                    .iter()
                    .map(|choice| routed(turn, choice, id.clone(), depth))
                    .collect()
            }
            TaskOutcome::Disagreed { .. } => vec![Routed::Nothing(NotUnderstoodReason::Unclear)],
            TaskOutcome::Failed { failure, .. } => {
                vec![Routed::Nothing(NotUnderstoodReason::TaskFailed {
                    task: "route".to_owned(),
                    code: failure.code(),
                })]
            }
        }
    }

    async fn frame(
        &self,
        scope: &TaskScope,
        turn: &UnderstandingInput,
        unit: &Seg,
    ) -> (UnderstoodQuestion, bool) {
        let (basis, continues_previous) = match &unit.unit {
            SegmentedUnit::Question {
                basis,
                continues_previous,
                ..
            } => (*basis, *continues_previous),
            _ => (
                turnframe_core::plan::AnswerBasis::CurrentCommittedState,
                false,
            ),
        };
        let workflows: Vec<&WorkflowBrief> = match &unit.workflow {
            Some(key) => turn.workflow(key).into_iter().collect(),
            None => turn.workflows.iter().collect(),
        };
        let input = QuestionInput {
            words: unit.span,
            records: workflows.iter().flat_map(|w| w.records.iter()).collect(),
            subjects: workflows
                .iter()
                .flat_map(|w| w.subjects.iter().map(String::as_str))
                .collect(),
        };
        let mut question = UnderstoodQuestion {
            unit: unit.id,
            words: unit.range,
            workflow: unit.workflow.clone(),
            record: None,
            subjects: Vec::new(),
            basis,
            topic: QuestionTopic::default(),
            continues_previous,
        };
        let id = unit.task().child("frame");
        let call = TaskCall {
            id: &id,
            parent: Some(&unit.parent),
            depth: unit.depth.saturating_add(1),
        };
        let mut asks_for_it = false;
        if let TaskOutcome::Accepted { output, .. } = self
            .engine
            .run(scope, call, &QuestionFrame::new(turn), &input)
            .await
        {
            asks_for_it = output.asks_for_it();
            question.record = input.record(&output.record).map(|r| r.token.clone());
            question.topic = output.topic();
            // Subjects name what a record holds or which values a field takes.
            if matches!(
                question.topic,
                QuestionTopic::RecordState | QuestionTopic::AcceptedValues
            ) {
                question.subjects = output.subjects;
            }
        }
        (question, asks_for_it)
    }
}

/// Tells `steps` where a unit routed; a failed call routed nowhere, and its own step says why.
fn report_routes(steps: &dyn StepSink, unit: UnitId, routed: &[Routed]) {
    for route in routed {
        let to = match route {
            Routed::Act {
                action: ActAction::Apply { operation },
                ..
            } => Routing::Operation {
                operation: operation.clone(),
            },
            Routed::Act {
                action: ActAction::Start { workflow },
                ..
            } => Routing::Start {
                workflow: workflow.clone(),
            },
            Routed::Nothing(NotUnderstoodReason::NoOperation) => Routing::Nothing,
            Routed::Nothing(_) => continue,
        };
        steps.step(Step::Routed { unit, to });
    }
}

fn routed(turn: &UnderstandingInput, choice: &str, task: TaskId, depth: u8) -> Routed {
    if choice == NONE {
        return Routed::Nothing(NotUnderstoodReason::NoOperation);
    }
    if let Some(workflow) = choice.strip_prefix(START) {
        let workflow = WorkflowKey::from(workflow);
        return Routed::Act {
            action: ActAction::Start {
                workflow: workflow.clone(),
            },
            workflow,
            task,
            depth,
        };
    }
    let operation = OperationKey::from(choice);
    match turn.operation(&operation) {
        Some((workflow, _)) => Routed::Act {
            workflow: workflow.key.clone(),
            action: ActAction::Apply { operation },
            task,
            depth,
        },
        None => Routed::Nothing(NotUnderstoodReason::NoOperation),
    }
}

fn plan<'a>(
    turn: &'a UnderstandingInput,
    units: &[Seg],
    routes: &BTreeMap<UnitId, Vec<Routed>>,
) -> Planning<'a> {
    let mut planning = Planning {
        planned: Vec::new(),
        superseded: Vec::new(),
        not_understood: Vec::new(),
    };
    let mut ordered: Vec<&Seg> = units.iter().collect();
    ordered.sort_by_key(|unit| unit.span);
    for unit in ordered {
        let not_understood = |reason| NotUnderstood {
            unit: unit.id,
            words: unit.range,
            reason,
        };
        if let Some(earlier) = unit.refers_to {
            // Of a unit asking for several things, a correction or a cancel changes the
            // last: the one its words follow.
            let position = planning.planned.iter().rposition(|p| p.unit == earlier);
            let Some(previous) = position.map(|p| planning.planned.remove(p)) else {
                if unit.kind() == UnitKind::Correction {
                    planning
                        .not_understood
                        .push(not_understood(NotUnderstoodReason::Unclear));
                }
                continue;
            };
            planning.superseded.push(Superseded {
                act: previous.id,
                action: previous.action.clone(),
                by: unit.id,
            });
            if unit.kind() == UnitKind::Correction {
                planning.planned.push(Planned {
                    id: ActId::new(unit.id, 1),
                    unit: unit.id,
                    label: label_of(UnitKind::Correction),
                    words: unit.span,
                    range: unit.range,
                    continues: Some(previous.continues.unwrap_or(previous.words)),
                    ..previous
                });
            }
            continue;
        }
        let Some(routed) = routes.get(&unit.id) else {
            continue;
        };
        // Acts of the same operation in one unit are read apart, each as the k-th of n.
        let occurrence = |index: usize| -> Option<(usize, usize)> {
            let Routed::Act { action, .. } = &routed[index] else {
                return None;
            };
            let same: Vec<usize> = routed
                .iter()
                .enumerate()
                .filter(|(_, other)| matches!(other, Routed::Act { action: a, .. } if a == action))
                .map(|(at, _)| at)
                .collect();
            let position = same.iter().position(|at| *at == index)?;
            (same.len() > 1).then_some((position + 1, same.len()))
        };
        for (number, route) in (1..).zip(routed) {
            let (action, workflow, task, depth) = match route {
                Routed::Act {
                    action,
                    workflow,
                    task,
                    depth,
                } => (action, workflow, task, *depth),
                Routed::Nothing(reason) => {
                    planning.not_understood.push(not_understood(reason.clone()));
                    continue;
                }
            };
            let Some(brief) = turn.workflow(workflow) else {
                planning
                    .not_understood
                    .push(not_understood(NotUnderstoodReason::NoOperation));
                continue;
            };
            let spec = match action {
                ActAction::Apply { operation } => brief.spec(operation),
                ActAction::Start { .. } => None,
            };
            let pending = match (&turn.expectation, unit.kind(), action) {
                (
                    Some(Expectation::Values(pending)),
                    UnitKind::ProvidesValue,
                    ActAction::Apply { operation },
                ) if *operation == pending.operation => Some(pending),
                _ => None,
            };
            let corrects = match (unit.kind(), action) {
                (UnitKind::Correction, ActAction::Apply { operation }) => turn
                    .done
                    .iter()
                    .filter(|done| done.operation == *operation)
                    .collect(),
                _ => Vec::new(),
            };
            planning.planned.push(Planned {
                id: ActId::new(unit.id, number),
                unit: unit.id,
                label: label_of(unit.kind()),
                words: unit.span,
                range: unit.range,
                action: action.clone(),
                workflow: brief,
                spec,
                continues: None,
                occurrence: occurrence(usize::from(number) - 1),
                pending,
                corrects,
                kin: units
                    .iter()
                    .filter(|other| {
                        other.id != unit.id
                            && routes.get(&other.id).is_some_and(|routed| {
                                routed.iter().any(|route| {
                                    matches!(route, Routed::Act { action: theirs, .. } if theirs == action)
                                })
                            })
                    })
                    .map(|other| other.span)
                    .collect(),
                guessed: unit.found_by == turnframe_core::understanding::FoundBy::Coverage,
                also: routed
                    .iter()
                    .filter_map(|other| match other {
                        Routed::Act {
                            action: ActAction::Apply { operation },
                            ..
                        } if !matches!(action, ActAction::Apply { operation: own } if own == operation) => {
                            Some(operation.clone())
                        }
                        _ => None,
                    })
                    .collect(),
                parent: task.clone(),
                depth,
            });
        }
    }
    planning
}

/// A value given beside a request and read as a unit of its own belongs to that request's
/// act: the same operation on the same record, or on a new record of the same workflow,
/// takes the values it lacks, and the second act is dropped. A value the request read from
/// the value's words is the value's own reading of them.
fn joins_its_request(units: &[Seg], chained: &mut Vec<Chained>) {
    use turnframe_core::understanding::{
        ArgumentValue, MessageRef, RecordValue, UnderstoodAct, UnderstoodArgument,
    };
    let kind = |unit: UnitId| units.iter().find(|u| u.id == unit).map(|u| u.unit.kind());
    let read_in = |unit: UnitId, given: &UnderstoodArgument| {
        let range = units.iter().find(|u| u.id == unit).map(|u| u.range);
        given
            .excerpt
            .as_ref()
            .zip(range)
            .is_some_and(|(excerpt, range)| {
                excerpt.message == MessageRef::Current
                    && excerpt.words.first >= range.first
                    && excerpt.words.last <= range.last
            })
    };
    let acts: Vec<UnderstoodAct> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act.clone()),
            Chained::NotUnderstood { .. } | Chained::Nothing => None,
        })
        .collect();
    let mut joined: Vec<(ActId, ActId)> = Vec::new();
    for value in &acts {
        if kind(value.id.unit) != Some(UnitKind::ProvidesValue) {
            continue;
        }
        let fits = |request: &UnderstoodAct| {
            request.id.unit != value.id.unit
                && kind(request.id.unit) == Some(UnitKind::Request)
                && request.action == value.action
                && request.target == value.target
                && value.arguments.iter().all(|(name, argument)| {
                    request.arguments.get(name).is_none_or(|given| {
                        given.value == argument.value || read_in(value.id.unit, given)
                    })
                })
                && value.arguments.keys().any(|name| {
                    request
                        .arguments
                        .get(name)
                        .is_none_or(|given| read_in(value.id.unit, given))
                })
        };
        let Some(request) = acts.iter().find(|request| fits(request)) else {
            continue;
        };
        joined.push((value.id, request.id));
        for outcome in chained.iter_mut() {
            let Chained::Act(act) = outcome else {
                continue;
            };
            if act.id != request.id {
                continue;
            }
            for (name, argument) in &value.arguments {
                if act
                    .arguments
                    .get(name)
                    .is_none_or(|given| read_in(value.id.unit, given))
                {
                    act.arguments.insert(name.clone(), argument.clone());
                }
            }
            for dependency in &value.depends_on {
                if !act.depends_on.contains(dependency) {
                    act.depends_on.push(*dependency);
                }
            }
            if let ActStatus::NeedsValue { arguments, .. } = &act.status {
                let missing: Vec<String> = arguments
                    .iter()
                    .filter(|name| !value.arguments.contains_key(*name))
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    act.status = value.status.clone();
                }
            }
        }
    }
    if joined.is_empty() {
        return;
    }
    let into = |id: ActId| {
        joined
            .iter()
            .find(|(from, _)| *from == id)
            .map_or(id, |(_, to)| *to)
    };
    chained.retain(|outcome| !matches!(outcome, Chained::Act(act) if joined.iter().any(|(from, _)| *from == act.id)));
    for outcome in chained.iter_mut() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        for dependency in &mut act.depends_on {
            *dependency = into(*dependency);
        }
        for argument in act.arguments.values_mut() {
            if let ArgumentValue::Record(RecordValue::SameTurn { act: created }) =
                &mut argument.value
            {
                *created = into(*created);
            }
        }
    }
}

/// An act whose whole part lies inside the value another act of its operation read across
/// parts, on the same record, read a piece of that value: the act is the other's.
fn pieces_of_one_value(units: &[Seg], chained: &mut [Chained]) {
    use turnframe_core::understanding::{ActTarget, MessageRef};
    let across: Vec<(ActId, ActAction, ActTarget, WordRange)> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act),
            _ => None,
        })
        .flat_map(|act| {
            let part = units
                .iter()
                .find(|unit| unit.id == act.id.unit)
                .map(|unit| unit.range);
            act.arguments.values().filter_map(move |given| {
                let excerpt = given.excerpt.filter(|e| e.message == MessageRef::Current)?;
                let part = part?;
                (excerpt.words.first < part.first || excerpt.words.last > part.last).then(|| {
                    (
                        act.id,
                        act.action.clone(),
                        act.target.clone(),
                        excerpt.words,
                    )
                })
            })
        })
        .collect();
    for outcome in chained.iter_mut() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        let Some(part) = units
            .iter()
            .find(|unit| unit.id == act.id.unit)
            .map(|unit| unit.range)
        else {
            continue;
        };
        let inside = across.iter().any(|(id, action, target, words)| {
            id.unit != act.id.unit
                && *action == act.action
                && *target == act.target
                && words.first <= part.first
                && part.last <= words.last
        });
        if inside && act.depends_on.is_empty() {
            *outcome = Chained::Nothing;
        }
    }
}

/// The same act asked twice in one message, the same operation on the same record with the
/// same values, is one act: the later copy is dropped, unless another act waits on it. So is
/// one whose values another act of it all gives, with more beside them.
fn once_each(chained: &mut Vec<Chained>) {
    use turnframe_core::understanding::UnderstoodAct;
    let same = |a: &UnderstoodAct, b: &UnderstoodAct| {
        a.action == b.action
            && a.target == b.target
            && a.status == b.status
            && a.arguments.len() == b.arguments.len()
            && a.arguments.iter().all(|(name, argument)| {
                b.arguments
                    .get(name)
                    .is_some_and(|other| other.value == argument.value)
            })
    };
    let acts: Vec<UnderstoodAct> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act.clone()),
            Chained::NotUnderstood { .. } | Chained::Nothing => None,
        })
        .collect();
    let waited_on = |id: ActId| acts.iter().any(|act| act.depends_on.contains(&id));
    let mut kept: Vec<UnderstoodAct> = Vec::new();
    chained.retain(|outcome| {
        let Chained::Act(act) = outcome else {
            return true;
        };
        if kept.iter().any(|earlier| same(earlier, act)) && !waited_on(act.id) {
            return false;
        }
        // Waiting for values another act of this turn already gives: that act does it,
        // when it gives each value this one has; with other values it is another act.
        let done_by_another = matches!(act.status, ActStatus::NeedsValue { .. })
            && acts.iter().any(|other| {
                other.id != act.id
                    && other.status == ActStatus::Ready
                    && other.action == act.action
                    && other.target == act.target
                    && act.arguments.iter().all(|(name, argument)| {
                        other
                            .arguments
                            .get(name)
                            .is_some_and(|theirs| theirs.value == argument.value)
                    })
            });
        if done_by_another && !waited_on(act.id) {
            return false;
        }
        // Ready with nothing another ready act of it does not give as well: read twice.
        let covered = act.status == ActStatus::Ready
            && acts.iter().any(|other| {
                other.id != act.id
                    && other.status == ActStatus::Ready
                    && other.action == act.action
                    && other.target == act.target
                    && other.arguments.len() > act.arguments.len()
                    && act.arguments.iter().all(|(name, argument)| {
                        other
                            .arguments
                            .get(name)
                            .is_some_and(|theirs| theirs.value == argument.value)
                    })
            });
        if covered && !waited_on(act.id) {
            return false;
        }
        kept.push(act.clone());
        true
    });
}

/// A part read both as creating a record and as giving a listed record of that workflow a
/// value, every value of the creation in the words the other act reads its value from, gives
/// the listed record the value: the creation is those words read twice. So is a creation
/// whose every value lies in the words another act chooses a listed record of that workflow
/// by. One another act waits on stands.
fn given_not_created(turn: &UnderstandingInput, chained: &mut Vec<Chained>) {
    use turnframe_core::understanding::{
        ActTarget, ArgumentValue, MessageRef, RecordValue, UnderstoodAct,
    };
    let acts: Vec<UnderstoodAct> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act.clone()),
            _ => None,
        })
        .collect();
    let words_of = |act: &UnderstoodAct| -> Vec<WordRange> {
        act.arguments
            .values()
            .filter_map(|argument| argument.excerpt)
            .filter(|excerpt| excerpt.message == MessageRef::Current)
            .map(|excerpt| excerpt.words)
            .collect()
    };
    let twice: Vec<ActId> = acts
        .iter()
        .filter(|creation| {
            let workflow = match (&creation.action, &creation.target) {
                (ActAction::Start { workflow }, _) | (_, ActTarget::New { workflow }) => workflow,
                _ => return false,
            };
            let created = words_of(creation);
            let waited_on = acts.iter().any(|act| act.depends_on.contains(&creation.id));
            !created.is_empty()
                && !waited_on
                && acts.iter().any(|other| {
                    let of_workflow = |token| {
                        turn.record(token)
                            .is_some_and(|(brief, _)| &brief.key == workflow)
                    };
                    let listed =
                        matches!(&other.target, ActTarget::Record { token } if of_workflow(token));
                    let given = words_of(other);
                    let chooses_it = other.arguments.values().any(|argument| {
                        matches!(&argument.value,
                            ArgumentValue::Record(RecordValue::Record { token }) if of_workflow(token))
                            && argument.excerpt.is_some_and(|excerpt| {
                                excerpt.message == MessageRef::Current
                                    && created.iter().all(|words| {
                                        excerpt.words.first <= words.first
                                            && words.last <= excerpt.words.last
                                    })
                            })
                    });
                    (other.id.unit == creation.id.unit
                        && listed
                        && created.iter().all(|words| given.contains(words)))
                        || (other.id != creation.id && chooses_it)
                })
        })
        .map(|creation| creation.id)
        .collect();
    chained.retain(|outcome| !matches!(outcome, Chained::Act(act) if twice.contains(&act.id)));
}

/// An act the verifier found was not asked for says nothing to the user when its unit asked
/// for something else that was understood, or when only coverage found the unit: a route
/// that listed one operation too many, or a guess, and nothing the user is owed an answer
/// about.
fn not_asked_quietly(units: &[Seg], chained: &mut Vec<Chained>) {
    use turnframe_core::understanding::FoundBy;
    let understood: Vec<UnitId> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act.id.unit),
            _ => None,
        })
        .collect();
    let guessed = |unit: UnitId| {
        units
            .iter()
            .any(|seg| seg.id == unit && seg.found_by == FoundBy::Coverage)
    };
    chained.retain(|outcome| match outcome {
        Chained::NotUnderstood {
            unit,
            reason: NotUnderstoodReason::NotRequested,
            ..
        } => !understood.contains(unit) && !guessed(*unit),
        Chained::Nothing => false,
        _ => true,
    });
}

/// A record named by the name one record this message creates is given, or by words of this
/// message around the words that name it, is that record: the name is not looked up among the
/// records that exist, and the act waits for the creation.
fn named_as_created(turn: &UnderstandingInput, chained: &mut [Chained]) {
    use turnframe_core::understanding::{ActTarget, ArgumentValue, MessageRef, RecordValue};
    type Created = (ActId, WorkflowKey, String, Option<WordRange>);
    let created: Vec<Created> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => {
                let workflow = match (&act.action, &act.target) {
                    (ActAction::Start { workflow }, _) => workflow.clone(),
                    (ActAction::Apply { .. }, ActTarget::New { workflow }) => workflow.clone(),
                    _ => return None,
                };
                let words = name_words(turn, act);
                Some((act.id, workflow, name_given(turn, act)?, words))
            }
            _ => None,
        })
        .collect();
    for outcome in chained.iter_mut() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        let id = act.id;
        let mut waits = Vec::new();
        for argument in act.arguments.values_mut() {
            let ArgumentValue::Record(RecordValue::Named { workflow, named }) = &argument.value
            else {
                continue;
            };
            let around = |words: &Option<WordRange>| {
                words.zip(argument.excerpt).is_some_and(|(words, excerpt)| {
                    excerpt.message == MessageRef::Current
                        && excerpt.words.first <= words.first
                        && words.last <= excerpt.words.last
                })
            };
            let matching: Vec<ActId> = created
                .iter()
                .filter(|(creation, of, name, words)| {
                    *creation != id && of == workflow && (same_name(name, named) || around(words))
                })
                .map(|(creation, ..)| *creation)
                .collect();
            if let [creation] = matching.as_slice() {
                argument.value = ArgumentValue::Record(RecordValue::SameTurn { act: *creation });
                waits.push(*creation);
            }
        }
        for creation in waits {
            if !act.depends_on.contains(&creation) {
                act.depends_on.push(creation);
            }
        }
    }
}

/// An act waiting on a record this message creates, whose creating act is gone, takes the one
/// other record of that workflow the message still creates: the gone one was a second
/// reading of it. With none or several, it stays waiting on what is gone and is held.
fn the_one_created(creations: &[Creation], chained: &mut [Chained]) {
    use turnframe_core::understanding::{ActTarget, ArgumentValue, RecordValue};
    let present: Vec<ActId> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act.id),
            _ => None,
        })
        .collect();
    let standing = |gone: ActId| -> Option<ActId> {
        let workflow = &creations.iter().find(|c| c.act == gone)?.workflow;
        let others: Vec<ActId> = creations
            .iter()
            .filter(|c| &c.workflow == workflow && c.act != gone && present.contains(&c.act))
            .map(|c| c.act)
            .collect();
        match others.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    };
    for outcome in chained.iter_mut() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        let gone: Vec<ActId> = act
            .depends_on
            .iter()
            .copied()
            .filter(|dependency| !present.contains(dependency))
            .collect();
        for dependency in gone {
            let Some(one) = standing(dependency) else {
                continue;
            };
            for depended in &mut act.depends_on {
                if *depended == dependency {
                    *depended = one;
                }
            }
            act.depends_on.dedup();
            for argument in act.arguments.values_mut() {
                if let ArgumentValue::Record(RecordValue::SameTurn { act: created }) =
                    &mut argument.value
                    && *created == dependency
                {
                    *created = one;
                }
            }
            if let ActTarget::SameTurn { act: created } = &mut act.target
                && *created == dependency
            {
                *created = one;
            }
        }
    }
}

/// A copied value of this message that runs into the value another act of its part gives:
/// the act's place, the argument, the other value's words and what gives it.
struct RunsInto {
    index: usize,
    argument: String,
    other: WordRange,
    by: String,
}

fn runs_into(turn: &UnderstandingInput, chained: &[Chained]) -> Vec<RunsInto> {
    use turnframe_core::operation::ValueShape;
    use turnframe_core::understanding::MessageRef;
    let values: Vec<(ActId, &ActAction, WordRange, String)> = chained
        .iter()
        .filter_map(|outcome| match outcome {
            Chained::Act(act) => Some(act),
            _ => None,
        })
        .flat_map(|act| {
            let by = match &act.action {
                ActAction::Apply { operation } => operation.to_string(),
                ActAction::Start { workflow } => format!("a new {workflow}"),
            };
            act.arguments.values().filter_map(move |argument| {
                let excerpt = argument.excerpt?;
                (excerpt.message == MessageRef::Current)
                    .then(|| (act.id, &act.action, excerpt.words, by.clone()))
            })
        })
        .collect();
    let mut found = Vec::new();
    for (index, outcome) in chained.iter().enumerate() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        let Some(spec) = act
            .operation()
            .and_then(|operation| turn.operation(operation))
            .map(|(_, spec)| spec)
        else {
            continue;
        };
        for (name, argument) in &act.arguments {
            let copied = matches!(
                spec.argument_named(name).map(|a| &a.shape),
                Some(ValueShape::Text { written: false })
            );
            let Some(excerpt) = argument.excerpt else {
                continue;
            };
            if !copied || excerpt.message != MessageRef::Current {
                continue;
            }
            let (first, last) = (excerpt.words.first, excerpt.words.last);
            // Two readings of one operation are read again apart, both of them.
            let other = values
                .iter()
                .filter(|(other, action, words, _)| {
                    *other != act.id
                        && other.unit == act.id.unit
                        && **action != act.action
                        && words.first > first
                        && words.first <= last
                })
                .min_by_key(|(_, _, words, _)| words.first);
            if let Some((_, _, words, by)) = other {
                found.push(RunsInto {
                    index,
                    argument: name.clone(),
                    other: *words,
                    by: by.clone(),
                });
            }
        }
    }
    found
}

impl Understander {
    /// A copied value that runs into the value another act of its part gives is read again
    /// once, told those words, and verified again. A reading that fails leaves the first.
    async fn kept_to_own_words(
        &self,
        cx: &Context<'_>,
        planned: &[Planned<'_>],
        chained: &mut [Chained],
    ) {
        let mut notes: BTreeMap<usize, String> = BTreeMap::new();
        for found in runs_into(cx.turn, chained) {
            let span = crate::words::Span::new(found.other.first, found.other.last);
            let said = cx.turn.message.slice(span).unwrap_or_default();
            let words = match span.shown() {
                (from, to) if from == to => format!("word {from}"),
                (from, to) => format!("words {from} to {to}"),
            };
            let note = notes.entry(found.index).or_default();
            if !note.is_empty() {
                note.push(' ');
            }
            note.push_str(&format!(
                "{words}, «{said}», is the value {} gives from this part: give `{}` from its own \
                 words alone, without the words that name or give that one.",
                found.by, found.argument
            ));
        }
        notes.extend(read_twice(cx.turn, planned, chained));
        let reads = join_all(notes.into_iter().filter_map(|(index, note)| {
            let Chained::Act(act) = &chained[index] else {
                return None;
            };
            let plan = planned.iter().find(|plan| plan.id == act.id)?;
            let act = act.clone();
            Some(async move {
                let read = chain::read_values_again(cx, plan, &act, note, ".after_overlap").await;
                (index, read)
            })
        }))
        .await;
        for (index, read) in reads {
            if matches!(read, Chained::Act(_)) {
                chained[index] = read;
            }
        }
    }
}

impl Understander {
    /// A part whose every act still needs a value and holds none of the part's words, right
    /// after a copied value ending where the part begins, may be that value's tail: the
    /// value's part is read once more with those words as its own. Read with them, the value
    /// takes them and the part asks nothing; read without them, both readings stand.
    async fn tails_read_again(
        &self,
        cx: &Context<'_>,
        units: &[Seg],
        planned: &[Planned<'_>],
        chained: &mut [Chained],
    ) {
        use turnframe_core::operation::ValueShape;
        use turnframe_core::understanding::MessageRef;
        let mut found = Vec::new();
        for tail in units
            .iter()
            .filter(|unit| matches!(unit.kind(), UnitKind::Request | UnitKind::ProvidesValue))
        {
            let within =
                |words: WordRange| tail.range.first <= words.first && words.last <= tail.range.last;
            let acts: Vec<usize> = chained
                .iter()
                .enumerate()
                .filter(
                    |(_, outcome)| matches!(outcome, Chained::Act(act) if act.id.unit == tail.id),
                )
                .map(|(index, _)| index)
                .collect();
            let gave_nothing = !acts.is_empty()
                && acts.iter().all(|index| match &chained[*index] {
                    Chained::Act(act) => {
                        matches!(act.status, ActStatus::NeedsValue { .. })
                            && act.arguments.values().all(|given| {
                                given.excerpt.is_none_or(|excerpt| {
                                    excerpt.message != MessageRef::Current || !within(excerpt.words)
                                })
                            })
                    }
                    _ => false,
                });
            if !gave_nothing || tail.range.first == 0 {
                continue;
            }
            let before = tail.range.first - 1;
            let value = chained.iter().enumerate().find_map(|(index, outcome)| {
                let Chained::Act(act) = outcome else {
                    return None;
                };
                let spec = act
                    .operation()
                    .and_then(|operation| cx.turn.operation(operation))
                    .map(|(_, spec)| spec)?;
                let unit = units.iter().find(|unit| unit.id == act.id.unit)?;
                (unit.range.last == before).then_some(())?;
                act.arguments.iter().find_map(|(name, given)| {
                    let excerpt = given.excerpt?;
                    let copied = matches!(
                        spec.argument_named(name).map(|a| &a.shape),
                        Some(ValueShape::Text { written: false })
                    );
                    (copied
                        && excerpt.message == MessageRef::Current
                        && excerpt.words.last == before)
                        .then(|| (index, name.clone()))
                })
            });
            if let Some((index, argument)) = value {
                found.push((index, argument, tail.span, tail.range, acts));
            }
        }
        for (index, argument, span, range, tail_acts) in found {
            let Chained::Act(act) = &chained[index] else {
                continue;
            };
            // Told of the tail in a note, a reading kept to the part; shown the tail as the
            // part's own words, it read the value through them.
            let Some(plan) = planned.iter().find(|plan| plan.id == act.id) else {
                continue;
            };
            let words = crate::words::Span::new(plan.words.from, span.to);
            let Ok(whole) = cx.turn.message.range(words) else {
                continue;
            };
            let plan = Planned {
                words,
                range: whole,
                ..plan.clone()
            };
            let read = chain::read_again(cx, &plan, act, ".after_tail").await;
            let took_them = matches!(&read, Chained::Act(again) if again
                .arguments
                .get(&argument)
                .and_then(|given| given.excerpt)
                .is_some_and(|excerpt| excerpt.words.last >= range.last));
            if took_them {
                chained[index] = read;
                for tail in tail_acts {
                    chained[tail] = Chained::Nothing;
                }
            }
        }
    }
}

/// Two readings of one operation in one part whose copied values share words: the note each
/// is read again with, by its place. Nothing tells which took the other's words, so both are.
fn read_twice(
    turn: &UnderstandingInput,
    planned: &[Planned<'_>],
    chained: &[Chained],
) -> Vec<(usize, String)> {
    use turnframe_core::operation::ValueShape;
    use turnframe_core::understanding::MessageRef;
    let copied = |act: &turnframe_core::understanding::UnderstoodAct| -> Vec<WordRange> {
        let Some(spec) = act
            .operation()
            .and_then(|operation| turn.operation(operation))
            .map(|(_, spec)| spec)
        else {
            return Vec::new();
        };
        act.arguments
            .iter()
            .filter(|(name, _)| {
                matches!(
                    spec.argument_named(name).map(|a| &a.shape),
                    Some(ValueShape::Text { written: false })
                )
            })
            .filter_map(|(_, argument)| argument.excerpt)
            .filter(|excerpt| excerpt.message == MessageRef::Current)
            .map(|excerpt| excerpt.words)
            .collect()
    };
    let acts: Vec<(usize, &turnframe_core::understanding::UnderstoodAct)> = chained
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| match outcome {
            Chained::Act(act) => Some((index, act)),
            _ => None,
        })
        .collect();
    let occurrence = |id: ActId| {
        planned
            .iter()
            .find(|plan| plan.id == id)
            .and_then(|plan| plan.occurrence)
    };
    // Naming the shared words draws both readings to them: the note names none.
    let mut notes = Vec::new();
    for (index, act) in &acts {
        let Some((number, _)) = occurrence(act.id) else {
            continue;
        };
        let shared = acts.iter().any(|(_, other)| {
            other.id != act.id
                && other.id.unit == act.id.unit
                && other.action == act.action
                && copied(act).iter().any(|mine| {
                    copied(other)
                        .iter()
                        .any(|theirs| mine.first <= theirs.last && theirs.first <= mine.last)
                })
        });
        if shared {
            notes.push((
                *index,
                format!(
                    "two occurrences read the same values, and each asks for its own: give the \
                     values of occurrence {number} alone, counting in the order the message says \
                     them."
                ),
            ));
        }
    }
    notes
}

/// What a second reading still copies past the value another act of its part gives ends
/// where that value begins.
fn copies_keep_to_their_words(turn: &UnderstandingInput, chained: &mut [Chained]) {
    use turnframe_core::understanding::ArgumentValue;
    for found in runs_into(turn, chained) {
        let Chained::Act(act) = &mut chained[found.index] else {
            continue;
        };
        let Some(argument) = act.arguments.get_mut(&found.argument) else {
            continue;
        };
        let Some(excerpt) = argument.excerpt.as_mut() else {
            continue;
        };
        let span = crate::words::Span::new(excerpt.words.first, found.other.first - 1);
        let (Ok(range), Ok(text)) = (turn.message.range(span), turn.message.slice(span)) else {
            continue;
        };
        let text = text.trim_end_matches([',', ';', ':']).trim();
        if text.is_empty() {
            continue;
        }
        excerpt.words = range;
        argument.value = ArgumentValue::Json(text.into());
    }
}

/// The acts that create a record: one that may also apply to an existing record does
/// only while its workflow lists none.
fn creations(planned: &[Planned<'_>]) -> Vec<Creation> {
    planned
        .iter()
        .filter(|plan| match &plan.action {
            ActAction::Start { .. } => true,
            ActAction::Apply { .. } => plan.spec.is_some_and(|spec| {
                spec.target_policy == TargetPolicy::NewCaseOnly
                    || (spec.target_policy == TargetPolicy::AllowsNewCase
                        && plan.workflow.records.is_empty())
            }),
        })
        .map(|plan| Creation {
            act: plan.id,
            workflow: plan.workflow.key.clone(),
            words: plan.words,
        })
        .collect()
}

/// The acts waiting on a record this message creates: the one the assistant asked about,
/// then those of earlier turns still waiting for a record the user named.
fn waiting_acts(turn: &UnderstandingInput) -> Vec<(&PendingAct, bool)> {
    let asked = match &turn.expectation {
        Some(Expectation::Values(pending)) => Some(pending),
        _ => None,
    };
    let same = |a: &PendingAct, b: &PendingAct| a.operation == b.operation && a.record == b.record;
    asked
        .map(|pending| (pending, true))
        .into_iter()
        .chain(
            turn.waiting
                .iter()
                .filter(|pending| !asked.is_some_and(|asked| same(asked, pending)))
                .map(|pending| (pending, false)),
        )
        .collect()
}

/// The name a pending act gave the record it waits for, when that record was not found.
fn named_in(pending: &PendingAct) -> Option<(&String, &String)> {
    use turnframe_core::understanding::{ArgumentValue, RecordValue};
    pending.missing.iter().find_map(|name| {
        match pending.given.get(name).map(|argument| &argument.value) {
            Some(ArgumentValue::Record(RecordValue::Named { named, .. })) => Some((name, named)),
            _ => None,
        }
    })
}

/// The name an act gives the record it creates, when it gives one.
fn name_given(
    turn: &UnderstandingInput,
    act: &turnframe_core::understanding::UnderstoodAct,
) -> Option<String> {
    use turnframe_core::understanding::ArgumentValue;
    let (_, spec) = turn.operation(act.operation()?)?;
    let naming = spec
        .arguments
        .iter()
        .find(|argument| argument.names_the_record)?;
    match &act.arguments.get(&naming.name)?.value {
        ArgumentValue::Json(serde_json::Value::String(name)) => Some(name.clone()),
        _ => None,
    }
}

/// The words of this message that give the record an act creates its name.
fn name_words(
    turn: &UnderstandingInput,
    act: &turnframe_core::understanding::UnderstoodAct,
) -> Option<WordRange> {
    use turnframe_core::understanding::MessageRef;
    let (_, spec) = turn.operation(act.operation()?)?;
    let naming = spec
        .arguments
        .iter()
        .find(|argument| argument.names_the_record)?;
    let excerpt = act.arguments.get(&naming.name)?.excerpt?;
    (excerpt.message == MessageRef::Current).then_some(excerpt.words)
}

/// Two names the user gave are one name when they differ only in case and spacing.
fn same_name(a: &str, b: &str) -> bool {
    let words = |name: &str| {
        name.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    words(a) == words(b)
}

/// Acts waiting on a record this message creates, completed by it: asked which record an
/// act is for, the user registers that record, and the act gets it once the record exists. Only when that record is all the act was missing; an act of the same operation
/// this message left waiting for it is that act. An act of an earlier turn is completed
/// only by a record given its name, or none.
fn complete_the_waiting_acts(turn: &UnderstandingInput, chained: &mut Vec<Chained>) {
    use turnframe_core::operation::ValueShape;
    use turnframe_core::understanding::{
        ActTarget, ArgumentValue, RecordValue, UnderstoodAct, UnderstoodArgument,
    };
    let mut used: Vec<ActId> = Vec::new();
    for (pending, asked) in waiting_acts(turn) {
        let acts: Vec<UnderstoodAct> = chained
            .iter()
            .filter_map(|outcome| match outcome {
                Chained::Act(act) => Some(act.clone()),
                Chained::NotUnderstood { .. } | Chained::Nothing => None,
            })
            .collect();
        let still_waiting = |act: &UnderstoodAct| matches!(&act.status, ActStatus::NeedsValue { arguments, .. } if arguments == &pending.missing);
        let same_act = |act: &UnderstoodAct| {
            act.operation() == Some(&pending.operation)
                && match (&pending.record, &act.target) {
                    (Some(token), ActTarget::Record { token: aimed }) => token == aimed,
                    _ => true,
                }
        };
        if let Some(done) = acts.iter().find(|act| same_act(act) && !still_waiting(act)) {
            // The message did the act itself: a record it created for it still takes the name.
            let created = done
                .arguments
                .values()
                .find_map(|argument| match &argument.value {
                    ArgumentValue::Record(RecordValue::SameTurn { act }) => Some(*act),
                    _ => None,
                });
            if let (Some(created), Some((_, named))) = (created, named_in(pending)) {
                names_the_created_record(turn, chained, created, named);
            }
            continue;
        }
        let waiting = acts.iter().find(|act| same_act(act)).map(|act| act.id);
        let [missing] = pending.missing.as_slice() else {
            continue;
        };
        let Some((brief, spec)) = turn.operation(&pending.operation) else {
            continue;
        };
        let Some(ValueShape::Record { workflow }) =
            spec.argument_named(missing).map(|argument| &argument.shape)
        else {
            continue;
        };
        let named = named_in(pending).map(|(_, named)| named);
        let creation = acts.iter().find(|act| {
            act.status == ActStatus::Ready
                && !used.contains(&act.id)
                && match (&act.action, &act.target) {
                    (ActAction::Start { workflow: started }, _) => started == workflow,
                    (ActAction::Apply { .. }, ActTarget::New { workflow: new }) => new == workflow,
                    _ => false,
                }
                && (asked
                    || match (name_given(turn, act), named) {
                        (Some(given), Some(named)) => same_name(&given, named),
                        _ => true,
                    })
        });
        let Some(creation) = creation else {
            continue;
        };
        used.push(creation.id);
        let number = acts
            .iter()
            .filter(|act| act.id.unit == creation.id.unit)
            .map(|act| act.id.act)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut arguments = pending.given.clone();
        arguments.insert(
            missing.clone(),
            UnderstoodArgument {
                value: ArgumentValue::Record(RecordValue::SameTurn { act: creation.id }),
                excerpt: None,
            },
        );
        let target = match &pending.record {
            Some(token) => ActTarget::Record {
                token: token.clone(),
            },
            None => ActTarget::New {
                workflow: brief.key.clone(),
            },
        };
        let completed = UnderstoodAct {
            id: waiting.unwrap_or_else(|| ActId::new(creation.id.unit, number)),
            action: ActAction::Apply {
                operation: pending.operation.clone(),
            },
            target,
            arguments,
            words: creation.words,
            depends_on: vec![creation.id],
            status: ActStatus::Ready,
        };
        if let Some(named) = named {
            names_the_created_record(turn, chained, creation.id, named);
        }
        chained.retain(|outcome| !matches!(outcome, Chained::Act(act) if act.id == completed.id));
        chained.push(Chained::Act(completed));
    }
}

/// A record the user named that was not found, then registered for the act waiting on
/// it, takes that name when the message gives it none.
fn names_the_created_record(
    turn: &UnderstandingInput,
    chained: &mut [Chained],
    created: ActId,
    named: &str,
) {
    use turnframe_core::understanding::{ArgumentValue, UnderstoodArgument};
    for outcome in chained.iter_mut() {
        let Chained::Act(act) = outcome else {
            continue;
        };
        if act.id != created {
            continue;
        }
        let naming = act
            .operation()
            .and_then(|operation| turn.operation(operation))
            .and_then(|(_, spec)| {
                spec.arguments
                    .iter()
                    .find(|argument| argument.names_the_record)
            })
            .map(|argument| argument.name.clone());
        if let Some(naming) = naming {
            act.arguments
                .entry(naming)
                .or_insert_with(|| UnderstoodArgument {
                    value: ArgumentValue::Json(named.to_owned().into()),
                    excerpt: None,
                });
        }
    }
}
