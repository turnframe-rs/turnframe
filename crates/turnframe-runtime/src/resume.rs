//! What a card remembers, so an answer continues the work it interrupted
//! (spec §13.3, §15.3).
//!
//! A card raised because an act could not finish carries that act as a [`DeferredAct`]
//! in its payload metadata, under [`DEFERRED_ACT_KEY`], covered by the payload hash.
//! When the card is answered, [`resume`] turns the stored option into the act to run,
//! and the runtime feeds it through the normal reducer, policy and execution path.
//! Nothing here executes, and nothing skips a confirmation.
//!
//! | Stored action | What resuming does |
//! | --- | --- |
//! | `SelectTarget` | rebinds the act to the case the user picked, and runs it |
//! | `ResolveClarification`, `Custom` | runs the act against the case it was aimed at |
//! | `Dismiss`, `DeclineCommands`, `DeclineAndRecord` | [`Resumption::Declined`] |
//! | `ConfirmCommands`, `ApplyOperation` | nothing: both have their own paths |
//!
//! A confirmation card may also carry [`DependentAct`]s: acts of the same message that
//! needed what the confirmed commands make. [`dependents`] returns them for the turn
//! that answers the card, after those commands have committed.

use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseRef;
use turnframe_core::interaction::{AcceptedResponse, InteractionPayload, StoredInteractionAction};
use turnframe_core::target::TargetTokenMap;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, RecordValue, UnderstoodAct, UnitId,
    WordRange,
};

/// Key the deferred act is stored under in a card's payload metadata.
pub const DEFERRED_ACT_KEY: &str = "turnframe.deferred_act";

/// The unit an act that came from a card belongs to: `u0`.
pub const CARD_UNIT: UnitId = UnitId(0);

/// The identifier of the act a card answer puts in the plan: `u0.a1`.
#[must_use]
pub const fn card_act_id() -> ActId {
    ActId::new(CARD_UNIT, 1)
}

/// The act a card is guarding, persisted with the card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredAct {
    /// The act as it was understood. Its target is replaced when it is resumed.
    pub act: UnderstoodAct,
    /// The case the act was aimed at, when the turn already knew it; `None` for a
    /// selection card, whose answer supplies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_ref: Option<CaseRef>,
}

impl DeferredAct {
    /// An act whose target the answer will supply.
    #[must_use]
    pub const fn unbound(act: UnderstoodAct) -> Self {
        Self {
            act,
            case_ref: None,
        }
    }

    /// An act already aimed at a case, waiting only for a yes.
    #[must_use]
    pub const fn on(act: UnderstoodAct, case_ref: CaseRef) -> Self {
        Self {
            act,
            case_ref: Some(case_ref),
        }
    }

    /// Writes the deferred act into a card payload. Metadata that is neither absent
    /// nor a JSON object is the application's own, and is left alone.
    #[must_use]
    pub fn attach_to(&self, mut payload: InteractionPayload) -> InteractionPayload {
        let Ok(value) = serde_json::to_value(self) else {
            return payload;
        };
        match &mut payload.metadata {
            serde_json::Value::Null => {
                let mut map = serde_json::Map::new();
                map.insert(DEFERRED_ACT_KEY.to_owned(), value);
                payload.metadata = serde_json::Value::Object(map);
            }
            serde_json::Value::Object(map) => {
                map.insert(DEFERRED_ACT_KEY.to_owned(), value);
            }
            _ => {
                tracing::warn!(
                    target: "turnframe.resume",
                    "card metadata is not an object; the card carries no deferred act"
                );
            }
        }
        payload
    }

    /// Reads the deferred act back, when the card carries one.
    #[must_use]
    pub fn of(payload: &InteractionPayload) -> Option<Self> {
        let value = payload.metadata.get(DEFERRED_ACT_KEY)?;
        serde_json::from_value(value.clone())
            .inspect_err(|_| {
                tracing::warn!(
                    target: "turnframe.resume",
                    "card metadata carries an unreadable deferred act; it is ignored"
                );
            })
            .ok()
    }
}

/// Key the acts waiting on a confirmation are stored under in its card's metadata.
pub const DEPENDENT_ACTS_KEY: &str = "turnframe.dependent_acts";

