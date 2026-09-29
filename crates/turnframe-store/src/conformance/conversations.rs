//! Checks for [`ConversationStore`](crate::conversation::ConversationStore).

use turnframe_core::ids::{ConversationId, InteractionId, TurnId};
use turnframe_core::replay::TurnPhase;

use super::ConformanceFailure;
use super::fixtures::{
    account, assistant_turn, at, card, card_spec, case, ensure, ensure_code, ensure_eq,
    ensure_error, ensure_ok, epoch, other_account, user_turn,
};
use crate::conversation::{ConversationRecord, RecoveryScope};
use crate::error::{StoreError, codes};
use crate::stores::Stores;

/// An assistant turn reloads exactly as it was returned, and the phase marker
/// tracks the turn for crash recovery (spec §22.3, §23.1).
///
/// The reload assertion is the point: the fixture turn carries an interaction
/// block, and a store that rebuilt cards from prose — or dropped the block, or
/// reordered the blocks — would fail here rather than in production, where the
/// user would see a card that no longer does anything.
pub async fn check_conversation_turn_persistence(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_conversation_turn_persistence";
    let conversations = stores.conversations();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    let record = ConversationRecord::new(conversation, account.clone(), epoch())
        .with_metadata(serde_json::json!({ "channel": "conformance" }));
    ensure_ok(
        CHECK,
        "creating the conversation",
        conversations.create_conversation(record.clone()).await,
    )?;
    ensure_error(
        CHECK,
        "creating the same conversation twice",
        conversations.create_conversation(record.clone()).await,
        &StoreError::Conflict,
    )?;
    let loaded = ensure_ok(
        CHECK,
        "loading the conversation",
        conversations
            .load_conversation(&account, &conversation)
            .await,
    )?;
    ensure_eq(CHECK, "the conversation read back", &loaded, &record)?;

    // A user turn is stored as received and starts at Received.
    let user = user_turn(&account, conversation, turn, epoch());
    ensure_ok(
        CHECK,
        "appending the user turn",
        conversations.append_user_turn(user.clone()).await,
    )?;
    ensure_error(
        CHECK,
        "appending the same turn twice",
        conversations.append_user_turn(user.clone()).await,
        &StoreError::Conflict,
    )?;
    let mut orphan = user_turn(&account, ConversationId::new(), TurnId::new(), epoch());
    orphan.received_at = at(1);
    ensure_error(
        CHECK,
        "appending a turn to a conversation that does not exist",
        conversations.append_user_turn(orphan).await,
        &StoreError::NotFound,
    )?;

    let marker = ensure_ok(
        CHECK,
        "reading the phase marker",
        conversations.turn_phase(&account, &turn).await,
    )?;
    ensure_eq(
        CHECK,
        "a new turn starts at Received",
        &marker.phase,
        &TurnPhase::Received,
    )?;
    ensure_eq(
        CHECK,
        "the marker carries its account",
        &marker.account_id,
        &account,
    )?;
    ensure_eq(
        CHECK,
        "the marker carries its conversation",
        &marker.conversation_id,
        &conversation,
    )?;

    for phase in [
        TurnPhase::Interpreted,
        TurnPhase::Executing,
        TurnPhase::Committed,
    ] {
        ensure_ok(
            CHECK,
            "advancing the phase marker",
            conversations.set_turn_phase(&account, &turn, phase).await,
        )?;
    }

    let unfinished = ensure_ok(
        CHECK,
        "listing unfinished turns",
        conversations
            .list_unfinished_turns(RecoveryScope::Account(account.clone()), 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "an unfinished turn is visible to recovery",
        &unfinished
            .iter()
            .map(|marker| marker.turn_id)
            .collect::<Vec<_>>(),
        &vec![turn],
    )?;

    // The assistant turn must survive the round trip byte for byte.
    let interaction = card(
        CHECK,
        card_spec("in-the-answer", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    let assistant = assistant_turn(conversation, turn, &interaction);
    ensure_code(
        CHECK,
        "an assistant turn addressed to another conversation",
        conversations
            .append_assistant_turn(&account, {
                let mut wrong = assistant.clone();
                wrong.conversation_id = ConversationId::new();
                wrong
            })
            .await,
        codes::IDENTITY_MISMATCH,
    )?;
    ensure_ok(
        CHECK,
        "persisting the assistant turn",
        conversations
            .append_assistant_turn(&account, assistant.clone())
            .await,
    )?;
    ensure_error(
        CHECK,
        "persisting a second assistant turn for the same user turn",
        conversations
            .append_assistant_turn(&account, assistant.clone())
            .await,
        &StoreError::Conflict,
    )?;

    let stored = ensure_ok(
        CHECK,
        "loading the turn",
        conversations.load_turn(&account, &turn).await,
    )?;
    ensure_eq(CHECK, "the user turn read back", &stored.user, &user)?;
    ensure_eq(
        CHECK,
        "the assistant turn must reload identically, blocks and order included",
        &stored.assistant,
        &Some(assistant.clone()),
    )?;
    let reloaded_block_ids = stored
        .assistant
        .as_ref()
        .map(|turn| turn.block_ids())
        .unwrap_or_default();
    ensure_eq(
        CHECK,
        "block identifiers in order",
        &reloaded_block_ids,
        &assistant.block_ids(),
    )?;

    // Terminal phases are final, and repeating one is accepted.
    ensure_ok(
        CHECK,
        "delivering the turn",
        conversations
            .set_turn_phase(&account, &turn, TurnPhase::Delivered)
            .await,
    )?;
    ensure_ok(
        CHECK,
        "delivering it again",
        conversations
            .set_turn_phase(&account, &turn, TurnPhase::Delivered)
            .await,
    )?;
    ensure_error(
        CHECK,
        "moving a delivered turn back",
        conversations
            .set_turn_phase(&account, &turn, TurnPhase::Composed)
            .await,
        &StoreError::Conflict,
    )?;
    let settled = ensure_ok(
        CHECK,
        "listing unfinished turns after delivery",
        conversations
            .list_unfinished_turns(RecoveryScope::AllAccounts, 10)
            .await,
    )?;
    ensure(
        CHECK,
        settled.is_empty(),
        "a delivered turn must not be offered to recovery",
    )?;

    // The recent-turn window is chronological and bounded.
    for seconds in [2_i64, 3, 4] {
        ensure_ok(
            CHECK,
            "appending a later turn",
            conversations
                .append_user_turn(user_turn(
                    &account,
                    conversation,
                    TurnId::new(),
                    at(seconds),
                ))
                .await,
        )?;
    }
    let recent = ensure_ok(
        CHECK,
        "loading the two most recent turns",
        conversations
            .load_recent_turns(&account, &conversation, 2)
            .await,
    )?;
    ensure_eq(CHECK, "the window is bounded", &recent.len(), &2)?;
    ensure(
        CHECK,
        recent
            .windows(2)
            .all(|pair| pair[0].user.received_at <= pair[1].user.received_at),
        "recent turns must come back oldest of the window first",
    )?;
    ensure_error(
        CHECK,
        "loading the recent turns of another tenant",
        conversations
            .load_recent_turns(&other_account(), &conversation, 2)
            .await,
        &StoreError::NotFound,
    )
}
