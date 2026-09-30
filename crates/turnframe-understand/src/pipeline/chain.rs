//! One act's chain: locate, extract, verify, check. Each step can only narrow the act.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use turnframe_core::ids::{OperationKey, TargetToken, WorkflowKey};
use turnframe_core::operation::{ArgumentSpec, DateExpr, OperationSpec, ValueShape};
use turnframe_core::plan::{ActMutability, TargetPolicy};
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, MessageRef, NotUnderstoodReason,
    RecordValue, UnderstoodAct, UnderstoodArgument, UnitId, WordRange,
};
use turnframe_tasks::{TaskCall, TaskId, TaskOutcome};

use crate::check::argument_of;
use crate::input::{Expectation, PendingAct, Speaker, UnderstandingInput, WorkflowBrief};
use crate::pipeline::{Context, VerifyPolicy};
use crate::progress::{Located, Step};
use crate::render;
use crate::tasks::extract::{
    Extract, ExtractInput, Extraction, Given, RecordChoice, RecordContext,
};
use crate::tasks::locate::{AMBIGUOUS, BY_NAME, Candidate, Locate, LocateInput, NEW};
use crate::tasks::verify::{ArgumentVerdict, Overall, Verdict, Verify, VerifyInput};
use crate::values::{Extracted, convert};
use crate::words::Span;

/// An act planned for a unit, before its chain runs.
#[derive(Debug, Clone)]
pub(crate) struct Planned<'a> {
    pub id: ActId,
    pub unit: UnitId,
    pub label: &'static str,
    pub words: Span,
    pub range: WordRange,
    pub action: ActAction,
    pub workflow: &'a WorkflowBrief,
    pub spec: Option<&'a OperationSpec>,
    pub continues: Option<Span>,
    /// Which of the unit's acts of the same operation this is, and how many there are.
    pub occurrence: Option<(usize, usize)>,
    pub pending: Option<&'a PendingAct>,
    /// Acts of its operation the last turn did, when its unit corrects one.
    pub corrects: Vec<&'a PendingAct>,
    /// The words of the other units routed to the same action.
    pub kin: Vec<Span>,
    /// The other operations its unit asks for.
    pub also: Vec<OperationKey>,
    /// Whether only coverage found its unit: a guess.
    pub guessed: bool,
    pub parent: TaskId,
    pub depth: u8,
}

/// An act the same message creates, which a later act may apply to or name.
#[derive(Debug, Clone)]
pub(crate) struct Creation {
    pub act: ActId,
    pub workflow: WorkflowKey,
    pub words: Span,
}

/// What a chain produced.
#[derive(Debug, Clone)]
pub(crate) enum Chained {
    Act(UnderstoodAct),
    NotUnderstood {
        unit: UnitId,
        words: WordRange,
        reason: NotUnderstoodReason,
        aimed: Option<ActTarget>,
    },
    /// An act whose every value lies in another part's words: that part's, read twice.
    Nothing,
}

struct Chain<'c, 'a> {
    cx: &'c Context<'a>,
    plan: &'c Planned<'a>,
    carried: BTreeMap<String, UnderstoodArgument>,
    unit: TaskId,
    parent: TaskId,
    depth: u8,
    /// What the whole-turn check found, shown to each task of a second reading.
    note: Option<String>,
    /// Appended to the name of each task call: `""`, `".after_cross_check"`,
    /// `".after_reroute"`, `".after_overlap"` or `".after_tail"`.
    after: &'static str,
}

impl<'c, 'a> Chain<'c, 'a> {
    fn new(cx: &'c Context<'a>, plan: &'c Planned<'a>, note: Option<String>) -> Self {
        Self {
            cx,
            plan,
            carried: plan
                .pending
                .map(|pending| pending.given.clone())
                .unwrap_or_default(),
            // A unit's first act keeps the unit's name; a second one asked in the same
            // words is named for itself.
            unit: TaskId::new(if plan.id.act == 1 {
                plan.unit.to_string()
            } else {
                plan.id.to_string()
            }),
            parent: plan.parent.clone(),
            depth: plan.depth,
            after: if note.is_some() {
                ".after_cross_check"
            } else {
                ""
            },
            note,
        }
    }
}

pub(crate) async fn run(cx: &Context<'_>, plan: &Planned<'_>) -> Chained {
    Chain::new(cx, plan, None).run().await
}

/// Reads an act of a unit routed again, its task calls named apart from the first reading's.
pub(crate) async fn run_rerouted(cx: &Context<'_>, plan: &Planned<'_>) -> Chained {
    let mut chain = Chain::new(cx, plan, None);
    chain.after = ".after_reroute";
    chain.run().await
}

/// A finding of the whole-turn check, sending one step of one act back.
#[derive(Debug, Clone)]
pub(crate) enum Revisit {
    /// Extract again, keeping the target; then verify.
    Value { note: String },
    /// Locate again; then, at another record, extract and verify.
    Record { note: String },
    /// Verify again, not told the doubt: the verifier judges afresh.
    Asked,
}

/// Reads `act` again from the step `revisit` names, the note shown to that step's task.
pub(crate) async fn revisit(
    cx: &Context<'_>,
    plan: &Planned<'_>,
    act: &UnderstoodAct,
    revisit: &Revisit,
) -> Chained {
    let note = match revisit {
        Revisit::Value { note } | Revisit::Record { note } => Some(note.clone()),
        Revisit::Asked => None,
    };
    let mut chain = Chain::new(cx, plan, note);
    chain.after = ".after_cross_check";
    match revisit {
        Revisit::Record { .. } => match chain.target().await {
            // Located again to the record it had, the act stands as it was verified.
            Ok(target) if target == act.target => Chained::Act(act.clone()),
            Ok(target) => chain.run_from(target).await,
            Err(reason) => chain.not_understood(reason, None),
        },
        Revisit::Value { .. } => chain.run_from(act.target.clone()).await,
        Revisit::Asked => chain.verify_again(act.clone()).await,
    }
}

