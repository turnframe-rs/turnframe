//! Property tests: serde round trips for every type family and stability of
//! the hashing primitives.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use turnframe_core::event::{ArtifactRef, OutboxStatus};
use turnframe_core::interaction::{InteractionOptionView, OptionStyle};
use turnframe_core::knowledge::Citation;
use turnframe_core::prelude::*;
use turnframe_core::response::{
    AnswerStatus, ArtifactView, GeneratedAnswer, GeneratedTransition, InteractionBlock,
    NarratableFact, NoticeSeverity, ReceiptBlock, ReplayToken,
};
use turnframe_core::understanding::{
    ActAction, ArgumentValue, ConstraintKind, Excerpt, MessageRef, QuestionTopic, RecordValue,
    TurnConstraint, UnderstoodArgument, UnderstoodQuestion, WordRange,
};
use uuid::Uuid;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value, "value round trip");
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "string round trip");
}

fn label() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_.-]{0,15}"
}

fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 àèìòù€]{0,24}"
}

fn uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

fn turn_id() -> impl Strategy<Value = TurnId> {
    uuid().prop_map(TurnId)
}

fn interaction_id() -> impl Strategy<Value = InteractionId> {
    uuid().prop_map(InteractionId)
}

fn revision() -> impl Strategy<Value = CaseRevision> {
    any::<u64>().prop_map(CaseRevision)
}

fn case_ref() -> impl Strategy<Value = CaseRef> {
    (label(), label(), revision()).prop_map(|(w, c, r)| CaseRef::new(w, c, r))
}

fn timestamp() -> impl Strategy<Value = DateTime<Utc>> {
    (0i64..4_000_000_000).prop_map(|s| DateTime::from_timestamp(s, 0).unwrap())
}

fn localized() -> impl Strategy<Value = LocalizedText> {
    (text(), proptest::option::of((label(), text()))).prop_map(|(d, tr)| {
        let mut l = LocalizedText::new(d);
        if let Some((locale, t)) = tr {
            l = l.with(locale, t);
        }
        l
    })
}

/// Arbitrary JSON value, including a top-level `Null`.
///
/// Explicit nulls are the point: a review card that clears a field carries one,
/// and the types must keep "cleared" apart from "not part of this change".
fn value() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        Just(serde_json::Value::Null),
        (label()).prop_map(|k| json!({ k: null })),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        text().prop_map(serde_json::Value::from),
        (label(), text()).prop_map(|(k, v)| json!({ k: v })),
    ]
}

/// A JSON value that is never `null` at the top level.
///
/// Used for generic payloads the library stores in an `Option<T>` and writes
/// with `skip_serializing_if`: `Commit::state` and the erased workflow outcome
/// document that a state or outcome serializing to `null` reads back as "there
/// is none", so the property under test is the documented contract, not the
/// impossible one.
fn non_null_value() -> impl Strategy<Value = serde_json::Value> {
    value().prop_filter("not null", |v| !v.is_null())
}

fn field_value() -> impl Strategy<Value = FieldValue> {
    prop_oneof![
        Just(FieldValue::Absent),
        value().prop_map(FieldValue::Present),
    ]
}

fn digest() -> impl Strategy<Value = Digest> {
    any::<[u8; 8]>().prop_map(|b| Digest::of_bytes(&b))
}

fn words() -> impl Strategy<Value = WordRange> {
    (0usize..32, 0usize..8).prop_map(|(first, extra)| WordRange {
        first,
        last: first + extra,
        start: first * 4,
        end: (first + extra) * 4 + 3,
    })
}

fn act_id() -> impl Strategy<Value = ActId> {
    (1u16..9, 1u16..3).prop_map(|(unit, act)| ActId::new(UnitId(unit), act))
}

fn act_target() -> impl Strategy<Value = ActTarget> {
    prop_oneof![
        label().prop_map(|t| ActTarget::Record {
            token: TargetToken::from(t)
        }),
        label().prop_map(|w| ActTarget::New {
            workflow: WorkflowKey::from(w)
        }),
        act_id().prop_map(|act| ActTarget::SameTurn { act }),
        Just(ActTarget::Card),
        (label(), proptest::option::of(words())).prop_map(|(w, words)| ActTarget::NotListed {
            workflow: WorkflowKey::from(w),
            words,
        }),
        proptest::collection::vec(label(), 0..3).prop_map(|c| ActTarget::Ambiguous {
            candidates: c.into_iter().map(TargetToken::from).collect(),
        }),
        Just(ActTarget::Nothing),
    ]
}

