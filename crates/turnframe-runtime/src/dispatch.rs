//! The outbox dispatcher: the second half of the external-effect saga
//! (spec §16.4, §16.5, ADR-007).
//!
//! [`execute`](crate::execute) enqueues an outbox row inside the turn's one
//! atomic write, and stops there — deliberately, because the row must be
//! durable before anything leaves the building. Somebody then has to pick the
//! row up and call the remote system. That is this module: a reference
//! dispatcher an adopter can use as it stands, or read and replace.
//!
//! # What it is not
//!
//! **It is not a background thread.** Nothing here spawns anything. The library
//! never starts work an application did not ask for: [`OutboxDispatcher`] has
//! one method that does a unit of work, [`OutboxDispatcher::run_once`], and the
//! application drives it from its own task, its own scheduler or its own
//! cron job. A hidden worker would dispatch external effects out of a process
//! that was only supposed to answer a turn, and would keep doing it while the
//! operator was shutting the process down.
//!
//! ```rust,ignore
//! // The application's own task. Cancel it, pause it, scale it — it is yours.
//! let mut ticker = tokio::time::interval(Duration::from_secs(1));
//! loop {
//!     tokio::select! {
//!         _ = shutdown.cancelled() => break,
//!         _ = ticker.tick() => {
//!             match dispatcher.run_once(Utc::now()).await {
//!                 Ok(report) => tracing::debug!(dispatched = report.claimed),
//!                 Err(error) => tracing::warn!(%error, "the outbox could not be read"),
//!             }
//!         }
//!     }
//! }
//! ```
//!
//! # The four ways one row ends
//!
//! [`OutboxSender::send`] classifies its own outcome, and the classification is
//! the whole safety contract of the module:
//!
//! | [`Dispatched`] | What the row becomes | Why |
//! | --- | --- | --- |
//! | [`Completed`](Dispatched::Completed) | `Completed` | the remote confirmed |
//! | [`Retryable`](Dispatched::Retryable) | `Pending` with a backoff, or `Failed` once the attempts are spent | the request demonstrably did not arrive |
//! | [`Permanent`](Dispatched::Permanent) | `Failed` | the remote refused, and will refuse again |
//! | [`Unknown`](Dispatched::Unknown) | `OutcomeUnknown` | the effect **may** exist, and repeating it is the duplicate the library exists to prevent (I15) |
//!
//! A send that does not answer within
//! [`DispatchConfig::send_timeout`] is [`Unknown`](Dispatched::Unknown), never a
//! retry. That is the same rule the executor applies to a domain timeout, for
//! the same reason: the request left, so nobody can say it did not land.
//!
//! # Exclusivity is the store's, and this module honours it
//!
//! [`OutboxWriter::claim_due`](turnframe_store::outbox::OutboxWriter::claim_due) moves rows to `Dispatching` under a worker
//! identifier with skip-locked semantics, so two dispatchers claim disjoint
//! sets. This module never bypasses it — it dispatches exactly what a claim
//! returned — and it never invents an idempotency key: the one the command was
//! admitted under travels on the row and is handed to the sender, so a remote
//! that deduplicates can.
//!
//! # Unknown outcomes are reconciled, not retried
//!
//! A row in `OutcomeUnknown` is out of the dispatcher's hands: only the
//! application knows how to ask the remote system what happened.
//! [`OutboxDispatcher::reconcile`] is the hook — it hands the stored record to
//! an [`OutboxReconciler`] and settles the row with the answer, including
//! putting it back in the queue when the remote is certain it never arrived.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use turnframe_core::error::StoreError;
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::ids::OutboxId;
use turnframe_core::observe::{NoopObserver, Observer, Signal, SignalLabels};
use turnframe_store::outbox::{OutboxRecord, OutboxStore};

use crate::signals::Stage;

