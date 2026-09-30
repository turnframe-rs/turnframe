//! A run as frames: each one decision of the turn, at the station where it was taken.
//!
//! Frames are built from what the runtime returns and records: the understanding it was
//! handed, the replay record of the turn, its response blocks, the ledger and the outbox.
//! Identifiers get short names in order of appearance, so the recording reads, and is the
//! same on every run.

use std::collections::BTreeMap;

use serde::Serialize;
use turnframe::command::{ConfirmationPolicy, RiskClass};
use turnframe::interaction::{FieldValue, InteractionView, OptionStyle};
use turnframe::locale::Locale;
use turnframe::replay::{CommandOutcome, ReplayRecord};
use turnframe::response::{AssistantTurn, ResponseBlock};
use turnframe::store::outbox::OutboxRecord;
use turnframe::target::TargetResolution;
use turnframe::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, NotUnderstoodReason, RecordValue,
    Understanding, UnderstoodAct, UnitKind,
};

use crate::desk::Entry;
use crate::order::euros;

/// Where a decision is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Station {
    /// What the model said the message means.
    Reading,
    /// The acts understanding hands over.
    Proposal,
    /// Resolution, the domain's checks, policy.
    Reducer,
    /// Blocked, a card, or a commit.
    Decision,
    /// What was committed, and what the reply may say.
    Ledger,
}

/// What a frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The user's message.
    Message,
    /// One part of the message, as read.
    Unit,
    /// An act proposed.
    Act,
    /// What the reducer made of an act.
    Result,
    /// A policy decision.
    Policy,
    /// Something that happened outside the turn.
    World,
    /// A card drawn.
    Card,
    /// A click on a card.
    Click,
    /// A notice the server gave.
    Notice,
    /// A refusal of the turn itself.
    Refused,
    /// An event committed.
    Event,
    /// A receipt the reply carries.
    Receipt,
    /// A row of the outbox.
    Outbox,
    /// The reply the user reads.
    Reply,
}

/// A card as it was drawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardFrame {
    /// Its title.
    pub title: String,
    /// Its body.
    pub body: String,
    /// Its buttons.
    pub options: Vec<CardOption>,
    /// What it shows changed.
    pub entries: Vec<CardEntry>,
    /// The revision it is bound to.
    pub bound_revision: u64,
}

/// A button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardOption {
    /// Its id.
    pub id: String,
    /// Its label.
    pub label: String,
    /// Whether it is the primary one.
    pub primary: bool,
}

/// A change a card shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardEntry {
    /// What changed.
    pub label: String,
    /// Before.
    pub before: String,
    /// After.
    pub after: String,
}

/// One decision, at its station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frame {
    /// Where.
    pub station: Station,
    /// What.
    pub kind: Kind,
    /// The replay's own tag: `proposed`, `decided`, `held`, `committed`, `persisted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    /// The frame in words.
    pub text: String,
    /// Whether a scripted model wrote it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub scripted: bool,
    /// The card, for a card or a click.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<CardFrame>,
    /// The option clicked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    /// The revision an event moved the order to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    /// A stable code: an event type, a notice code, an outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A short name for an id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Frame {
    fn new(station: Station, kind: Kind, text: impl Into<String>) -> Self {
        Self {
            station,
            kind,
            state: None,
            text: text.into(),
            scripted: false,
            card: None,
            option: None,
            rev: None,
            code: None,
            id: None,
        }
    }

    const fn state(mut self, state: &'static str) -> Self {
        self.state = Some(state);
        self
    }
}

/// How a run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    /// No refund was sent.
    NothingMoved,
    /// A card is open, and nothing is sent until it is clicked.
    WaitingOnAClick,
    /// One refund was sent, and its answer recorded once.
    OneRefund,
}

/// The verdict, with the sentence the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// Which.
    pub kind: VerdictKind,
    /// In words.
    pub text: &'static str,
}

/// The verdict of a ledger, and whether a card is still open.
#[must_use]
pub fn verdict(ledger: &[Entry], card_open: bool) -> Verdict {
    let sent = ledger
        .iter()
        .filter(|entry| entry.event_type == "order.refund_sent")
        .count();
    let (kind, text) = match (sent, card_open) {
        (0, false) => (VerdictKind::NothingMoved, "Nothing moved"),
        (0, true) => (VerdictKind::WaitingOnAClick, "Waiting on a click"),
        _ => (VerdictKind::OneRefund, "One refund, recorded once"),
    };
    Verdict { kind, text }
}