fn argument() -> impl Strategy<Value = UnderstoodArgument> {
    (
        prop_oneof![
            value().prop_map(ArgumentValue::Json),
            label().prop_map(|t| ArgumentValue::Record(RecordValue::Record {
                token: TargetToken::from(t),
            })),
            act_id().prop_map(|act| ArgumentValue::Record(RecordValue::SameTurn { act })),
        ],
        proptest::option::of((words(), proptest::option::of(0usize..4))),
    )
        .prop_map(|(value, excerpt)| UnderstoodArgument {
            value,
            excerpt: excerpt.map(|(words, earlier)| Excerpt {
                message: earlier.map_or(MessageRef::Current, |index| MessageRef::Earlier { index }),
                words,
            }),
        })
}

fn understood_act() -> impl Strategy<Value = UnderstoodAct> {
    (
        act_id(),
        label(),
        act_target(),
        proptest::collection::btree_map(label(), argument(), 0..3),
        words(),
        proptest::collection::vec(act_id(), 0..2),
        prop_oneof![
            Just(ActStatus::Ready),
            proptest::collection::vec(label(), 1..3).prop_map(|arguments| ActStatus::NeedsValue {
                arguments,
                reason: None,
            }),
            (1u16..9).prop_map(|unit| ActStatus::Held {
                because: UnitId(unit)
            }),
        ],
    )
        .prop_map(
            |(id, operation, target, arguments, words, depends_on, status)| UnderstoodAct {
                id,
                action: ActAction::Apply {
                    operation: OperationKey::from(operation),
                },
                target,
                arguments,
                words,
                depends_on,
                status,
            },
        )
}

fn basis() -> impl Strategy<Value = AnswerBasis> {
    prop_oneof![
        Just(AnswerBasis::CurrentCommittedState),
        Just(AnswerBasis::ProposedState),
        Just(AnswerBasis::CommittedStateAfterTurn),
        Just(AnswerBasis::GeneralDomainKnowledge),
    ]
}

fn understanding() -> impl Strategy<Value = Understanding> {
    (
        proptest::collection::vec(understood_act(), 0..4),
        proptest::collection::vec((1u16..9, words(), basis(), any::<bool>()), 0..3),
        proptest::collection::vec((1u16..9, words()), 0..2),
    )
        .prop_map(|(acts, questions, constraints)| Understanding {
            acts,
            questions: questions
                .into_iter()
                .map(
                    |(unit, words, basis, continues_previous)| UnderstoodQuestion {
                        unit: UnitId(unit),
                        words,
                        workflow: None,
                        record: None,
                        subjects: Vec::new(),
                        basis,
                        topic: QuestionTopic::default(),
                        continues_previous,
                    },
                )
                .collect(),
            constraints: constraints
                .into_iter()
                .map(|(unit, words)| TurnConstraint {
                    unit: UnitId(unit),
                    kind: ConstraintKind::DoNotSubmit,
                    words,
                })
                .collect(),
            ..Understanding::default()
        })
}

fn command_ref() -> impl Strategy<Value = CommandRef> {
    (uuid(), uuid()).prop_map(|(b, c)| CommandRef {
        batch_id: BatchId(b),
        command_id: CommandId(c),
    })
}

fn stored_action() -> impl Strategy<Value = StoredInteractionAction> {
    prop_oneof![
        proptest::collection::vec(command_ref(), 0..3)
            .prop_map(|refs| StoredInteractionAction::ConfirmCommands { command_refs: refs }),
        Just(StoredInteractionAction::DeclineCommands),
        case_ref().prop_map(|c| StoredInteractionAction::SelectTarget { case_ref: c }),
        label().prop_map(|k| StoredInteractionAction::ResolveClarification { answer_key: k }),
        (label(), value()).prop_map(|(o, a)| StoredInteractionAction::ApplyOperation {
            operation: OperationKey::from(o),
            arguments: a,
            freeform_argument: None,
        }),
        Just(StoredInteractionAction::Dismiss),
        (label(), value())
            .prop_map(|(k, p)| StoredInteractionAction::Custom { key: k, payload: p }),
    ]
}

