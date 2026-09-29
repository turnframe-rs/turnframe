//! The whole-turn check: shown the message and what was understood, it names what is
//! missing or wrong. Each finding sends one step of one act back, once. The check advises
//! and the verifier decides: a doubt raised again after that holds nothing.

use std::collections::{BTreeMap, BTreeSet};

use futures::future::join_all;
use turnframe_core::understanding::{
    ActAction, ActStatus, ActTarget, MessageRef, NotUnderstood, NotUnderstoodReason, UnderstoodAct,
    UnderstoodQuestion, UnitId, UnitKind, WordRange,
};
use turnframe_tasks::{TaskCall, TaskId, TaskOutcome};

use super::chain::{self, Chained, Planned, Revisit};
use super::units::{self, Seg};
use super::{Context, Understander, plan};
use crate::input::UnderstandingInput;
use crate::progress::Step;
use crate::render;
use crate::tasks::cross_check::{CrossCheck, CrossCheckInput, Finding, ShownAct};
use crate::words::Span;

/// What the pipeline holds after the chains, which the check reads and repairs.
pub(super) struct Reading<'r, 'a> {
    pub units: &'r mut Vec<Seg>,
    pub planned: &'r mut Vec<Planned<'a>>,
    pub chained: &'r mut Vec<Chained>,
    pub not_understood: &'r mut Vec<NotUnderstood>,
    pub questions: &'r mut BTreeMap<UnitId, UnderstoodQuestion>,
}

impl Understander {
    /// Runs the rounds `cx.settings` asks for. A round that cannot run ends the check.
    pub(super) async fn cross_checked<'a>(&self, cx: &Context<'a>, mut reading: Reading<'_, 'a>) {
        let rounds = cx.settings.cross_check_rounds;
        let mut answered: BTreeSet<(&'static str, String)> = BTreeSet::new();
        for round in 1..=rounds {
            let input = shown(cx.turn, &reading);
            if input.acts.is_empty() {
                return;
            }
            let id = if round == 1 {
                TaskId::new("turn/cross_check")
            } else {
                TaskId::new(format!("turn/cross_check.round{round}"))
            };
            let call = TaskCall {
                id: &id,
                parent: None,
                depth: 1,
            };
            let outcome = cx
                .engine
                .run(cx.scope, call, &CrossCheck::new(cx.turn), &input)
                .await;
            let findings = match outcome {
                TaskOutcome::Accepted { output, .. } => output.findings,
                TaskOutcome::Disagreed { .. } => {
                    cx.steps.step(Step::CrossCheckSkipped {
                        round,
                        code: "vote_disagreement".to_owned(),
                    });
                    return;
                }
                TaskOutcome::Failed { failure, .. } => {
                    cx.steps.step(Step::CrossCheckSkipped {
                        round,
                        code: failure.code(),
                    });
                    return;
                }
            };
            cx.steps.step(Step::CrossChecked {
                round,
                findings: findings.len(),
            });
            if findings.is_empty() {
                return;
            }
            for finding in &findings {
                if answered.insert(doubt(finding)) {
                    self.repair(cx, &mut reading, finding, &id).await;
                }
            }
        }
    }

    async fn repair<'a>(
        &self,
        cx: &Context<'a>,
        reading: &mut Reading<'_, 'a>,
        finding: &Finding,
        parent: &TaskId,
    ) {
        let revisit = match finding {
            Finding::Missing { words } => {
                let Some(seg) = units::found_by_check(cx.turn, reading.units, *words, parent)
                else {
                    return;
                };
                let (routes, questions, asked) = self
                    .routes_and_frames(cx.scope, cx.turn, std::slice::from_ref(&seg), cx.steps)
                    .await;
                let seg = if asked.contains(&seg.id) {
                    seg.as_request()
                } else {
                    seg
                };
                reading.units.push(seg.clone());
                let found = std::slice::from_ref(&seg);
                reading.questions.extend(questions);
                let planning = plan(cx.turn, found, &routes);
                reading.not_understood.extend(planning.not_understood);
                let read = join_all(planning.planned.iter().map(|p| chain::run(cx, p))).await;
                reading.chained.extend(read);
                reading.planned.extend(planning.planned);
                return;
            }
            Finding::WrongValue { .. } => Revisit::Value {
                note: note(cx.turn, finding),
            },
            Finding::WrongRecord { .. } => Revisit::Record {
                note: note(cx.turn, finding),
            },
            Finding::NotAsked { .. } => Revisit::Asked,
        };
        let Some((index, act)) = named(reading.chained, finding) else {
            return;
        };
        // An act this message completed for the one waiting has no plan to read again.
        let Some(planned) = reading.planned.iter().find(|p| p.id == act.id) else {
            return;
        };
        let read = chain::revisit(cx, planned, &act, &revisit).await;
        let unpaid = matches!(
            &read,
            Chained::NotUnderstood {
                reason: NotUnderstoodReason::TaskFailed { code, .. },
                ..
            } if code.starts_with("budget")
        );
        // Read again to nothing, a value the verifier confirmed stays: the check only doubted.
        let emptied = matches!((finding, &read), (Finding::WrongValue { argument, .. }, Chained::Act(again))
            if act.arguments.contains_key(argument) && !again.arguments.contains_key(argument));
        reading.chained[index] = if unpaid {
            held(act, finding)
        } else if emptied {
            Chained::Act(act)
        } else {
            read
        };
    }
}