/// A case an act of an earlier turn opened, which a dependent act refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MadeCase {
    /// The act that opened it.
    pub act: ActId,
    /// The case.
    pub case_ref: CaseRef,
}

/// An act that waits for a confirmation card: once the card's commands commit, it
/// runs under its own origin and policy (spec §6.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependentAct {
    /// The act as it was understood.
    pub act: UnderstoodAct,
    /// The cases its prerequisites open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made: Vec<MadeCase>,
}

impl DependentAct {
    /// Appends this act to the card's dependents.
    #[must_use]
    pub fn attach_to(self, mut payload: InteractionPayload) -> InteractionPayload {
        let mut dependents = Self::of(&payload);
        dependents.push(self);
        let Ok(value) = serde_json::to_value(dependents) else {
            return payload;
        };
        match &mut payload.metadata {
            serde_json::Value::Null => {
                let mut map = serde_json::Map::new();
                map.insert(DEPENDENT_ACTS_KEY.to_owned(), value);
                payload.metadata = serde_json::Value::Object(map);
            }
            serde_json::Value::Object(map) => {
                map.insert(DEPENDENT_ACTS_KEY.to_owned(), value);
            }
            _ => tracing::warn!(
                target: "turnframe.resume",
                "card metadata is not an object; the card carries no dependent act"
            ),
        }
        payload
    }

    /// The dependents a card carries, in the order they were understood.
    #[must_use]
    pub fn of(payload: &InteractionPayload) -> Vec<Self> {
        payload
            .metadata
            .get(DEPENDENT_ACTS_KEY)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }
}

/// The acts a confirmed card lets run, numbered after the card's own act.
///
/// A reference to a case a prerequisite opened becomes that record's token; a
/// reference to another dependent keeps pointing at it under its new number.
#[must_use]
pub fn dependents(payload: &InteractionPayload, tokens: &TargetTokenMap) -> Vec<UnderstoodAct> {
    let waiting = DependentAct::of(payload);
    let renumbered: std::collections::BTreeMap<ActId, ActId> = waiting
        .iter()
        .zip(2..)
        .map(|(dependent, position)| (dependent.act.id, ActId::new(CARD_UNIT, position)))
        .collect();
    waiting
        .into_iter()
        .map(|dependent| {
            let made = |earlier: &ActId| {
                let case_ref = &dependent.made.iter().find(|m| m.act == *earlier)?.case_ref;
                tokens.token_for(&case_ref.key()).cloned()
            };
            let mut act = dependent.act.clone();
            act.id = renumbered[&dependent.act.id];
            if let ActTarget::SameTurn { act: earlier } = &act.target {
                act.target = match (renumbered.get(earlier), made(earlier)) {
                    (Some(renamed), _) => ActTarget::SameTurn { act: *renamed },
                    (None, Some(token)) => ActTarget::Record { token },
                    (None, None) => act.target.clone(),
                };
            }
            for argument in act.arguments.values_mut() {
                let ArgumentValue::Record(RecordValue::SameTurn { act: earlier }) = &argument.value
                else {
                    continue;
                };
                argument.value = match (renumbered.get(earlier), made(earlier)) {
                    (Some(renamed), _) => {
                        ArgumentValue::Record(RecordValue::SameTurn { act: *renamed })
                    }
                    (None, Some(token)) => ArgumentValue::Record(RecordValue::Record { token }),
                    (None, None) => continue,
                };
            }
            act.depends_on = act
                .depends_on
                .iter()
                .filter_map(|earlier| renumbered.get(earlier).copied())
                .collect();
            act
        })
        .collect()
}

/// What answering a card means for the act it was guarding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resumption {
    /// The answer continues the work: this act goes through the normal path.
    Continue(Box<UnderstoodAct>),
    /// The answer refused it. `record` is the operation a `DeclineAndRecord` option
    /// names, so the case can remember it was asked.
    Declined {
        /// The act that records the refusal, when the option named one.
        record: Option<Box<UnderstoodAct>>,
    },
    /// Nothing to continue: no act on the card, an answer with its own path, or a
    /// case this turn cannot address.
    Nothing,
}

