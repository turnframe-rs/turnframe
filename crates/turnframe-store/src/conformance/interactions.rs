//! Checks for [`InteractionStore`](crate::interaction::InteractionStore).

use turnframe_core::ids::{CaseRevision, ConversationId, InteractionId, OptionId, TurnId};
use turnframe_core::interaction::InteractionStatus;

use super::ConformanceFailure;
use super::fixtures::{
    account, at, card, card_spec, case, case_key, ensure, ensure_eq, ensure_error, ensure_ok,
    epoch, journal_entry, other_account, other_case, ttl, user_turn,
};
use crate::conversation::ConversationRecord;
use crate::error::StoreError;
use crate::interaction::{InvalidationReason, ResolutionOutcome};
use crate::stores::Stores;

/// A case holds at most one open blocking card (I5, spec §15.6).
///
/// Proves the slot is per case and per account, that non-blocking cards do not
/// take it, and that a duplicate identifier is refused.
pub async fn check_blocking_interaction_conflict(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_blocking_interaction_conflict";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    let first = card(
        CHECK,
        card_spec("first", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the first blocking card",
        interactions.insert(first.clone()).await,
    )?;

    let second = card(
        CHECK,
        card_spec("second", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    ensure_error(
        CHECK,
        "a second blocking card on the same case",
        interactions.insert(second).await,
        &StoreError::Conflict,
    )?;

    let non_blocking = card(
        CHECK,
        card_spec("aside", case(1)).non_blocking(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(2),
    )?;
    ensure_ok(
        CHECK,
        "a non-blocking card on the same case does not take the slot",
        interactions.insert(non_blocking).await,
    )?;

    let other = card(
        CHECK,
        card_spec("other-case", other_case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(3),
    )?;
    ensure_ok(
        CHECK,
        "a blocking card on another case",
        interactions.insert(other).await,
    )?;

    let elsewhere = card(
        CHECK,
        card_spec("other-tenant", case(1)),
        InteractionId::new(),
        other_account(),
        conversation,
        turn,
        at(4),
    )?;
    ensure_ok(
        CHECK,
        "the slot is per account, so another tenant may hold its own",
        interactions.insert(elsewhere).await,
    )?;

    ensure_error(
        CHECK,
        "re-inserting the same identifier",
        interactions.insert(first.clone()).await,
        &StoreError::Conflict,
    )?;

    let open = ensure_ok(
        CHECK,
        "listing the open cards of the case",
        interactions.list_open_for_case(&account, &case_key()).await,
    )?;
    ensure_eq(CHECK, "open cards on the case", &open.len(), &2)?;
    ensure_eq(CHECK, "the first open card", &open[0].id, &first.id)
}

/// Replacing the blocking card invalidates the occupant and mints a new
/// identifier; a `Resolving` occupant is never replaced (spec §15.6).
pub async fn check_blocking_interaction_replace(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_blocking_interaction_replace";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    let first = card(
        CHECK,
        card_spec("first", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the occupant",
        interactions.insert(first.clone()).await,
    )?;

    let replacement = card(
        CHECK,
        card_spec("replacement", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    let invalidated = ensure_ok(
        CHECK,
        "replacing the occupant",
        interactions
            .insert_replacing_blocking(replacement.clone())
            .await,
    )?;
    ensure_eq(
        CHECK,
        "invalidated occupants",
        &invalidated,
        &vec![first.id],
    )?;
    ensure(
        CHECK,
        replacement.id != first.id,
        "the replacement must carry a new identifier",
    )?;

    let old = ensure_ok(
        CHECK,
        "reading the replaced card",
        interactions.get(&account, &first.id).await,
    )?;
    ensure_eq(
        CHECK,
        "status of the replaced card",
        &old.status(),
        &InteractionStatus::Invalidated,
    )?;
    ensure_eq(
        CHECK,
        "reason recorded on the replaced card",
        &old.invalidation.map(|record| record.reason),
        &Some(InvalidationReason::Superseded { by: replacement.id }),
    )?;

    let current = ensure_ok(
        CHECK,
        "reading the replacement",
        interactions.get(&account, &replacement.id).await,
    )?;
    ensure_eq(
        CHECK,
        "status of the replacement",
        &current.status(),
        &InteractionStatus::Active,
    )?;

    // A card whose commands are executing must not be swept away underneath them.
    ensure_ok(
        CHECK,
        "starting resolution of the replacement",
        interactions
            .begin_resolution(
                &account,
                &replacement.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    let third = card(
        CHECK,
        card_spec("third", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(2),
    )?;
    ensure_error(
        CHECK,
        "replacing a card that is already resolving",
        interactions.insert_replacing_blocking(third).await,
        &StoreError::Conflict,
    )
}

/// Another tenant's records are `NotFound`, never a different error
/// (spec §25.4).
pub async fn check_cross_tenant_isolation(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_cross_tenant_isolation";
    let account = account();
    let intruder = other_account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    ensure_ok(
        CHECK,
        "creating the conversation",
        stores
            .conversations()
            .create_conversation(ConversationRecord::new(
                conversation,
                account.clone(),
                epoch(),
            ))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "appending the user turn",
        stores
            .conversations()
            .append_user_turn(user_turn(&account, conversation, turn, epoch()))
            .await,
    )?;
    let interaction = card(
        CHECK,
        card_spec("private", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the card",
        stores.interactions().insert(interaction.clone()).await,
    )?;
    let command = turnframe_core::ids::CommandId::new();
    ensure_ok(
        CHECK,
        "admitting the command",
        stores
            .journal()
            .begin(journal_entry(&account, turn, command, "isolation", epoch()))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "writing the replay record",
        stores
            .replay()
            .put(turnframe_core::replay::ReplayRecord::received(
                turn,
                conversation,
                account.clone(),
                epoch(),
            ))
            .await,
    )?;

    ensure_error(
        CHECK,
        "loading another tenant's conversation",
        stores
            .conversations()
            .load_conversation(&intruder, &conversation)
            .await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "loading another tenant's turn",
        stores.conversations().load_turn(&intruder, &turn).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading another tenant's phase marker",
        stores.conversations().turn_phase(&intruder, &turn).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading another tenant's card",
        stores.interactions().get(&intruder, &interaction.id).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "resolving another tenant's card",
        stores
            .interactions()
            .begin_resolution(
                &intruder,
                &interaction.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "settling another tenant's card",
        stores
            .interactions()
            .finish_resolution(
                &intruder,
                &interaction.id,
                ResolutionOutcome::Failed {
                    code: "nope".to_owned(),
                },
            )
            .await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading another tenant's journal entry",
        stores.journal().get(&intruder, &command).await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "reading another tenant's replay record",
        stores.replay().get(&intruder, &turn).await,
        &StoreError::NotFound,
    )?;

    let listed = ensure_ok(
        CHECK,
        "listing another tenant's open cards",
        stores
            .interactions()
            .list_open_for_conversation(&intruder, &conversation)
            .await,
    )?;
    ensure(
        CHECK,
        listed.is_empty(),
        "another tenant must see no cards of this conversation",
    )?;
    let entries = ensure_ok(
        CHECK,
        "listing another tenant's journal entries",
        stores.journal().for_turn(&intruder, &turn).await,
    )?;
    ensure(
        CHECK,
        entries.is_empty(),
        "another tenant must see no journal entries of this turn",
    )?;

    // An identifier that never existed answers exactly the same way.
    ensure_error(
        CHECK,
        "reading an identifier that never existed",
        stores
            .interactions()
            .get(&account, &InteractionId::new())
            .await,
        &StoreError::NotFound,
    )
}

/// Resolution starts only from the status the caller expected.
pub async fn check_begin_resolution_cas(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_begin_resolution_cas";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let interaction = card(
        CHECK,
        card_spec("cas", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the card",
        interactions.insert(interaction.clone()).await,
    )?;

    ensure_error(
        CHECK,
        "expecting Resolving on an Active card",
        interactions
            .begin_resolution(
                &account,
                &interaction.id,
                InteractionStatus::Resolving,
                OptionId::from("ack"),
                turn,
            )
            .await,
        &StoreError::Conflict,
    )?;
    ensure_error(
        CHECK,
        "resolving an identifier that does not exist",
        interactions
            .begin_resolution(
                &account,
                &InteractionId::new(),
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
        &StoreError::NotFound,
    )?;

    let record = ensure_ok(
        CHECK,
        "starting resolution from Active",
        interactions
            .begin_resolution(
                &account,
                &interaction.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    ensure_eq(
        CHECK,
        "status after begin_resolution",
        &record.status(),
        &InteractionStatus::Resolving,
    )?;
    ensure_eq(
        CHECK,
        "option recorded on the card",
        &record.interaction.resolved_option_id,
        &Some(OptionId::from("ack")),
    )?;
    ensure_eq(
        CHECK,
        "turn recorded on the card",
        &record.resolved_by_turn,
        &Some(turn),
    )?;

    // The second click loses the race: it must see it, not silently re-resolve.
    ensure_error(
        CHECK,
        "a second begin_resolution expecting Active",
        interactions
            .begin_resolution(
                &account,
                &interaction.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
        &StoreError::Conflict,
    )?;
    ensure_error(
        CHECK,
        "beginning resolution from Resolving",
        interactions
            .begin_resolution(
                &account,
                &interaction.id,
                InteractionStatus::Resolving,
                OptionId::from("ack"),
                turn,
            )
            .await,
        &StoreError::Conflict,
    )
}

/// Settling the same way twice is accepted; settling differently is a conflict.
pub async fn check_finish_resolution_idempotent(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_finish_resolution_idempotent";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let events = vec![turnframe_core::ids::EventId::new()];

    let resolved = card(
        CHECK,
        card_spec("resolved", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the card",
        interactions.insert(resolved.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "starting resolution",
        interactions
            .begin_resolution(
                &account,
                &resolved.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    let settled = ensure_ok(
        CHECK,
        "settling the card as Resolved",
        interactions
            .finish_resolution(
                &account,
                &resolved.id,
                ResolutionOutcome::Resolved {
                    event_ids: events.clone(),
                },
            )
            .await,
    )?;
    ensure_eq(
        CHECK,
        "status after Resolved",
        &settled.status(),
        &InteractionStatus::Resolved,
    )?;
    ensure_eq(
        CHECK,
        "events backing the resolution",
        &settled.resolution_event_ids,
        &events,
    )?;

    let repeat = ensure_ok(
        CHECK,
        "settling the same way again",
        interactions
            .finish_resolution(
                &account,
                &resolved.id,
                ResolutionOutcome::Resolved {
                    event_ids: events.clone(),
                },
            )
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the repeated finish must change nothing",
        &repeat,
        &settled,
    )?;
    ensure_error(
        CHECK,
        "settling as Resolved with different events",
        interactions
            .finish_resolution(
                &account,
                &resolved.id,
                ResolutionOutcome::Resolved {
                    event_ids: vec![turnframe_core::ids::EventId::new()],
                },
            )
            .await,
        &StoreError::Conflict,
    )?;
    ensure_error(
        CHECK,
        "settling a resolved card as Failed",
        interactions
            .finish_resolution(
                &account,
                &resolved.id,
                ResolutionOutcome::Failed {
                    code: "late".to_owned(),
                },
            )
            .await,
        &StoreError::Conflict,
    )?;

    // RestoreActive puts the card back in the user's hands, clearing the answer.
    let restored = card(
        CHECK,
        card_spec("restored", other_case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    ensure_ok(
        CHECK,
        "inserting the card to restore",
        interactions.insert(restored.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "starting resolution of the card to restore",
        interactions
            .begin_resolution(
                &account,
                &restored.id,
                InteractionStatus::Active,
                OptionId::from("ack"),
                turn,
            )
            .await,
    )?;
    let back = ensure_ok(
        CHECK,
        "restoring the card",
        interactions
            .finish_resolution(&account, &restored.id, ResolutionOutcome::RestoreActive)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "status after RestoreActive",
        &back.status(),
        &InteractionStatus::Active,
    )?;
    ensure_eq(
        CHECK,
        "the chosen option is cleared so the card is answerable again",
        &back.interaction.resolved_option_id,
        &None,
    )?;
    ensure_ok(
        CHECK,
        "restoring an already restored card",
        interactions
            .finish_resolution(&account, &restored.id, ResolutionOutcome::RestoreActive)
            .await,
    )
    .map(|_| ())
}

/// A revision change invalidates bound cards and spares independent ones
/// (spec §15.5).
pub async fn check_revision_invalidation_respects_independence(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_revision_invalidation_respects_independence";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    let bound = card(
        CHECK,
        card_spec("bound", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    let independent = card(
        CHECK,
        card_spec("independent", case(1))
            .non_blocking()
            .revision_independent(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(1),
    )?;
    let current = card(
        CHECK,
        card_spec("current", case(2)).non_blocking(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(2),
    )?;
    let elsewhere = card(
        CHECK,
        card_spec("elsewhere", other_case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        at(3),
    )?;
    for (what, interaction) in [
        ("bound", &bound),
        ("independent", &independent),
        ("current", &current),
        ("elsewhere", &elsewhere),
    ] {
        ensure_ok(
            CHECK,
            &format!("inserting the {what} card"),
            interactions.insert(interaction.clone()).await,
        )?;
    }

    let invalidated = ensure_ok(
        CHECK,
        "invalidating the case at revision 2",
        interactions
            .invalidate_for_case(
                &account,
                &case_key(),
                turnframe_core::ids::CaseRevision(2),
                InvalidationReason::RevisionChanged,
            )
            .await,
    )?;
    ensure_eq(
        CHECK,
        "only the card bound to the old revision is invalidated",
        &invalidated,
        &vec![bound.id],
    )?;

    let stale = ensure_ok(
        CHECK,
        "reading the invalidated card",
        interactions.get(&account, &bound.id).await,
    )?;
    ensure_eq(
        CHECK,
        "status of the invalidated card",
        &stale.status(),
        &InteractionStatus::Invalidated,
    )?;
    ensure_eq(
        CHECK,
        "reason recorded on the invalidated card",
        &stale.invalidation.as_ref().map(|record| &record.reason),
        &Some(&InvalidationReason::RevisionChanged),
    )?;
    ensure_eq(
        CHECK,
        "the revision that made the card stale",
        &stale.invalidation.and_then(|record| record.new_revision),
        &Some(turnframe_core::ids::CaseRevision(2)),
    )?;

    for (what, id) in [
        ("revision-independent", independent.id),
        ("already current", current.id),
        ("on another case", elsewhere.id),
    ] {
        let survivor = ensure_ok(
            CHECK,
            &format!("reading the {what} card"),
            interactions.get(&account, &id).await,
        )?;
        ensure_eq(
            CHECK,
            &format!("status of the {what} card"),
            &survivor.status(),
            &InteractionStatus::Active,
        )?;
    }
    Ok(())
}

/// A card the user answered is remembered until the case moves.
///
/// A `ConfirmCommand` must offer a way to decline, and declining ends the card
/// without effect: nothing is written and the case does not move. So the
/// projection declares the same requirement, the same card goes back up, and
/// the user who pressed "not now" is asked again. The runtime does not re-raise
/// a requirement whose card was answered at the revision the case is still on,
/// and this is the query that lets it know.
pub async fn check_answered_blocking_card_is_remembered(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_answered_blocking_card_is_remembered";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let key = case(1).key();

    ensure_eq(
        CHECK,
        "nothing has been answered before anything exists",
        &ensure_ok(
            CHECK,
            "asking about a case with no cards",
            interactions
                .blocking_answered_at(&account, &key, CaseRevision(1))
                .await,
        )?,
        &false,
    )?;

    let interaction = card(
        CHECK,
        card_spec("send_confirmation", case(1)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the card",
        interactions.insert(interaction.clone()).await,
    )?;
    ensure_eq(
        CHECK,
        "a card on screen has not been answered",
        &ensure_ok(
            CHECK,
            "asking about an active card",
            interactions
                .blocking_answered_at(&account, &key, CaseRevision(1))
                .await,
        )?,
        &false,
    )?;

    ensure_ok(
        CHECK,
        "beginning the resolution",
        interactions
            .begin_resolution(
                &account,
                &interaction.id,
                InteractionStatus::Active,
                OptionId::from("not_now"),
                turn,
            )
            .await,
    )?;
    ensure_ok(
        CHECK,
        "settling it with nothing committed, which is what declining is",
        interactions
            .finish_resolution(
                &account,
                &interaction.id,
                ResolutionOutcome::Resolved {
                    event_ids: Vec::new(),
                },
            )
            .await,
    )?;

    ensure_eq(
        CHECK,
        "the answer is remembered at the revision it was given at",
        &ensure_ok(
            CHECK,
            "asking after the answer",
            interactions
                .blocking_answered_at(&account, &key, CaseRevision(1))
                .await,
        )?,
        &true,
    )?;
    ensure_eq(
        CHECK,
        "and the question is open again once the case has moved",
        &ensure_ok(
            CHECK,
            "asking at the next revision",
            interactions
                .blocking_answered_at(&account, &key, CaseRevision(2))
                .await,
        )?,
        &false,
    )?;
    Ok(())
}

/// Expiry moves `Active` cards whose deadline has passed, and only those.
pub async fn check_interaction_expiry(stores: &Stores) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_interaction_expiry";
    let interactions = stores.interactions();
    let account = account();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    let expiring = card(
        CHECK,
        card_spec("expiring", case(1)).expires_in(ttl(60)),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    let permanent = card(
        CHECK,
        card_spec("permanent", case(1)).non_blocking(),
        InteractionId::new(),
        account.clone(),
        conversation,
        turn,
        epoch(),
    )?;
    ensure_ok(
        CHECK,
        "inserting the expiring card",
        interactions.insert(expiring.clone()).await,
    )?;
    ensure_ok(
        CHECK,
        "inserting the card without a deadline",
        interactions.insert(permanent.clone()).await,
    )?;

    let early = ensure_ok(
        CHECK,
        "sweeping before the deadline",
        interactions.expire_due(at(30)).await,
    )?;
    ensure(
        CHECK,
        early.is_empty(),
        "a card must not expire before its deadline",
    )?;

    let due = ensure_ok(
        CHECK,
        "sweeping at the deadline",
        interactions.expire_due(at(60)).await,
    )?;
    ensure_eq(
        CHECK,
        "cards expired by the sweep",
        &due,
        &vec![expiring.id],
    )?;
    let expired = ensure_ok(
        CHECK,
        "reading the expired card",
        interactions.get(&account, &expiring.id).await,
    )?;
    ensure_eq(
        CHECK,
        "status after expiry",
        &expired.status(),
        &InteractionStatus::Expired,
    )?;
    let kept = ensure_ok(
        CHECK,
        "reading the card without a deadline",
        interactions.get(&account, &permanent.id).await,
    )?;
    ensure_eq(
        CHECK,
        "a card without a deadline is untouched",
        &kept.status(),
        &InteractionStatus::Active,
    )?;

    let again = ensure_ok(
        CHECK,
        "sweeping a second time",
        interactions.expire_due(at(600)).await,
    )?;
    ensure(
        CHECK,
        again.is_empty(),
        "a card must expire once, not on every sweep",
    )
}

/// An operator can withdraw a card for a reason the revision does not describe
/// (spec §15.5).
///
/// Revision-driven invalidation cannot express this: it fires only for a card
/// bound to a revision the case has left, so a decision that has nothing to do
/// with the revision — a workflow rolled back to a version that cannot compile
/// the option the card offers, an account suspended, a card withdrawn — would
/// have no way to run, and the honest step would be to leave the user holding
/// an option nothing will honour.
///
/// A card mid-resolution is deliberately spared: a command it authorized is in
/// flight, and taking the card away underneath it would settle nothing while
/// making the outcome unattributable.
pub async fn check_administrative_invalidation_ignores_the_revision(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_administrative_invalidation_ignores_the_revision";
    let interactions = stores.interactions();
    let account = account();
    let case_ref = case(4);
    let conversation = ConversationId::new();

    // Two cards on the same case at its current revision: the blocking one the
    // case is waiting on, and a non-blocking one that declares itself
    // independent of the revision. Neither is reachable by the revision-driven
    // path, and only one may block, which is the rule that makes the second
    // non-blocking rather than a second blocking card.
    let plain_id = InteractionId::new();
    let independent_id = InteractionId::new();
    let plain = card(
        CHECK,
        card_spec("administrative.plain", case_ref.clone()),
        plain_id,
        account.clone(),
        conversation,
        TurnId::new(),
        epoch(),
    )?;
    let independent = card(
        CHECK,
        card_spec("administrative.independent", case_ref.clone())
            .non_blocking()
            .revision_independent(),
        independent_id,
        account.clone(),
        conversation,
        TurnId::new(),
        at(1),
    )?;
    ensure_ok(
        CHECK,
        "writing a card at the current revision",
        interactions.insert(plain).await,
    )?;
    ensure_ok(
        CHECK,
        "writing a revision-independent card",
        interactions.insert(independent).await,
    )?;

    let by_revision = ensure_ok(
        CHECK,
        "sweeping by revision at the revision the cards are bound to",
        interactions
            .invalidate_for_case(
                &account,
                &case_ref.key(),
                case_ref.expected_revision,
                InvalidationReason::RevisionChanged,
            )
            .await,
    )?;
    ensure(
        CHECK,
        by_revision.is_empty(),
        "the revision-driven sweep finds nothing while the case has not moved",
    )?;

    let withdrawn = ensure_ok(
        CHECK,
        "withdrawing the case's cards administratively",
        interactions
            .invalidate_case_cards(
                &account,
                &case_ref.key(),
                InvalidationReason::Administrative {
                    code: String::from("conformance.withdrawn"),
                },
            )
            .await,
    )?;
    ensure(
        CHECK,
        withdrawn.contains(&plain_id) && withdrawn.contains(&independent_id),
        "both cards are withdrawn, the revision-independent one included",
    )?;

    for id in [plain_id, independent_id] {
        let record = ensure_ok(
            CHECK,
            "reading a withdrawn card",
            interactions.get(&account, &id).await,
        )?;
        ensure_eq(
            CHECK,
            "status after an administrative withdrawal",
            &record.interaction.status,
            &InteractionStatus::Invalidated,
        )?;
    }

    Ok(())
}