/// What a finding doubts, whatever words it points at: one doubt sends its act back once.
fn doubt(finding: &Finding) -> (&'static str, String) {
    match finding {
        Finding::Missing { words } => ("missing", format!("{}-{}", words.from, words.to)),
        Finding::WrongValue { act, argument, .. } => ("wrong_value", format!("{act} {argument}")),
        Finding::WrongRecord { act, .. } => ("wrong_record", act.clone()),
        Finding::NotAsked { act } => ("not_asked", act.clone()),
    }
}

/// A doubt the budget cannot pay to read again: a value is asked for; a record or request
/// runs nothing.
fn held(mut act: UnderstoodAct, finding: &Finding) -> Chained {
    if let Finding::WrongValue { argument, .. } = finding {
        act.arguments.remove(argument);
        act.status = ActStatus::NeedsValue {
            arguments: vec![argument.clone()],
            reason: None,
        };
        return Chained::Act(act);
    }
    Chained::NotUnderstood {
        unit: act.id.unit,
        words: act.words,
        reason: NotUnderstoodReason::Unclear,
        aimed: Some(act.target),
    }
}

/// The act a finding names, and where it sits.
fn named(chained: &[Chained], finding: &Finding) -> Option<(usize, UnderstoodAct)> {
    let name = match finding {
        Finding::WrongValue { act, .. }
        | Finding::WrongRecord { act, .. }
        | Finding::NotAsked { act } => act,
        Finding::Missing { .. } => return None,
    };
    chained
        .iter()
        .enumerate()
        .find_map(|(index, outcome)| match outcome {
            Chained::Act(act) if &act.id.to_string() == name => Some((index, act.clone())),
            _ => None,
        })
}

/// What the task reading an act again is told, written by code from the finding.
fn note(turn: &UnderstandingInput, finding: &Finding) -> String {
    let said = |span: &Span| turn.message.slice(*span).unwrap_or_default().to_owned();
    match finding {
        Finding::WrongValue {
            argument, words, ..
        } => format!(
            "it doubts the value of {argument}, perhaps in «{}»; read it again from the words, \
             and give not_given if they give none",
            said(words)
        ),
        Finding::WrongRecord { words, .. } => {
            format!("the record meant is named in «{}»", said(words))
        }
        Finding::Missing { words } => format!("«{}» asks for something", said(words)),
        // Never shown: the verifier judges a doubted request afresh.
        Finding::NotAsked { .. } => String::new(),
    }
}

