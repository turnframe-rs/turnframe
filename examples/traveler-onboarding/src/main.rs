//! Traveler onboarding: a flat collection workflow, given in text and activated by a card.
//!
//! Four turns run against the sample traveler domain and the in-memory stores: two
//! fields nobody asked for arrive together and commit under one revision; the email
//! settles the last field and raises the activation card; a click activates the
//! traveler; a new contact address on the active traveler applies at once. Narration
//! is off.
//!
//! What each message is understood to say is scripted with
//! [`ScriptedUnderstanding`], so the program needs no key and no network. A real
//! deployment omits `.understander(..)`, and the runtime understands each turn
//! with small model tasks over the configured providers.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::sync::Arc;

use turnframe::flow::{CaseKey, CaseRef, WorkflowDefinition, WorkflowRegistry, check_view};
use turnframe::ids::{AccountId, CaseId, CaseRevision, ConversationId, TargetToken, TurnId};
use turnframe::locale::Locale;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::router::ProviderPool;
use turnframe::provider::trace::TracedProvider;
use turnframe::response::{AssistantTurn, ResponseBlock};
use turnframe::runtime::config::{NarrationConfig, OrchestratorConfig};
use turnframe::runtime::orchestrator::{CaseCandidate, Orchestrator, StaticCaseDirectory};
use turnframe::runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe::runtime::trace::JsonlTrace;
use turnframe::store::conversation::ConversationRecord;
use turnframe::testing::providers::{
    ScriptedProvider, ScriptedUnderstanding, UnderstandingBuilder,
};
use turnframe::testing::stores::FakeStores;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::traveler::{
    ACTIVATE_OPTION, OTHER_EMAIL, SAMPLE_EMAIL, SAMPLE_LOYALTY_NUMBER, SAMPLE_NAME, TravelerState,
    TravelerWorkflow, operations,
};
use turnframe::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe::understand::TurnUnderstander;

/// The tenant every turn in this example runs in.
/// Where `TURNFRAME_TRACE=1` writes: the repository's gitignored `traces/`.
const TRACES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../traces");

const ACCOUNT: &str = "aurora";

/// The workflow key of the sample traveler domain.
const TRAVELER: &str = "traveler";

/// The one case this example works on.
const CASE: &str = "trav-1";