fn freeform() -> impl Strategy<Value = FreeformPolicy> {
    prop_oneof![
        Just(FreeformPolicy::Forbidden),
        (1usize..500).prop_map(|m| FreeformPolicy::Optional { max_len: m }),
        (1usize..500).prop_map(|m| FreeformPolicy::Required { max_len: m }),
    ]
}

fn option() -> impl Strategy<Value = InteractionOption> {
    (
        label(),
        localized(),
        stored_action(),
        freeform(),
        prop_oneof![
            Just(OptionStyle::Primary),
            Just(OptionStyle::Secondary),
            Just(OptionStyle::Danger)
        ],
    )
        .prop_map(|(id, l, a, f, s)| {
            InteractionOption::new(id, l, a)
                .with_freeform(f)
                .with_style(s)
        })
}

fn payload() -> impl Strategy<Value = InteractionPayload> {
    (
        localized(),
        proptest::option::of(localized()),
        proptest::collection::vec(option(), 0..4),
        proptest::collection::vec((label(), localized(), field_value(), field_value()), 0..3),
        value(),
    )
        .prop_map(|(title, body, options, entries, metadata)| {
            let mut p = InteractionPayload::new(title).with_metadata(metadata);
            p.body = body;
            p.options = options;
            p.review_entries = entries
                .into_iter()
                .map(|(field, label, before, after)| ReviewDiffEntry {
                    field,
                    label,
                    before,
                    after,
                })
                .collect();
            p
        })
}

fn kind() -> impl Strategy<Value = InteractionKind> {
    prop_oneof![
        Just(InteractionKind::Boolean),
        Just(InteractionKind::SingleSelect),
        Just(InteractionKind::MultiSelect),
        Just(InteractionKind::Freeform),
        Just(InteractionKind::ReviewChanges),
        Just(InteractionKind::ConfirmCommand),
        Just(InteractionKind::SelectTarget),
        Just(InteractionKind::ResolveValidationError),
        Just(InteractionKind::Reauthenticate),
        Just(InteractionKind::ExternalSignature),
    ]
}

fn text_resolution() -> impl Strategy<Value = TextResolutionPolicy> {
    prop_oneof![
        Just(TextResolutionPolicy::Never),
        Just(TextResolutionPolicy::ModelInterpretedLowRisk),
    ]
}

fn status() -> impl Strategy<Value = InteractionStatus> {
    proptest::sample::select(InteractionStatus::ALL.to_vec())
}

fn risk() -> impl Strategy<Value = RiskClass> {
    prop_oneof![
        Just(RiskClass::ReadOnly),
        Just(RiskClass::ReversibleLowRisk),
        Just(RiskClass::SensitiveDataChange),
        Just(RiskClass::Destructive),
        Just(RiskClass::Irreversible),
        Just(RiskClass::ExternalRegulated),
    ]
}

fn interaction() -> impl Strategy<Value = Interaction> {
    (
        (interaction_id(), label(), uuid(), case_ref(), turn_id()),
        (kind(), any::<bool>(), payload(), status(), any::<bool>()),
        (
            text_resolution(),
            risk(),
            timestamp(),
            proptest::option::of(timestamp()),
        ),
        (
            proptest::option::of(timestamp()),
            proptest::option::of(label()),
        ),
    )
        .prop_map(
            |(
                (id, account, conv, case_ref, turn),
                (kind, blocking, payload, status, independent),
                (text_resolution, confirms_risk, created_at, expires_at),
                (resolved_at, resolved_option),
            )| {
                let payload_hash = payload.hash().unwrap();
                Interaction {
                    id,
                    account_id: AccountId::from(account),
                    conversation_id: ConversationId(conv),
                    case_ref,
                    created_by_turn: turn,
                    kind,
                    blocking,
                    payload,
                    payload_hash,
                    status,
                    revision_independent: independent,
                    text_resolution,
                    confirms_risk,
                    created_at,
                    expires_at,
                    resolved_at,
                    resolved_option_id: resolved_option.map(OptionId::from),
                }
            },
        )
}