fn words_of(turn: &UnderstandingInput, range: WordRange) -> String {
    turn.message
        .slice(Span::new(range.first, range.last))
        .unwrap_or_default()
        .to_owned()
}

/// The understanding as the check is shown it.
fn shown(turn: &UnderstandingInput, reading: &Reading<'_, '_>) -> CrossCheckInput {
    let mut input = CrossCheckInput::default();
    for outcome in reading.chained.iter() {
        match outcome {
            Chained::Act(act) => {
                input.acts.push(shown_act(turn, act));
                // An act's own words are read: found again, they would be the same act twice.
                input.held.push(Span::new(act.words.first, act.words.last));
                for argument in act.arguments.values() {
                    if let Some(excerpt) = &argument.excerpt
                        && excerpt.message == MessageRef::Current
                    {
                        input
                            .held
                            .push(Span::new(excerpt.words.first, excerpt.words.last));
                    }
                }
            }
            Chained::NotUnderstood { words, .. } => input.unread.push(words_of(turn, *words)),
            Chained::Nothing => {}
        }
    }
    for item in reading.not_understood.iter() {
        input.unread.push(words_of(turn, item.words));
    }
    for question in reading.questions.values() {
        input.questions.push(words_of(turn, question.words));
    }
    for unit in reading.units.iter() {
        match unit.kind() {
            UnitKind::Constraint => {
                input.constraints.push(words_of(turn, unit.range));
                input.held.push(unit.span);
            }
            UnitKind::Question | UnitKind::Chitchat | UnitKind::Dispute => {
                input.held.push(unit.span);
            }
            _ => {}
        }
    }
    input
}

pub(super) fn shown_act(turn: &UnderstandingInput, act: &UnderstoodAct) -> ShownAct {
    let (operation, arguments) = match &act.action {
        ActAction::Apply { operation } => (
            operation.to_string(),
            turn.operation(operation)
                .map(|(_, spec)| {
                    spec.arguments
                        .iter()
                        .filter(|argument| render::model_given(&argument.source))
                        .map(|argument| argument.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
        ),
        ActAction::Start { workflow } => (format!("start a new {workflow}"), Vec::new()),
    };
    let record = match &act.target {
        ActTarget::Record { token } => turn
            .record(token)
            .map_or_else(|| token.to_string(), |(_, record)| record.label.clone()),
        ActTarget::New { workflow } => format!("a new {workflow} record"),
        ActTarget::SameTurn { act } => format!("the record {act} creates"),
        ActTarget::Card => "the record of the card on screen".to_owned(),
        ActTarget::NotListed { workflow, .. } => format!("a {workflow} record named, not listed"),
        _ => "no record yet".to_owned(),
    };
    let values: Vec<String> = act
        .arguments
        .iter()
        .map(|(name, argument)| {
            let value = render::understood(argument, turn, |v| chain::record_label(turn, v));
            format!("{name} {value}")
        })
        .collect();
    let mut line = format!("{} {operation} on {record}", act.id);
    if !values.is_empty() {
        line.push_str(&format!(": {}", values.join("; ")));
    }
    if let ActStatus::NeedsValue {
        arguments: asked, ..
    } = &act.status
    {
        line.push_str(&format!(" (still asks for {})", asked.join(", ")));
    }
    ShownAct {
        id: act.id.to_string(),
        line,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doubted_value_is_told_as_a_doubt_that_may_find_none() {
        let turn =
            UnderstandingInput::new("register the new traveler", "en-GB", chrono::NaiveDate::MIN);
        let finding = Finding::WrongValue {
            act: "u1.a1".to_owned(),
            argument: "full_name".to_owned(),
            words: Span::new(2, 3),
        };
        assert_eq!(
            note(&turn, &finding),
            "it doubts the value of full_name, perhaps in «new traveler»; read it again from \
             the words, and give not_given if they give none"
        );
    }
}
