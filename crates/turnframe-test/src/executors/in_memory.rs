//! The reference [`ExecutorFactory`]: the kit's own in-memory executor, wired
//! up for any [`PureWorkflow`].
//!
//! It is here for three reasons. It is what proves the suite is runnable
//! against a real implementation rather than only compilable. It is the
//! smallest complete example of what an adopter has to write to point the suite
//! at their own executor. And it is what the kit's own falsification tests
//! break on purpose, one rule at a time, to prove each check catches the defect
//! it claims to catch and nothing else.

use std::fmt;

use turnframe_core::case::CaseRef;
use turnframe_core::ids::{AccountId, UserId};
use turnframe_core::turn::ActorContext;

use super::{BatchOf, ExecutorFactory, SeedOf, SeededCase};
use crate::workflows::{InMemoryExecutor, PureWorkflow};

/// Account the reference factory seeds its case in.
pub const CONFORMANCE_ACCOUNT: &str = "turnframe-executor-conformance";

/// User the reference factory acts as.
pub const CONFORMANCE_USER: &str = "conformance-user";

/// An [`ExecutorFactory`] over [`InMemoryExecutor`], for any workflow whose
/// transitions are a pure function.
///
/// ```
/// use turnframe_core::case::CaseRef;
/// use turnframe_core::ids::CaseRevision;
/// use turnframe_test::executors::{self, InMemoryCase};
/// use turnframe_test::workflows::trip::{
///     TripCommand, TripWorkflow, complete_case, sample_new_extra,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # tokio::runtime::Runtime::new()?.block_on(async {
/// let case = InMemoryCase::<TripWorkflow>::new(
///     CaseRef::new("trip", "trip-conformance", CaseRevision(4)),
///     Some(complete_case()),
///     TripCommand::AddExtra { extra: sample_new_extra(1) },
///     TripCommand::SetName { value: "Another name".to_owned() },
///     TripCommand::Rebook,
/// );
///
/// let report = executors::run_all(&case).await;
/// assert!(report.passed(), "{report}");
/// # });
/// # Ok(())
/// # }
/// ```
pub struct InMemoryCase<W: PureWorkflow> {
    account: AccountId,
    user: UserId,
    case_ref: CaseRef,
    seed_state: Option<W::State>,
    first: W::Command,
    second: W::Command,
    refused: W::Command,
}

impl<W: PureWorkflow> InMemoryCase<W> {
    /// Describes a case and the three commands the suite drives it with.
    ///
    /// `seed_state` is the state the case starts from, installed at
    /// `case_ref.expected_revision`; `None` means the case does not exist yet,
    /// in which case the revision must be
    /// [`CaseRevision::ZERO`](turnframe_core::ids::CaseRevision::ZERO) and
    /// `first` must be the command that brings it into existence.
    ///
    /// See [`SeededCase`] for what the three commands have to satisfy — in
    /// particular that `first` must not be naturally idempotent.
    #[must_use]
    pub fn new(
        case_ref: CaseRef,
        seed_state: Option<W::State>,
        first: W::Command,
        second: W::Command,
        refused: W::Command,
    ) -> Self {
        Self {
            account: AccountId::from(CONFORMANCE_ACCOUNT),
            user: UserId::from(CONFORMANCE_USER),
            case_ref,
            seed_state,
            first,
            second,
            refused,
        }
    }

    /// Runs the suite as another tenant.
    #[must_use]
    pub fn with_actor(mut self, account: impl Into<AccountId>, user: impl Into<UserId>) -> Self {
        self.account = account.into();
        self.user = user.into();
        self
    }

    /// The case the factory seeds.
    #[must_use]
    pub const fn case_ref(&self) -> &CaseRef {
        &self.case_ref
    }
}

impl<W: PureWorkflow> fmt::Debug for InMemoryCase<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryCase")
            .field("account", &self.account)
            .field("case_ref", &self.case_ref)
            .field("seeded", &self.seed_state.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl<W> ExecutorFactory for InMemoryCase<W>
where
    W: PureWorkflow + Default,
{
    type Workflow = W;
    type Executor = InMemoryExecutor<W>;

    async fn seed(&self) -> Result<SeedOf<Self>, String> {
        let executor = InMemoryExecutor::<W>::default();
        if let Some(state) = self.seed_state.clone() {
            executor.seed(
                &self.account,
                &self.case_ref.case_id,
                state,
                self.case_ref.expected_revision,
            );
        }
        Ok(SeededCase::new(
            executor,
            ActorContext::new(self.account.clone(), self.user.clone()),
            self.case_ref.clone(),
            self.first.clone(),
            self.second.clone(),
            self.refused.clone(),
        ))
    }

    async fn interrupt_after(
        &self,
        seeded: &SeedOf<Self>,
        batch: &BatchOf<Self>,
        applied: usize,
    ) -> Result<(), String> {
        seeded
            .executor
            .execute_prefix(batch, applied)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }
}