/// The server-authored label understanding sees instead of the case identifier.
const LABEL: &str = "the new traveler";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let account = AccountId::from(ACCOUNT);
    let conversation = ConversationId::nil();
    let case_key = CaseKey::new(TRAVELER, CASE);
    let workflow = TravelerWorkflow::default();

    let travelers = Arc::new(InMemoryExecutor::new(TravelerWorkflow::default()));
    travelers.seed(
        &account,
        &CaseId::from(CASE),
        TravelerState::default(),
        CaseRevision(1),
    );

    let collect = TurnId::from(uuid::Uuid::from_u128(1));
    let complete = TurnId::from(uuid::Uuid::from_u128(2));
    let click = TurnId::from(uuid::Uuid::from_u128(3));
    let change = TurnId::from(uuid::Uuid::from_u128(4));

    // ---- what each of the three messages is understood to say ----------
    let collect_text =
        "Her loyalty number is AZ1234567, and her full name is Marta Bianchi by the way";
    let collect_understood = UnderstandingBuilder::of(collect_text)
        .apply(
            operations::SET_LOYALTY_NUMBER,
            token_for(&account, collect),
            serde_json::json!({ "value": SAMPLE_LOYALTY_NUMBER }),
            "Her loyalty number is AZ1234567",
        )
        .apply(
            operations::SET_NAME,
            token_for(&account, collect),
            serde_json::json!({ "value": SAMPLE_NAME }),
            "her full name is Marta Bianchi",
        )
        .build()?;

    let complete_text = "Her email is marta@aurora.example";
    let complete_understood = UnderstandingBuilder::of(complete_text)
        .apply(
            operations::CHANGE_EMAIL,
            token_for(&account, complete),
            serde_json::json!({ "value": SAMPLE_EMAIL }),
            complete_text,
        )
        .build()?;

    let change_text = "Send the notifications to marta.bianchi@aurora.example instead";
    let change_understood = UnderstandingBuilder::of(change_text)
        .apply(
            operations::CHANGE_EMAIL,
            token_for(&account, change),
            serde_json::json!({ "value": OTHER_EMAIL }),
            change_text,
        )
        .build()?;

    // Three understandings for three messages; a turn nobody scripted would be
    // understood as unreadable, not improvised.
    let understander = Arc::new(
        ScriptedUnderstanding::new()
            .then(collect_understood)
            .then(complete_understood)
            .then(change_understood),
    );
    // No steps: narration is off and understanding is scripted, so a single
    // model call would be a violation `verify` reports.
    let provider = ScriptedProvider::builder("scripted", "model-1").build_shared();

    // ---- the runtime ---------------------------------------------------
    let stores = FakeStores::new();
    // `TURNFRAME_TRACE=1` writes every turn event and model call to `traces/`.
    let trace = JsonlTrace::from_environment(TRACES)?.map(Arc::new);
    let traced: Arc<dyn ModelProvider> = match &trace {
        Some(trace) => Arc::new(TracedProvider::new(
            Arc::clone(&provider) as _,
            Arc::clone(trace) as _,
        )),
        None => Arc::clone(&provider) as _,
    };
    let builder = Orchestrator::builder()
        .workflows(Arc::new(
            WorkflowRegistry::builder()
                .register(TravelerWorkflow::default(), Arc::clone(&travelers))
                .build()?,
        ))
        .providers(Arc::new(
            ProviderPool::builder()
                .provider(traced)
                .build()?,
        ))
        .understander(Arc::clone(&understander) as Arc<dyn TurnUnderstander>)
        .stores(stores.stores().clone())
        .case_directory(Arc::new(
            StaticCaseDirectory::new().with_case(CaseCandidate::new(case_key.clone(), LABEL)),
        ))
        // Narration off: every line of the transcript below is server-owned.
        .config(quiet_config());
    let orchestrator = match &trace {
        Some(trace) => builder.trace(Arc::clone(trace) as _),
        None => builder,
    }
    .build()?;
    stores
        .stores()
        .conversations()
        .create_conversation(ConversationRecord::new(
            conversation,
            account.clone(),
            stores.now(),
        ))
        .await?;

    banner(
        "Traveler onboarding",
        "A flat collection workflow, information arriving in no particular order,\n\
         and one card: the one that activates the traveler.",
    );

    // -------------------------------------------------------------------
    section("1. An empty draft");
    print_view(
        &workflow,
        &load(&travelers, &account).await,
        revision(&travelers, &account),
    );
    println!("  Three obligations, none of them parameterized, and no card.");

    // -------------------------------------------------------------------
    section("2. Two fields nobody asked for, in the wrong order");
    println!("  user  {collect_text:?}\n");
    let turn = orchestrator
        .handle_turn(text_turn(&account, conversation, collect, collect_text))
        .await?;
    print_turn(&turn);
    print_view(
        &workflow,
        &load(&travelers, &account).await,
        revision(&travelers, &account),
    );
    println!(
        "  Both fields committed under one revision, and the only obligation left\n\
         \x20 is the one the user did not answer. Nothing had to be asked in order."
    );

    // -------------------------------------------------------------------
    section("3. The last field settles, and the activation card appears");
    println!("  user  {complete_text:?}\n");
    let turn = orchestrator
        .handle_turn(text_turn(&account, conversation, complete, complete_text))
        .await?;
    print_turn(&turn);
    let card = stores
        .open_interactions(&account, &case_key)
        .await?
        .into_iter()
        .find(|interaction| interaction.blocking)
        .expect("a settled draft asks for its activation");
    let ledger = stores.event_types(&account, &case_key).await?;
    assert!(
        !ledger.iter().any(|event| event == "traveler.activated"),
        "settling is not activating"
    );
    println!(
        "  Every field is given in text. Activation is the one step the user confirms,\n\
         \x20 on a card the server persisted before any sentence mentioned it."
    );

    // -------------------------------------------------------------------
    section("4. A click activates the traveler");
    let at = revision(&travelers, &account);
    println!("  user  [clicks {ACTIVATE_OPTION:?} on card {}]\n", card.id);
    let turn = orchestrator
        .handle_turn(TurnInput {
            interaction_response: Some(InteractionResponse {
                interaction_id: card.id,
                option_id: ACTIVATE_OPTION.into(),
                expected_case_revision: at,
                freeform_input: None,
            }),
            text: None,
            ..text_turn(&account, conversation, click, "")
        })
        .await?;
    print_turn(&turn);
    let ledger = stores.event_types(&account, &case_key).await?;
    assert_eq!(
        ledger.last().map(String::as_str),
        Some("traveler.activated"),
        "the click activates the traveler"
    );
    println!("  event ledger: {ledger:?}");

    // -------------------------------------------------------------------
    section("5. A new address on the active traveler applies at once");
    println!("  user  {change_text:?}\n");
    let turn = orchestrator
        .handle_turn(text_turn(&account, conversation, change, change_text))
        .await?;
    print_turn(&turn);
    let address = load(&travelers, &account)
        .await
        .and_then(|state| state.email);
    assert_eq!(address.as_deref(), Some(OTHER_EMAIL), "applied at once");
    assert!(
        stores
            .open_interactions(&account, &case_key)
            .await?
            .is_empty(),
        "no card for an address"
    );
    println!("  stored address: {address:?}");

    // -------------------------------------------------------------------
    section("6. Where the traveler ended up");
    print_view(
        &workflow,
        &load(&travelers, &account).await,
        revision(&travelers, &account),
    );

    provider.verify()?;
    assert_eq!(provider.call_count(), 0, "no model was called");
    assert_eq!(
        understander.remaining(),
        0,
        "every scripted message was used"
    );
    println!("  Three messages were understood and one card clicked; no model was called.\n");
    Ok(())
}