fn action_class() -> impl Strategy<Value = ActionClass> {
    prop_oneof![
        Just(ActionClass::ConfirmsCommands),
        Just(ActionClass::AppliesOperation),
        Just(ActionClass::NoCommands),
    ]
}

fn channel() -> impl Strategy<Value = ResolutionChannel> {
    proptest::sample::select(ResolutionChannel::ALL.to_vec())
}

fn origin() -> impl Strategy<Value = CommandOrigin> {
    prop_oneof![
        digest().prop_map(|d| CommandOrigin::DirectSafeUserAct { evidence_digest: d }),
        (
            interaction_id(),
            digest(),
            kind(),
            action_class(),
            channel()
        )
            .prop_map(|(i, d, k, a, c)| CommandOrigin::ConfirmedInteraction {
                interaction_id: i,
                payload_hash: d,
                interaction_kind: k,
                action_class: a,
                channel: c,
            }),
        label().prop_map(|k| CommandOrigin::InternalPolicy { policy_key: k }),
        (label(), any::<bool>()).prop_map(|(c, v)| CommandOrigin::ExternalCallback {
            callback_id: c,
            signature_verified: v,
        }),
    ]
}

fn policy() -> impl Strategy<Value = CommandPolicy> {
    (
        risk(),
        prop_oneof![
            Just(ConfirmationPolicy::None),
            Just(ConfirmationPolicy::ReviewCard),
            Just(ConfirmationPolicy::ExplicitClick),
            Just(ConfirmationPolicy::Reauthentication),
            Just(ConfirmationPolicy::QualifiedSignature),
            Just(ConfirmationPolicy::HumanProfessionalReview),
        ],
        scope(),
        prop_oneof![
            Just(ClaimMode::ServerReceiptOnly),
            Just(ClaimMode::EventReferencedParaphrase),
            Just(ClaimMode::FreeExplanation),
        ],
    )
        .prop_map(
            |(risk, confirmation, atomicity, claim_mode)| CommandPolicy {
                risk,
                confirmation,
                atomicity,
                claim_mode,
            },
        )
}

fn envelope() -> impl Strategy<Value = CommandEnvelope<serde_json::Value>> {
    (
        uuid(),
        turn_id(),
        label(),
        label(),
        case_ref(),
        origin(),
        value(),
    )
        .prop_map(|(c, t, acc, user, case_ref, origin, command)| {
            let account = AccountId::from(acc.clone());
            let idempotency_key =
                IdempotencyKey::derive(&account, &t, &case_ref, &origin, &command).unwrap();
            let mut actor = ActorContext::new(account, user).with_role(format!("role_{acc}"));
            actor
                .attributes
                .insert("tier".into(), serde_json::Value::from(acc));
            CommandEnvelope {
                command_id: CommandId(c),
                turn_id: t,
                actor,
                case_ref,
                idempotency_key,
                origin,
                command,
            }
        })
}

fn scope() -> impl Strategy<Value = AtomicityScope> {
    prop_oneof![
        Just(AtomicityScope::PerCommand),
        Just(AtomicityScope::PerCase),
        label().prop_map(|g| AtomicityScope::ExplicitGroup { group: g }),
        label().prop_map(|s| AtomicityScope::ExternalSaga { saga: s }),
    ]
}

fn batch() -> impl Strategy<Value = CommandBatch<serde_json::Value>> {
    (uuid(), scope(), proptest::collection::vec(envelope(), 0..3)).prop_map(
        |(b, scope, envelopes)| CommandBatch {
            batch_id: BatchId(b),
            scope,
            envelopes,
        },
    )
}

