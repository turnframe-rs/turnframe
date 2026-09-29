# A minimal turn, end to end

Nothing below needs an API key, a database or a network. It uses the sample
travel domain, the in-memory stores and a scripted model from the
[`testing`](crate::testing) and [`tasks`](crate::tasks) modules, which is what the
`test-kit` feature is for. Swap the doubles for a real workflow, a real store and a
real adapter and the rest of the code is unchanged.

```rust
use std::sync::Arc;

use serde_json::json;
use turnframe::effort::Effort;
use turnframe::flow::{CaseKey, WorkflowRegistry};
use turnframe::ids::{AccountId, CaseId, CaseRevision, ConversationId, TurnId};
use turnframe::locale::Locale;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::router::ProviderPool;
use turnframe::runtime::config::{NarrationConfig, OrchestratorConfig};
use turnframe::runtime::orchestrator::{CaseCandidate, Orchestrator, StaticCaseDirectory};
use turnframe::store::conversation::ConversationRecord;
use turnframe::store::stores::Stores;
use turnframe::tasks::testing::ScriptedTasks;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::trip::{TripWorkflow, incomplete_case};
use turnframe::turn::{ActorContext, TurnInput};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
let account = AccountId::from("aurora");
let conversation = ConversationId::new();

// 1. A domain. The projector and the executor are the two things an adopter
//    writes; here they come ready-made from the test kit.
let trips = Arc::new(InMemoryExecutor::new(TripWorkflow::default()));
trips.seed(&account, &CaseId::from("trip-1"), incomplete_case(), CaseRevision(3));
let workflows = Arc::new(
    WorkflowRegistry::builder()
        .register(TripWorkflow::default(), Arc::clone(&trips))
        .build()?,
);

// 2. A model. Understanding is a few small tasks, each answered by id: split the
//    message into requests, check none was missed, route the request to an
//    operation, point at the value in the user's words, verify it.
let text = "Set the name of Trip 1 to Lisbon";
let tasks = Arc::new(
    ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", json!({
            "analysis": "One request.",
            "units": [{"kind": "request", "words": {"from": 1, "to": 8}, "workflow": "trip"}]
        }))
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/route", json!({"operations": ["trip.set_name"]}))
        .answer("u1/extract", json!({"arguments": {
            "value": {"kind": "words", "text": "Lisbon", "message": "current", "from": 8, "to": 8}
        }}))
        .answer("u1/verify", json!({
            "reason": "The user said so.", "arguments": {"value": "stated"}, "overall": "confirmed"
        })),
);
let providers = Arc::new(
    ProviderPool::builder()
        .provider(Arc::clone(&tasks) as Arc<dyn ModelProvider>)
        .build()?,
);

// 3. Persistence, and the records this user may address. The model never sees a
//    record identifier: it sees the label, and the runtime issues an opaque token.
let stores = Stores::in_memory();
stores
    .conversations()
    .create_conversation(ConversationRecord::new(conversation, account.clone(), chrono::Utc::now()))
    .await?;
let directory = StaticCaseDirectory::new()
    .with_case(CaseCandidate::new(CaseKey::new("trip", "trip-1"), "Trip 1"));

// Receipts, notices and cards only: nothing here writes prose.
let mut config = OrchestratorConfig::conservative();
config.narration = NarrationConfig::conservative().with_enabled(false);
let orchestrator = Orchestrator::builder()
    .workflows(workflows)
    .providers(providers)
    .stores(stores)
    .case_directory(Arc::new(directory))
    .config(config)
    .build()?;

// 4. One turn.
let answer = orchestrator
    .handle_turn(TurnInput {
        turn_id: TurnId::new(),
        conversation_id: conversation,
        actor: ActorContext::new(account.clone(), "u1"),
        text: Some(text.to_owned()),
        interaction_response: None,
        attachments: Vec::new(),
        origin: None,
        locale: Locale::from("en-GB"),
        // One reading per task, since the script answers each once; higher levels vote.
        effort: Some(Effort::Low),
    })
    .await?;

// The field really changed, under a new revision.
assert_eq!(trips.revision_of(&account, &CaseId::from("trip-1")), CaseRevision(4));

// And the reply says so only because a committed event backs it.
let receipts: Vec<&str> = answer.receipts().map(|r| r.status_code.as_str()).collect();
assert_eq!(receipts, ["trip.name_set"]);
assert!(answer.receipts().all(|receipt| receipt.is_event_backed()));
assert!(tasks.unanswered().is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
# })?;
# Ok(())
# }
```

Read that in the order the pipeline runs it. The projector turned the stored
trip into a view. Understanding split the message into one request, routed it
to `trip.set_name`, and pointed at the user's own words for the value, which
code sliced out of the message. The reducer resolved the one trip in view to a
case and compiled a typed command with an expected revision and an idempotency
key. The executor committed it. The receipt was rendered from the committed event,
not from a model's prose.