impl Resumption {
    /// The act to run, when there is one.
    #[must_use]
    pub fn act(&self) -> Option<&UnderstoodAct> {
        match self {
            Self::Continue(act) => Some(act),
            _ => None,
        }
    }

    /// Returns `true` when the user refused the instruction the card guarded.
    #[must_use]
    pub const fn is_declined(&self) -> bool {
        matches!(self, Self::Declined { .. })
    }
}

/// An act a card answer applies to the card's own case, with the card's words.
#[must_use]
pub fn card_act(action: ActAction, target: ActTarget) -> UnderstoodAct {
    UnderstoodAct {
        id: card_act_id(),
        action,
        target,
        arguments: std::collections::BTreeMap::new(),
        words: WordRange {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
        },
        depends_on: Vec::new(),
        status: ActStatus::Ready,
    }
}

/// Decides what an accepted card answer does to the act the card was guarding.
///
/// `tokens` is the answering turn's map: a case the actor may not address this turn
/// has no token, and the answer resumes nothing.
#[must_use]
pub fn resume(
    accepted: &AcceptedResponse,
    payload: &InteractionPayload,
    tokens: &TargetTokenMap,
) -> Resumption {
    // A refusal is a fact about the click, whatever the card carried.
    if accepted.action.declines() {
        let record = accepted.action.records().and_then(|operation| {
            let token = tokens.token_for(&accepted.case_ref.key())?;
            Some(Box::new(card_act(
                ActAction::Apply {
                    operation: operation.clone(),
                },
                ActTarget::Record {
                    token: token.clone(),
                },
            )))
        });
        return Resumption::Declined { record };
    }
    let Some(deferred) = DeferredAct::of(payload) else {
        return Resumption::Nothing;
    };
    let case_ref = match &accepted.action {
        StoredInteractionAction::SelectTarget { case_ref } => Some(case_ref.clone()),
        StoredInteractionAction::ResolveClarification { .. }
        | StoredInteractionAction::Custom { .. } => deferred.case_ref.clone(),
        _ => return Resumption::Nothing,
    };
    let mut act = deferred.act;
    act.id = card_act_id();
    act.depends_on.clear();
    act.status = ActStatus::Ready;
    if !matches!(act.action, ActAction::Start { .. }) {
        let Some(token) = case_ref
            .as_ref()
            .and_then(|case_ref| tokens.token_for(&case_ref.key()))
        else {
            tracing::warn!(
                target: "turnframe.resume",
                "the case a card named is not addressable in this turn; nothing was resumed"
            );
            return Resumption::Nothing;
        };
        act.target = ActTarget::Record {
            token: token.clone(),
        };
    }
    Resumption::Continue(Box::new(act))
}

#[cfg(test)]
mod tests {
    use turnframe_core::hash::Digest;
    use turnframe_core::ids::{
        AccountId, CaseRevision, InteractionId, OperationKey, OptionId, TurnId,
    };
    use turnframe_core::interaction::InteractionKind;
    use turnframe_core::understanding::UnderstoodArgument;

    use super::*;

    fn case(id: &str) -> CaseRef {
        CaseRef::new("trip", id, CaseRevision(3))
    }

    fn act() -> UnderstoodAct {
        let mut act = card_act(
            ActAction::Apply {
                operation: OperationKey::from("trip.set_name"),
            },
            ActTarget::Ambiguous {
                candidates: Vec::new(),
            },
        );
        act.id = ActId::new(UnitId(1), 1);
        act.arguments.insert(
            "value".to_owned(),
            UnderstoodArgument {
                value: ArgumentValue::Json(serde_json::json!("Lisbon")),
                excerpt: None,
            },
        );
        act
    }

    fn tokens() -> TargetTokenMap {
        let mut map = TargetTokenMap::new(AccountId::from("acct"), TurnId::nil());
        map.issue(case("trip-1"), "Rossi".to_owned());
        map.issue(case("trip-2"), "Rossi".to_owned());
        map
    }