fn receipt() -> impl Strategy<Value = OperationalReceipt> {
    (
        uuid(),
        proptest::collection::vec(uuid(), 0..3),
        prop_oneof![
            Just(ReceiptSeverity::Info),
            Just(ReceiptSeverity::Success),
            Just(ReceiptSeverity::Warning),
            Just(ReceiptSeverity::Error),
        ],
        localized(),
        localized(),
        label(),
        proptest::collection::vec(artifact(), 0..2),
    )
        .prop_map(|(id, events, severity, title, body, code, artifact_refs)| {
            OperationalReceipt {
                receipt_id: ReceiptId(id),
                event_ids: events.into_iter().map(EventId).collect(),
                severity,
                title,
                body,
                status_code: code,
                artifact_refs,
            }
        })
}

fn committed_event() -> impl Strategy<Value = CommittedEvent<serde_json::Value>> {
    (uuid(), label(), timestamp(), value()).prop_map(|(id, ty, at, payload)| CommittedEvent {
        event_id: EventId(id),
        event_type: ty,
        occurred_at: at,
        payload,
    })
}

fn commit() -> impl Strategy<Value = Commit<serde_json::Value, serde_json::Value>> {
    (
        proptest::option::of(non_null_value()),
        revision(),
        proptest::collection::vec(committed_event(), 0..3),
        any::<bool>(),
    )
        .prop_map(|(state, new_revision, events, replay)| Commit {
            state,
            new_revision,
            events,
            idempotency_replay: replay,
        })
}

fn fact() -> impl Strategy<Value = NarratableFact> {
    prop_oneof![
        (case_ref(), label(), value()).prop_map(|(c, f, v)| NarratableFact::StateValue {
            case_ref: c,
            field: f,
            value: v,
        }),
        (
            case_ref(),
            proptest::option::of(label()),
            value(),
            // An outcome of null is no outcome on the wire, and no workflow reports one.
            proptest::option::of(value().prop_filter("an outcome is not null", |v| !v.is_null()))
        )
            .prop_map(|(c, l, p, o)| NarratableFact::Record {
                case_ref: c,
                label: l,
                phase: p,
                outcome: o,
            }),
        (case_ref(), label(), field_value(), field_value()).prop_map(|(c, f, before, after)| {
            NarratableFact::ProposedChange {
                case_ref: c,
                field: f,
                before,
                after,
            }
        }),
        (uuid(), proptest::collection::vec(uuid(), 0..2), label()).prop_map(|(r, e, s)| {
            NarratableFact::OperationalOutcome {
                receipt_id: ReceiptId(r),
                event_ids: e.into_iter().map(EventId).collect(),
                status_code: s,
            }
        }),
        (interaction_id(), kind()).prop_map(|(i, k)| NarratableFact::InteractionAvailable {
            interaction_id: i,
            interaction_kind: k,
        }),
    ]
}

fn citation() -> impl Strategy<Value = Citation> {
    (
        label(),
        text(),
        proptest::option::of(label()),
        proptest::option::of(label()),
        proptest::option::of(label()),
    )
        .prop_map(|(source_id, label, uri, locator, version)| Citation {
            source_id,
            label,
            uri,
            locator,
            version,
        })
}

fn answer_status() -> impl Strategy<Value = AnswerStatus> {
    prop_oneof![
        Just(AnswerStatus::Answered),
        Just(AnswerStatus::ClarificationRequested),
        Just(AnswerStatus::Unsupported),
        Just(AnswerStatus::SourceUnavailable),
    ]
}

fn artifact() -> impl Strategy<Value = ArtifactRef> {
    (
        label(),
        label(),
        localized(),
        proptest::option::of(label()),
        proptest::option::of(label()),
    )
        .prop_map(|(artifact_id, kind, label, uri, media_type)| ArtifactRef {
            artifact_id,
            kind,
            label,
            uri,
            media_type,
        })
}

