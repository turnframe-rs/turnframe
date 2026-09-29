//! The desk's world: two orders in view, one in another shop, in-memory stores, a payment
//! provider behind the outbox, a fixed clock and fixed turn ids, so every run is the same.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use turnframe::command::{
    AtomicityScope, CommandBatch, CommandEnvelope, CommandOrigin, IdempotencyKey,
};
use turnframe::error::{ExecutionError, StoreError};
use turnframe::event::{Commit, OutboxEntry};
use turnframe::flow::{CaseKey, CaseRef, Versioned, WorkflowExecutor, WorkflowRegistry};
use turnframe::ids::{
    AccountId, BatchId, CaseId, CaseRevision, CommandId, ConversationId, EventId, TargetToken,
    TurnId,
};
use turnframe::interaction::Interaction;
use turnframe::locale::Locale;
use turnframe::provider::router::ProviderPool;
use turnframe::runtime::config::{NarrationConfig, OrchestratorConfig};
use turnframe::runtime::dispatch::{
    DispatchConfig, Dispatched, OutboxDispatcher, OutboxReconciler, OutboxSender, Reconciled,
};
use turnframe::runtime::orchestrator::{
    CaseCandidate, FixedTurnClock, Orchestrator, StaticCaseDirectory,
};
use turnframe::runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe::store::conversation::ConversationRecord;
use turnframe::store::outbox::{OutboxRecord, OutboxStore};
use turnframe::tasks::testing::ScriptedTasks;
use turnframe::testing::providers::{ScriptedProvider, ScriptedUnderstanding};
use turnframe::testing::stores::FakeStores;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe::understand::TurnUnderstander;
use turnframe::understanding::{ActId, Understanding, UnitId};

use crate::order::{OrderCommand, OrderEvent, OrderState, OrderWorkflow};

/// The desk's account.
pub const ACCOUNT: &str = "desk";
/// The account of another shop, whose orders the desk never sees.
pub const OTHER_SHOP: &str = "other-shop";
const WORKFLOW: &str = "order";

/// The instant every run happens at.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 9, 0, 0)
        .single()
        .expect("a valid instant")
}

/// The turn id numbered `n`.
#[must_use]
pub fn turn_id(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// The case id of order `number`.
#[must_use]
pub fn case_id(number: u32) -> CaseId {
    CaseId::from(format!("order-{number}"))
}

fn label(number: u32) -> String {
    format!("Order {number}")
}

/// The opaque token the runtime issues for order `number` in turn `turn`.
#[must_use]
pub fn token(turn: u128, number: u32) -> TargetToken {
    let key = CaseKey::new(WORKFLOW, case_id(number).as_str());
    TargetResolver::builder(AccountId::from(ACCOUNT), turn_id(turn))
        .candidate(AuthorizedCase::new(
            CaseRef::new(WORKFLOW, case_id(number).as_str(), CaseRevision::ZERO),
            label(number),
        ))
        .build()
        .token_map()
        .token_for(&key)
        .cloned()
        .expect("a token is issued for every authorized case")
}

/// The payment provider behind the outbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Takes the refund and says so.
    Accepts,
    /// Takes the refund and never answers.
    Silent,
}

struct Accepting;

#[async_trait]
impl OutboxSender for Accepting {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        Dispatched::completed()
    }
}

struct Silent;

#[async_trait]
impl OutboxSender for Silent {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Dispatched::completed()
    }
}

/// Asked later, the provider says it has the refund.
struct ProviderHasIt;

#[async_trait]
impl OutboxReconciler for ProviderHasIt {
    async fn reconcile(&self, _record: &OutboxRecord) -> Reconciled {
        Reconciled::Completed
    }
}

/// One event an order committed, from a turn or from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The order's number.
    pub order: u32,
    /// Its type, `order.refund_sent`.
    pub event_type: String,
    /// Its id.
    pub event_id: EventId,
    /// The revision the commit moved the order to.
    pub revision: CaseRevision,
}

