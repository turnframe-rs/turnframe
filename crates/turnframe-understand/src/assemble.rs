//! Assembly: the task outputs, in message order, as one [`Understanding`].
//!
//! It is a function of the outputs alone. A unit not understood holds every act aimed
//! at the same record, and an act whose same-turn prerequisite is gone is held too.

use std::collections::{BTreeMap, BTreeSet};

use turnframe_core::ids::OptionId;
use turnframe_core::understanding::{
    ActId, ActStatus, ActTarget, CardAnswer, Dispute, MessageRef, NotUnderstood, Superseded,
    TurnConstraint, Understanding, UnderstoodAct, UnderstoodQuestion, Unit, UnitId,
};

use crate::pipeline::{Chained, Seg};
use crate::tasks::segment::{SegmentedUnit, UNKNOWN};

pub(crate) fn assemble(
    units: &[Seg],
    superseded: Vec<Superseded>,
    mut not_understood: Vec<NotUnderstood>,
    chained: Vec<Chained>,
    questions: BTreeMap<UnitId, UnderstoodQuestion>,
) -> Understanding {
    let mut acts = Vec::new();
    let mut aimed: Vec<(UnitId, ActTarget)> = Vec::new();
    for outcome in chained {
        match outcome {
            Chained::Act(act) => acts.push(act),
            Chained::NotUnderstood {
                unit,
                words,
                reason,
                aimed: target,
            } => {
                not_understood.push(NotUnderstood {
                    unit,
                    words,
                    reason,
                });
                if let Some(target) = target {
                    aimed.push((unit, target));
                }
            }
            Chained::Nothing => {}
        }
    }
    let present: BTreeSet<ActId> = acts.iter().map(|act| act.id).collect();
    for act in &mut acts {
        if let Some(gone) = act.depends_on.iter().find(|dep| !present.contains(dep)) {
            act.status = ActStatus::Held { because: gone.unit };
        }
        let held_by = aimed.iter().find(|(_, target)| {
            matches!(
                target,
                ActTarget::Record { .. } | ActTarget::SameTurn { .. }
            ) && *target == act.target
        });
        if let Some((unit, _)) = held_by {
            act.status = ActStatus::Held { because: *unit };
        }
    }
    // Within a part, acts stand where their values stand, unless one of them waits on an
    // act or has no value placed in this message: then they keep routing's order. A value
    // pointed at the whole part says nothing of where it stands.
    let said = |act: &UnderstoodAct| {
        act.arguments
            .values()
            .filter_map(|given| given.excerpt.as_ref())
            .filter(|excerpt| {
                excerpt.message == MessageRef::Current
                    && !(excerpt.words.first <= act.words.first
                        && act.words.last <= excerpt.words.last)
            })
            .map(|excerpt| excerpt.words.first)
            .min()
    };
    let routing_order: BTreeSet<UnitId> = acts
        .iter()
        .filter(|act| !act.depends_on.is_empty() || said(act).is_none())
        .map(|act| act.id.unit)
        .collect();
    acts.sort_by_key(|act| {
        let place = (!routing_order.contains(&act.id.unit))
            .then(|| said(act))
            .flatten();
        (act.words.first, place, act.id)
    });
    not_understood.sort_by_key(|item| (item.words.first, item.unit));

    let mut understanding = Understanding {
        acts,
        superseded,
        not_understood,
        ..Understanding::default()
    };
    let mut ordered: Vec<&Seg> = units.iter().collect();
    ordered.sort_by_key(|unit| (unit.span, unit.id));
    for unit in ordered {
        understanding.units.push(Unit {
            id: unit.id,
            kind: unit.kind(),
            words: unit.range,
            workflow: unit.workflow.clone(),
            found_by: unit.found_by,
        });
        match &unit.unit {
            SegmentedUnit::Constraint { constraint, .. } => {
                understanding.constraints.push(TurnConstraint {
                    unit: unit.id,
                    kind: *constraint,
                    words: unit.range,
                });
            }
            SegmentedUnit::CardAnswer { option, .. } if understanding.card_answer.is_none() => {
                understanding.card_answer = Some(CardAnswer {
                    unit: unit.id,
                    option: OptionId::from(option.as_str()),
                    words: unit.range,
                });
            }
            SegmentedUnit::Dispute { receipt, .. } => {
                understanding.disputes.push(Dispute {
                    unit: unit.id,
                    words: unit.range,
                    receipt: (receipt != UNKNOWN).then(|| receipt.clone()),
                });
            }
            SegmentedUnit::Question { .. } => {
                if let Some(question) = questions.get(&unit.id) {
                    understanding.questions.push(question.clone());
                }
            }
            _ => {}
        }
    }
    understanding
}