    fn answer(action: StoredInteractionAction) -> AcceptedResponse {
        AcceptedResponse {
            interaction_id: InteractionId::nil(),
            case_ref: case("trip-1"),
            kind: InteractionKind::SelectTarget,
            option_id: OptionId::from("pick"),
            action,
            channel: turnframe_core::command::ResolutionChannel::Click,
            freeform_input: None,
            payload_hash: Digest::of_bytes(b"p"),
        }
    }

    fn payload_with(deferred: &DeferredAct) -> InteractionPayload {
        deferred.attach_to(InteractionPayload::new("Which one?"))
    }

    fn token_of(tokens: &TargetTokenMap, id: &str) -> ActTarget {
        ActTarget::Record {
            token: tokens.token_for(&case(id).key()).unwrap().clone(),
        }
    }

    #[test]
    fn a_deferred_act_survives_the_payload_round_trip() {
        let deferred = DeferredAct::unbound(act());
        let payload = payload_with(&deferred);
        assert_eq!(DeferredAct::of(&payload), Some(deferred.clone()));
        let json = serde_json::to_value(&payload).unwrap();
        let back: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(DeferredAct::of(&back), Some(deferred));
        assert_eq!(DeferredAct::of(&InteractionPayload::new("bare")), None);
    }

    #[test]
    fn attaching_keeps_the_applications_own_metadata() {
        let payload = InteractionPayload::new("Which one?")
            .with_metadata(serde_json::json!({"preview": "abc"}));
        let attached = DeferredAct::unbound(act()).attach_to(payload);
        assert_eq!(attached.metadata["preview"], "abc");
        assert!(DeferredAct::of(&attached).is_some());
        let odd = InteractionPayload::new("Which one?").with_metadata(serde_json::json!(7));
        let untouched = DeferredAct::unbound(act()).attach_to(odd.clone());
        assert_eq!(untouched.metadata, odd.metadata);
    }

    #[test]
    fn a_selection_binds_the_act_to_the_case_the_user_picked() {
        let tokens = tokens();
        let payload = payload_with(&DeferredAct::unbound(act()));
        let resumed = resume(
            &answer(StoredInteractionAction::SelectTarget {
                case_ref: case("trip-2"),
            }),
            &payload,
            &tokens,
        );
        let act = resumed.act().expect("the act continues");
        assert_eq!(act.target, token_of(&tokens, "trip-2"));
        assert_eq!(act.id, card_act_id());
        assert_eq!(
            act.arguments["value"].value,
            ArgumentValue::Json(serde_json::json!("Lisbon"))
        );
    }

    #[test]
    fn a_decline_may_record_that_the_card_was_answered() {
        let tokens = tokens();
        let payload = payload_with(&DeferredAct::on(act(), case("trip-1")));
        assert_eq!(
            resume(
                &answer(StoredInteractionAction::DeclineCommands),
                &payload,
                &tokens
            ),
            Resumption::Declined { record: None }
        );
        let recording = resume(
            &answer(StoredInteractionAction::DeclineAndRecord {
                operation: OperationKey::from("trip.note_declined"),
            }),
            &payload,
            &tokens,
        );
        let Resumption::Declined { record: Some(act) } = recording else {
            panic!("the refusal is recorded: {recording:?}");
        };
        assert_eq!(
            act.action,
            ActAction::Apply {
                operation: OperationKey::from("trip.note_declined")
            }
        );
        assert_eq!(act.target, token_of(&tokens, "trip-1"));
    }

    #[test]
    fn a_clarification_runs_the_act_against_the_case_it_was_aimed_at() {
        let tokens = tokens();
        let payload = payload_with(&DeferredAct::on(act(), case("trip-1")));
        let resumed = resume(
            &answer(StoredInteractionAction::ResolveClarification {
                answer_key: crate::reduce::CONDITION_HOLDS_ANSWER.to_owned(),
            }),
            &payload,
            &tokens,
        );
        assert_eq!(
            resumed.act().map(|act| &act.target),
            Some(&token_of(&tokens, "trip-1"))
        );
        assert!(!resumed.is_declined());
    }

