//! An answer read as the act that asked, giving none of what it asked, is routed once more,
//! told so: it may take up something the last reply offered. A second route that finds
//! only the same operation, or none, leaves the first reading, which asks again. An answer
//! read only as acts the user did not ask for is read as the act that asked.

use futures::future::join_all;
use turnframe_core::understanding::{ActAction, MessageRef, UnderstoodAct, UnitKind};

use super::chain::{self, Chained};
use super::{
    Context, Planning, Routed, Seg, Understander, creations, every_reading_refused, plan,
    report_routes,
};
use crate::input::Expectation;
use crate::render;

impl Understander {
    pub(super) async fn routed_again<'a>(
        &self,
        cx: &mut Context<'a>,
        units: &[Seg],
        planning: &mut Planning<'a>,
        chained: &mut Vec<Chained>,
    ) {
        for unit in units {
            if let Some(asked) = answered_elsewhere(cx, unit, chained) {
                self.read_as_asked(cx, unit, asked, planning, chained).await;
                continue;
            }
            let Some(note) = gave_nothing_asked(cx, unit, planning, chained) else {
                continue;
            };
            let others = units
                .iter()
                .filter(|other| other.id != unit.id && super::routable(other))
                .map(|other| other.span)
                .collect();
            let routed = self
                .route(cx.scope, cx.turn, unit, others, Some(note))
                .await;
            let asking = planning
                .planned
                .iter()
                .find(|planned| planned.unit == unit.id)
                .map(|planned| planned.action.clone());
            let elsewhere = routed.iter().any(|route| {
                matches!(route, Routed::Act { action, .. } if Some(action) != asking.as_ref())
            });
            if !elsewhere {
                continue;
            }
            self.read_as(cx, unit, routed, planning, chained).await;
        }
    }

    /// Reads `unit` again as the act the last reply asked for, kept in place of what it was
    /// read as only when its own words give every value asked.
    async fn read_as_asked<'a>(
        &self,
        cx: &mut Context<'a>,
        unit: &Seg,
        asked: Routed,
        planning: &mut Planning<'a>,
        chained: &mut Vec<Chained>,
    ) {
        let Some(Expectation::Values(pending)) = &cx.turn.expectation else {
            return;
        };
        let routed = vec![asked];
        let routes = std::iter::once((unit.id, routed.clone())).collect();
        let again = plan(cx.turn, std::slice::from_ref(unit), &routes);
        let read = join_all(again.planned.iter().map(|p| chain::run_rerouted(cx, p))).await;
        let given = !read.is_empty()
            && read.iter().all(|outcome| {
                matches!(outcome, Chained::Act(act) if in_own_words(act, unit, &pending.missing))
            });
        if given {
            report_routes(cx.steps, unit.id, &routed);
            replace(cx, unit, again, planning, chained);
            chained.extend(read);
        }
    }

    /// Reads `unit` again as `routed`, in place of what it was read as.
    async fn read_as<'a>(
        &self,
        cx: &mut Context<'a>,
        unit: &Seg,
        routed: Vec<Routed>,
        planning: &mut Planning<'a>,
        chained: &mut Vec<Chained>,
    ) {
        report_routes(cx.steps, unit.id, &routed);
        let routes = std::iter::once((unit.id, routed)).collect();
        let again = plan(cx.turn, std::slice::from_ref(unit), &routes);
        replace(cx, unit, again, planning, chained);
        let fresh: Vec<_> = planning
            .planned
            .iter()
            .filter(|planned| planned.unit == unit.id)
            .collect();
        let read = join_all(fresh.into_iter().map(|p| chain::run_rerouted(cx, p))).await;
        chained.extend(read);
    }
}

/// Puts `again` in place of how `unit` was planned and read.
fn replace<'a>(
    cx: &mut Context<'a>,
    unit: &Seg,
    again: Planning<'a>,
    planning: &mut Planning<'a>,
    chained: &mut Vec<Chained>,
) {
    planning.planned.retain(|planned| planned.unit != unit.id);
    chained.retain(|outcome| match outcome {
        Chained::Act(act) => act.id.unit != unit.id,
        Chained::NotUnderstood { unit: at, .. } => *at != unit.id,
        Chained::Nothing => true,
    });
    planning.planned.extend(again.planned);
    planning.not_understood.extend(again.not_understood);
    cx.creations = creations(&planning.planned);
}

/// Whether every value `act` was asked for is in `unit`'s own words.
fn in_own_words(act: &UnderstoodAct, unit: &Seg, missing: &[String]) -> bool {
    missing.iter().all(|name| {
        let excerpt = act
            .arguments
            .get(name)
            .and_then(|argument| argument.excerpt.as_ref());
        excerpt.is_some_and(|excerpt| {
            excerpt.message == MessageRef::Current
                && excerpt.words.first >= unit.range.first
                && excerpt.words.last <= unit.range.last
        })
    })
}

/// The act the last reply asked for, as `unit`'s route, when `unit` gives a value and every
/// reading of it was an act the user did not ask for.
fn answered_elsewhere(cx: &Context<'_>, unit: &Seg, chained: &[Chained]) -> Option<Routed> {
    let Some(Expectation::Values(pending)) = &cx.turn.expectation else {
        return None;
    };
    if unit.kind() != UnitKind::ProvidesValue || !every_reading_refused(chained, unit.id) {
        return None;
    }
    let (workflow, _) = cx.turn.operation(&pending.operation)?;
    Some(Routed::Act {
        action: ActAction::Apply {
            operation: pending.operation.clone(),
        },
        workflow: workflow.key.clone(),
        task: unit.task().child("asked"),
        depth: unit.depth,
        taken: None,
    })
}

/// The note for routing `unit` again, when its one act is the act that asked and each
/// value it asked for is still only what the asking carried.
fn gave_nothing_asked(
    cx: &Context<'_>,
    unit: &Seg,
    planning: &Planning<'_>,
    chained: &[Chained],
) -> Option<String> {
    let [planned] = planning
        .planned
        .iter()
        .filter(|planned| planned.unit == unit.id)
        .collect::<Vec<_>>()[..]
    else {
        return None;
    };
    let pending = planned.pending?;
    let act = chained.iter().find_map(|outcome| match outcome {
        Chained::Act(act) if act.id == planned.id => Some(act),
        _ => None,
    })?;
    let nothing = pending
        .missing
        .iter()
        .all(|name| act.arguments.get(name) == pending.given.get(name));
    if !nothing || !matches!(planned.action, ActAction::Apply { .. }) {
        return None;
    }
    let (_, spec) = cx.turn.operation(&pending.operation)?;
    let labels: Vec<String> = pending
        .missing
        .iter()
        .map(|name| render::argument_label(spec, name, cx.turn))
        .collect();
    Some(format!(
        "These words give no {}, so they do not answer with one: choose what else they \
         ask for, perhaps something the last assistant message offered.",
        labels.join(" or ")
    ))
}