/// Short names for ids, in order of appearance, per prefix.
#[derive(Debug, Clone, Default)]
pub struct Names {
    given: BTreeMap<(String, String), String>,
    counts: BTreeMap<String, usize>,
}

impl Names {
    /// The short name of `id` under `prefix`: `evt_1`, `card_2`.
    pub fn name(&mut self, prefix: &str, id: &str) -> String {
        let key = (prefix.to_owned(), id.to_owned());
        if let Some(name) = self.given.get(&key) {
            return name.clone();
        }
        let count = self.counts.entry(prefix.to_owned()).or_default();
        *count += 1;
        let name = format!("{prefix}_{count}");
        self.given.insert(key, name.clone());
        name
    }
}

/// A value an act was given, as the desk writes it.
fn value_text(value: &ArgumentValue) -> String {
    match value {
        ArgumentValue::Json(json) => match (json.get("minor"), json.get("currency")) {
            (Some(minor), Some(_)) => euros(minor.as_i64().unwrap_or_default()),
            _ => json
                .as_str()
                .map_or_else(|| json.to_string(), str::to_owned),
        },
        ArgumentValue::Record(RecordValue::Named { named, .. }) => format!("«{named}»"),
        ArgumentValue::Record(other) => format!("{other:?}"),
    }
}

fn operation_of(action: &ActAction) -> String {
    match action {
        ActAction::Apply { operation } => operation.to_string(),
        ActAction::Start { workflow } => format!("a new {workflow}"),
    }
}

fn unit_kind(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Request => "a request",
        UnitKind::Question => "a question",
        UnitKind::Constraint => "a condition",
        UnitKind::Correction => "a correction",
        UnitKind::Cancel => "a cancellation",
        UnitKind::ProvidesValue => "an answer",
        _ => "small talk",
    }
}

/// An act by what it came from: a click's act is the click.
fn act_name(act: &ActId) -> String {
    if act.unit.0 == 0 {
        "The click".to_owned()
    } else {
        act.to_string()
    }
}

fn reason_text(reason: &NotUnderstoodReason) -> String {
    match reason {
        NotUnderstoodReason::TaskFailed { task, code } => {
            format!("the model's {task} call failed ({code})")
        }
        NotUnderstoodReason::NotRequested => "nobody asked for it".to_owned(),
        other => format!("{other:?}"),
    }
}

fn policy_text(risk: RiskClass, confirmation: ConfirmationPolicy) -> String {
    let risk = match risk {
        RiskClass::ReversibleLowRisk => "low risk",
        RiskClass::ExternalRegulated => "external and regulated",
        RiskClass::Destructive => "destructive",
        RiskClass::SensitiveDataChange => "a sensitive change",
        _ => "at risk",
    };
    let confirmation = match confirmation {
        ConfirmationPolicy::None => "no confirmation",
        ConfirmationPolicy::ExplicitClick => "an explicit click",
        ConfirmationPolicy::ReviewCard => "a review card",
        _ => "a confirmation",
    };
    format!("Policy: {risk}, needs {confirmation}")
}

fn card_frame(view: &InteractionView, locale: &Locale) -> CardFrame {
    let shown = |value: &FieldValue| match value {
        FieldValue::Present(value) => value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned),
        FieldValue::Absent => String::new(),
    };
    CardFrame {
        title: view.title.resolve(locale).to_owned(),
        body: view
            .body
            .as_ref()
            .map(|body| body.resolve(locale).to_owned())
            .unwrap_or_default(),
        options: view
            .options
            .iter()
            .map(|option| CardOption {
                id: option.id.as_str().to_owned(),
                label: option.label.resolve(locale).to_owned(),
                primary: option.style == OptionStyle::Primary,
            })
            .collect(),
        entries: view
            .review_entries
            .iter()
            .map(|entry| CardEntry {
                label: entry.label.resolve(locale).to_owned(),
                before: shown(&entry.before),
                after: shown(&entry.after),
            })
            .collect(),
        bound_revision: view.case_ref.expected_revision.value(),
    }
}

/// The answers a model wrote for the turn's questions.
fn answers(turn: &AssistantTurn) -> Vec<String> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer.text.clone()),
            _ => None,
        })
        .collect()
}

/// Accumulates one run's frames.
#[derive(Debug, Default)]
pub struct Recorder {
    /// The frames so far.
    pub frames: Vec<Frame>,
    names: Names,
    seen: usize,
    stopped_at: Option<Station>,
}