/// The orders' executor, keeping the ledger of every event it commits: whoever writes, a
/// turn or an outside caller, writes through it. A replayed batch commits nothing new.
pub struct Ledgered {
    inner: InMemoryExecutor<OrderWorkflow>,
    ledger: Mutex<Vec<Entry>>,
}

impl std::fmt::Debug for Ledgered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledgered")
            .field("ledger", &self.entries())
            .finish_non_exhaustive()
    }
}

impl Ledgered {
    /// Every event committed so far, in order.
    pub fn entries(&self) -> Vec<Entry> {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The revision order `number` of `account` is at.
    #[must_use]
    pub fn revision_of(&self, account: &AccountId, number: u32) -> CaseRevision {
        self.inner.revision_of(account, &case_id(number))
    }
}

#[async_trait]
impl WorkflowExecutor<OrderWorkflow> for Ledgered {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<OrderState>>, StoreError> {
        WorkflowExecutor::load(&self.inner, account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<OrderCommand>,
    ) -> Result<Commit<OrderState, OrderEvent>, ExecutionError> {
        let number = batch
            .envelopes
            .first()
            .and_then(|envelope| envelope.case_ref.case_id.as_str().strip_prefix("order-"))
            .and_then(|number| number.parse().ok())
            .unwrap_or_default();
        let commit = self.inner.execute(batch).await?;
        if !commit.idempotency_replay {
            let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            ledger.extend(commit.events.iter().map(|event| Entry {
                order: number,
                event_type: event.event_type.clone(),
                event_id: event.event_id,
                revision: commit.new_revision,
            }));
        }
        Ok(commit)
    }
}

/// One run's world, and the runtime over it.
pub struct Desk {
    /// The stores: journal, interactions, outbox, turns.
    pub stores: FakeStores,
    /// The orders, and the ledger of what they committed.
    pub orders: Arc<Ledgered>,
    /// The desk's account.
    pub account: AccountId,
    /// The runtime.
    pub orchestrator: Orchestrator,
    conversation: ConversationId,
}

impl std::fmt::Debug for Desk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desk")
            .field("account", &self.account)
            .field("orders", &self.orders)
            .finish_non_exhaustive()
    }
}

/// The orders every run starts from.
fn seeded() -> Arc<Ledgered> {
    let orders = InMemoryExecutor::new(OrderWorkflow);
    let desk = AccountId::from(ACCOUNT);
    let seed = [
        OrderState::delivered(381, "Giulia Neri", 12_900, "2026-09-18", "2026-10-18"),
        OrderState::delivered(318, "Luca Moretti", 18_900, "2026-09-10", "2026-10-10"),
    ];
    for order in seed {
        orders.seed(&desk, &case_id(order.number), order, CaseRevision(1));
    }
    let other = OrderState::delivered(
        402,
        "a customer of another shop",
        12_900,
        "2026-09-25",
        "2026-10-25",
    );
    orders.seed(
        &AccountId::from(OTHER_SHOP),
        &case_id(402),
        other,
        CaseRevision(1),
    );
    Arc::new(Ledgered {
        inner: orders,
        ledger: Mutex::new(Vec::new()),
    })
}

/// The conservative configuration; with narration off, every line of a reply is the server's.
fn configured(narrating: bool) -> OrchestratorConfig {
    let mut config = OrchestratorConfig::conservative();
    config.narration = NarrationConfig::conservative().with_enabled(narrating);
    config
}

impl Desk {
    /// The desk with each message read as `understood` says, in order, and `answers` the
    /// model's wording of each answer to a question: only a model writes an answer, so a
    /// desk given answers narrates.
    ///
    /// # Errors
    ///
    /// When the runtime or the conversation cannot be built.
    pub async fn scripted(
        understood: Vec<Understanding>,
        answers: &[&str],
    ) -> anyhow::Result<Self> {
        let understander = understood
            .into_iter()
            .fold(ScriptedUnderstanding::new(), ScriptedUnderstanding::then);
        let provider = answers
            .iter()
            .fold(
                ScriptedProvider::builder("scripted", "model-1"),
                |script, answer| script.answering(*answer),
            )
            .build_shared();
        let pool = ProviderPool::builder().provider(provider as _).build()?;
        Self::build(pool, Some(Arc::new(understander)), !answers.is_empty()).await
    }