/// Stable codes this module records on a row it settled itself.
///
/// There is exactly one: every other settlement carries a code its author
/// chose, and inventing a second vocabulary next to theirs would only make a
/// dashboard harder to read.
pub mod code {
    /// The retry budget of the row ran out, so a retryable failure became a
    /// permanent one.
    pub const ATTEMPTS_EXHAUSTED: &str = "turnframe.dispatch.attempts_exhausted";
}

/// How one send ended, as the sender classifies it.
///
/// There is no `Result` around it on purpose. Every failure mode of an external
/// call has to land in exactly one of these four, and an author who returns an
/// error instead of choosing has not answered the only question that matters:
/// *may this be sent again?*
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Dispatched {
    /// The remote system accepted it, and said so.
    Completed {
        /// Reference the remote gave, when it gave one.
        remote_ref: Option<String>,
    },
    /// It did not arrive, and sending it again is safe.
    Retryable {
        /// Stable code, never free text.
        code: String,
    },
    /// The remote refused it, and would refuse it again.
    Permanent {
        /// Stable code, never free text.
        code: String,
    },
    /// The request left and no answer came back. The effect may exist (I15).
    Unknown {
        /// Reference the remote gave before it went quiet, when it gave one.
        remote_ref: Option<String>,
    },
}

impl Dispatched {
    /// The remote accepted it, with no reference.
    #[must_use]
    pub const fn completed() -> Self {
        Self::Completed { remote_ref: None }
    }

    /// Stable snake-case label of the outcome.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::Retryable { .. } => "retryable",
            Self::Permanent { .. } => "permanent",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Calls the external system for one outbox row.
///
/// The implementation owns the transport and the credentials; this module owns
/// the bookkeeping. Two obligations are the application's:
///
/// * **forward [`OutboxEntry::idempotency_key`]** to the remote, in whatever
///   header or field it deduplicates on. It is the same key the command was
///   admitted under, so a row dispatched twice after a crash is one effect;
/// * **classify honestly.** A transport error after the bytes were written is
///   [`Dispatched::Unknown`], not [`Dispatched::Retryable`]. If you cannot tell
///   the two apart, it is `Unknown`.
#[async_trait]
pub trait OutboxSender: Send + Sync {
    /// Sends one row.
    async fn send(&self, entry: &OutboxEntry) -> Dispatched;
}

/// What a reconciler found out about a row whose outcome was unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reconciled {
    /// The remote has it. The row is `Completed`.
    Completed,
    /// The remote definitively does not have it, and never will. The row is
    /// `Failed`.
    Failed {
        /// Stable code, never free text.
        code: String,
    },
    /// The remote definitively never received it, so it may be queued again.
    /// Only answer this when the remote is *certain*: it is the one path that
    /// can turn an unknown outcome back into a second send.
    Resend,
    /// Still unknown. The row is left exactly as it is, for the next sweep.
    Unresolved,
}

impl Reconciled {
    /// Stable snake-case label of the answer, for a log line or a metric
    /// dimension. Never carries the code an author chose.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::Resend => "resend",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Asks the external system what happened to a row whose outcome is unknown
/// (spec §16.5).
#[async_trait]
pub trait OutboxReconciler: Send + Sync {
    /// Settles one row against the remote system.
    async fn reconcile(&self, record: &OutboxRecord) -> Reconciled;
}

/// How the dispatcher works (all of it optional, all of it conservative).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DispatchConfig {
    /// Identifier this dispatcher claims rows under. Distinct per process.
    pub worker_id: String,
    /// Rows claimed per [`OutboxDispatcher::run_once`].
    pub batch_size: usize,
    /// Deadline for one [`OutboxSender::send`]. Exceeding it is an unknown
    /// outcome, not a retry.
    pub send_timeout: Duration,
    /// Delay before the second attempt of a row.
    pub initial_backoff: Duration,
    /// Ceiling on the computed delay.
    pub max_backoff: Duration,
    /// Multiplier applied per further attempt.
    pub backoff_multiplier: u32,
    /// Attempts a row gets before a retryable failure becomes a permanent one.
    pub max_attempts: u32,
}