impl Recorder {
    /// The user's message.
    pub fn message(&mut self, text: &str) {
        self.frames
            .push(Frame::new(Station::Reading, Kind::Message, text));
    }

    /// What the model made of the message, and the acts it handed over; `label` names the
    /// record a token stands for.
    pub fn reading(
        &mut self,
        message: &str,
        understanding: &Understanding,
        label: &dyn Fn(&ActTarget) -> String,
    ) {
        let words: Vec<&str> = message.split_whitespace().collect();
        let said =
            |first: usize, last: usize| words.get(first..=last).unwrap_or_default().join(" ");
        for unit in &understanding.units {
            let mut frame = Frame::new(
                Station::Reading,
                Kind::Unit,
                format!(
                    "«{}» is {}.",
                    said(unit.words.first, unit.words.last),
                    unit_kind(unit.kind)
                ),
            );
            frame.scripted = true;
            self.frames.push(frame);
        }
        for act in &understanding.acts {
            self.frames.push(Self::act_frame(act, label));
        }
        for gone in &understanding.superseded {
            let mut frame = Frame::new(
                Station::Proposal,
                Kind::Act,
                format!(
                    "{} is taken back by part {} of the same message.",
                    operation_of(&gone.action),
                    gone.by.0
                ),
            )
            .state("held");
            frame.scripted = true;
            self.frames.push(frame);
        }
        for question in &understanding.questions {
            let about = question.record.as_ref().map_or_else(String::new, |token| {
                format!(
                    " about {}",
                    label(&ActTarget::Record {
                        token: token.clone()
                    })
                )
            });
            let mut frame = Frame::new(
                Station::Proposal,
                Kind::Act,
                format!(
                    "A question{about}: «{}».",
                    said(question.words.first, question.words.last)
                ),
            )
            .state("proposed");
            frame.scripted = true;
            self.frames.push(frame);
        }
        for missed in &understanding.not_understood {
            self.frames.push(
                Frame::new(
                    Station::Reading,
                    Kind::Unit,
                    format!(
                        "«{}» was not understood: {}.",
                        said(missed.words.first, missed.words.last),
                        reason_text(&missed.reason)
                    ),
                )
                .state("held"),
            );
        }
    }

    fn act_frame(act: &UnderstoodAct, label: &dyn Fn(&ActTarget) -> String) -> Frame {
        let values: Vec<String> = act
            .arguments
            .iter()
            .map(|(name, argument)| format!("{name} {}", value_text(&argument.value)))
            .collect();
        let values = if values.is_empty() {
            String::new()
        } else {
            format!(", {}", values.join(", "))
        };
        let status = match &act.status {
            ActStatus::Ready => "ready".to_owned(),
            ActStatus::NeedsValue { arguments, .. } => format!("asks for {}", arguments.join(", ")),
            other => format!("{other:?}"),
        };
        let mut frame = Frame::new(
            Station::Proposal,
            Kind::Act,
            format!(
                "{} on {}{values}: {status}.",
                operation_of(&act.action),
                label(&act.target)
            ),
        )
        .state("proposed");
        frame.scripted = true;
        frame
    }

