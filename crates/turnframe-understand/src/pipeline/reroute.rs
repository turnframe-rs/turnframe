//! An answer read as the act that asked, giving none of what it asked, is routed once more,
//! told so: it may take up something the last reply offered. A second route that finds
//! only the same operation, or none, leaves the first reading, which asks again.

use futures::future::join_all;
use turnframe_core::understanding::ActAction;

use super::chain::{self, Chained};
use super::{Context, Planning, Routed, Seg, Understander, creations, plan, report_routes};
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
            report_routes(cx.steps, unit.id, &routed);
            let routes = std::iter::once((unit.id, routed)).collect();
            let again = plan(cx.turn, std::slice::from_ref(unit), &routes);
            planning.planned.retain(|planned| planned.unit != unit.id);
            chained.retain(|outcome| match outcome {
                Chained::Act(act) => act.id.unit != unit.id,
                Chained::NotUnderstood { unit: at, .. } => *at != unit.id,
                Chained::Nothing => true,
            });
            planning.planned.extend(again.planned);
            planning.not_understood.extend(again.not_understood);
            cx.creations = creations(&planning.planned);
            let fresh: Vec<_> = planning
                .planned
                .iter()
                .filter(|planned| planned.unit == unit.id)
                .collect();
            let read = join_all(fresh.into_iter().map(|p| chain::run_rerouted(cx, p))).await;
            chained.extend(read);
        }
    }
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