    /// The desk reading each message with the real understanding pipeline, whose model calls
    /// `tasks` answers.
    ///
    /// # Errors
    ///
    /// When the runtime or the conversation cannot be built.
    pub async fn reading(tasks: ScriptedTasks) -> anyhow::Result<Self> {
        let pool = ProviderPool::builder()
            .provider(Arc::new(tasks) as _)
            .build()?;
        Self::build(pool, None, false).await
    }

    async fn build(
        pool: ProviderPool,
        understander: Option<Arc<ScriptedUnderstanding>>,
        narrating: bool,
    ) -> anyhow::Result<Self> {
        let orders = seeded();
        let stores = FakeStores::at(now());
        let mut directory = StaticCaseDirectory::new();
        for number in [381, 318] {
            directory = directory.with_case(CaseCandidate::new(
                CaseKey::new(WORKFLOW, case_id(number).as_str()),
                label(number),
            ));
        }
        let mut builder = Orchestrator::builder()
            .workflows(Arc::new(
                WorkflowRegistry::builder()
                    .register(OrderWorkflow, Arc::clone(&orders))
                    .build()?,
            ))
            .providers(Arc::new(pool))
            .stores(stores.stores().clone())
            .case_directory(Arc::new(directory))
            .clock(Arc::new(FixedTurnClock(now())))
            .config(configured(narrating));
        if let Some(understander) = understander {
            builder = builder.understander(understander as Arc<dyn TurnUnderstander>);
        }
        let orchestrator = builder.build()?;
        let account = AccountId::from(ACCOUNT);
        let conversation = ConversationId::nil();
        stores
            .stores()
            .conversations()
            .create_conversation(ConversationRecord::new(
                conversation,
                account.clone(),
                stores.now(),
            ))
            .await?;
        Ok(Self {
            stores,
            orders,
            account,
            orchestrator,
            conversation,
        })
    }

