//! Strategies for identifiers, case references and locales.
//!
//! Identifiers are generated from bytes rather than from a clock or a random
//! source, so a failing case shrinks and reproduces from its seed.

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::hash::Digest;
use turnframe_core::ids::{
    AccountId, AttachmentId, CaseId, CaseRevision, CommandId, ConversationId, EventId,
    InteractionId, OperationKey, OptionId, OriginToken, ReceiptId, TargetToken, TurnId, UserId,
    WorkflowKey,
};
use turnframe_core::locale::Locale;
use uuid::Uuid;

/// Words the label strategies draw from. A closed vocabulary keeps generated
/// identifiers readable in a failure report and free of anything that could be
/// mistaken for user text.
pub const LABEL_WORDS: [&str; 8] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
];

/// Locales the strategies draw from.
pub const LOCALES: [&str; 4] = ["en", "en-GB", "it", "it-IT"];

/// A UUID built from arbitrary bytes: reproducible and shrinkable, unlike
/// [`Uuid::now_v7`].
pub fn uuid() -> impl Strategy<Value = Uuid> {
    any::<[u8; 16]>().prop_map(Uuid::from_bytes)
}

/// A short opaque label such as `"delta-3"`.
pub fn label() -> impl Strategy<Value = String> {
    (proptest::sample::select(&LABEL_WORDS[..]), 0_u8..8)
        .prop_map(|(word, index)| format!("{word}-{index}"))
}

/// An arbitrary tenant.
pub fn account_id() -> impl Strategy<Value = AccountId> {
    label().prop_map(AccountId::new)
}

/// An arbitrary user within an account.
pub fn user_id() -> impl Strategy<Value = UserId> {
    label().prop_map(UserId::new)
}

/// An arbitrary case identifier.
pub fn case_id() -> impl Strategy<Value = CaseId> {
    label().prop_map(CaseId::new)
}

/// An arbitrary workflow key.
pub fn workflow_key() -> impl Strategy<Value = WorkflowKey> {
    proptest::sample::select(&["trip", "traveler", "note"][..]).prop_map(WorkflowKey::from)
}

/// An arbitrary operation key, always namespaced by a workflow.
pub fn operation_key() -> impl Strategy<Value = OperationKey> {
    (workflow_key(), label()).prop_map(|(workflow, name)| {
        OperationKey::new(format!("{workflow}.{}", name.replace('-', "_")))
    })
}

/// An arbitrary option identifier.
pub fn option_id() -> impl Strategy<Value = OptionId> {
    label().prop_map(OptionId::new)
}

/// An arbitrary opaque target token.
pub fn target_token() -> impl Strategy<Value = TargetToken> {
    label().prop_map(|value| TargetToken::new(format!("t_{value}")))
}

/// An arbitrary origin token.
pub fn origin_token() -> impl Strategy<Value = OriginToken> {
    label().prop_map(|value| OriginToken::new(format!("o_{value}")))
}

/// An arbitrary attachment identifier.
pub fn attachment_id() -> impl Strategy<Value = AttachmentId> {
    label().prop_map(|value| AttachmentId::new(format!("att_{value}")))
}

/// An arbitrary conversation identifier.
pub fn conversation_id() -> impl Strategy<Value = ConversationId> {
    uuid().prop_map(ConversationId::from)
}

/// An arbitrary turn identifier.
pub fn turn_id() -> impl Strategy<Value = TurnId> {
    uuid().prop_map(TurnId::from)
}

/// An arbitrary interaction identifier.
pub fn interaction_id() -> impl Strategy<Value = InteractionId> {
    uuid().prop_map(InteractionId::from)
}

/// An arbitrary command identifier.
pub fn command_id() -> impl Strategy<Value = CommandId> {
    uuid().prop_map(CommandId::from)
}

/// An arbitrary event identifier.
pub fn event_id() -> impl Strategy<Value = EventId> {
    uuid().prop_map(EventId::from)
}

/// An arbitrary receipt identifier.
pub fn receipt_id() -> impl Strategy<Value = ReceiptId> {
    uuid().prop_map(ReceiptId::from)
}

/// An arbitrary case revision, small enough to stay readable.
pub fn case_revision() -> impl Strategy<Value = CaseRevision> {
    (0_u64..64).prop_map(CaseRevision)
}

/// An arbitrary digest, derived from bytes rather than from a real payload.
pub fn digest() -> impl Strategy<Value = Digest> {
    any::<[u8; 16]>().prop_map(|bytes| Digest::of_bytes(&bytes))
}

/// An arbitrary case identity without a revision.
pub fn case_key() -> impl Strategy<Value = CaseKey> {
    (workflow_key(), case_id()).prop_map(|(workflow, case_id)| CaseKey { workflow, case_id })
}

/// An arbitrary case reference.
pub fn case_ref() -> impl Strategy<Value = CaseRef> {
    (case_key(), case_revision()).prop_map(|(key, revision)| key.at(revision))
}

/// An arbitrary locale from the supported set.
pub fn locale() -> impl Strategy<Value = Locale> {
    proptest::sample::select(&LOCALES[..]).prop_map(Locale::from)
}

/// An arbitrary instant between 2001 and 2033, always a valid timestamp.
pub fn instant() -> impl Strategy<Value = DateTime<Utc>> {
    (1_000_000_000_i64..2_000_000_000_i64).prop_filter_map("timestamp out of range", |seconds| {
        DateTime::from_timestamp(seconds, 0)
    })
}
