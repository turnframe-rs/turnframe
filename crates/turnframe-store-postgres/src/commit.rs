//! The all-or-nothing write of a [`CommitBundle`] (spec §16.3, §23 step N).
//!
//! Everything one commit produces — the journal outcome of each command, the
//! events, the resolution of the card that authorized them, the cards the new
//! revision invalidates, the cards the new state requires, the outbox rows, the
//! replay record and the phase marker — travels in one bundle and lands in one
//! PostgreSQL transaction, in the normative order of
//! [`turnframe_store::commit`].
//!
//! Every item goes through exactly the same function its own trait uses, so it
//! obeys exactly the same rules: the same compare-and-swap, the same uniqueness,
//! the same tenant scoping. The only difference is that the transaction is not
//! committed until the last item has succeeded. An item that fails returns its
//! error, the transaction is dropped, PostgreSQL rolls it back, and nothing the
//! bundle carried was ever visible to another connection.
//!
//! What is deliberately *outside* this transaction is the domain executor's own
//! state commit, which may live in another database; Turnframe does not attempt
//! a distributed transaction. When the domain tables do live here, enlist them
//! with [`PgStores::commit_in`] and the two become one transaction.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use turnframe_core::ids::AccountId;
use turnframe_store::commit::{CommitBundle, CommitReceipt, CommitStore};
use turnframe_store::error::StoreError;

use crate::codec::now;
use crate::store::{PgStores, commit as commit_transaction};
use crate::{conversations, events, interactions, journal, outbox, replay};

/// Applies every item of `bundle` on `conn`, in the contract's order.
///
/// The caller owns the transaction: nothing here commits, and returning an error
/// leaves it to be rolled back.
pub(crate) async fn apply_bundle(
    conn: &mut PgConnection,
    account: &AccountId,
    bundle: CommitBundle,
    at: DateTime<Utc>,
) -> Result<CommitReceipt, StoreError> {
    // The account and empty-batch checks every implementation owes, before a
    // single row is touched.
    bundle.validate(account)?;
    let mut event_ids = Vec::new();
    let mut inserted_interactions = Vec::new();
    let mut invalidated_interactions = Vec::new();

    for completion in bundle.journal_completions {
        journal::complete(
            &mut *conn,
            account,
            &completion.command_id,
            completion.outcome,
            at,
        )
        .await?;
    }

    for batch in bundle.events {
        event_ids.extend(events::append(&mut *conn, batch).await?);
    }

    for finish in bundle.interaction_finishes {
        interactions::finish_resolution(
            &mut *conn,
            account,
            &finish.interaction_id,
            finish.outcome,
        )
        .await?;
    }

    // Invalidation comes before the inserts: a card the new revision retires
    // must leave the blocking slot before the card that replaces it takes it.
    for invalidation in bundle.interaction_invalidations {
        invalidated_interactions.extend(
            interactions::invalidate_for_case(
                &mut *conn,
                account,
                &invalidation.case_key,
                invalidation.new_revision,
                invalidation.reason,
                at,
            )
            .await?,
        );
    }

    for insert in bundle.interaction_inserts {
        let id = insert.interaction.id;
        invalidated_interactions.extend(
            interactions::insert_interaction(
                &mut *conn,
                insert.interaction,
                insert.replace_blocking,
                at,
            )
            .await?,
        );
        inserted_interactions.push(id);
    }

    for entry in bundle.outbox_entries {
        outbox::enqueue(&mut *conn, entry).await?;
    }

    if let Some(record) = bundle.replay_record {
        replay::put(&mut *conn, record).await?;
    }

    if let Some(update) = bundle.turn_phase {
        conversations::set_turn_phase(&mut *conn, account, &update.turn_id, update.phase, at)
            .await?;
    }

    Ok(CommitReceipt {
        event_ids,
        inserted_interactions,
        invalidated_interactions,
        committed_at: at,
    })
}

impl PgStores {
    /// Applies a bundle inside a transaction the caller owns.
    ///
    /// This is the seam an adopter needs when the domain tables live in the same
    /// database as the stores: begin one transaction, run the workflow
    /// executor's own writes on it, hand it to this method, and commit once. The
    /// journal admission still happens before execution, so recovery works the
    /// same way if the process dies — the transaction simply removes the window
    /// in which the domain state and the bookkeeping could disagree.
    ///
    /// Nothing is committed here. Commit the transaction to make the bundle
    /// visible; drop it to discard the bundle along with your own writes.
    ///
    /// ```rust,no_run
    /// use turnframe_core::ids::AccountId;
    /// use turnframe_store::commit::CommitBundle;
    /// use turnframe_store_postgres::PgStores;
    ///
    /// # async fn example(store: &PgStores, bundle: CommitBundle) -> Result<(), Box<dyn std::error::Error>> {
    /// let mut transaction = store.pool().begin().await?;
    /// // ... the executor's own writes go here, on the same transaction ...
    /// let receipt = store
    ///     .commit_in(&mut transaction, &AccountId::from("aurora"), bundle)
    ///     .await?;
    /// transaction.commit().await?;
    /// # let _ = receipt;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// Any error an item would raise through its own store trait, plus the
    /// refusals of [`CommitBundle::validate`].
    pub async fn commit_in(
        &self,
        conn: &mut PgConnection,
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, StoreError> {
        apply_bundle(conn, account, bundle, now()).await
    }
}

#[async_trait]
impl CommitStore for PgStores {
    async fn commit(
        &self,
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, StoreError> {
        // Refuse a foreign item before a connection is even taken.
        bundle.validate(account)?;
        let mut transaction = self.transaction().await?;
        let receipt = apply_bundle(&mut transaction, account, bundle, now()).await?;
        commit_transaction(transaction).await?;
        Ok(receipt)
    }
}