    /// A message, as turn `turn`.
    #[must_use]
    pub fn send(&self, turn: u128, text: &str) -> TurnInput {
        TurnInput {
            turn_id: turn_id(turn),
            conversation_id: self.conversation,
            actor: ActorContext::new(self.account.clone(), "agent"),
            text: Some(text.to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("en-GB"),
            effort: None,
        }
    }

    /// A click on `option` of `card`, at the revision the card was drawn for, as turn `turn`.
    #[must_use]
    pub fn click(&self, turn: u128, card: &Interaction, option: &str) -> TurnInput {
        TurnInput {
            text: None,
            interaction_response: Some(InteractionResponse {
                interaction_id: card.id,
                option_id: option.into(),
                expected_case_revision: card.case_ref.expected_revision,
                freeform_input: None,
            }),
            ..self.send(turn, "")
        }
    }

    fn key(number: u32) -> CaseKey {
        CaseKey::new(WORKFLOW, case_id(number).as_str())
    }

    /// The blocking card open on order `number`, if any.
    pub async fn card(&self, number: u32) -> Option<Interaction> {
        self.stores
            .open_interactions(&self.account, &Self::key(number))
            .await
            .expect("the store answers")
            .into_iter()
            .find(|card| card.blocking)
    }

    /// The event types of order `number`, in the order they were committed.
    #[must_use]
    pub fn events(&self, number: u32) -> Vec<String> {
        self.orders
            .entries()
            .into_iter()
            .filter(|entry| entry.order == number)
            .map(|entry| entry.event_type)
            .collect()
    }

    /// Order `number` as it stands.
    pub async fn order(&self, number: u32) -> OrderState {
        WorkflowExecutor::load(self.orders.as_ref(), &self.account, &case_id(number))
            .await
            .expect("the in-memory executor answers")
            .value
            .expect("the order exists")
    }

    /// The revision order `number` is at.
    #[must_use]
    pub fn revision(&self, number: u32) -> CaseRevision {
        self.orders.revision_of(&self.account, number)
    }

    /// A command from outside any turn, under a key naming its delivery.
    ///
    /// # Errors
    ///
    /// When the domain refuses it or the executor fails.
    pub async fn outside(
        &self,
        number: u32,
        key: &str,
        command: OrderCommand,
    ) -> anyhow::Result<()> {
        self.deliver(number, key, command, 1).await?;
        Ok(())
    }

    /// The same outside delivery `times` times, as a sender that retries does: the batch is
    /// one, so it is one effect. Returns, per delivery, whether it was a replay.
    ///
    /// # Errors
    ///
    /// When the domain refuses it or the executor fails.
    pub async fn deliver(
        &self,
        number: u32,
        key: &str,
        command: OrderCommand,
        times: usize,
    ) -> anyhow::Result<Vec<bool>> {
        let seed = key.bytes().fold(0xa1_u128, |hash, byte| {
            hash.wrapping_mul(131).wrapping_add(u128::from(byte))
        });
        let turn_id = TurnId::from(uuid::Uuid::from_u128(seed));
        let case_ref = CaseRef::new(WORKFLOW, case_id(number).as_str(), self.revision(number));
        let batch = CommandBatch {
            batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
            scope: AtomicityScope::PerCase,
            envelopes: vec![CommandEnvelope {
                command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
                turn_id,
                actor: ActorContext::new(self.account.clone(), "outside"),
                case_ref,
                idempotency_key: IdempotencyKey::new(format!("outside:{key}")),
                origin: CommandOrigin::ExternalCallback {
                    callback_id: key.to_owned(),
                    signature_verified: true,
                },
                command,
            }],
        };
        let mut replays = Vec::new();
        for _ in 0..times {
            replays.push(self.orders.execute(batch.clone()).await?.idempotency_replay);
        }
        Ok(replays)
    }

    fn dispatcher(&self, provider: Provider) -> OutboxDispatcher {
        let sender: Arc<dyn OutboxSender> = match provider {
            Provider::Accepts => Arc::new(Accepting),
            Provider::Silent => Arc::new(Silent),
        };
        OutboxDispatcher::new(
            Arc::clone(self.stores.memory()) as Arc<dyn OutboxStore>,
            sender,
            DispatchConfig::new("desk").with_send_timeout(Duration::from_millis(20)),
        )
    }

    /// Sends what the outbox holds to the payment provider, once.
    ///
    /// # Errors
    ///
    /// When the outbox cannot be read or written.
    pub async fn dispatch(&self, provider: Provider) -> anyhow::Result<()> {
        self.dispatcher(provider)
            .run_once(self.stores.now())
            .await?;
        Ok(())
    }

    /// The outbox rows of the commands turn `turn` ran.
    ///
    /// # Errors
    ///
    /// When the journal or the outbox cannot be read.
    pub async fn outbox(&self, turn: u128) -> anyhow::Result<Vec<OutboxRecord>> {
        let mut rows = Vec::new();
        for entry in self
            .stores
            .journal_for_turn(&self.account, &turn_id(turn))
            .await?
        {
            rows.extend(self.stores.outbox_for_command(&entry.command_id).await?);
        }
        Ok(rows)
    }

    /// Asks the payment provider what became of `row`.
    ///
    /// # Errors
    ///
    /// When the outbox cannot be read or written.
    pub async fn reconcile(&self, row: &OutboxRecord) -> anyhow::Result<Reconciled> {
        Ok(self
            .dispatcher(Provider::Silent)
            .reconcile(&row.entry.outbox_id, &ProviderHasIt, self.stores.now())
            .await?)
    }
}
