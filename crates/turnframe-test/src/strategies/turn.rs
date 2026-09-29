//! Strategies for the turn input protocol (spec §9).

use proptest::prelude::*;
use turnframe_core::turn::{
    ActorContext, AttachmentRef, InteractionResponse, OriginRef, TurnInput,
};

use crate::strategies::ids;

/// Sentences the text strategies draw from. They are deliberately ordinary
/// domain phrases: the point is to have real byte offsets and multi-byte
/// characters, not to fuzz Unicode.
pub const SENTENCES: [&str; 6] = [
    "Set the name to Lisbon offsite and rebook it tomorrow",
    "Cambia il nome in \u{201c}Offsite Lisbona\u{201d} perche' serve oggi",
    "Add an extra for the checked bag, then say who pays",
    "Actually leave the travel date unchanged",
    "Change the traveler to Luca Ferri and tell me the total",
    "Non inviare nulla, voglio solo capire cosa manca",
];

/// Roles an actor may hold in the samples.
pub const ROLES: [&str; 3] = ["owner", "accountant", "viewer"];

/// A non-blank user message drawn from [`SENTENCES`].
pub fn user_text() -> impl Strategy<Value = String> {
    proptest::sample::select(&SENTENCES[..]).prop_map(str::to_owned)
}

/// An authenticated actor with zero to two roles.
pub fn actor_context() -> impl Strategy<Value = ActorContext> {
    (
        ids::account_id(),
        ids::user_id(),
        proptest::collection::vec(proptest::sample::select(&ROLES[..]), 0..3),
    )
        .prop_map(|(account_id, user_id, roles)| {
            roles
                .into_iter()
                .fold(ActorContext::new(account_id, user_id), |actor, role| {
                    actor.with_role(role)
                })
        })
}

/// A structured reply to a card.
pub fn interaction_response() -> impl Strategy<Value = InteractionResponse> {
    (
        ids::interaction_id(),
        ids::option_id(),
        ids::case_revision(),
        proptest::option::of(proptest::sample::select(&["ok", "please proceed"][..])),
    )
        .prop_map(
            |(interaction_id, option_id, expected_case_revision, freeform_input)| {
                InteractionResponse {
                    interaction_id,
                    option_id,
                    expected_case_revision,
                    freeform_input: freeform_input.map(str::to_owned),
                }
            },
        )
}

/// An attachment reference.
pub fn attachment_ref() -> impl Strategy<Value = AttachmentRef> {
    ids::attachment_id().prop_map(|attachment_id| AttachmentRef {
        attachment_id,
        media_type: "application/pdf".to_owned(),
        filename: None,
        size_bytes: None,
        digest: None,
    })
}

/// A server-issued origin reference.
pub fn origin_ref() -> impl Strategy<Value = OriginRef> {
    ids::origin_token().prop_map(|origin_token| OriginRef {
        origin_token,
        signature: None,
        surface: Some("trip_detail".to_owned()),
    })
}

/// A turn that always passes [`TurnInput::validate_shape`].
///
/// Text and a card reply may coexist — that is the point of spec §9 — but they
/// are never both absent, because an empty turn is not a turn. Attachments are
/// generated on top, never instead.
pub fn turn_input() -> impl Strategy<Value = TurnInput> {
    let content = prop_oneof![
        user_text().prop_map(|text| (Some(text), None)),
        interaction_response().prop_map(|response| (None, Some(response))),
        (user_text(), interaction_response())
            .prop_map(|(text, response)| (Some(text), Some(response))),
    ];
    (
        ids::turn_id(),
        ids::conversation_id(),
        actor_context(),
        content,
        proptest::collection::vec(attachment_ref(), 0..2),
        proptest::option::of(origin_ref()),
        ids::locale(),
    )
        .prop_map(
            |(
                turn_id,
                conversation_id,
                actor,
                (text, interaction_response),
                attachments,
                origin,
                locale,
            )| TurnInput {
                turn_id,
                conversation_id,
                actor,
                text,
                interaction_response,
                attachments,
                origin,
                locale,
                effort: None,
            },
        )
}