    #[test]
    fn refusing_is_recorded_and_never_silently_dropped() {
        let payload = payload_with(&DeferredAct::on(act(), case("trip-1")));
        for action in [
            StoredInteractionAction::Dismiss,
            StoredInteractionAction::DeclineCommands,
        ] {
            let resumed = resume(&answer(action), &payload, &tokens());
            assert_eq!(resumed, Resumption::Declined { record: None });
        }
    }

    #[test]
    fn there_is_nothing_to_resume_without_a_card_that_remembers() {
        let tokens = tokens();
        let bare = InteractionPayload::new("Which one?");
        let select = StoredInteractionAction::SelectTarget {
            case_ref: case("trip-1"),
        };
        assert_eq!(resume(&answer(select), &bare, &tokens), Resumption::Nothing);
        let payload = payload_with(&DeferredAct::on(act(), case("trip-1")));
        let confirm = StoredInteractionAction::ConfirmCommands {
            command_refs: Vec::new(),
        };
        assert_eq!(
            resume(&answer(confirm), &payload, &tokens),
            Resumption::Nothing
        );
        let elsewhere = payload_with(&DeferredAct::on(act(), case("trip-9")));
        let clarify = StoredInteractionAction::ResolveClarification {
            answer_key: "yes".to_owned(),
        };
        assert_eq!(
            resume(&answer(clarify), &elsewhere, &tokens),
            Resumption::Nothing
        );
    }

    #[test]
    fn a_start_is_resumed_as_it_stands() {
        let start = card_act(
            ActAction::Start {
                workflow: "trip".into(),
            },
            ActTarget::New {
                workflow: "trip".into(),
            },
        );
        let resumed = resume(
            &answer(StoredInteractionAction::Custom {
                key: "app.yes".to_owned(),
                payload: serde_json::Value::Null,
            }),
            &payload_with(&DeferredAct::unbound(start)),
            &tokens(),
        );
        assert!(matches!(
            resumed.act().map(|act| &act.action),
            Some(ActAction::Start { .. })
        ));
    }

    fn dependent(id: u16, target: ActTarget, depends_on: &[ActId]) -> UnderstoodAct {
        let mut act = card_act(
            ActAction::Apply {
                operation: OperationKey::from("trip.set_traveler"),
            },
            target,
        );
        act.id = ActId::new(UnitId(1), id);
        act.depends_on = depends_on.to_vec();
        act
    }

    #[test]
    fn dependents_point_at_the_case_their_prerequisite_made() {
        let tokens = tokens();
        let opener = ActId::new(UnitId(1), 1);
        let mut first = dependent(2, ActTarget::SameTurn { act: opener }, &[opener]);
        first.arguments.insert(
            "traveler".to_owned(),
            UnderstoodArgument {
                value: ArgumentValue::Record(RecordValue::SameTurn { act: opener }),
                excerpt: None,
            },
        );
        let second_id = ActId::new(UnitId(1), 2);
        let second = dependent(3, ActTarget::SameTurn { act: second_id }, &[second_id]);
        let made = vec![MadeCase {
            act: opener,
            case_ref: case("trip-1"),
        }];
        let payload = DependentAct {
            act: second,
            made: Vec::new(),
        }
        .attach_to(DependentAct { act: first, made }.attach_to(InteractionPayload::new("Sure?")));
        assert_eq!(DependentAct::of(&payload).len(), 2);

        let acts = dependents(&payload, &tokens);
        assert_eq!(acts[0].id, ActId::new(CARD_UNIT, 2));
        assert_eq!(acts[0].target, token_of(&tokens, "trip-1"));
        assert!(acts[0].depends_on.is_empty());
        let ActTarget::Record { token } = token_of(&tokens, "trip-1") else {
            unreachable!()
        };
        assert_eq!(
            acts[0].arguments["traveler"].value,
            ArgumentValue::Record(RecordValue::Record { token })
        );
        let renamed = ActId::new(CARD_UNIT, 2);
        assert_eq!(acts[1].target, ActTarget::SameTurn { act: renamed });
        assert_eq!(acts[1].depends_on, vec![renamed]);
    }

    #[test]
    fn a_card_without_dependents_lets_nothing_run() {
        assert!(dependents(&InteractionPayload::new("Sure?"), &tokens()).is_empty());
    }
}