/// The conservative configuration with narration switched off, so every line
/// of the transcript is server-owned copy.
fn quiet_config() -> OrchestratorConfig {
    let mut config = OrchestratorConfig::conservative();
    config.narration = NarrationConfig::conservative().with_enabled(false);
    config
}

/// The opaque token the runtime will issue for the case in this turn.
fn token_for(account: &AccountId, turn_id: TurnId) -> TargetToken {
    TargetResolver::builder(account.clone(), turn_id)
        .candidate(AuthorizedCase::new(
            CaseRef::new(TRAVELER, CASE, CaseRevision::ZERO),
            LABEL,
        ))
        .build()
        .token_map()
        .token_for(&CaseKey::new(TRAVELER, CASE))
        .cloned()
        .expect("a token is issued for every authorized case")
}

fn text_turn(
    account: &AccountId,
    conversation: ConversationId,
    turn_id: TurnId,
    text: &str,
) -> TurnInput {
    TurnInput {
        turn_id,
        conversation_id: conversation,
        actor: ActorContext::new(account.clone(), "u1"),
        text: Some(text.to_owned()),
        interaction_response: None,
        attachments: Vec::new(),
        origin: None,
        locale: Locale::from("en-GB"),
        effort: None,
    }
}

async fn load(
    travelers: &Arc<InMemoryExecutor<TravelerWorkflow>>,
    account: &AccountId,
) -> Option<TravelerState> {
    turnframe::WorkflowExecutor::load(travelers.as_ref(), account, &CaseId::from(CASE))
        .await
        .expect("the in-memory executor always answers")
        .value
}

fn revision(
    travelers: &Arc<InMemoryExecutor<TravelerWorkflow>>,
    account: &AccountId,
) -> CaseRevision {
    travelers.revision_of(account, &CaseId::from(CASE))
}

fn print_view(workflow: &TravelerWorkflow, state: &Option<TravelerState>, revision: CaseRevision) {
    let view = workflow.project(CaseRef::new(TRAVELER, CASE, revision), state.as_ref());
    check_view(workflow, &view).expect("the sample projector keeps its own invariants");
    println!("  case          {TRAVELER}/{CASE} @ revision {revision}");
    println!(
        "  phase         {:?}  (owned by {:?})",
        view.phase,
        workflow.phase_ownership(&view.phase)
    );
    if view.obligations.is_empty() {
        println!("  obligations   none open");
    } else {
        println!("  obligations   {} open", view.obligations.len());
        for obligation in &view.obligations {
            println!("                  - {obligation:?}");
        }
    }
    match &view.blocking_interaction {
        Some(requirement) => println!(
            "  blocking card {} ({:?})",
            requirement.key, requirement.kind
        ),
        None => println!("  blocking card none"),
    }
    println!();
}

fn print_turn(turn: &AssistantTurn) {
    let locale = Locale::from("en-GB");
    if turn.blocks.is_empty() {
        println!("  reply         (no blocks)\n");
        return;
    }
    println!("  reply, block by block:");
    for block in &turn.blocks {
        match block {
            ResponseBlock::Receipt(receipt) => println!(
                "    receipt      {:<28} {:?}",
                receipt.receipt.status_code,
                receipt.receipt.title.resolve(&locale)
            ),
            ResponseBlock::Notice(notice) => println!(
                "    notice       {:<28} {:?}",
                notice.code,
                notice.text.resolve(&locale)
            ),
            ResponseBlock::Interaction(card) => println!(
                "    card         {:<28} {:?}",
                format!("{:?}", card.view.kind),
                card.view.title.resolve(&locale)
            ),
            ResponseBlock::Answer(answer) => println!(
                "    answer       {:<28} {:?}",
                format!("{:?}", answer.status),
                answer.text
            ),
            ResponseBlock::Transition(transition) => {
                println!("    transition   {:?}", transition.text);
            }
            other => println!("    block        {other:?}"),
        }
    }
    println!();
}

fn banner(title: &str, subtitle: &str) {
    println!("\n{}", "=".repeat(74));
    println!("{title}");
    println!("{}", "=".repeat(74));
    println!("{subtitle}");
}

fn section(title: &str) {
    println!(
        "\n-- {title} {}",
        "-".repeat(70_usize.saturating_sub(title.len()))
    );
}
