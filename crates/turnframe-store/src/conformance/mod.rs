//! An executable statement of the persistence contract.
//!
//! The traits in this crate carry their rules in prose, and prose does not
//! fail a build. This module turns every rule an adopter could plausibly get
//! wrong into a check that runs against *any* implementation through the
//! public API only. If your store passes, the runtime's guarantees hold on it;
//! if it fails, the failure names the rule and what it saw.
//!
//! It is always compiled — not behind a feature — because an adapter that
//! cannot be verified from a dependency is an adapter nobody will verify.
//!
//! # Running it
//!
//! Give [`run_all`] something that builds an **empty** set of stores. It is
//! called once per check, so checks never see each other's writes.
//!
//! ```rust
//! use turnframe_store::conformance;
//! use turnframe_store::stores::Stores;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let report = conformance::run_all(&Stores::in_memory).await;
//! assert!(report.passed(), "{report}");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # })?;
//! # Ok(())
//! # }
//! ```
//!
//! While an adapter is still being written, run one rule at a time: every
//! `check_*` function is public and takes a fresh [`Stores`].
//!
//! # It never panics
//!
//! Every check returns [`Result<(), ConformanceFailure>`](ConformanceFailure)
//! and [`run_all`] collects them into a [`ConformanceReport`]. Nothing here
//! unwraps, asserts or panics, so the suite is usable outside a test harness —
//! in a deployment health check, or as a gate in a migration tool. Turning a
//! failure into a test failure is the caller's choice: `assert!(report.passed())`,
//! or `report.into_result()?`.
//!
//! # What is covered
//!
//! | Check | Rule |
//! |---|---|
//! | [`check_blocking_interaction_conflict`] | at most one open blocking card per case (I5) |
//! | [`check_blocking_interaction_replace`] | replacing invalidates the occupant and mints a new id; a `Resolving` occupant is never replaced (spec §15.6) |
//! | [`check_cross_tenant_isolation`] | another tenant's record is `NotFound`, indistinguishable from absent (spec §25.4) |
//! | [`check_begin_resolution_cas`] | resolution starts only from the expected status |
//! | [`check_finish_resolution_idempotent`] | settling twice the same way is accepted, differently is `Conflict` |
//! | [`check_revision_invalidation_respects_independence`] | a revision change invalidates bound cards and spares independent ones (spec §15.5) |
//! | [`check_interaction_expiry`] | expiry moves `Active` cards past their deadline, once |
//! | [`check_answered_blocking_card_is_remembered`] | an answered card is remembered until the case moves |
//! | [`check_journal_idempotency_replay`] | one `Fresh` per key, ever; every repeat replays the persisted outcome (I14) |
//! | [`check_pending_for_turn_after_partial_write`] | a turn interrupted mid-flight is found by its unfinished entries (spec §23.1) |
//! | [`check_awaiting_confirmation_is_never_resumed`] | a command journaled for a card is not unfinished work until the card is confirmed |
//! | [`check_event_append_ordering_and_get_by_ids`] | append order is readback order; batches are atomic; reads are account-scoped |
//! | [`check_event_stream_cursor_pages_exactly_once`] | the sequence cursor delivers every event of an account once, in order, while the journal grows underneath |
//! | [`check_revision_read_truncates_inside_a_revision`] | the revision read caps its answer inside a revision and cannot resume — which is what the cursor read is for |
//! | [`check_event_redaction_preserves_identity_and_order`] | erasing a payload leaves the event, its position and its neighbours exactly where they were — it is not a delete |
//! | [`check_event_redaction_is_audited_and_idempotent`] | the erasure is recorded on the event with its authority, repeating it keeps the first record, and it cannot be aimed across tenants |
//! | [`check_outbox_claim_exclusivity_and_reschedule`] | a claimed row is invisible to other workers until it is rescheduled or settled |
//! | [`check_refused_settlement_writes_nothing`] | a refused settlement leaves the row byte-for-byte unchanged |
//! | [`check_replay_put_get`] | one record per turn, upserted |
//! | [`check_conversation_turn_persistence`] | an assistant turn reloads with identical, identically ordered blocks (spec §22.3) |
//! | [`check_commit_bundle_applies_all`] | a bundle writes every item it carries |
//! | [`check_commit_bundle_atomic_on_invalid_item`] | a bundle that fails writes nothing at all (spec §16.3) |
//! | [`check_commit_bundle_restores_modified_records`] | a bundle that fails also puts back every record it *changed*, not only the ones it created |
//! | [`check_commit_bundle_rejects_foreign_account_items`] | a bundle refuses an item of another tenant before writing |

