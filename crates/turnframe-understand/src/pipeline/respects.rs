//! The keep-unchanged constraint: each act that changes a record is judged against the words
//! asking to keep something as it is. One judged to change it runs nothing; the rest of the
//! turn runs. A judgment that cannot be had holds every act on the record, as a failed task
//! does.

use futures::future::join_all;
use turnframe_core::plan::ActMutability;
use turnframe_core::understanding::{
    ActAction, ConstraintKind, NotUnderstoodReason, UnderstoodAct, UnitId,
};
use turnframe_tasks::{TaskCall, TaskId, TaskOutcome};

use super::chain::Chained;
use super::cross_check::shown_act;
use super::units::Seg;
use super::{Context, Understander};
use crate::tasks::respects::{Respects, RespectsInput};
use crate::tasks::segment::SegmentedUnit;
use crate::words::Span;

impl Understander {
    /// Holds the acts that would change what a keep-unchanged constraint keeps.
    pub(super) async fn respected(&self, cx: &Context<'_>, units: &[Seg], chained: &mut [Chained]) {
        let kept: Vec<(UnitId, Span)> = units
            .iter()
            .filter(|unit| {
                matches!(
                    unit.unit,
                    SegmentedUnit::Constraint {
                        constraint: ConstraintKind::KeepUnchanged,
                        ..
                    }
                )
            })
            .map(|unit| (unit.id, unit.span))
            .collect();
        if kept.is_empty() {
            return;
        }
        let kept = &kept;
        let judged = join_all(chained.iter().map(|outcome| async move {
            match outcome {
                Chained::Act(act) if changes_a_record(cx, act) => judged(cx, act, kept).await,
                _ => None,
            }
        }))
        .await;
        for (outcome, held) in chained.iter_mut().zip(judged) {
            if let Some(held) = held {
                *outcome = held;
            }
        }
    }
}

fn changes_a_record(cx: &Context<'_>, act: &UnderstoodAct) -> bool {
    match &act.action {
        ActAction::Apply { operation } => cx
            .turn
            .operation(operation)
            .is_none_or(|(_, spec)| spec.mutability == ActMutability::Mutating),
        _ => false,
    }
}

/// The act held, or `None` when it keeps to every constraint. The first constraint's call is
/// `u2/respects`, a further one's `u2/respects.u5`.
async fn judged(cx: &Context<'_>, act: &UnderstoodAct, kept: &[(UnitId, Span)]) -> Option<Chained> {
    let task = Respects::new(cx.turn);
    let shown = shown_act(cx.turn, act);
    let base = TaskId::new(if act.id.act == 1 {
        act.id.unit.to_string()
    } else {
        act.id.to_string()
    });
    for (position, (constraint, span)) in kept.iter().enumerate() {
        let id = if position == 0 {
            base.child("respects")
        } else {
            base.child(format!("respects.{constraint}"))
        };
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 1,
        };
        let input = RespectsInput {
            constraint: *span,
            act: shown.line.clone(),
        };
        let reason = match cx.engine.run(cx.scope, call, &task, &input).await {
            TaskOutcome::Accepted { output, .. } if !output.changes => continue,
            TaskOutcome::Accepted { .. } => {
                return Some(Chained::NotUnderstood {
                    unit: act.id.unit,
                    words: act.words,
                    reason: NotUnderstoodReason::KeptUnchanged {
                        constraint: *constraint,
                    },
                    aimed: None,
                });
            }
            TaskOutcome::Disagreed { .. } => NotUnderstoodReason::Unclear,
            TaskOutcome::Failed { failure, .. } => NotUnderstoodReason::TaskFailed {
                task: "respects".to_owned(),
                code: failure.code(),
            },
        };
        return Some(Chained::NotUnderstood {
            unit: act.id.unit,
            words: act.words,
            reason,
            aimed: Some(act.target.clone()),
        });
    }
    None
}
