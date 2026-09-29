//! The records the user may address: every trip and traveler the executors hold, so
//! a record the assistant opened is there to talk about on the next turn.

use std::sync::Arc;

use turnframe::StoreError;
use turnframe::flow::CaseKey;
use turnframe::ids::{AccountId, CaseId, ConversationId, WorkflowKey};
use turnframe::runtime::orchestrator::{CaseCandidate, CaseDirectory};
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::traveler::TravelerWorkflow;
use turnframe::testing::workflows::trip::TripWorkflow;
use turnframe::turn::ActorContext;

use crate::{TRAVELER, TRIP};

/// The console's records: trips as «Trip 1», «Trip 2», … in the order they
/// were opened, travelers by their registered name.
#[derive(Clone)]
pub struct Records {
    pub trips: Arc<InMemoryExecutor<TripWorkflow>>,
    pub travelers: Arc<InMemoryExecutor<TravelerWorkflow>>,
}

impl Records {
    /// Every record with its label: trips first, each list oldest first.
    pub fn labelled(&self, account: &AccountId) -> Vec<(CaseKey, String)> {
        let trips = self
            .trips
            .case_ids(account)
            .into_iter()
            .enumerate()
            .map(|(index, case_id)| (CaseKey::new(TRIP, case_id), format!("Trip {}", index + 1)));
        let travelers = self
            .travelers
            .case_ids(account)
            .into_iter()
            .enumerate()
            .map(|(index, case_id)| {
                let name = self
                    .travelers
                    .state_of(account, &case_id)
                    .and_then(|state| state.full_name);
                let label = name.unwrap_or_else(|| format!("New traveler {}", index + 1));
                (CaseKey::new(TRAVELER, case_id), label)
            });
        trips.chain(travelers).collect()
    }

    /// The revision a record is at.
    pub fn revision_of(&self, account: &AccountId, key: &CaseKey) -> turnframe::ids::CaseRevision {
        let case_id: &CaseId = &key.case_id;
        if key.workflow.as_str() == TRAVELER {
            self.travelers.revision_of(account, case_id)
        } else {
            self.trips.revision_of(account, case_id)
        }
    }
}

#[async_trait::async_trait]
impl CaseDirectory for Records {
    async fn candidates(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        Ok(self
            .labelled(&actor.account_id)
            .into_iter()
            .map(|(key, label)| CaseCandidate::new(key, label))
            .collect())
    }

    /// The records of `workflow` whose name holds the words the user named them by, as
    /// a search box over the application's own records would find them.
    async fn find(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
        workflow: &WorkflowKey,
        named: Option<&str>,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let Some(named) = named.map(str::to_lowercase) else {
            return Ok(Vec::new());
        };
        Ok(self
            .labelled(&actor.account_id)
            .into_iter()
            .filter(|(key, label)| {
                key.workflow == *workflow && label.to_lowercase().contains(&named)
            })
            .map(|(key, label)| CaseCandidate::new(key, label))
            .collect())
    }
}