/// Reads `act`'s values again, told `note`, then verifies them; its calls are named `after`.
pub(crate) async fn read_values_again(
    cx: &Context<'_>,
    plan: &Planned<'_>,
    act: &UnderstoodAct,
    note: String,
    after: &'static str,
) -> Chained {
    let mut chain = Chain::new(cx, plan, Some(note));
    chain.after = after;
    chain.run_from(act.target.clone()).await
}

/// Reads an act's values again with no note, as a chain of its own named `after`.
pub(crate) async fn read_again(
    cx: &Context<'_>,
    plan: &Planned<'_>,
    act: &UnderstoodAct,
    after: &'static str,
) -> Chained {
    let mut chain = Chain::new(cx, plan, None);
    chain.after = after;
    chain.run_from(act.target.clone()).await
}

/// How a record an argument names is shown to a task.
pub(crate) fn record_label(turn: &UnderstandingInput, value: &RecordValue) -> String {
    match value {
        RecordValue::Record { token } => turn
            .record(token)
            .map_or_else(|| token.to_string(), |(_, record)| record.label.clone()),
        RecordValue::SameTurn { act } => format!("the record act {act} creates"),
        RecordValue::Named { workflow, named } => {
            format!(
                "a {workflow} record named {}, not listed",
                render::quoted(named)
            )
        }
    }
}

/// A record an act of this message creates, by the words that create it.
fn created_label(turn: &UnderstandingInput, creation: &Creation) -> String {
    let said = turn.message.slice(creation.words).unwrap_or_default();
    format!(
        "the {} record this message creates, {}",
        creation.workflow,
        render::quoted(said)
    )
}

fn failed(task: &str, code: String) -> NotUnderstoodReason {
    NotUnderstoodReason::TaskFailed {
        task: task.to_owned(),
        code,
    }
}