fn block() -> impl Strategy<Value = ResponseBlock> {
    prop_oneof![
        (
            (label(), proptest::option::of(label())),
            text(),
            basis(),
            answer_status(),
            proptest::collection::vec(fact(), 0..2),
            proptest::collection::vec(citation(), 0..2),
        )
            .prop_map(|((b, question), t, basis, status, facts, citations)| {
                ResponseBlock::Answer(GeneratedAnswer {
                    block_id: BlockId::from(b),
                    question_id: question.map(turnframe_core::ids::QuestionId::from),
                    text: t,
                    basis,
                    status,
                    facts_used: facts,
                    citations,
                    enumerations: Vec::new(),
                })
            }),
        (label(), text(), proptest::collection::vec(fact(), 0..2)).prop_map(|(b, t, facts)| {
            ResponseBlock::Transition(GeneratedTransition {
                block_id: BlockId::from(b),
                text: t,
                facts_used: facts,
            })
        }),
        (label(), receipt()).prop_map(|(b, r)| ResponseBlock::Receipt(ReceiptBlock {
            block_id: BlockId::from(b),
            receipt: r,
        })),
        (
            label(),
            label(),
            localized(),
            prop_oneof![
                Just(NoticeSeverity::Info),
                Just(NoticeSeverity::Warning),
                Just(NoticeSeverity::Error),
            ],
        )
            .prop_map(|(b, c, t, severity)| ResponseBlock::Notice(ServerNotice {
                block_id: BlockId::from(b),
                code: c,
                severity,
                text: t,
            })),
        (label(), interaction()).prop_map(|(b, i)| ResponseBlock::Interaction(InteractionBlock {
            block_id: BlockId::from(b),
            view: i.view(),
        })),
        (label(), artifact()).prop_map(|(b, artifact)| ResponseBlock::Artifact(ArtifactView {
            block_id: BlockId::from(b),
            artifact,
        })),
    ]
}