    /// A turn the runtime answered: what the reducer decided, what the turn committed, the
    /// cards and notices it gave, the new events of `ledger` and the reply.
    pub fn turn(&mut self, record: &ReplayRecord, turn: &AssistantTurn, ledger: &[Entry]) {
        let locale = Locale::from("en-GB");
        for resolved in &record.target_resolutions {
            let text = match &resolved.resolution {
                TargetResolution::Exact { case_ref } => format!(
                    "{} is about order {} at revision {}.",
                    act_name(&resolved.act),
                    case_ref.case_id.as_str().trim_start_matches("order-"),
                    case_ref.expected_revision.value()
                ),
                TargetResolution::Missing => {
                    format!("{}: this desk has no such order.", act_name(&resolved.act))
                }
                other => format!("{}: {other:?}.", act_name(&resolved.act)),
            };
            self.frames
                .push(Frame::new(Station::Reducer, Kind::Result, text).state("decided"));
        }
        for outcome in &record.act_outcomes {
            let held = !matches!(outcome.as_str(), "ready_to_execute");
            let frame = Frame::new(Station::Reducer, Kind::Result, outcome.replace('_', " "));
            self.frames
                .push(frame.state(if held { "held" } else { "decided" }));
        }
        for decision in &record.policy_decisions {
            let why = if decision.requires_interaction.is_some() {
                ": a card, not a write"
            } else if decision.policy.confirmation == ConfirmationPolicy::None {
                ""
            } else {
                ": the click is it"
            };
            let text = format!(
                "{}{why}.",
                policy_text(decision.policy.risk, decision.policy.confirmation)
            );
            self.frames
                .push(Frame::new(Station::Reducer, Kind::Policy, text).state("decided"));
        }
        for outcome in &record.command_outcomes {
            let (text, state) = match &outcome.outcome {
                CommandOutcome::Committed { new_revision, .. } => (
                    format!("Committed at revision {}.", new_revision.value()),
                    "committed",
                ),
                CommandOutcome::IdempotentReplay => (
                    "Already done under this key: nothing runs twice.".to_owned(),
                    "held",
                ),
                CommandOutcome::OutcomeUnknown { .. } => (
                    "Sent, and no answer: the outcome is unknown.".to_owned(),
                    "held",
                ),
                other => (format!("{other:?}"), "held"),
            };
            self.frames
                .push(Frame::new(Station::Decision, Kind::Result, text).state(state));
        }
        for block in &turn.blocks {
            match block {
                ResponseBlock::Interaction(card) => {
                    let mut frame = Frame::new(
                        Station::Decision,
                        Kind::Card,
                        card.view.title.resolve(&locale),
                    )
                    .state("persisted");
                    frame.id = Some(self.names.name("card", &card.view.id.to_string()));
                    frame.card = Some(card_frame(&card.view, &locale));
                    self.frames.push(frame);
                }
                ResponseBlock::Notice(notice) => {
                    let mut frame = Frame::new(
                        Station::Decision,
                        Kind::Notice,
                        notice.text.resolve(&locale),
                    )
                    .state("held");
                    frame.code = Some(notice.code.clone());
                    self.frames.push(frame);
                }
                _ => {}
            }
        }
        self.ledger(ledger);
        for block in &turn.blocks {
            match block {
                ResponseBlock::Receipt(receipt) => {
                    let mut frame = Frame::new(
                        Station::Ledger,
                        Kind::Receipt,
                        format!(
                            "{}: {}",
                            receipt.receipt.title.resolve(&locale),
                            receipt.receipt.body.resolve(&locale)
                        ),
                    );
                    frame.code = Some(receipt.receipt.status_code.clone());
                    self.frames.push(frame);
                }
                // The reply is the answers, the model's words, then what the server adds.
                ResponseBlock::Transition(transition) => {
                    let mut rest = transition.text.clone();
                    for answer in answers(turn) {
                        rest = rest.replacen(&answer, "", 1);
                        let mut frame = Frame::new(Station::Ledger, Kind::Reply, answer);
                        frame.scripted = true;
                        self.frames.push(frame);
                    }
                    let rest = rest.trim();
                    if !rest.is_empty() {
                        self.frames
                            .push(Frame::new(Station::Ledger, Kind::Reply, rest));
                    }
                }
                _ => {}
            }
        }
    }

    /// The events of `ledger` not recorded yet.
    pub fn ledger(&mut self, ledger: &[Entry]) {
        for entry in ledger.iter().skip(self.seen) {
            let mut frame = Frame::new(
                Station::Ledger,
                Kind::Event,
                format!("{} on order {}", entry.event_type, entry.order),
            )
            .state("committed");
            frame.rev = Some(entry.revision.value());
            frame.code = Some(entry.event_type.clone());
            frame.id = Some(self.names.name("evt", &entry.event_id.to_string()));
            self.frames.push(frame);
        }
        self.seen = self.seen.max(ledger.len());
    }

    /// Something that happened outside the turn.
    pub fn world(&mut self, station: Station, text: &str) {
        self.frames.push(Frame::new(station, Kind::World, text));
    }

    /// A turn the runtime refused whole, with its reason.
    pub fn refused(&mut self, station: Station, text: &str) {
        self.frames
            .push(Frame::new(station, Kind::Refused, text).state("held"));
    }

    /// A click on `option` of the card `view`.
    pub fn click(&mut self, view: &InteractionView, option: &str) {
        let locale = Locale::from("en-GB");
        let label = view
            .options
            .iter()
            .find(|shown| shown.id.as_str() == option)
            .map_or_else(
                || option.to_owned(),
                |shown| shown.label.resolve(&locale).to_owned(),
            );
        let mut frame = Frame::new(Station::Decision, Kind::Click, format!("Clicks «{label}»."));
        frame.id = Some(self.names.name("card", &view.id.to_string()));
        frame.option = Some(option.to_owned());
        frame.card = Some(card_frame(view, &locale));
        self.frames.push(frame);
    }