impl DispatchConfig {
    /// Sixteen rows a sweep, thirty seconds a send, one second of backoff
    /// growing by four up to a minute, five attempts.
    #[must_use]
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            batch_size: 16,
            send_timeout: Duration::from_secs(30),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 4,
            max_attempts: 5,
        }
    }

    /// Returns a copy claiming at most `batch_size` rows a sweep.
    #[must_use]
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Returns a copy with another send deadline.
    #[must_use]
    pub const fn with_send_timeout(mut self, send_timeout: Duration) -> Self {
        self.send_timeout = send_timeout;
        self
    }

    /// Returns a copy with another attempt budget.
    #[must_use]
    pub const fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Returns a copy with another backoff schedule.
    #[must_use]
    pub const fn with_backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.initial_backoff = initial;
        self.max_backoff = max;
        self
    }

    /// The delay before attempt number `attempt`, 1-based.
    #[must_use]
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        let step = attempt.saturating_sub(1);
        if step == 0 {
            return self.initial_backoff;
        }
        self.initial_backoff
            .saturating_mul(self.backoff_multiplier.saturating_pow(step.min(16)))
            .min(self.max_backoff)
    }
}

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DispatchReport {
    /// Rows completed by the remote.
    pub completed: Vec<OutboxId>,
    /// Rows put back in the queue with a backoff.
    pub retried: Vec<OutboxId>,
    /// Rows the remote refused, or whose attempts ran out.
    pub failed: Vec<OutboxId>,
    /// Rows whose outcome nobody knows yet; a reconciler must settle them.
    pub unknown: Vec<OutboxId>,
    /// Rows the store refused to settle, left as the store has them.
    pub unsettled: Vec<OutboxId>,
}

impl DispatchReport {
    /// How many rows the sweep claimed.
    #[must_use]
    pub fn claimed(&self) -> usize {
        self.completed.len()
            + self.retried.len()
            + self.failed.len()
            + self.unknown.len()
            + self.unsettled.len()
    }

    /// Returns `true` when the sweep found nothing due.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.claimed() == 0
    }
}

/// Claims due outbox rows, sends them and settles each one.
#[derive(Clone)]
pub struct OutboxDispatcher {
    outbox: Arc<dyn OutboxStore>,
    sender: Arc<dyn OutboxSender>,
    config: DispatchConfig,
    observer: Arc<dyn Observer>,
}

impl fmt::Debug for OutboxDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutboxDispatcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OutboxDispatcher {
    /// Builds a dispatcher over `outbox`, sending through `sender`.
    #[must_use]
    pub fn new(
        outbox: Arc<dyn OutboxStore>,
        sender: Arc<dyn OutboxSender>,
        config: DispatchConfig,
    ) -> Self {
        Self {
            outbox,
            sender,
            config,
            observer: Arc::new(NoopObserver),
        }
    }