fn assistant_turn() -> impl Strategy<Value = AssistantTurn> {
    (
        turn_id(),
        uuid(),
        proptest::collection::vec(block(), 0..4),
        label(),
    )
        .prop_map(|(t, c, blocks, token)| AssistantTurn {
            turn_id: t,
            conversation_id: ConversationId(c),
            blocks,
            subjects: Vec::new(),
            expectations: Vec::new(),
            replay_token: ReplayToken::from(token.as_str()),
            done: Vec::new(),
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn ids_round_trip(t in turn_id(), c in case_ref(), r in revision(), l in label()) {
        round_trip(&t);
        round_trip(&c);
        round_trip(&r);
        round_trip(&WorkflowKey::from(l.clone()));
        round_trip(&OptionId::from(l.clone()));
        round_trip(&TargetToken::from(l.clone()));
        round_trip(&AccountId::from(l));
    }

    #[test]
    fn understanding_round_trips(u in understanding()) {
        round_trip(&u);
        prop_assert_eq!(u.hash().unwrap(), u.clone().hash().unwrap());
    }

    #[test]
    fn interaction_round_trips(i in interaction()) {
        round_trip(&i);
        round_trip(&i.view());
        prop_assert!(i.verify_payload_hash().unwrap());
        // The hash still matches after a trip through a JSON store, explicit
        // nulls in the diff included.
        let reloaded: Interaction = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
        prop_assert!(reloaded.verify_payload_hash().unwrap());
        let option_views: Vec<InteractionOptionView> = i.payload.options.iter().map(InteractionOption::view).collect();
        prop_assert_eq!(option_views.len(), i.view().options.len());
    }

    #[test]
    fn command_round_trips(b in batch(), p in policy()) {
        round_trip(&b);
        round_trip(&p);
        for e in &b.envelopes {
            let derived = IdempotencyKey::derive(e.account_id(), &e.turn_id, &e.case_ref, &e.origin, &e.command).unwrap();
            prop_assert_eq!(&derived, &e.idempotency_key);
        }
    }

    #[test]
    fn event_round_trips(c in commit(), r in receipt(), s in proptest::sample::select(vec![
        OutboxStatus::Pending, OutboxStatus::Dispatching, OutboxStatus::OutcomeUnknown, OutboxStatus::Completed, OutboxStatus::Failed
    ])) {
        round_trip(&c);
        round_trip(&r);
        round_trip(&s);
        prop_assert_eq!(c.event_ids().len(), c.events.len());
    }

    #[test]
    fn response_round_trips(t in assistant_turn()) {
        round_trip(&t);
        prop_assert_eq!(t.block_ids().len(), t.blocks.len());
    }

    #[test]
    fn idempotency_key_is_stable_and_sensitive(
        acc in label(), t in turn_id(), c in case_ref(), o in origin(), cmd in value(), other_turn in turn_id(), other_cmd in value(),
    ) {
        let account = AccountId::from(acc.clone());
        let k1 = IdempotencyKey::derive(&account, &t, &c, &o, &cmd).unwrap();
        let k2 = IdempotencyKey::derive(&account, &t, &c, &o, &cmd).unwrap();
        prop_assert_eq!(&k1, &k2);
        if other_turn != t {
            prop_assert_ne!(&k1, &IdempotencyKey::derive(&account, &other_turn, &c, &o, &cmd).unwrap());
        }
        if other_cmd != cmd {
            prop_assert_ne!(&k1, &IdempotencyKey::derive(&account, &t, &c, &o, &other_cmd).unwrap());
        }
        prop_assert_ne!(&k1, &IdempotencyKey::derive(&AccountId::from(format!("{acc}x")), &t, &c, &o, &cmd).unwrap());
        prop_assert_ne!(&k1, &IdempotencyKey::derive(&account, &t, &c.with_revision(c.expected_revision.next().next()), &o, &cmd).unwrap());
    }

    #[test]
    fn payload_hash_is_stable_and_sensitive(p in payload(), extra in label()) {
        let h1 = p.hash().unwrap();
        let h2 = p.clone().hash().unwrap();
        prop_assert_eq!(&h1, &h2);
        let mut changed_title = p.clone();
        changed_title.title = LocalizedText::new(format!("{}{extra}!", p.title.default));
        prop_assert_ne!(&h1, &changed_title.hash().unwrap());
        let with_option = p.clone().with_option(InteractionOption::new(
            format!("{extra}_new"),
            "New",
            StoredInteractionAction::Dismiss,
        ));
        prop_assert_ne!(&h1, &with_option.hash().unwrap());
    }

    #[test]
    fn a_policy_that_demands_nothing_accepts_any_origin(o in origin(), p in policy()) {
        if !p.requires_trusted_origin() {
            prop_assert!(origin_satisfies(&o, &p));
        }
    }

    #[test]
    fn whatever_satisfies_a_demanding_policy_is_trusted(o in origin(), p in policy()) {
        if origin_satisfies(&o, &p) && p.requires_trusted_origin() {
            prop_assert!(o.is_trusted());
        }
    }

    #[test]
    fn an_inferred_answer_never_authorizes_more_than_low_risk(
        i in interaction_id(), d in digest(), k in kind(), a in action_class(), p in policy(),
    ) {
        let inferred = CommandOrigin::ConfirmedInteraction {
            interaction_id: i,
            payload_hash: d,
            interaction_kind: k,
            action_class: a,
            channel: ResolutionChannel::ModelInterpreted,
        };
        prop_assert!(!inferred.is_trusted());
        if p.risk > RiskClass::ReversibleLowRisk || p.confirmation != ConfirmationPolicy::None {
            prop_assert!(!origin_satisfies(&inferred, &p));
        }
    }

    #[test]
    fn an_option_that_authorizes_nothing_produces_no_origin(p in payload(), k in kind()) {
        for option in &p.options {
            let accepted = AcceptedResponse {
                interaction_id: InteractionId::nil(),
                case_ref: CaseRef::new("w", "c", CaseRevision(1)),
                kind: k,
                option_id: option.id.clone(),
                action: option.action.clone(),
                channel: ResolutionChannel::Click,
                freeform_input: None,
                payload_hash: p.hash().unwrap(),
            };
            prop_assert_eq!(
                accepted.origin().is_some(),
                option.action.action_class().authorizes_commands()
            );
        }
    }

    #[test]
    fn a_persisted_card_is_always_answerable(p in payload(), k in kind()) {
        // Whatever the generator produces, `from_spec` either refuses it or
        // returns a card whose payload validates for its kind.
        let spec = InteractionSpec::new(
            "k",
            CaseRef::new("w", "c", CaseRevision(1)),
            k,
            p,
        );
        let created = Interaction::from_spec(
            spec,
            InteractionId::nil(),
            AccountId::from("a"),
            ConversationId::nil(),
            TurnId::nil(),
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        if let Ok(card) = created {
            prop_assert!(card.payload.validate_for(card.kind).is_ok());
            prop_assert!(card.verify_payload_hash().unwrap());
        }
    }
}