    /// A row of the outbox, as it stands.
    pub fn outbox(&mut self, row: &OutboxRecord) {
        let mut frame = Frame::new(
            Station::Ledger,
            Kind::Outbox,
            format!(
                "Outbox: {:?}, sent {} time(s).",
                row.entry.status, row.entry.attempt_count
            ),
        )
        .state("persisted");
        frame.code = Some(format!("{:?}", row.entry.status));
        frame.id = Some(self.names.name("ob", &row.entry.outbox_id.to_string()));
        self.frames.push(frame);
    }

    /// Marks the station where the attack was stopped.
    pub const fn stop(&mut self, station: Station) {
        self.stopped_at = Some(station);
    }

    /// The frames, the station the attack was stopped at, and the verdict.
    #[must_use]
    pub fn finish(
        self,
        ledger: &[Entry],
        card_open: bool,
    ) -> (Vec<Frame>, Option<Station>, Verdict) {
        (self.frames, self.stopped_at, verdict(ledger, card_open))
    }
}

/// One attack, recorded.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    /// Its id: `wrong-order`.
    pub id: &'static str,
    /// `none`, `model`, `user` or `world`.
    pub group: &'static str,
    /// The control's label.
    pub label: &'static str,
    /// The attack in one line.
    pub attack: &'static str,
    /// The message.
    pub message: String,
    /// Where it was stopped.
    pub stopped_at: Option<Station>,
    /// How it ended.
    pub verdict: Verdict,
    /// What happened.
    pub frames: Vec<Frame>,
}

/// An order as the page introduces it.
#[derive(Debug, Clone, Serialize)]
pub struct OrderLine {
    /// `Order 381`.
    pub label: String,
    /// Who paid.
    pub customer: String,
    /// What was paid.
    pub paid: String,
    /// When it was delivered.
    pub delivered: String,
    /// Whether the desk sees it.
    pub listed: bool,
}

/// Every run.
#[derive(Debug, Clone, Serialize)]
pub struct Recording {
    /// The command that wrote it.
    pub recorded_with: &'static str,
    /// The library's version.
    pub version: &'static str,
    /// The orders.
    pub orders: Vec<OrderLine>,
    /// The runs.
    pub runs: Vec<Run>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe::ids::{CaseRevision, EventId};

    use crate::desk::Entry;

    fn entry(event_type: &str, n: u128) -> Entry {
        Entry {
            order: 381,
            event_type: event_type.to_owned(),
            event_id: EventId::from(uuid::Uuid::from_u128(n)),
            revision: CaseRevision(1 + u64::try_from(n).unwrap_or_default()),
        }
    }

    #[test]
    fn the_same_id_keeps_its_short_name() {
        let mut names = Names::default();
        assert_eq!(names.name("evt", "a"), "evt_1");
        assert_eq!(names.name("evt", "b"), "evt_2");
        assert_eq!(names.name("evt", "a"), "evt_1");
        assert_eq!(names.name("card", "a"), "card_1");
    }

    #[test]
    fn no_refund_and_no_card_is_nothing_moved() {
        let requested = [entry("order.refund_requested", 1)];
        assert_eq!(verdict(&requested, false).kind, VerdictKind::NothingMoved);
    }

    #[test]
    fn an_open_card_is_waiting_on_a_click() {
        let requested = [entry("order.refund_requested", 1)];
        assert_eq!(verdict(&requested, true).kind, VerdictKind::WaitingOnAClick);
    }

    #[test]
    fn one_refund_sent_is_one_refund() {
        let sent = [
            entry("order.refund_requested", 1),
            entry("order.refund_sent", 2),
            entry("order.provider_outcome_recorded", 3),
        ];
        let found = verdict(&sent, false);
        assert_eq!(found.kind, VerdictKind::OneRefund);
        assert_eq!(found.text, "One refund, recorded once");
    }

    #[test]
    fn frames_keep_the_order_they_were_recorded_in() {
        let mut recorder = Recorder::default();
        recorder.message("Refund order 381 for €129");
        recorder.world(Station::Ledger, "A colleague refunds €30.00.");
        recorder.refused(Station::Decision, "The card is stale.");
        let stations: Vec<Station> = recorder.frames.iter().map(|frame| frame.station).collect();
        assert_eq!(
            stations,
            [Station::Reading, Station::Ledger, Station::Decision]
        );
    }
}