    /// Sends this module's signals to `observer` (spec §26.2, §28).
    ///
    /// The dispatcher is driven by the application's own task rather than by
    /// the orchestrator, so it is given its observer here rather than
    /// inheriting one. Without it the external half of the saga is invisible:
    /// [`ExternalLatency`](Signal::ExternalLatency) is the only measure of how
    /// long the remote system takes, and
    /// [`ExternalReconciled`](Signal::ExternalReconciled) is the other end of
    /// [`ExternalOutcomeUnknown`](Signal::ExternalOutcomeUnknown) — a rising
    /// count of unknowns with no reconciliations behind it is the shape of an
    /// operator who has stopped settling them.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = observer;
        self
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// Claims the rows due at `now`, sends each and settles it.
    ///
    /// This is the unit of work an application's own task calls. It returns
    /// when every claimed row has been settled — completed, rescheduled, failed
    /// or handed to reconciliation — so a caller that awaits it knows exactly
    /// what happened.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the claim itself could not be made. A row that could
    /// not be *settled* is not an error: it is reported in
    /// [`DispatchReport::unsettled`], and the store's claim timeout will release
    /// it for another sweep.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<DispatchReport, StoreError> {
        let claimed = self
            .outbox
            .claim_due(now, self.config.batch_size, &self.config.worker_id)
            .await?;
        let mut report = DispatchReport::default();
        for entry in claimed {
            self.dispatch_one(&entry, now, &mut report).await;
        }
        Ok(report)
    }

    /// Sends one claimed row and settles it.
    async fn dispatch_one(
        &self,
        entry: &OutboxEntry,
        now: DateTime<Utc>,
        report: &mut DispatchReport,
    ) {
        let outcome = self.send(entry).await;
        tracing::debug!(
            target: "turnframe.dispatch",
            outbox_id = %entry.outbox_id,
            destination = entry.destination.as_str(),
            attempt = entry.attempt_count,
            outcome = outcome.as_str(),
            "outbox row dispatched"
        );
        let settled = match &outcome {
            Dispatched::Completed { .. } => self.outbox.mark_completed(&entry.outbox_id).await,
            Dispatched::Unknown { remote_ref } => {
                self.outbox
                    .mark_outcome_unknown(&entry.outbox_id, remote_ref.clone())
                    .await
            }
            Dispatched::Permanent { code } => {
                self.outbox
                    .mark_failed(&entry.outbox_id, code.clone(), None)
                    .await
            }
            Dispatched::Retryable { code } => {
                if entry.attempt_count >= self.config.max_attempts {
                    self.outbox
                        .mark_failed(&entry.outbox_id, code::ATTEMPTS_EXHAUSTED.to_owned(), None)
                        .await
                } else {
                    let delay = self.config.backoff_for(entry.attempt_count);
                    // A schedule so long that chrono refuses it is a
                    // misconfiguration, not a reason to retry immediately.
                    let retry_at = now
                        + chrono::TimeDelta::from_std(delay)
                            .unwrap_or_else(|_| chrono::TimeDelta::hours(1));
                    self.outbox
                        .mark_failed(&entry.outbox_id, code.clone(), Some(retry_at))
                        .await
                }
            }
        };
        if let Err(error) = settled {
            tracing::warn!(
                target: "turnframe.dispatch",
                outbox_id = %entry.outbox_id,
                error = %error,
                "the outbox row could not be settled; it stays claimed until the claim expires"
            );
            report.unsettled.push(entry.outbox_id);
            return;
        }
        match outcome {
            Dispatched::Completed { .. } => report.completed.push(entry.outbox_id),
            Dispatched::Unknown { .. } => report.unknown.push(entry.outbox_id),
            Dispatched::Permanent { .. } => report.failed.push(entry.outbox_id),
            Dispatched::Retryable { .. } => {
                if entry.attempt_count >= self.config.max_attempts {
                    report.failed.push(entry.outbox_id);
                } else {
                    report.retried.push(entry.outbox_id);
                }
            }
        }
    }

    /// Sends one row under the configured deadline.
    ///
    /// A send that does not answer in time is an unknown outcome and never a
    /// retry: the bytes left, and nobody can say they did not land (I15).
    async fn send(&self, entry: &OutboxEntry) -> Dispatched {
        // The stage is the call to the remote system and not the bookkeeping
        // around it (§28), and it is measured whether the call answered,
        // refused or timed out: a send that hangs for the whole deadline is the
        // most interesting point in the distribution.
        let stage = Stage::enter();
        let outcome =
            match tokio::time::timeout(self.config.send_timeout, self.sender.send(entry)).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    tracing::warn!(
                        target: "turnframe.dispatch",
                        outbox_id = %entry.outbox_id,
                        "the send did not answer in time; the outcome is unknown, not a failure"
                    );
                    Dispatched::Unknown { remote_ref: None }
                }
            };
        stage.observe(
            self.observer.as_ref(),
            Signal::ExternalLatency,
            &SignalLabels::none(),
        );
        outcome
    }

    /// Settles one row whose outcome is unknown, through `reconciler`
    /// (spec §16.5).
    ///
    /// The row is read, handed to the reconciler and settled with its answer.
    /// A row that is not in [`OutboxStatus::OutcomeUnknown`] is left alone and
    /// reported as [`Reconciled::Unresolved`]: reconciliation is for the rows
    /// nobody knows about, and a completed row is not one of them.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the row could not be read or the settlement was
    /// refused.
    pub async fn reconcile(
        &self,
        outbox_id: &OutboxId,
        reconciler: &dyn OutboxReconciler,
        now: DateTime<Utc>,
    ) -> Result<Reconciled, StoreError> {
        let record = self.outbox.get(outbox_id).await?;
        if record.entry.status != OutboxStatus::OutcomeUnknown {
            return Ok(Reconciled::Unresolved);
        }
        let answer = reconciler.reconcile(&record).await;
        tracing::debug!(
            target: "turnframe.dispatch",
            outbox_id = %outbox_id,
            answer = answer.as_str(),
            "unknown outcome reconciled"
        );
        match &answer {
            Reconciled::Completed => self.outbox.mark_completed(outbox_id).await?,
            Reconciled::Failed { code } => {
                self.outbox
                    .mark_failed(outbox_id, code.clone(), None)
                    .await?;
            }
            Reconciled::Resend => self.outbox.reschedule(outbox_id, now).await?,
            Reconciled::Unresolved => {}
        }
        // An unknown outcome that is now known, and only then: `Unresolved`
        // settled nothing and the row is still waiting for the next sweep.
        if !matches!(answer, Reconciled::Unresolved) {
            self.observer
                .observe_labeled(&Signal::ExternalReconciled, &SignalLabels::none());
        }
        Ok(answer)
    }

    /// Releases rows a crashed dispatcher left in `Dispatching`, so another
    /// sweep can claim them.
    ///
    /// `claimed_before` is the age at which a claim is considered abandoned;
    /// it must be older than the longest send this dispatcher can make, or a
    /// slow send is released while it is still running.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the sweep could not be made.
    pub async fn release_expired_claims(
        &self,
        claimed_before: DateTime<Utc>,
    ) -> Result<Vec<OutboxId>, StoreError> {
        self.outbox.release_expired_claims(claimed_before).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_grows_and_is_capped() {
        let config =
            DispatchConfig::new("w").with_backoff(Duration::from_secs(1), Duration::from_secs(60));
        assert_eq!(config.backoff_for(0), Duration::from_secs(1));
        assert_eq!(config.backoff_for(1), Duration::from_secs(1));
        assert_eq!(config.backoff_for(2), Duration::from_secs(4));
        assert_eq!(config.backoff_for(3), Duration::from_secs(16));
        assert_eq!(config.backoff_for(9), Duration::from_secs(60));
        assert_eq!(config.backoff_for(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn a_report_counts_every_row_it_settled() {
        let mut report = DispatchReport::default();
        assert!(report.is_empty());
        report.completed.push(OutboxId::nil());
        report.unknown.push(OutboxId::nil());
        assert_eq!(report.claimed(), 2);
        assert!(!report.is_empty());
    }

    #[test]
    fn every_outcome_names_itself() {
        assert_eq!(Dispatched::completed().as_str(), "completed");
        assert_eq!(
            Dispatched::Retryable {
                code: "x".to_owned()
            }
            .as_str(),
            "retryable"
        );
        assert_eq!(
            Dispatched::Permanent {
                code: "x".to_owned()
            }
            .as_str(),
            "permanent"
        );
        assert_eq!(Dispatched::Unknown { remote_ref: None }.as_str(), "unknown");
        assert!(
            format!("{:?}", DispatchConfig::new("w")).contains("worker_id"),
            "the configuration is inspectable"
        );
    }
}
