//! Strategies for persistent interactions (spec §15).
//!
//! Every generated card is *valid*: it satisfies
//! [`InteractionSpec::validate`], which means it can be answered and it does
//! not claim that typed text may confirm something consequential. A card the
//! user cannot say no to, or one that lets prose authorize a signature, is a
//! defect rather than a test case, so neither is generated here — write those
//! by hand when refusal is what you want to test.

use proptest::prelude::*;
use turnframe_core::command::RiskClass;
use turnframe_core::ids::{OperationKey, OptionId};
use turnframe_core::interaction::{
    FieldValue, FreeformPolicy, Interaction, InteractionKind, InteractionOption,
    InteractionPayload, InteractionSpec, OptionStyle, ReviewDiffEntry, StoredInteractionAction,
    TextResolutionPolicy,
};
use turnframe_core::locale::LocalizedText;

use crate::strategies::ids;

/// Identifier of the confirming option on every generated payload.
pub const CONFIRM_OPTION: &str = "confirm";

/// Identifier of the declining option on every generated payload.
pub const DECLINE_OPTION: &str = "decline";

/// Identifier of the free-text option on a generated free-form payload.
pub const NOTE_OPTION: &str = "note";

/// The kinds a payload can be generated for.
///
/// [`InteractionKind::MultiSelect`] is absent because core refuses to persist
/// it: the input protocol carries one option id, so a multi-select card could
/// not be answered.
pub const SUPPORTED_KINDS: [InteractionKind; 9] = [
    InteractionKind::Boolean,
    InteractionKind::SingleSelect,
    InteractionKind::Freeform,
    InteractionKind::ReviewChanges,
    InteractionKind::ConfirmCommand,
    InteractionKind::SelectTarget,
    InteractionKind::ResolveValidationError,
    InteractionKind::Reauthenticate,
    InteractionKind::ExternalSignature,
];

/// An arbitrary persistable interaction shape.
pub fn interaction_kind() -> impl Strategy<Value = InteractionKind> {
    proptest::sample::select(&SUPPORTED_KINDS[..])
}

/// The option that confirms: choosing it compiles and runs an operation.
pub fn confirming_option() -> impl Strategy<Value = InteractionOption> {
    ids::operation_key().prop_map(|operation| {
        InteractionOption::new(
            CONFIRM_OPTION,
            LocalizedText::new("Confirm").with("it", "Conferma"),
            StoredInteractionAction::ApplyOperation {
                operation,
                arguments: serde_json::Value::Null,
                freeform_argument: None,
            },
        )
        .with_style(OptionStyle::Primary)
    })
}

/// The option that declines: choosing it drops the pending commands.
#[must_use]
pub fn declining_option() -> InteractionOption {
    InteractionOption::new(
        DECLINE_OPTION,
        LocalizedText::new("Not now").with("it", "Non ora"),
        StoredInteractionAction::DeclineCommands,
    )
    .with_style(OptionStyle::Danger)
}

/// The option a free-form card needs: one that requires typed text.
#[must_use]
pub fn note_option() -> InteractionOption {
    InteractionOption::new(
        NOTE_OPTION,
        LocalizedText::new("Send the note").with("it", "Invia la nota"),
        StoredInteractionAction::ResolveClarification {
            answer_key: "note".to_owned(),
        },
    )
    .with_freeform(FreeformPolicy::Required { max_len: 200 })
}

/// A payload that satisfies [`InteractionPayload::validate_for`] for `kind`.
///
/// Every shape gets exactly what its kind requires and nothing more, because
/// "one option too many" is itself a failure for a boolean card.
pub fn answerable_payload_for(kind: InteractionKind) -> BoxedStrategy<InteractionPayload> {
    confirming_option()
        .prop_map(move |confirm| {
            let base = InteractionPayload::new(
                LocalizedText::new("Confirm this operation?").with("it", "Confermi l'operazione?"),
            )
            .with_body(
                LocalizedText::new("It cannot be undone.")
                    .with("it", "L'operazione non e reversibile."),
            );
            match kind {
                InteractionKind::Freeform => base.with_option(note_option()).with_freeform_prompt(
                    LocalizedText::new("What should it say?").with("it", "Cosa deve dire?"),
                ),
                InteractionKind::ReviewChanges => base
                    .with_option(confirm)
                    .with_option(declining_option())
                    .with_review_entry(ReviewDiffEntry {
                        field: "name".to_owned(),
                        label: LocalizedText::new("Subject").with("it", "Oggetto"),
                        before: FieldValue::present("Lisbon"),
                        after: FieldValue::present("Lisbon offsite"),
                    }),
                _ => base.with_option(confirm).with_option(declining_option()),
            }
        })
        .boxed()
}