mod commit_bundle;
mod conversations;
mod events;
mod fixtures;
mod interactions;
mod journal;
mod outbox;
mod replay;

use std::fmt;

use crate::stores::Stores;

pub use self::commit_bundle::{
    check_commit_bundle_applies_all, check_commit_bundle_atomic_on_invalid_item,
    check_commit_bundle_rejects_foreign_account_items,
    check_commit_bundle_restores_modified_records,
};
pub use self::conversations::check_conversation_turn_persistence;
pub use self::events::{
    check_event_append_ordering_and_get_by_ids, check_event_redaction_is_audited_and_idempotent,
    check_event_redaction_preserves_identity_and_order,
    check_event_stream_cursor_pages_exactly_once, check_revision_read_truncates_inside_a_revision,
};
pub use self::interactions::{
    check_administrative_invalidation_ignores_the_revision,
    check_answered_blocking_card_is_remembered, check_begin_resolution_cas,
    check_blocking_interaction_conflict, check_blocking_interaction_replace,
    check_cross_tenant_isolation, check_finish_resolution_idempotent, check_interaction_expiry,
    check_revision_invalidation_respects_independence,
};
pub use self::journal::{
    check_awaiting_confirmation_is_never_resumed, check_journal_idempotency_replay,
    check_pending_for_turn_after_partial_write,
};
pub use self::outbox::{
    check_outbox_claim_exclusivity_and_reschedule, check_refused_settlement_writes_nothing,
};
pub use self::replay::check_replay_put_get;

/// One rule of the persistence contract that an implementation broke.
///
/// `detail` names what the check expected and what it observed. It contains
/// fixture data only — the suite writes its own records — so it is safe to log
/// in full.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("conformance check `{check}` failed: {detail}")]
pub struct ConformanceFailure {
    /// Name of the failing check, matching its function name.
    pub check: &'static str,
    /// What was expected and what happened.
    pub detail: String,
}

impl ConformanceFailure {
    /// Builds a failure.
    #[must_use]
    pub fn new(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            detail: detail.into(),
        }
    }
}

/// What one check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Name of the check.
    pub check: &'static str,
    /// The failure, when it failed.
    pub failure: Option<ConformanceFailure>,
}

impl CheckOutcome {
    /// Returns `true` when the check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// The result of a whole [`run_all`].
///
/// `Display` renders one line per check, so `assert!(report.passed(), "{report}")`
/// prints a usable diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// Every check, in the order [`run_all`] ran them.
    pub outcomes: Vec<CheckOutcome>,
}

impl ConformanceReport {
    /// Returns `true` when every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(CheckOutcome::passed)
    }

    /// Every failure, in run order.
    pub fn failures(&self) -> impl Iterator<Item = &ConformanceFailure> {
        self.outcomes
            .iter()
            .filter_map(|outcome| outcome.failure.as_ref())
    }

    /// Turns the report into a `Result`, keeping the first failure.
    ///
    /// # Errors
    /// * The first [`ConformanceFailure`] in run order.
    pub fn into_result(self) -> Result<(), ConformanceFailure> {
        match self.outcomes.into_iter().find_map(|o| o.failure) {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failed = self.failures().count();
        writeln!(
            f,
            "{} of {} conformance checks passed",
            self.outcomes.len() - failed,
            self.outcomes.len()
        )?;
        for outcome in &self.outcomes {
            match &outcome.failure {
                None => writeln!(f, "  pass  {}", outcome.check)?,
                Some(failure) => writeln!(f, "  FAIL  {}: {}", outcome.check, failure.detail)?,
            }
        }
        Ok(())
    }
}