impl<'a> Chain<'_, 'a> {
    fn not_understood(&self, reason: NotUnderstoodReason, aimed: Option<ActTarget>) -> Chained {
        Chained::NotUnderstood {
            unit: self.plan.unit,
            words: self.plan.range,
            reason,
            aimed,
        }
    }

    fn advance<O>(&mut self, id: TaskId, outcome: &TaskOutcome<O>) {
        self.depth = outcome.depth();
        self.parent = id;
    }

    async fn run(mut self) -> Chained {
        let target = match self.target().await {
            Ok(target) => target,
            Err(reason) => return self.not_understood(reason, None),
        };
        self.run_from(target).await
    }

    /// Verifies `act` again, afresh. The verifier decides.
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
        for name in verdict.at_fault() {
            act.arguments.remove(&name);
        }
        let asked = self.to_ask(&verdict);
        if !asked.is_empty() {
            act.status = ActStatus::NeedsValue {
                arguments: asked,
                reason: None,
            };
        }
        Chained::Act(act)
    }

    /// The values a verdict found wanting that are asked back: an optional one the user did
    /// not give is only left out.
    fn to_ask(&self, verdict: &Verdict) -> Vec<String> {
        let optional = |name: &str| {
            self.plan
                .spec
                .and_then(|spec| spec.argument_named(name))
                .is_some_and(|argument| !argument.required)
        };
        verdict
            .at_fault()
            .into_iter()
            .filter(|name| {
                !(verdict.arguments.get(name) == Some(&ArgumentVerdict::NotStated)
                    && optional(name))
            })
            .collect()
    }

    async fn run_from(mut self, target: ActTarget) -> Chained {
        self.cx.steps.step(Step::Located {
            act: self.plan.id,
            record: self.located(&target),
        });
        let mut input = self.extract_input(&target);
        let mut extraction = None;
        let mut extracted = Extracted::default();
        if let Some(input) = &mut input {
            match self.extract(input, "extract", None).await {
                Ok((output, values)) => {
                    let values = self.dated_as_corrected(input, &output, values).await;
                    let input = &*input;
                    let only_elsewhere =
                        values.arguments.is_empty() && !values.elsewhere.is_empty();
                    // A guessed part pointing only at another part's words is that part.
                    if only_elsewhere && self.plan.guessed {
                        return Chained::Nothing;
                    }
                    let (output, values) = self.own_values(input, output, values).await;
                    if only_elsewhere && values.arguments.is_empty() {
                        return Chained::Nothing;
                    }
                    let (output, values) = self.own_end(input, output, values).await;
                    let (output, values) = self.records_read_again(input, output, values).await;
                    let (output, values) =
                        self.asked_read_again(input, &target, output, values).await;
                    extraction = Some(output);
                    extracted = values;
                }
                Err(reason) => return self.not_understood(reason, Some(target)),
            }
        }
        self.cx.steps.step(Step::Extracted {
            act: self.plan.id,
            given: extracted
                .arguments
                .iter()
                .map(|(name, argument)| (name.clone(), self.shown(argument)))
                .collect(),
            not_given: extracted.not_given.clone(),
        });
        // A correction changes what it says of the act it corrects, and keeps the rest.
        if let Some(done) = self.corrected(&target) {
            for (name, value) in &done.given {
                if !extracted.arguments.contains_key(name) {
                    self.carried
                        .entry(name.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        }
        let mut target = target;
        let mut status = self.missing(&extracted);
        // An act that would ask for a value asks only when the verdict finds it asked for.
        if matches!(status, ActStatus::NeedsValue { .. })
            && self.verifies()
            && let Ok(verdict) = self.verify(&target, &extracted, "verify").await
            && verdict.overall == Overall::NotRequested
        {
            return self.not_understood(NotUnderstoodReason::NotRequested, Some(target));
        }
        if status == ActStatus::Ready && self.verifies() {
            match self
                .verified(&mut target, input.as_ref(), &mut extraction, &mut extracted)
                .await
            {
                Ok(next) => status = next,
                Err(reason) => return self.not_understood(reason, Some(target)),
            }
        }
        let mut arguments = self.carried.clone();
        arguments.extend(extracted.arguments.clone());
        if !extracted.aside.is_empty()
            && let Ok(mut aside) = self.cx.aside.lock()
        {
            aside.insert(self.plan.id, extracted.aside.clone());
        }
        let mut act = self.act(target, arguments, status);
        if act.status == ActStatus::Ready && self.cx.checker.is_active() {
            self.checked(&mut act, input.as_ref(), extraction.as_ref())
                .await;
        }
        Chained::Act(act)
    }

    /// The dates the act the last turn did gave, by argument, when this corrects it.
    fn corrected_dates(&self, target: &ActTarget) -> BTreeMap<String, chrono::NaiveDate> {
        self.corrected(target)
            .into_iter()
            .flat_map(|done| &done.given)
            .filter_map(|(name, given)| match &given.value {
                ArgumentValue::Json(serde_json::Value::String(text)) => {
                    Some((name.clone(), text.parse().ok()?))
                }
                _ => None,
            })
            .collect()
    }

    /// A correction giving a date without its year, of a request earlier in the message,
    /// reads that request's date once: the correction keeps its year.
    async fn dated_as_corrected(
        &mut self,
        input: &mut ExtractInput<'a>,
        output: &Extraction,
        values: Extracted,
    ) -> Extracted {
        let Some(continued) = self
            .plan
            .continues
            .filter(|_| self.plan.label == "Correction")
        else {
            return values;
        };
        // A date given with no year, or with one its words do not say, takes the corrected one.
        let turn = self.cx.turn;
        let unsaid = |given: &Given| match given {
            Given::Date {
                date: DateExpr::Absolute { year: None, .. },
                ..
            } => true,
            Given::Date {
                date:
                    DateExpr::Absolute {
                        year: Some(year),
                        day,
                        ..
                    },
                message,
                ..
            } if message == crate::tasks::extract::CURRENT => given
                .pointer()
                .and_then(|(_, span)| turn.message.slice(span).ok())
                .is_some_and(|said| !crate::values::says_year(said, *year, *day)),
            _ => false,
        };
        let undated: Vec<&String> = output
            .arguments
            .iter()
            .filter(|(name, given)| unsaid(given) && !input.corrected.contains_key(*name))
            .map(|(name, _)| name)
            .collect();
        if undated.is_empty() {
            return values;
        }
        let mut earlier = input.clone();
        earlier.label = "Request";
        earlier.words = continued;
        earlier.continues = None;
        earlier.others.push(input.words);
        earlier
            .arguments
            .retain(|argument| undated.contains(&&argument.name));
        let Ok((_, read)) = self.extract(&earlier, "extract.corrected", None).await else {
            return values;
        };
        for name in undated {
            if let Some(ArgumentValue::Json(serde_json::Value::String(text))) =
                read.arguments.get(name).map(|given| &given.value)
                && let Ok(date) = text.parse()
            {
                input.corrected.insert(name.clone(), date);
            }
        }
        convert(self.cx.turn, input, output).unwrap_or(values)
    }

    /// The act the last turn did that this correction changes: the one of its operation on
    /// the record it reached, when there is exactly one.
    fn corrected(&self, target: &ActTarget) -> Option<&'a PendingAct> {
        let ActTarget::Record { token } = target else {
            return None;
        };
        let on_it: Vec<&'a PendingAct> = self
            .plan
            .corrects
            .iter()
            .copied()
            .filter(|done| done.record.as_ref() == Some(token))
            .collect();
        match on_it[..] {
            [done] => Some(done),
            _ => None,
        }
    }

    /// Values pointed at in words another part of the message holds are read again once,
    /// told whose words they are; the first reading stands when the second fails.
    async fn own_values(
        &mut self,
        input: &ExtractInput<'a>,
        output: Extraction,
        values: Extracted,
    ) -> (Extraction, Extracted) {
        if values.elsewhere.is_empty() {
            return (output, values);
        }
        let mut note = String::new();
        for (name, span) in &values.elsewhere {
            let said = self.cx.turn.message.slice(*span).unwrap_or_default();
            let (from, to) = span.shown();
            let _ = writeln!(
                note,
                "`{name}` points at words {from} to {to}, «{said}», which another part of the \
                 message holds, asking for something of its own: give this part's value from \
                 its own words or an earlier message, or not_given."
            );
        }
        match self
            .extract(
                input,
                "extract.after_elsewhere",
                Some((&output, note.trim_end())),
            )
            .await
        {
            // What the first reading set aside stays aside when the second gives nothing for it.
            Ok((repaired, mut read)) => {
                for (name, given) in values.aside {
                    if !read.arguments.contains_key(&name) {
                        read.aside.entry(name).or_insert(given);
                    }
                }
                (repaired, read)
            }
            Err(_) => (output, values),
        }
    }

    /// A record argument the reading gave no value is read once more, told so: words of the
    /// part that name the record give it, and with none it stays unset. The second reading
    /// gives only those arguments.
    async fn records_read_again(
        &mut self,
        input: &ExtractInput<'a>,
        output: Extraction,
        mut values: Extracted,
    ) -> (Extraction, Extracted) {
        let unread: Vec<String> = input
            .arguments
            .iter()
            .filter(|argument| matches!(argument.shape, ValueShape::Record { .. }))
            .map(|argument| argument.name.clone())
            .filter(|name| {
                values.not_given.contains(name)
                    && !values.elsewhere.iter().any(|(other, _)| other == name)
                    && !self.carried.contains_key(name)
            })
            .collect();
        if unread.is_empty() {
            return (output, values);
        }
        let note = unread
            .iter()
            .map(|name| {
                format!(
                    "`{name}` was given no value: when words of this part name the record, give \
                     it; otherwise give not_given again."
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Ok((again, read)) = self
            .extract(input, "extract.after_not_given", Some((&output, &note)))
            .await
        else {
            return (output, values);
        };
        let mut took = false;
        for name in &unread {
            if let Some(given) = read.arguments.get(name) {
                values.arguments.insert(name.clone(), given.clone());
                values.not_given.retain(|other| other != name);
                took = true;
            }
        }
        (if took { again } else { output }, values)
    }

    /// An answer read as giving the text the assistant asked for no value is read once more,
    /// told so: the user's own words are the value even when they are also a record's label.
    /// Asked what a record still needs, what its operation requires is what was asked.
    async fn asked_read_again(
        &mut self,
        input: &ExtractInput<'a>,
        target: &ActTarget,
        output: Extraction,
        mut values: Extracted,
    ) -> (Extraction, Extracted) {
        if self.plan.label != "Answer" {
            return (output, values);
        }
        let owing = match (&self.cx.turn.expectation, target) {
            (Some(Expectation::Obligation { record, .. }), ActTarget::Record { token }) => {
                record == token
            }
            _ => false,
        };
        let asked = |argument: &ArgumentSpec| match self.plan.pending {
            Some(pending) => pending.missing.contains(&argument.name),
            None => owing && argument.required,
        };
        let unread: Vec<String> = input
            .arguments
            .iter()
            .filter(|argument| matches!(argument.shape, ValueShape::Text { .. }) && asked(argument))
            .map(|argument| argument.name.clone())
            .filter(|name| {
                values.not_given.contains(name)
                    && !values.elsewhere.iter().any(|(other, _)| other == name)
                    && !self.carried.contains_key(name)
            })
            .collect();
        if unread.is_empty() {
            return (output, values);
        }
        let note = unread
            .iter()
            .map(|name| {
                format!(
                    "`{name}` is the value the assistant asked for, and this part answers it. The \
                     user's own words are the value even when they are also a record's name or \
                     label: give them, unless the part says it does not know or will not say."
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Ok((again, read)) = self
            .extract(input, "extract.after_asked", Some((&output, &note)))
            .await
        else {
            return (output, values);
        };
        let mut took = false;
        for name in &unread {
            if let Some(given) = read.arguments.get(name) {
                values.arguments.insert(name.clone(), given.clone());
                values.not_given.retain(|other| other != name);
                took = true;
            }
        }
        (if took { again } else { output }, values)
    }

    /// A copied value ending on its part's last word, where the next part begins, is read
    /// again once, told that word: the word joining two parts is no value's.
    async fn own_end(
        &mut self,
        input: &ExtractInput<'a>,
        output: Extraction,
        values: Extracted,
    ) -> (Extraction, Extracted) {
        let last = self.plan.range.last;
        let meets_next = self.cx.units.iter().any(|unit| unit.from == last + 1);
        let ends_the_part = |name: &String| {
            let copied = input.arguments.iter().any(|argument| {
                &argument.name == name && argument.shape == ValueShape::Text { written: false }
            });
            values.arguments.get(name).is_some_and(|argument| {
                argument.excerpt.is_some_and(|excerpt| {
                    excerpt.message == MessageRef::Current && excerpt.words.last == last
                })
            }) && copied
        };
        let Some(name) = values
            .arguments
            .keys()
            .find(|name| ends_the_part(name))
            .cloned()
        else {
            return (output, values);
        };
        if !meets_next {
            return (output, values);
        }
        let span = Span::new(last, last);
        let (word, _) = span.shown();
        let said = self.cx.turn.message.slice(span).unwrap_or_default();
        let note = format!(
            "`{name}` ends at word {word}, «{said}», the last word of this part, and the next \
             part of the message begins at word {}: if that word joins this part to the next \
             one, it is no value's; give the value without it. If it is the value's own word, \
             give the same value.",
            word + 1
        );
        match self
            .extract(input, "extract.at_part_end", Some((&output, &note)))
            .await
        {
            Ok(repaired) => repaired,
            Err(_) => (output, values),
        }
    }

    fn located(&self, target: &ActTarget) -> Located {
        match target {
            ActTarget::Record { token } => Located::Record {
                token: token.clone(),
                label: self
                    .cx
                    .turn
                    .record(token)
                    .map_or_else(|| token.to_string(), |(_, record)| record.label.clone()),
            },
            ActTarget::New { .. } => Located::New,
            ActTarget::SameTurn { act } => Located::SameTurn { act: *act },
            ActTarget::Card => Located::Card,
            ActTarget::NotListed { .. } => Located::NotListed,
            ActTarget::Ambiguous { .. } => Located::Ambiguous,
            _ => Located::Nothing,
        }
    }

    fn shown(&self, argument: &UnderstoodArgument) -> String {
        render::understood(argument, self.cx.turn, |record| self.record_label(record))
    }

    fn verifies(&self) -> bool {
        let mutating = match self.plan.spec {
            Some(spec) => spec.mutability == ActMutability::Mutating,
            None => true,
        };
        match self.cx.settings.verify {
            VerifyPolicy::Off => false,
            VerifyPolicy::All => true,
            VerifyPolicy::Mutating => mutating,
        }
    }

    /// Verifies, repairs the extraction once with the verifier's reason, verifies again.
    async fn verified(
        &mut self,
        target: &mut ActTarget,
        input: Option<&ExtractInput<'a>>,
        extraction: &mut Option<Extraction>,
        extracted: &mut Extracted,
    ) -> Result<ActStatus, NotUnderstoodReason> {
        let verdict = self.verify(target, extracted, "verify").await?;
        if verdict.confirmed() {
            return Ok(ActStatus::Ready);
        }
        let doubted_words: Vec<(String, Option<WordRange>)> = verdict
            .arguments
            .iter()
            .filter(|(_, judged)| **judged == ArgumentVerdict::TooMuch)
            .map(|(name, _)| {
                let words = extracted
                    .arguments
                    .get(name)
                    .and_then(|given| given.excerpt);
                (name.clone(), words.map(|excerpt| excerpt.words))
            })
            .collect();
        let first_reading = extracted.arguments.clone();
        if let (Some(input), Some(previous)) = (input, extraction.as_ref()) {
            let earlier = self
                .cx
                .turn
                .transcript
                .iter()
                .any(|m| m.speaker == Speaker::User);
            let feedback = verdict.feedback_given(|name| {
                earlier
                    && extracted.arguments.get(name).is_some_and(|argument| {
                        argument
                            .excerpt
                            .is_some_and(|excerpt| excerpt.message == MessageRef::Current)
                    })
            });
            if let Ok((output, mut values)) = self
                .extract(input, "extract.after_verify", Some((previous, &feedback)))
                .await
            {
                // A repair pointing into another part found nothing better, and one giving
                // no value the verdict found stated re-read what it was not asked to: the
                // first reading's value stands for the second verification to judge.
                let stood = values.elsewhere.iter().map(|(name, _)| name.clone()).chain(
                    verdict
                        .arguments
                        .iter()
                        .filter(|(name, judged)| {
                            **judged == ArgumentVerdict::Stated
                                && !values.arguments.contains_key(*name)
                        })
                        .map(|(name, _)| name.clone()),
                );
                for name in stood.collect::<Vec<_>>() {
                    if let Some(first) = extracted.arguments.get(&name) {
                        values.arguments.insert(name.clone(), first.clone());
                        values.not_given.retain(|given| *given != name);
                    }
                }
                *extraction = Some(output);
                *extracted = values;
            }
        }
        let status = self.missing(extracted);
        // A repair re-reads values: read the same, or lost, they are still an act most
        // verdicts found nobody asked for. A lone verdict gets its second reading.
        if verdict.overall == Overall::NotRequested
            && self.cx.settings.doubt_votes > 0
            && (extracted.arguments == first_reading || status != ActStatus::Ready)
        {
            return Err(NotUnderstoodReason::NotRequested);
        }
        if status != ActStatus::Ready {
            return Ok(status);
        }
        let mut second = self
            .verify(target, extracted, "verify.after_repair")
            .await?;
        // Told its words were too many, the reading gave the same words: two readings agree.
        for (name, words) in &doubted_words {
            let again = extracted
                .arguments
                .get(name)
                .and_then(|given| given.excerpt);
            if words.is_some()
                && again.map(|excerpt| excerpt.words) == *words
                && let Some(judged) = second.arguments.get_mut(name)
                && *judged == ArgumentVerdict::TooMuch
            {
                *judged = ArgumentVerdict::Stated;
            }
        }
        if second.confirmed() {
            return Ok(ActStatus::Ready);
        }
        match second.overall {
            Overall::NotRequested => return Err(NotUnderstoodReason::NotRequested),
            Overall::WrongRecord => *target = self.retarget(target),
            Overall::Confirmed => {}
        }
        for name in second.at_fault() {
            extracted.arguments.remove(&name);
        }
        let asked = self.to_ask(&second);
        Ok(if asked.is_empty() {
            ActStatus::Ready
        } else {
            ActStatus::NeedsValue {
                arguments: asked,
                reason: None,
            }
        })
    }

    /// Runs the domain's check, and repairs one rejected argument with its explanation.
    async fn checked(
        &mut self,
        act: &mut UnderstoodAct,
        input: Option<&ExtractInput<'a>>,
        extraction: Option<&Extraction>,
    ) {
        let Err(rejection) = self.cx.checker.check(act) else {
            self.cx.steps.step(Step::Checked {
                act: self.plan.id,
                refused: None,
            });
            return;
        };
        let explanation = rejection.explanation.as_ref().map_or_else(
            || rejection.message_key.clone(),
            |text| text.resolve(&self.cx.turn.locale).to_owned(),
        );
        self.cx.steps.step(Step::Checked {
            act: self.plan.id,
            refused: Some((
                argument_of(&rejection).unwrap_or_else(|| "the act".to_owned()),
                explanation.clone(),
            )),
        });
        let Some(argument) = argument_of(&rejection) else {
            return;
        };
        // Which value was refused, so neither the user nor the re-read is left guessing.
        let refused = act
            .arguments
            .get(&argument)
            .map(|given| match &given.value {
                turnframe_core::understanding::ArgumentValue::Json(value) => {
                    render::quoted(&render::value_text(value))
                }
                turnframe_core::understanding::ArgumentValue::Record(record) => {
                    render::quoted(&self.record_label(record))
                }
            });
        let reason = refused.as_ref().map_or_else(
            || explanation.clone(),
            |refused| format!("{refused}: {explanation}"),
        );
        let needs = |act: &mut UnderstoodAct| {
            act.arguments.remove(&argument);
            act.status = ActStatus::NeedsValue {
                arguments: vec![argument.clone()],
                reason: Some(reason.clone()),
            };
        };
        let (Some(input), Some(previous)) = (input, extraction) else {
            return needs(act);
        };
        let feedback = match &refused {
            Some(refused) => format!("{refused} was refused as {argument}: {explanation}"),
            None => format!("The value of {argument} was refused: {explanation}"),
        };
        let Ok((_, values)) = self
            .extract(input, "extract.after_check", Some((previous, &feedback)))
            .await
        else {
            return needs(act);
        };
        let mut repaired = act.clone();
        repaired.arguments.extend(values.arguments.clone());
        if self.missing(&values) != ActStatus::Ready || self.cx.checker.check(&repaired).is_err() {
            return needs(act);
        }
        if self.verifies() {
            let target = repaired.target.clone();
            match self.verify(&target, &values, "verify.after_check").await {
                Ok(verdict) if verdict.confirmed() => {}
                _ => return needs(act),
            }
        }
        *act = repaired;
    }

    fn missing(&self, extracted: &Extracted) -> ActStatus {
        let Some(spec) = self.plan.spec else {
            return ActStatus::Ready;
        };
        let missing: Vec<String> = spec
            .arguments
            .iter()
            .filter(|argument| argument.required && render::model_given(&argument.source))
            .filter(|argument| {
                !extracted.arguments.contains_key(&argument.name)
                    && !self.carried.contains_key(&argument.name)
            })
            .map(|argument| argument.name.clone())
            .collect();
        if missing.is_empty() {
            ActStatus::Ready
        } else {
            ActStatus::NeedsValue {
                arguments: missing,
                reason: None,
            }
        }
    }

    fn act(
        &self,
        target: ActTarget,
        arguments: BTreeMap<String, UnderstoodArgument>,
        status: ActStatus,
    ) -> UnderstoodAct {
        let mut depends_on = Vec::new();
        if let ActTarget::SameTurn { act } = &target {
            depends_on.push(*act);
        }
        for argument in arguments.values() {
            if let turnframe_core::understanding::ArgumentValue::Record(RecordValue::SameTurn {
                act,
            }) = &argument.value
                && !depends_on.contains(act)
            {
                depends_on.push(*act);
            }
        }
        UnderstoodAct {
            id: self.plan.id,
            action: self.plan.action.clone(),
            target,
            arguments,
            words: self.plan.range,
            depends_on,
            status,
        }
    }

    fn candidates(&self) -> Vec<Candidate<'a>> {
        let (plan, workflow) = (self.plan, self.plan.workflow);
        let mut candidates: Vec<Candidate<'a>> = workflow
            .records
            .iter()
            .filter(|record| plan.spec.is_some_and(|spec| record.offers(&spec.key)))
            .map(Candidate::Record)
            .collect();
        candidates.extend(
            self.cx
                .creations
                .iter()
                .filter(|creation| {
                    let earlier = creation.words.from < plan.words.from
                        || (creation.act.unit == plan.id.unit && creation.act.act < plan.id.act);
                    creation.workflow == workflow.key && earlier && creation.act != plan.id
                })
                .map(|creation| Candidate::SameTurn {
                    act: creation.act,
                    words: creation.words,
                }),
        );
        candidates
    }

    async fn target(&mut self) -> Result<ActTarget, NotUnderstoodReason> {
        let workflow = self.plan.workflow.key.clone();
        let spec = match &self.plan.action {
            ActAction::Start { workflow } => {
                return Ok(ActTarget::New {
                    workflow: workflow.clone(),
                });
            }
            ActAction::Apply { .. } => self.plan.spec,
        };
        // A waiting act keeps its record; with none, it creates one or applies to none, as
        // its operation says.
        if let Some(pending) = self.plan.pending {
            let creates = spec.is_some_and(|spec| {
                spec.target_policy == TargetPolicy::NewCaseOnly
                    || (spec.target_policy == TargetPolicy::AllowsNewCase
                        && self.plan.workflow.new_case.contains(&spec.key))
            });
            return Ok(match pending.record.clone() {
                Some(token) => ActTarget::Record { token },
                None if creates => ActTarget::New { workflow },
                None => ActTarget::Nothing,
            });
        }
        let Some(spec) = spec else {
            return Ok(ActTarget::Nothing);
        };
        match spec.target_policy {
            TargetPolicy::NewCaseOnly => return Ok(ActTarget::New { workflow }),
            TargetPolicy::ActiveInteractionOnly => return Ok(ActTarget::Card),
            TargetPolicy::None => return Ok(ActTarget::Nothing),
            _ => {}
        }
        let candidates = self.candidates();
        let allow_new = spec.target_policy == TargetPolicy::AllowsNewCase
            && self.plan.workflow.new_case.contains(&spec.key);
        let allow_not_listed = matches!(
            spec.target_policy,
            TargetPolicy::RequiresExistingCase | TargetPolicy::AllowsNewCase
        );
        match candidates.as_slice() {
            [only] if !allow_new => return Ok(candidate_target(*only)),
            [] if allow_new => return Ok(ActTarget::New { workflow }),
            [] => {
                return Ok(ActTarget::NotListed {
                    workflow,
                    words: None,
                });
            }
            _ => {}
        }
        let input = LocateInput {
            label: self.plan.label,
            words: self.plan.words,
            spec,
            workflow: &self.plan.workflow.key,
            candidates,
            allow_new,
            allow_not_listed,
            note: self.note.clone(),
        };
        let id = self.unit.child(format!("locate{}", self.after));
        let call = TaskCall {
            id: &id,
            parent: Some(&self.parent),
            depth: self.depth.saturating_add(1),
        };
        let outcome = self
            .cx
            .engine
            .run(self.cx.scope, call, &Locate::new(self.cx.turn), &input)
            .await;
        self.advance(id.clone(), &outcome);
        let tokens = |handles: Vec<&str>| -> Vec<TargetToken> {
            handles
                .into_iter()
                .filter_map(|handle| match input.candidate(handle) {
                    Some(Candidate::Record(record)) => Some(record.token.clone()),
                    _ => None,
                })
                .collect()
        };
        match outcome {
            TaskOutcome::Accepted { output, .. } => Ok(match output.record.as_str() {
                NEW => ActTarget::New { workflow },
                BY_NAME => ActTarget::NotListed {
                    workflow,
                    words: output
                        .named
                        .and_then(|span| self.cx.turn.message.range(span).ok()),
                },
                AMBIGUOUS => ActTarget::Ambiguous {
                    candidates: tokens(input.handles().iter().map(|(h, _)| h.as_str()).collect()),
                },
                handle => input
                    .candidate(handle)
                    .map_or(ActTarget::Nothing, |candidate| candidate_target(*candidate)),
            }),
            TaskOutcome::Disagreed { answers, .. } => Ok(ActTarget::Ambiguous {
                candidates: tokens(answers.iter().map(|a| a.record.as_str()).collect()),
            }),
            TaskOutcome::Failed { failure, .. } => Err(failed("locate", failure.code())),
        }
    }

    fn retarget(&self, current: &ActTarget) -> ActTarget {
        let records: Vec<TargetToken> = self
            .candidates()
            .iter()
            .filter_map(|candidate| match candidate {
                Candidate::Record(record) => Some(record.token.clone()),
                Candidate::SameTurn { .. } => None,
            })
            .collect();
        let others = records
            .iter()
            .any(|token| !matches!(current, ActTarget::Record { token: t } if t == token));
        if records.len() > 1 && others {
            ActTarget::Ambiguous {
                candidates: records,
            }
        } else {
            ActTarget::NotListed {
                workflow: self.plan.workflow.key.clone(),
                words: None,
            }
        }
    }

    fn extract_input(&self, target: &ActTarget) -> Option<ExtractInput<'a>> {
        let spec = self.plan.spec?;
        let asked: Vec<&'a ArgumentSpec> = spec
            .arguments
            .iter()
            .filter(|argument| render::model_given(&argument.source))
            .filter(|argument| {
                self.plan
                    .pending
                    .is_none_or(|pending| pending.missing.contains(&argument.name))
            })
            .collect();
        if asked.is_empty() {
            return None;
        }
        let record = match target {
            ActTarget::Record { token } => self
                .cx
                .turn
                .record(token)
                .map_or(RecordContext::Nothing, |(_, record)| {
                    RecordContext::Existing(record)
                }),
            ActTarget::New { .. } => RecordContext::New,
            ActTarget::SameTurn { .. } => RecordContext::SameTurn,
            _ => RecordContext::Nothing,
        };
        let record_choices = asked
            .iter()
            .filter_map(|argument| match &argument.shape {
                ValueShape::Record { workflow } => {
                    Some((argument.name.clone(), self.record_choices(workflow)))
                }
                _ => None,
            })
            .collect();
        Some(ExtractInput {
            label: self.plan.label,
            words: self.plan.words,
            spec,
            workflow: self.plan.workflow,
            record,
            arguments: asked,
            record_choices,
            continues: self.plan.continues,
            others: self.others(),
            kin: self.plan.kin.clone(),
            also: self.plan.also.clone(),
            transcript: self.cx.settings.transcript,
            note: self.note.clone(),
            occurrence: self.plan.occurrence,
            corrected: self.corrected_dates(target),
        })
    }

    /// The words of this message the other parts hold: every unit's, save this act's own
    /// and those it continues.
    fn others(&self) -> Vec<Span> {
        let within = |outer: Span, span: Span| outer.from <= span.from && span.to <= outer.to;
        self.cx
            .units
            .iter()
            .copied()
            .filter(|span| {
                !within(self.plan.words, *span)
                    && !self
                        .plan
                        .continues
                        .is_some_and(|continued| within(continued, *span))
            })
            .collect()
    }

    fn record_choices(&self, workflow: &WorkflowKey) -> Vec<RecordChoice> {
        let turn = self.cx.turn;
        let mut choices: Vec<RecordChoice> = turn
            .workflow(workflow)
            .map(|brief| brief.records.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(position, record)| RecordChoice {
                handle: format!("r{}", position + 1),
                value: RecordValue::Record {
                    token: record.token.clone(),
                },
                label: render::record_line(record, true),
            })
            .collect();
        let created = self
            .cx
            .creations
            .iter()
            .filter(|c| &c.workflow == workflow && c.act != self.plan.id);
        for (position, creation) in created.enumerate() {
            choices.push(RecordChoice {
                handle: format!("s{}", position + 1),
                value: RecordValue::SameTurn { act: creation.act },
                label: created_label(turn, creation),
            });
        }
        choices
    }

    async fn extract(
        &mut self,
        input: &ExtractInput<'a>,
        name: &str,
        feedback: Option<(&Extraction, &str)>,
    ) -> Result<(Extraction, Extracted), NotUnderstoodReason> {
        let id = self.unit.child(format!("{name}{}", self.after));
        let task = Extract::new(self.cx.turn);
        let call = TaskCall {
            id: &id,
            parent: Some(&self.parent),
            depth: self.depth.saturating_add(1),
        };
        let outcome = match feedback {
            None => self.cx.engine.run(self.cx.scope, call, &task, input).await,
            Some((previous, note)) => {
                self.cx.steps.step(Step::Repairing {
                    act: self.plan.id,
                    because: note.lines().next().unwrap_or_default().to_owned(),
                });
                self.cx
                    .engine
                    .run_with_feedback(self.cx.scope, call, &task, input, previous, note)
                    .await
            }
        };
        self.advance(id, &outcome);
        match outcome {
            TaskOutcome::Accepted { output, .. } => {
                let values = convert(self.cx.turn, input, &output)
                    .map_err(|error| failed("extract", error.code.to_owned()))?;
                Ok((output, values))
            }
            TaskOutcome::Disagreed { .. } => Err(NotUnderstoodReason::Unclear),
            TaskOutcome::Failed { failure, .. } => Err(failed("extract", failure.code())),
        }
    }

    async fn verify(
        &mut self,
        target: &ActTarget,
        extracted: &Extracted,
        name: &str,
    ) -> Result<Verdict, NotUnderstoodReason> {
        let turn = self.cx.turn;
        let meaning = match (&self.plan.action, self.plan.spec) {
            (_, Some(spec)) => render::operation_line(spec, turn),
            (ActAction::Start { workflow }, None) => format!("start a new {workflow} record"),
            (ActAction::Apply { operation }, None) => operation.to_string(),
        };
        let record = match target {
            // As extraction saw it: what tells it from the others, and what it holds.
            ActTarget::Record { token } => turn.record(token).map_or_else(
                || token.to_string(),
                |(_, record)| render::record_line(record, false),
            ),
            ActTarget::New { workflow } => format!("a new {workflow} record"),
            ActTarget::SameTurn { .. } => {
                format!("the {} record this message creates", self.plan.workflow.key)
            }
            ActTarget::Card => "the record of the card on screen".to_owned(),
            _ => "still to be chosen".to_owned(),
        };
        let spec = self.plan.spec;
        let labels = extracted
            .arguments
            .keys()
            .map(|name| {
                let label = spec.map_or_else(
                    || name.clone(),
                    |spec| render::argument_label(spec, name, turn),
                );
                let deducible =
                    spec.and_then(|spec| spec.argument_named(name))
                        .is_some_and(|argument| {
                            matches!(
                                argument.source,
                                turnframe_core::operation::ArgumentSource::Inferred
                            )
                        });
                let label = if deducible {
                    format!("{label} (may be deduced)")
                } else {
                    label
                };
                (name.clone(), label)
            })
            .collect();
        let record_labels = extracted
            .arguments
            .iter()
            .filter_map(|(name, argument)| match &argument.value {
                turnframe_core::understanding::ArgumentValue::Record(value) => {
                    Some((name.clone(), self.record_label(value)))
                }
                turnframe_core::understanding::ArgumentValue::Json(_) => None,
            })
            .collect();
        // A value of a closed set is a code: what it means is what the verifier reads.
        let meanings = extracted
            .arguments
            .keys()
            .filter_map(|name| {
                let argument = spec?.argument_named(name)?;
                matches!(argument.shape, ValueShape::Enum { .. })
                    .then(|| Some((name.clone(), argument.description.clone()?)))
                    .flatten()
            })
            .collect();
        let input = VerifyInput {
            label: self.plan.label,
            words: self.plan.words,
            meaning,
            record,
            arguments: &extracted.arguments,
            labels,
            record_labels,
            meanings,
            occurrence: self
                .plan
                .occurrence
                .and_then(|(number, of)| match &self.plan.action {
                    ActAction::Apply { operation } => Some((operation.to_string(), number, of)),
                    ActAction::Start { .. } => None,
                }),
            note: self.note.clone(),
            continues: self.plan.continues,
        };
        let name = format!("{name}{}", self.after);
        let (parent, depth) = (self.parent.clone(), self.depth);
        let mut output = self.judge(&name, &input, extracted).await?;
        // One verdict finding fault is a doubt: the majority of more decides. Each vote is
        // cast beside the first, and the chain goes on from the first.
        let again = self.cx.settings.doubt_votes;
        if !output.confirmed() && again > 0 {
            let (after, reached) = (self.parent.clone(), self.depth);
            let mut confirming = None;
            let mut confirmed = 0;
            for vote in 1..=again {
                (self.parent, self.depth) = (parent.clone(), depth);
                let verdict = self
                    .judge(&format!("{name}.doubt{vote}"), &input, extracted)
                    .await;
                (self.parent, self.depth) = (after.clone(), reached);
                let Ok(verdict) = verdict else {
                    continue;
                };
                if verdict.confirmed() {
                    confirmed += 1;
                    confirming.get_or_insert(verdict);
                }
            }
            if let Some(verdict) = confirming
                && 2 * confirmed > again + 1
            {
                output = verdict;
            }
        }
        self.cx.steps.step(Step::Verified {
            act: self.plan.id,
            confirmed: output.confirmed(),
            reason: output.reason.clone(),
            at_fault: output.at_fault(),
        });
        Ok(output)
    }

    /// One verdict of the verifier, read by the value each argument holds.
    async fn judge(
        &mut self,
        name: &str,
        input: &VerifyInput<'_>,
        extracted: &Extracted,
    ) -> Result<Verdict, NotUnderstoodReason> {
        let id = self.unit.child(name);
        let call = TaskCall {
            id: &id,
            parent: Some(&self.parent),
            depth: self.depth.saturating_add(1),
        };
        let outcome = self
            .cx
            .engine
            .run(self.cx.scope, call, &Verify::new(self.cx.turn), input)
            .await;
        self.advance(id, &outcome);
        match outcome {
            TaskOutcome::Accepted { output, .. } => Ok(self.judged_by_value(output, extracted)),
            TaskOutcome::Disagreed { .. } => Err(NotUnderstoodReason::Unclear),
            TaskOutcome::Failed { failure, .. } => Err(failed("verify", failure.code())),
        }
    }

    fn record_label(&self, value: &RecordValue) -> String {
        let created = match value {
            RecordValue::SameTurn { act } => self.cx.creations.iter().find(|c| &c.act == act),
            _ => None,
        };
        created.map_or_else(
            || record_label(self.cx.turn, value),
            |creation| created_label(self.cx.turn, creation),
        )
    }

    /// Too many words is a doubt about copied text, a record's name included: a value
    /// chosen, counted or computed is that value whatever words it was read from. «Not stated» is what a deduced
    /// value is. A correction's own words are the user's last word, so a value read in
    /// them is not «different» from the request they change. None holds the act up.
    fn judged_by_value(&self, mut verdict: Verdict, extracted: &Extracted) -> Verdict {
        // An answer to the value the assistant asked for was asked for: code knows it.
        if self.plan.pending.is_some() && verdict.overall == Overall::NotRequested {
            verdict.overall = Overall::Confirmed;
        }
        let Some(spec) = self.plan.spec else {
            return verdict;
        };
        for (name, judged) in &mut verdict.arguments {
            let Some(argument) = spec.argument_named(name) else {
                continue;
            };
            let copied = matches!(argument.shape, ValueShape::Text { .. })
                || extracted.arguments.get(name).is_some_and(|given| {
                    matches!(
                        given.value,
                        turnframe_core::understanding::ArgumentValue::Record(
                            RecordValue::Named { .. }
                        )
                    )
                });
            let deduced = matches!(
                argument.source,
                turnframe_core::operation::ArgumentSource::Inferred
            );
            let corrected = self.plan.label == "Correction"
                && extracted
                    .arguments
                    .get(name)
                    .and_then(|given| given.excerpt)
                    .is_some_and(|excerpt| {
                        excerpt.message == turnframe_core::understanding::MessageRef::Current
                            && self.plan.words.from <= excerpt.words.first
                            && excerpt.words.last <= self.plan.words.to
                    });
            let holds_nothing = (!copied && *judged == ArgumentVerdict::TooMuch)
                || (deduced && *judged == ArgumentVerdict::NotStated)
                || (corrected && *judged == ArgumentVerdict::Different);
            if holds_nothing {
                *judged = ArgumentVerdict::Stated;
            }
        }
        verdict
    }
}

fn candidate_target(candidate: Candidate<'_>) -> ActTarget {
    match candidate {
        Candidate::Record(record) => ActTarget::Record {
            token: record.token.clone(),
        },
        Candidate::SameTurn { act, .. } => ActTarget::SameTurn { act },
    }
}