/// A payload for a [`InteractionKind::ConfirmCommand`] card: the shape most
/// tests want.
pub fn answerable_payload() -> impl Strategy<Value = InteractionPayload> {
    answerable_payload_for(InteractionKind::ConfirmCommand)
}

/// A text resolution policy allowed for a card confirming at most
/// `confirms_risk` of `kind`.
fn allowed_text_resolution(
    kind: InteractionKind,
    confirms_risk: RiskClass,
) -> BoxedStrategy<TextResolutionPolicy> {
    if kind.authorizes_commands() || confirms_risk > RiskClass::ReversibleLowRisk {
        return Just(TextResolutionPolicy::Never).boxed();
    }
    prop_oneof![
        Just(TextResolutionPolicy::Never),
        Just(TextResolutionPolicy::ModelInterpretedLowRisk),
    ]
    .boxed()
}

/// An arbitrary risk class a card may confirm.
pub fn confirms_risk() -> impl Strategy<Value = RiskClass> {
    prop_oneof![
        Just(RiskClass::ReadOnly),
        Just(RiskClass::ReversibleLowRisk),
        Just(RiskClass::SensitiveDataChange),
        Just(RiskClass::Destructive),
        Just(RiskClass::Irreversible),
        Just(RiskClass::ExternalRegulated),
    ]
}

/// An arbitrary text resolution policy, without regard for what it is attached
/// to. Use [`interaction_spec`] when you need one that is actually allowed.
pub fn text_resolution_policy() -> impl Strategy<Value = TextResolutionPolicy> {
    prop_oneof![
        Just(TextResolutionPolicy::Never),
        Just(TextResolutionPolicy::ModelInterpretedLowRisk),
    ]
}

/// An arbitrary interaction specification that passes
/// [`InteractionSpec::validate`].
pub fn interaction_spec() -> impl Strategy<Value = InteractionSpec> {
    (interaction_kind(), confirms_risk())
        .prop_flat_map(|(kind, risk)| {
            (
                Just(kind),
                Just(risk),
                ids::label(),
                ids::case_ref(),
                answerable_payload_for(kind),
                allowed_text_resolution(kind, risk),
                any::<bool>(),
                any::<bool>(),
            )
        })
        .prop_map(
            |(
                kind,
                risk,
                key,
                case_ref,
                payload,
                text_resolution,
                blocking,
                revision_independent,
            )| {
                let spec = InteractionSpec::new(key, case_ref, kind, payload)
                    .with_confirms_risk(risk)
                    .with_text_resolution(text_resolution);
                let spec = if blocking { spec } else { spec.non_blocking() };
                if revision_independent {
                    spec.revision_independent()
                } else {
                    spec
                }
            },
        )
        .prop_filter("the card must be answerable", |spec| {
            spec.validate().is_ok()
        })
}

/// An arbitrary persisted interaction in the `Active` status, with a payload
/// hash that matches its payload.
pub fn interaction() -> impl Strategy<Value = Interaction> {
    (
        interaction_spec(),
        ids::interaction_id(),
        ids::account_id(),
        ids::conversation_id(),
        ids::turn_id(),
        ids::instant(),
    )
        .prop_filter_map(
            "payload must be hashable",
            |(spec, id, account_id, conversation_id, turn_id, now)| {
                Interaction::from_spec(spec, id, account_id, conversation_id, turn_id, now).ok()
            },
        )
}

/// The operation a generated confirming option applies, for assertions.
#[must_use]
pub fn confirming_operation(payload: &InteractionPayload) -> Option<&OperationKey> {
    match payload.option(&OptionId::from(CONFIRM_OPTION))?.action {
        StoredInteractionAction::ApplyOperation { ref operation, .. } => Some(operation),
        _ => None,
    }
}