/// Builds an empty set of stores, once per check.
///
/// Any `Fn() -> Stores` implements it, so `&Stores::in_memory` or a closure
/// that opens a fresh schema both work.
pub trait StoreFactory: Send + Sync {
    /// Returns a set of stores with no records in it.
    fn build(&self) -> Stores;
}

impl<F> StoreFactory for F
where
    F: Fn() -> Stores + Send + Sync,
{
    fn build(&self) -> Stores {
        self()
    }
}

/// How many checks [`run_all`] runs.
///
/// Exported so a caller asserting full coverage cannot drift from the suite:
/// a check added here changes this constant, while an assertion written
/// against a literal would keep passing while covering less.
pub const CHECK_COUNT: usize = 25;

/// Runs every check against a fresh set of stores each and reports.
///
/// Never panics and never stops early: a broken implementation usually breaks
/// several rules, and seeing all of them at once is faster to fix.
pub async fn run_all(factory: &dyn StoreFactory) -> ConformanceReport {
    let mut outcomes = Vec::new();
    macro_rules! run {
        ($($check:path),* $(,)?) => {
            $(
                let stores = factory.build();
                outcomes.push(CheckOutcome {
                    check: stringify!($check).rsplit("::").next().unwrap_or(stringify!($check)),
                    failure: $check(&stores).await.err(),
                });
            )*
        };
    }
    run!(
        check_blocking_interaction_conflict,
        check_blocking_interaction_replace,
        check_cross_tenant_isolation,
        check_begin_resolution_cas,
        check_finish_resolution_idempotent,
        check_revision_invalidation_respects_independence,
        check_interaction_expiry,
        check_answered_blocking_card_is_remembered,
        check_administrative_invalidation_ignores_the_revision,
        check_journal_idempotency_replay,
        check_pending_for_turn_after_partial_write,
        check_awaiting_confirmation_is_never_resumed,
        check_event_append_ordering_and_get_by_ids,
        check_event_stream_cursor_pages_exactly_once,
        check_revision_read_truncates_inside_a_revision,
        check_event_redaction_preserves_identity_and_order,
        check_event_redaction_is_audited_and_idempotent,
        check_outbox_claim_exclusivity_and_reschedule,
        check_refused_settlement_writes_nothing,
        check_replay_put_get,
        check_conversation_turn_persistence,
        check_commit_bundle_applies_all,
        check_commit_bundle_atomic_on_invalid_item,
        check_commit_bundle_restores_modified_records,
        check_commit_bundle_rejects_foreign_account_items,
    );
    ConformanceReport { outcomes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_renders_and_keeps_the_first_failure() {
        let report = ConformanceReport {
            outcomes: vec![
                CheckOutcome {
                    check: "check_a",
                    failure: None,
                },
                CheckOutcome {
                    check: "check_b",
                    failure: Some(ConformanceFailure::new("check_b", "expected 1, got 2")),
                },
            ],
        };
        assert!(!report.passed());
        assert_eq!(report.failures().count(), 1);
        let rendered = report.to_string();
        assert!(rendered.contains("1 of 2 conformance checks passed"));
        assert!(rendered.contains("FAIL  check_b: expected 1, got 2"));
        assert_eq!(
            report.into_result().unwrap_err().check,
            "check_b",
            "into_result keeps the first failure"
        );
    }

    #[test]
    fn empty_report_passes() {
        let report = ConformanceReport {
            outcomes: Vec::new(),
        };
        assert!(report.passed());
        assert!(report.into_result().is_ok());
    }
}
